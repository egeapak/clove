//! The crates.io API client: the name probe and the reverse-dependency join.
//!
//! Pure over the [`Fetch`] trait — no `ureq` here, so every test in this file is
//! offline and driven by JSON fixtures recorded from the live API.

use std::collections::HashMap;

use serde::Deserialize;

use super::{Fetch, FetchError, RegistryPlugin};

/// The crates.io API root.
pub const DEFAULT_API_ROOT: &str = "https://crates.io/api/v1";

/// Overrides the API root — for tests (pointing at a local mock or a dead
/// address) and for a registry mirror. Mirrors the existing
/// `CLOVE_GITHUB_API_URL` seam used by the GitHub sync tests.
///
/// # Why this is trusted when the discovery cache is not
///
/// `install` refuses to read the TTL cache (`cache.rs`) on the grounds that it
/// is writable by anyone who can set `$CLOVE_HOME` — which invites the obvious
/// objection that this variable is settable by that same actor, and controls the
/// *whole* registry answer including gate 1. The two are genuinely different,
/// and the difference is **when** the influence is exercised:
///
/// - The environment of the running process is already fully trusted, and
///   necessarily so. Whoever sets `$CLOVE_REGISTRY_URL` for this run can equally
///   set `$PATH` or `$CLOVE_PLUGIN_PATH` and simply have clove exec their own
///   binary — no registry required. Defending the registry root against an actor
///   who already chooses what clove executes buys nothing.
/// - The cache is a **file that outlives the run that wrote it**. A later
///   `clove plugin install`, in a clean environment, with no attacker present,
///   would read it and let it decide what counts as a plugin. That is influence
///   crossing a trust boundary, and it is the one worth closing.
///
/// Consequently this is a plain override with no scheme restriction (the test
/// harness points it at `http://127.0.0.1:<port>`), while the cache exclusion is
/// absolute. If clove ever grows a *persisted* registry-root setting — a config
/// file rather than an env var — it lands on the cache's side of this line and
/// needs the same treatment.
pub const API_ROOT_ENV: &str = "CLOVE_REGISTRY_URL";

/// crates.io caps `per_page` at 100 — `per_page=200` is an HTTP 400, not a
/// silent clamp.
const PER_PAGE: usize = 100;

/// A hard bound on pagination, so a server that keeps returning full pages
/// cannot spin the client forever.
const MAX_PAGES: usize = 50;

/// Deserialize a field that crates.io may send as an explicit `null` rather than
/// omitting it, yielding the default instead of failing.
///
/// `#[serde(default)]` alone is not enough: it fires only when the key is
/// **missing**. crates.io sends `"bin_names": null` for versions that predate
/// bin-name recording (verified live on `libc` 0.2.141), and one such row
/// anywhere in a response would otherwise abort the whole parse — discarding
/// every other crate in it. `FetchError::Decode` is deliberately not retryable,
/// so that failure would repeat on every invocation, forever.
fn null_as_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

/// The subset of `GET /crates/{name}` this client reads.
#[derive(Debug, Deserialize)]
struct CrateResponse {
    #[serde(rename = "crate")]
    krate: CrateObject,
    #[serde(default, deserialize_with = "null_as_default")]
    versions: Vec<VersionObject>,
}

#[derive(Debug, Deserialize)]
struct CrateObject {
    name: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    repository: Option<String>,
    #[serde(default)]
    downloads: u64,
}

/// One published version. `bin_names` is the field that makes a name probe
/// sufficient to tell whether a crate builds a dispatch-resolvable binary — it
/// is returned by `GET /crates/{name}` as well as by `reverse_dependencies`.
#[derive(Debug, Deserialize)]
struct VersionObject {
    #[serde(default)]
    id: u64,
    /// The **dependent** crate's name. In a `reverse_dependencies` response this
    /// is the crate we are looking for; see [`reverse_dependents`].
    #[serde(default)]
    #[serde(rename = "crate")]
    krate: String,
    #[serde(default)]
    num: String,
    #[serde(default, deserialize_with = "null_as_default")]
    bin_names: Vec<Option<String>>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    repository: Option<String>,
    #[serde(default)]
    published_by: Option<PublishedBy>,
    #[serde(default)]
    yanked: bool,
}

#[derive(Debug, Deserialize)]
struct PublishedBy {
    #[serde(default)]
    login: Option<String>,
}

