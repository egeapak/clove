//! A TTL cache for the discovery result, so `plugin list --all` stays fast and
//! keeps working offline after a first successful fetch.
//!
//! The clock is a **parameter**, never `Utc::now()` inline, so expiry is testable
//! without sleeping. Writes go through a temp file + rename so a crash or a
//! concurrent run can never leave a half-written cache — the read path tolerates
//! corruption anyway, but preventing beats recovering.
//!
//! Scope note: this cache serves `list --all` and `search` only. `install`
//! fetches its verification evidence **live** and never reads this file — a
//! cache is a file anyone who can set `$CLOVE_HOME` can write, and it must not
//! be able to decide what counts as a plugin. That boundary is only meaningful
//! alongside the one on `$CLOVE_REGISTRY_URL`; see `crates_io.rs`.

use camino::{Utf8Path, Utf8PathBuf};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use super::RegistryPlugin;

/// The cache-file format version, so a future shape change can be migrated (or
/// simply ignored) rather than mis-parsed.
// Bumped to 2 when `latest` stopped including pre-releases: a v1 file's `latest`
// may hold a `2.0.0-alpha.1` under the old meaning, and a schema mismatch is
// already a cache miss, so one refetch retires every stale file. Adding the
// field alone would not have done it — the *meaning* of an existing field
// changed, which is exactly what the version number is for.
const CACHE_SCHEMA: u32 = 2;

/// How long a cached discovery result stays fresh.
pub const TTL: Duration = Duration::hours(24);

/// The cache file, relative to the clove home directory.
const FILE_NAME: &str = "registry-cache.json";

/// How many temp-file names to try before giving up on writing the cache.
const MAX_TEMP_ATTEMPTS: u32 = 8;

/// A ceiling on the cache file read back from disk. The cache is written from
/// a network response, so a hostile or misconfigured registry could otherwise
/// persist an arbitrarily large file that is re-read and re-parsed on every
/// invocation for the next 24 hours.
const MAX_CACHE_BYTES: u64 = 16 * 1024 * 1024;

/// The registry root this process is configured against.
fn current_root() -> String {
    std::env::var(super::crates_io::API_ROOT_ENV)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| super::crates_io::DEFAULT_API_ROOT.to_owned())
}

#[derive(Debug, Serialize, Deserialize)]
struct CacheFile {
    schema: u32,
    /// Which registry produced this. A cache file says nothing about *where*
    /// its answers came from otherwise, so pointing `$CLOVE_REGISTRY_URL` at a
    /// different registry kept serving the previous one's plugins until the TTL
    /// expired.
    #[serde(default)]
    registry_root: String,
    fetched_at: DateTime<Utc>,
    /// `None` records that the registry itself is absent (`clove-plugin`
    /// unpublished) — distinct from a published registry with no dependents.
    plugins: Option<Vec<CachedPlugin>>,
}

#[derive(Debug, Serialize, Deserialize)]
struct CachedPlugin {
    crate_name: String,
    #[serde(default)]
    latest: Option<String>,
    #[serde(default)]
    latest_prerelease: Option<String>,
    #[serde(default)]
    latest_yanked: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    repository: Option<String>,
    #[serde(default)]
    bin_names: Vec<String>,
    #[serde(default)]
    published_by: Option<String>,
    #[serde(default)]
    downloads: u64,
}

impl From<&RegistryPlugin> for CachedPlugin {
    fn from(p: &RegistryPlugin) -> Self {
        CachedPlugin {
            crate_name: p.crate_name.clone(),
            latest: p.latest.as_ref().map(|v| v.to_string()),
            latest_prerelease: p.latest_prerelease.as_ref().map(|v| v.to_string()),
            latest_yanked: p.latest_yanked.as_ref().map(|v| v.to_string()),
            description: p.description.clone(),
            repository: p.repository.clone(),
            bin_names: p.bin_names.clone(),
            published_by: p.published_by.clone(),
            downloads: p.downloads,
        }
    }
}

impl From<CachedPlugin> for RegistryPlugin {
    fn from(c: CachedPlugin) -> Self {
        RegistryPlugin {
            crate_name: c.crate_name,
            latest: c.latest.and_then(|v| semver::Version::parse(&v).ok()),
            latest_prerelease: c
                .latest_prerelease
                .and_then(|v| semver::Version::parse(&v).ok()),
            latest_yanked: c
                .latest_yanked
                .and_then(|v| semver::Version::parse(&v).ok()),
            description: c.description,
            repository: c.repository,
            bin_names: c.bin_names,
            published_by: c.published_by,
            downloads: c.downloads,
        }
    }
}

/// The cache file path inside `home`.
fn path_in(home: &Utf8Path) -> Utf8PathBuf {
    home.join(FILE_NAME)
}

/// Read the cache if it exists and is still fresh at `now`.
///
/// Returns `None` for every failure mode — absent, unreadable, corrupt, wrong
/// schema, or expired — because a cache miss is always recoverable by fetching.
/// A corrupt cache must never be fatal.
pub fn read(
    home: &Utf8Path,
    now: DateTime<Utc>,
    ttl: Duration,
) -> Option<Option<Vec<RegistryPlugin>>> {
    let path = path_in(home);
    // Refuse an implausibly large cache rather than reading it into memory.
    if std::fs::metadata(&path).ok()?.len() > MAX_CACHE_BYTES {
        return None;
    }
    let raw = std::fs::read_to_string(&path).ok()?;
    let parsed: CacheFile = serde_json::from_str(&raw).ok()?;
    if parsed.registry_root != current_root() {
        // A cache written against a different registry answers for a registry
        // the user is no longer pointing at — `list --all` served the previous
        // one's plugins for 24h, and only `--refresh` escaped.
        return None;
    }
    if parsed.schema != CACHE_SCHEMA {
        return None;
    }
    // A cache stamped in the future (clock skew, a restored backup) is treated as
    // stale rather than trusted indefinitely.
    if now < parsed.fetched_at || now - parsed.fetched_at > ttl {
        return None;
    }
    Some(
        parsed
            .plugins
            .map(|plugins| plugins.into_iter().map(RegistryPlugin::from).collect()),
    )
}