/// Note there is deliberately no `meta` field: `meta.total` is *not* used to
/// bound pagination. It is optional in practice (a mirror may omit or approximate
/// it), and a missing value deserializes to `0`, which would end the walk on the
/// first page with a silently truncated list. A short page is the reliable signal.
#[derive(Debug, Deserialize)]
struct ReverseDepsResponse {
    #[serde(default, deserialize_with = "null_as_default")]
    dependencies: Vec<DependencyObject>,
    #[serde(default, deserialize_with = "null_as_default")]
    versions: Vec<VersionObject>,
}

/// One dependency edge.
///
/// **The `crate_id` here is the crate being *depended on*** — i.e. always
/// `clove-plugin` for our query — *not* the dependent. The dependent is reached
/// by joining `version_id` into the `versions[]` array and reading
/// `versions[].crate`. Getting this backwards yields a list of the same name
/// repeated, which looks plausible enough to ship.
#[derive(Debug, Deserialize)]
struct DependencyObject {
    #[serde(default)]
    version_id: u64,
    #[serde(default)]
    kind: String,
    /// The **dependent crate's** total downloads. Note the version objects in the
    /// same response carry their own per-*version* `downloads`, which is a much
    /// smaller number; the crate total is the popularity signal, and it is what
    /// [`plugin_from_crate`] reports on the name-probe path — so both paths must
    /// read the crate-level figure or the same plugin shows two different counts
    /// depending on how it was found.
    #[serde(default)]
    downloads: u64,
}

/// A crates.io client bound to an API root and a transport.
pub struct CratesIo<'a> {
    fetch: &'a dyn Fetch,
    api_root: String,
}

impl<'a> CratesIo<'a> {
    /// A client against the configured API root: `$CLOVE_REGISTRY_URL` when set,
    /// otherwise crates.io.
    pub fn new(fetch: &'a dyn Fetch) -> Self {
        let root = std::env::var(API_ROOT_ENV)
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_API_ROOT.to_owned());
        Self::with_root(fetch, &root)
    }

    pub fn with_root(fetch: &'a dyn Fetch, api_root: &str) -> Self {
        CratesIo {
            fetch,
            api_root: api_root.trim_end_matches('/').to_owned(),
        }
    }

    /// Probe for a crate by exact name.
    ///
    /// `Ok(None)` means crates.io authoritatively reports the crate does not
    /// exist (404); `Err` means we could not find out. The caller must not treat
    /// those alike — "absent" and "unreachable" lead to different messages and
    /// different exit codes.
    ///
    /// The name must already be validated ([`super::validate_crate_name`]); it is
    /// re-checked here so no caller can bypass it.
    pub fn crate_exists(&self, name: &str) -> Result<Option<RegistryPlugin>, FetchError> {
        if super::validate_crate_name(name).is_err() {
            // An invalid name cannot exist; never put it in a URL.
            return Ok(None);
        }
        let url = format!("{}/crates/{name}", self.api_root);
        let Some(body) = self.fetch.get(&url)? else {
            return Ok(None);
        };
        let parsed: CrateResponse =
            serde_json::from_str(&body).map_err(|e| FetchError::Decode(e.to_string()))?;
        Ok(Some(plugin_from_crate(parsed)))
    }

    /// Every crate that depends on `of` (in practice, `clove-plugin`).
    ///
    /// Returns `Ok(None)` when `of` itself is not published — an **absent**
    /// registry, which is different from a published-but-empty one
    /// (`Ok(Some(vec![]))`). Discovery is meaningless in the first case and
    /// simply has no results in the second; collapsing them would make an
    /// unpublished `clove-plugin` indistinguishable from "nobody has written a
    /// plugin yet", and later would let a transient empty response masquerade as
    /// a definitive answer.
    pub fn reverse_dependents(&self, of: &str) -> Result<Option<Vec<RegistryPlugin>>, FetchError> {
        let mut by_crate: HashMap<String, RegistryPlugin> = HashMap::new();
        let mut page = 1usize;

        loop {
            let url = format!(
                "{}/crates/{of}/reverse_dependencies?per_page={PER_PAGE}&page={page}",
                self.api_root
            );
            let Some(body) = self.fetch.get(&url)? else {
                if page == 1 {
                    // The registry crate itself does not exist.
                    return Ok(None);
                }
                // A 404 on a *later* page means we walked past the end (or hit a
                // mirror that 404s beyond its last page) — not that `of` is
                // unpublished. Returning `None` here would discard everything
                // already collected and report "the registry is not available",
                // which would then be cached for a day.
                break;
            };
            let parsed: ReverseDepsResponse =
                serde_json::from_str(&body).map_err(|e| FetchError::Decode(e.to_string()))?;

            let page_rows = parsed.dependencies.len();
            merge_page(&mut by_crate, parsed);

            // Stop on a short or empty page (the usual end, and a server that
            // always returns rows cannot loop), or at the hard page cap.
            //
            // Deliberately *not* keyed on `meta.total`: it is `#[serde(default)]`,
            // so a response without a `meta` key yields `0` and `seen >= 0` would
            // break on page 1 with a silently truncated list. `$CLOVE_REGISTRY_URL`
            // is a supported mirror seam, and a mirror is exactly what omits or
            // approximates `total`. A short page is the reliable signal; the cost
            // of ignoring `total` is at most one extra request.
            if page_rows < PER_PAGE || page >= MAX_PAGES {
                break;
            }
            page += 1;
        }

        let mut plugins: Vec<RegistryPlugin> = by_crate
            .into_values()
            // A reverse dependent that builds no binary at all is a *library* that
            // happens to depend on the registry root (a shared helper, a plugin's
            // own lib half, a test harness). It can never be installed as a
            // command — `cargo install` would fail with "no binaries", after a
            // full download and build.
            //
            // Only the "builds something" rule lives here; whether a binary is
            // *dispatch-resolvable* is the `clove-<mux>-<provider>` naming
            // convention, which belongs to the command layer, not to a generic
            // crates.io client.
            .filter(|plugin| !plugin.bin_names.is_empty())
            .collect();
        plugins.sort_by(|a, b| a.crate_name.cmp(&b.crate_name));
        Ok(Some(plugins))
    }
}

/// Fold one `reverse_dependencies` page into the accumulating result.
///
/// Three things happen here, each guarding a distinct hazard:
///
/// 1. **the join** — `dependencies[].version_id` → `versions[].id`, taking the
///    dependent's identity from `versions[].crate` (never `dependencies[].crate_id`,
///    which is the depended-on crate);
/// 2. **the kind filter** — only `kind == "normal"` counts. A crate that merely
///    *dev*-depends on `clove-plugin` (to test against it) is not a plugin;
/// 3. **the dedup** — rows are *versions*, so one crate can appear several
///    times. Versions are compared as semver, not as strings.
fn merge_page(by_crate: &mut HashMap<String, RegistryPlugin>, page: ReverseDepsResponse) {
    let versions_by_id: HashMap<u64, &VersionObject> =
        page.versions.iter().map(|v| (v.id, v)).collect();

    for dep in &page.dependencies {
        if dep.kind != "normal" {
            continue;
        }
        let Some(version) = versions_by_id.get(&dep.version_id) else {
            continue;
        };
        if version.krate.is_empty() {
            continue;
        }
        let parsed = semver::Version::parse(&version.num).ok();

        let entry = by_crate
            .entry(version.krate.clone())
            .or_insert_with(|| RegistryPlugin {
                crate_name: version.krate.clone(),
                latest: None,
                latest_yanked: None,
                description: None,
                repository: None,
                bin_names: Vec::new(),
                published_by: None,
                downloads: 0,
            });

        // The crate-level total from the dependency row, not `version.downloads`
        // (which is this single version's count) — see `DependencyObject`.
        entry.downloads = entry.downloads.max(dep.downloads);

        // Metadata is taken from whichever version is currently the best
        // candidate, so the displayed description/bins match the version a user
        // would actually get.
        let is_new_best = match (&parsed, version.yanked) {
            (Some(v), false) => entry.latest.as_ref().is_none_or(|cur| v > cur),
            (Some(v), true) => {
                entry.latest.is_none() && entry.latest_yanked.as_ref().is_none_or(|cur| v > cur)
            }
            (None, _) => entry.latest.is_none() && entry.latest_yanked.is_none(),
        };

        if let Some(v) = parsed {
            if version.yanked {
                if entry.latest_yanked.as_ref().is_none_or(|cur| &v > cur) {
                    entry.latest_yanked = Some(v);
                }
            } else if entry.latest.as_ref().is_none_or(|cur| &v > cur) {
                entry.latest = Some(v);
            }
        }

        if is_new_best {
            entry.description = version.description.clone();
            entry.repository = version.repository.clone();
            entry.bin_names = version.bin_names.iter().flatten().cloned().collect();
            entry.published_by = version.published_by.as_ref().and_then(|p| p.login.clone());
        }
    }
}