/// Write the discovery result, stamped at `now`.
///
/// Best-effort: a cache that cannot be written is not an error worth failing a
/// read-only command over, so this returns `()` and swallows I/O failures.
pub fn write(home: &Utf8Path, now: DateTime<Utc>, plugins: Option<&[RegistryPlugin]>) {
    let file = CacheFile {
        schema: CACHE_SCHEMA,
        registry_root: current_root(),
        fetched_at: now,
        plugins: plugins.map(|ps| ps.iter().map(CachedPlugin::from).collect()),
    };
    let Ok(encoded) = serde_json::to_string_pretty(&file) else {
        return;
    };
    if std::fs::create_dir_all(home).is_err() {
        return;
    }
    // Temp + rename: a torn write can never be observed, even if two runs race.
    //
    // The temp file is created with `create_new` (`O_EXCL`), which fails rather
    // than following a symlink. A predictable name written with plain
    // `fs::write` is an arbitrary-file-truncation primitive: `O_CREAT|O_TRUNC`
    // follows symlinks, so anyone able to pre-plant
    // `.registry-cache.json.tmp<pid>` in the clove home chooses the file that
    // gets clobbered — and `rename` then moves the *symlink* into place, so it
    // survives and the victim is re-clobbered on every later run. PIDs are
    // small and sequential, so blanketing the range is trivial. That needs write
    // access to the clove home, which the default user-owned path does not give
    // away — but a shared `$CLOVE_HOME` (a CI cache dir, `/tmp/shared`) does.
    for attempt in 0..MAX_TEMP_ATTEMPTS {
        let temp = home.join(format!(".{FILE_NAME}.tmp{}-{attempt}", std::process::id()));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
        {
            Ok(mut file) => {
                use std::io::Write;
                let wrote = file
                    .write_all(encoded.as_bytes())
                    .and_then(|()| file.sync_all());
                drop(file);
                if wrote.is_err() || std::fs::rename(&temp, path_in(home)).is_err() {
                    let _ = std::fs::remove_file(&temp);
                }
                return;
            }
            // Taken (a concurrent run, or a planted symlink): try the next name.
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            // Anything else (no permission, missing dir) is not worth failing a
            // read-only command over.
            Err(_) => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Vec<RegistryPlugin> {
        vec![RegistryPlugin {
            crate_name: "clove-sync-gitlab".to_owned(),
            latest: Some(semver::Version::new(0, 10, 0)),
            latest_prerelease: None,
            latest_yanked: Some(semver::Version::new(0, 11, 0)),
            description: Some("Two-way GitLab sync".to_owned()),
            repository: None,
            bin_names: vec!["clove-sync-gitlab".to_owned()],
            published_by: Some("someone".to_owned()),
            downloads: 41,
        }]
    }

    fn home() -> (tempfile::TempDir, Utf8PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = Utf8PathBuf::from_path_buf(dir.path().to_owned()).unwrap();
        (dir, path)
    }

    #[test]
    fn fresh_cache_round_trips() {
        let (_dir, home) = home();
        let now = Utc::now();
        write(&home, now, Some(&sample()));

        let read_back = read(&home, now + Duration::hours(1), TTL).expect("fresh hit");
        let plugins = read_back.expect("registry present");
        assert_eq!(plugins.len(), 1);
        assert_eq!(plugins[0].crate_name, "clove-sync-gitlab");
        assert_eq!(
            plugins[0].latest.as_ref(),
            Some(&semver::Version::new(0, 10, 0)),
            "semver must survive the string round-trip through the cache"
        );
        assert_eq!(
            plugins[0].latest_yanked,
            Some(semver::Version::new(0, 11, 0))
        );
    }

    #[test]
    fn expired_cache_is_a_miss() {
        let (_dir, home) = home();
        let now = Utc::now();
        write(&home, now, Some(&sample()));
        assert!(read(&home, now + TTL + Duration::minutes(1), TTL).is_none());
        // Still fresh one minute before expiry.
        assert!(read(&home, now + TTL - Duration::minutes(1), TTL).is_some());
    }

    #[test]
    fn corrupt_cache_is_a_miss_not_a_failure() {
        let (_dir, home) = home();
        std::fs::write(path_in(&home), "{ this is not json").unwrap();
        assert!(read(&home, Utc::now(), TTL).is_none());

        // Valid JSON of the wrong shape is equally survivable.
        std::fs::write(path_in(&home), r#"{"schema":1}"#).unwrap();
        assert!(read(&home, Utc::now(), TTL).is_none());
    }

    #[test]
    fn a_future_stamped_cache_is_stale() {
        // Clock skew or a restored backup must not pin a cache as fresh forever.
        let (_dir, home) = home();
        let now = Utc::now();
        write(&home, now + Duration::hours(48), Some(&sample()));
        assert!(read(&home, now, TTL).is_none());
    }

    #[test]
    fn a_cache_from_another_registry_is_a_miss() {
        // `$CLOVE_REGISTRY_URL` is a documented seam, and the cache file is a
        // single path under `$CLOVE_HOME` — so without this the answers from
        // one registry were served for the next 24h after pointing at another.
        let home = tempfile::tempdir().unwrap();
        let home = Utf8Path::from_path(home.path()).unwrap();
        let now = Utc::now();

        std::fs::write(
            path_in(home),
            format!(
                r#"{{"schema":{CACHE_SCHEMA},"registry_root":"https://other.example/api/v1",
                    "fetched_at":"{}","plugins":[]}}"#,
                now.to_rfc3339()
            ),
        )
        .unwrap();

        assert!(
            read(home, now, Duration::hours(24)).is_none(),
            "a cache written against a different registry must not answer for this one"
        );
    }

    #[test]
    fn wrong_schema_is_a_miss() {
        let (_dir, home) = home();
        let body = format!(
            r#"{{"schema":99,"fetched_at":"{}","plugins":[]}}"#,
            Utc::now().to_rfc3339()
        );
        std::fs::write(path_in(&home), body).unwrap();
        assert!(read(&home, Utc::now(), TTL).is_none());
    }

    #[test]
    fn absent_registry_round_trips_distinctly_from_empty() {
        let (_dir, home) = home();
        let now = Utc::now();

        // Absent: `clove-plugin` is not published.
        write(&home, now, None);
        assert_eq!(read(&home, now, TTL), Some(None));

        // Empty: published, but nothing depends on it yet.
        write(&home, now, Some(&[]));
        assert_eq!(read(&home, now, TTL), Some(Some(vec![])));
    }

    #[test]
    fn a_planted_symlink_cannot_redirect_the_cache_write() {
        // The temp name is predictable (pid-based), so if it were created with
        // plain `fs::write` (O_CREAT|O_TRUNC, follows symlinks) anyone able to
        // write to the clove home could pre-plant a link and have clove truncate
        // an arbitrary file — and `rename` would then move the *symlink* into
        // place, re-clobbering the victim on every later run.
        #[cfg(unix)]
        {
            let (_dir, home) = home();
            std::fs::create_dir_all(&home).unwrap();
            let victim = home.join("victim");
            std::fs::write(&victim, "ORIGINAL").unwrap();

            // Plant links over the whole name range this pid would use.
            for attempt in 0..8 {
                let temp = home.join(format!(".{FILE_NAME}.tmp{}-{attempt}", std::process::id()));
                std::os::unix::fs::symlink(&victim, &temp).unwrap();
            }

            write(&home, Utc::now(), Some(&sample()));

            assert_eq!(
                std::fs::read_to_string(&victim).unwrap(),
                "ORIGINAL",
                "the symlink target must not be written through"
            );
        }
    }

    #[test]
    fn an_oversized_cache_file_is_refused() {
        let (_dir, home) = home();
        std::fs::create_dir_all(&home).unwrap();
        // Valid JSON, but larger than the ceiling: must not be read into memory.
        let filler = "x".repeat((MAX_CACHE_BYTES + 1) as usize);
        std::fs::write(path_in(&home), format!(r#"{{"pad":"{filler}"}}"#)).unwrap();
        assert!(read(&home, Utc::now(), TTL).is_none());
    }

    #[test]
    fn write_leaves_no_temp_file_behind() {
        let (_dir, home) = home();
        write(&home, Utc::now(), Some(&sample()));
        let stray: Vec<_> = std::fs::read_dir(&home)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|name| name.contains(".tmp"))
            .collect();
        assert!(stray.is_empty(), "temp files left behind: {stray:?}");
    }

    #[test]
    fn write_creates_the_home_directory() {
        let (_dir, home) = home();
        let nested = home.join("nested/clove");
        write(&nested, Utc::now(), Some(&sample()));
        assert!(read(&nested, Utc::now(), TTL).is_some());
    }
}