/// Build a [`RegistryPlugin`] from a `GET /crates/{name}` response.
fn plugin_from_crate(response: CrateResponse) -> RegistryPlugin {
    let mut latest: Option<semver::Version> = None;
    let mut latest_yanked: Option<semver::Version> = None;
    let mut best: Option<&VersionObject> = None;
    let mut best_yanked: Option<&VersionObject> = None;

    for version in &response.versions {
        let Ok(parsed) = semver::Version::parse(&version.num) else {
            continue;
        };
        if version.yanked {
            if latest_yanked.as_ref().is_none_or(|cur| &parsed > cur) {
                latest_yanked = Some(parsed);
                best_yanked = Some(version);
            }
        } else {
            let better = latest.as_ref().is_none_or(|cur| &parsed > cur);
            if better {
                latest = Some(parsed);
                best = Some(version);
            }
        }
    }
    // With no non-yanked release, describe the crate from its highest-semver
    // yanked version so `bin_names` is still populated — tracked alongside
    // `latest_yanked` rather than taken from `versions.first()`, which is
    // *publication* order. A crate that shipped 0.9.0 and then backported 0.8.1
    // (both yanked) would otherwise report version 0.9.0 with 0.8.1's metadata.
    let best = best.or(best_yanked).or_else(|| response.versions.first());

    RegistryPlugin {
        crate_name: response.krate.name,
        latest,
        latest_yanked,
        description: best
            .and_then(|v| v.description.clone())
            .or(response.krate.description),
        repository: best
            .and_then(|v| v.repository.clone())
            .or(response.krate.repository),
        bin_names: best
            .map(|v| v.bin_names.iter().flatten().cloned().collect())
            .unwrap_or_default(),
        published_by: best
            .and_then(|v| v.published_by.as_ref())
            .and_then(|p| p.login.clone()),
        downloads: response.krate.downloads,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// A [`Fetch`] that serves canned responses and records the URLs requested.
    struct FakeFetch {
        responses: Vec<Result<Option<String>, FetchError>>,
        seen: RefCell<Vec<String>>,
        cursor: RefCell<usize>,
    }

    impl FakeFetch {
        fn ok(body: &str) -> Self {
            FakeFetch {
                responses: vec![Ok(Some(body.to_owned()))],
                seen: RefCell::new(Vec::new()),
                cursor: RefCell::new(0),
            }
        }
        fn sequence(responses: Vec<Result<Option<String>, FetchError>>) -> Self {
            FakeFetch {
                responses,
                seen: RefCell::new(Vec::new()),
                cursor: RefCell::new(0),
            }
        }
    }

    impl Fetch for FakeFetch {
        fn get(&self, url: &str) -> Result<Option<String>, FetchError> {
            self.seen.borrow_mut().push(url.to_owned());
            let mut cursor = self.cursor.borrow_mut();
            let response = self
                .responses
                .get(*cursor)
                .cloned()
                .unwrap_or(Ok(Some("{}".to_owned())));
            *cursor += 1;
            response
        }
    }

    fn fixture(name: &str) -> String {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/registry")
            .join(name);
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path:?}: {e}"))
    }

    #[test]
    fn reverse_deps_join_resolves_dependents_not_the_depended_on_crate() {
        // The hazard: `dependencies[].crate_id` is `cargo-subcommand` on every
        // row. Joining on it yields the same name repeated; the dependent's
        // identity lives at `versions[].crate`. Fixture recorded live.
        let fetch = FakeFetch::ok(&fixture("reverse_deps.json"));
        let client = CratesIo::with_root(&fetch, "https://example.invalid/api/v1");
        let plugins = client
            .reverse_dependents("cargo-subcommand")
            .expect("fetch ok")
            .expect("registry present");

        let names: Vec<&str> = plugins.iter().map(|p| p.crate_name.as_str()).collect();
        assert!(
            names.contains(&"cargo-apk") && names.contains(&"cargo-xcodebuild"),
            "expected real dependents, got {names:?}"
        );
        assert!(
            !names.contains(&"cargo-subcommand"),
            "the depended-on crate must never appear as its own dependent: {names:?}"
        );
    }

    #[test]
    fn dev_dependents_are_filtered_out() {
        // Dev-depending on clove-plugin (to test against it) does not make a
        // crate a plugin.
        let fetch = FakeFetch::ok(&fixture("reverse_deps_hazards.json"));
        let client = CratesIo::with_root(&fetch, "https://example.invalid/api/v1");
        let plugins = client.reverse_dependents("clove-plugin").unwrap().unwrap();
        let names: Vec<&str> = plugins.iter().map(|p| p.crate_name.as_str()).collect();
        assert!(
            !names.contains(&"some-test-harness"),
            "a kind=dev dependent must not be listed as a plugin: {names:?}"
        );
        assert_eq!(names, vec!["clove-import-jira", "clove-sync-gitlab"]);
    }

    #[test]
    fn duplicate_versions_collapse_to_the_highest_by_semver_not_string() {
        // The fixture carries 0.2.0, 0.10.0 and a yanked 0.11.0 of one crate.
        // A string comparison picks "0.2.0" as the greatest — this pins semver.
        let fetch = FakeFetch::ok(&fixture("reverse_deps_hazards.json"));
        let client = CratesIo::with_root(&fetch, "https://example.invalid/api/v1");
        let plugins = client.reverse_dependents("clove-plugin").unwrap().unwrap();

        let gitlab = plugins
            .iter()
            .find(|p| p.crate_name == "clove-sync-gitlab")
            .expect("gitlab present");
        assert_eq!(
            gitlab.latest.as_ref(),
            Some(&semver::Version::new(0, 10, 0)),
            "0.10.0 > 0.2.0 by semver (a string compare gets this backwards)"
        );
        // The yanked 0.11.0 is tracked but never offered as installable.
        assert_eq!(gitlab.latest_yanked, Some(semver::Version::new(0, 11, 0)));
        assert!(!gitlab.fully_yanked());
        // One row per crate, not one per version.
        assert_eq!(
            plugins
                .iter()
                .filter(|p| p.crate_name == "clove-sync-gitlab")
                .count(),
            1
        );
    }

    #[test]
    fn absent_registry_is_distinct_from_an_empty_one() {
        // `clove-plugin` unpublished → None (we cannot tell anything).
        let absent = FakeFetch::sequence(vec![Ok(None)]);
        let client = CratesIo::with_root(&absent, "https://example.invalid/api/v1");
        assert_eq!(client.reverse_dependents("clove-plugin").unwrap(), None);

        // Published but nothing depends on it → Some(empty).
        let empty = FakeFetch::ok(r#"{"dependencies":[],"versions":[],"meta":{"total":0}}"#);
        let client = CratesIo::with_root(&empty, "https://example.invalid/api/v1");
        assert_eq!(
            client.reverse_dependents("clove-plugin").unwrap(),
            Some(vec![])
        );
    }

    #[test]
    fn crate_probe_reads_bin_names_and_version() {
        let fetch = FakeFetch::ok(&fixture("crate_exists.json"));
        let client = CratesIo::with_root(&fetch, "https://example.invalid/api/v1");
        let found = client
            .crate_exists("cargo-subcommand")
            .unwrap()
            .expect("crate exists");
        assert_eq!(found.crate_name, "cargo-subcommand");
        assert_eq!(
            found.bin_names,
            vec!["cargo-subcommand"],
            "bin_names comes back from the name probe alone — no reverse-deps needed"
        );
        assert!(found.latest.as_ref().is_some());
        assert_eq!(
            fetch.seen.borrow().as_slice(),
            ["https://example.invalid/api/v1/crates/cargo-subcommand"]
        );
    }

    #[test]
    fn a_404_is_absence_and_a_transport_failure_is_not() {
        let absent = FakeFetch::sequence(vec![Ok(None)]);
        let client = CratesIo::with_root(&absent, "https://example.invalid/api/v1");
        assert_eq!(client.crate_exists("clove-sync-gitlab").unwrap(), None);

        let broken = FakeFetch::sequence(vec![Err(FetchError::Transport("dns".to_owned()))]);
        let client = CratesIo::with_root(&broken, "https://example.invalid/api/v1");
        assert!(
            client.crate_exists("clove-sync-gitlab").is_err(),
            "a transport failure must not be reported as 'crate does not exist'"
        );
    }

    #[test]
    fn an_invalid_name_never_reaches_the_network() {
        let fetch = FakeFetch::ok("{}");
        let client = CratesIo::with_root(&fetch, "https://example.invalid/api/v1");
        assert_eq!(client.crate_exists("../../summary").unwrap(), None);
        assert!(
            fetch.seen.borrow().is_empty(),
            "a traversal name must not be put into a URL"
        );
    }

    #[test]
    fn pagination_stops_on_a_short_page() {
        let fetch = FakeFetch::ok(&fixture("reverse_deps.json"));
        let client = CratesIo::with_root(&fetch, "https://example.invalid/api/v1");
        client.reverse_dependents("cargo-subcommand").unwrap();
        assert_eq!(
            fetch.seen.borrow().len(),
            1,
            "an 11-row page is short of per_page=100, so one request suffices"
        );
        assert!(fetch.seen.borrow()[0].contains("per_page=100"));
    }

    #[test]
    fn an_explicitly_null_bin_names_does_not_fail_the_whole_response() {
        // crates.io emits `"bin_names": null` (not a missing key) for versions
        // that predate bin-name recording — verified live on libc 0.2.141.
        // `#[serde(default)]` only covers a *missing* key, so a field-level null
        // used to abort the entire parse: one such row anywhere in a response
        // discarded every other crate in it, permanently and unretryably.
        let body = r#"{
          "crate":{"name":"libc","description":"d","repository":null,"downloads":9},
          "versions":[
            {"id":1,"crate":"libc","num":"0.2.141","yanked":false,"bin_names":null},
            {"id":2,"crate":"libc","num":"0.2.142","yanked":false,"bin_names":["libc-tool"]}
          ]
        }"#;
        let fetch = FakeFetch::ok(body);
        let client = CratesIo::with_root(&fetch, "https://example.invalid/api/v1");
        let found = client
            .crate_exists("libc")
            .expect("a null bin_names must not fail the request")
            .expect("crate exists");
        assert_eq!(found.crate_name, "libc");
        assert_eq!(found.bin_names, vec!["libc-tool"]);

        // Same in a reverse-deps page: the null row is tolerated and the other
        // rows survive.
        let page = r#"{
          "dependencies":[
            {"version_id":1,"crate_id":"clove-plugin","kind":"normal","downloads":1},
            {"version_id":2,"crate_id":"clove-plugin","kind":"normal","downloads":2}
          ],
          "versions":[
            {"id":1,"crate":"clove-old","num":"0.1.0","yanked":false,"bin_names":null},
            {"id":2,"crate":"clove-sync-new","num":"0.1.0","yanked":false,"bin_names":["clove-sync-new"]}
          ]
        }"#;
        let fetch = FakeFetch::ok(page);
        let client = CratesIo::with_root(&fetch, "https://example.invalid/api/v1");
        let plugins = client
            .reverse_dependents("clove-plugin")
            .expect("a null bin_names must not fail the page")
            .unwrap();
        assert_eq!(
            plugins.len(),
            1,
            "the null-bin row builds no binary, so it is dropped"
        );
        assert_eq!(plugins[0].crate_name, "clove-sync-new");
    }

    #[test]
    fn library_only_dependents_are_not_listed_as_plugins() {
        // A crate can depend on `clove-plugin` as a *library* (a shared helper, a
        // plugin's own lib half) without building any dispatchable binary. The
        // recorded fixture contains exactly that shape.
        let fetch = FakeFetch::ok(&fixture("reverse_deps.json"));
        let client = CratesIo::with_root(&fetch, "https://example.invalid/api/v1");
        let plugins = client
            .reverse_dependents("cargo-subcommand")
            .unwrap()
            .unwrap();

        assert!(
            plugins.iter().all(|p| !p.bin_names.is_empty()),
            "every listed dependent must build a binary, got {:?}",
            plugins
                .iter()
                .map(|p| (&p.crate_name, &p.bin_names))
                .collect::<Vec<_>>()
        );
        // `simics-package` is in the fixture as a lib-only dependent.
        assert!(!plugins.iter().any(|p| p.crate_name == "simics-package"));
    }

    #[test]
    fn downloads_come_from_the_crate_total_not_the_version_count() {
        // The response carries BOTH: `versions[].downloads` is that single
        // version's count, `dependencies[].downloads` is the crate total. The
        // crate total is the popularity signal and is what the name-probe path
        // reports, so the two paths must not disagree.
        let body = r#"{
          "dependencies":[{"version_id":1,"crate_id":"clove-plugin","kind":"normal","downloads":199183}],
          "versions":[{"id":1,"crate":"clove-sync-x","num":"1.0.0","yanked":false,
                       "bin_names":["clove-sync-x"],"downloads":16039}],
          "meta":{"total":1}
        }"#;
        let fetch = FakeFetch::ok(body);
        let client = CratesIo::with_root(&fetch, "https://example.invalid/api/v1");
        let plugins = client.reverse_dependents("clove-plugin").unwrap().unwrap();
        assert_eq!(plugins[0].downloads, 199_183, "expected the crate total");
    }

    #[test]
    fn a_404_on_a_later_page_keeps_what_was_collected() {
        // Only page 1 answering 404 means "the registry crate does not exist".
        // Later pages 404 when we walk past the end, or on a mirror; treating that
        // as an absent registry would discard a full page of real results and
        // report "not published" — and cache that for a day.
        let mut page1 = String::from(r#"{"dependencies":["#);
        let mut versions = String::from(r#"],"versions":["#);
        for i in 0..100 {
            if i > 0 {
                page1.push(',');
                versions.push(',');
            }
            page1.push_str(&format!(
                r#"{{"version_id":{i},"crate_id":"clove-plugin","kind":"normal","downloads":1}}"#
            ));
            versions.push_str(&format!(
                r#"{{"id":{i},"crate":"clove-sync-p{i}","num":"1.0.0","yanked":false,"bin_names":["clove-sync-p{i}"]}}"#
            ));
        }
        let page1 = format!("{page1}{versions}]}}");

        let fetch = FakeFetch::sequence(vec![Ok(Some(page1)), Ok(None)]);
        let client = CratesIo::with_root(&fetch, "https://example.invalid/api/v1");
        let plugins = client
            .reverse_dependents("clove-plugin")
            .unwrap()
            .expect("a later-page 404 must not read as an absent registry");
        assert_eq!(plugins.len(), 100, "the first full page must be kept");
    }

    #[test]
    fn pagination_does_not_truncate_when_meta_total_is_missing() {
        // `meta` is optional; a missing one used to deserialize to total = 0 and
        // end the walk after page 1 with a silently partial list.
        let mut deps = String::new();
        let mut versions = String::new();
        for i in 0..100 {
            if i > 0 {
                deps.push(',');
                versions.push(',');
            }
            deps.push_str(&format!(
                r#"{{"version_id":{i},"crate_id":"clove-plugin","kind":"normal","downloads":1}}"#
            ));
            versions.push_str(&format!(
                r#"{{"id":{i},"crate":"clove-sync-q{i}","num":"1.0.0","yanked":false,"bin_names":["clove-sync-q{i}"]}}"#
            ));
        }
        // No `meta` key at all on either page.
        let full = format!(r#"{{"dependencies":[{deps}],"versions":[{versions}]}}"#);
        let tail = r#"{"dependencies":[{"version_id":900,"crate_id":"clove-plugin","kind":"normal","downloads":1}],
                       "versions":[{"id":900,"crate":"clove-sync-tail","num":"1.0.0","yanked":false,"bin_names":["clove-sync-tail"]}]}"#;

        let fetch = FakeFetch::sequence(vec![Ok(Some(full)), Ok(Some(tail.to_owned()))]);
        let client = CratesIo::with_root(&fetch, "https://example.invalid/api/v1");
        let plugins = client.reverse_dependents("clove-plugin").unwrap().unwrap();

        assert_eq!(plugins.len(), 101, "page 2 must still be fetched");
        assert!(plugins.iter().any(|p| p.crate_name == "clove-sync-tail"));
    }

    #[test]
    fn a_fully_yanked_crate_reports_no_installable_version() {
        let body = r#"{
          "dependencies":[{"version_id":1,"crate_id":"clove-plugin","kind":"normal"}],
          "versions":[{"id":1,"crate":"clove-sync-gone","num":"0.3.0","yanked":true,
                       "bin_names":["clove-sync-gone"]}],
          "meta":{"total":1}
        }"#;
        let fetch = FakeFetch::ok(body);
        let client = CratesIo::with_root(&fetch, "https://example.invalid/api/v1");
        let plugins = client.reverse_dependents("clove-plugin").unwrap().unwrap();
        assert_eq!(plugins.len(), 1);
        assert!(plugins[0].fully_yanked());
        assert_eq!(plugins[0].latest.as_ref(), None);
        assert_eq!(plugins[0].display_version().as_deref(), Some("0.3.0"));
    }
}
