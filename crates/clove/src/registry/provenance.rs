//! What clove installed, and where it came from — read from cargo's own
//! bookkeeping in `<clove-home>/.crates2.json`.
//!
//! `cargo install --root <dir>` records every install it made under that root:
//! the package id (which carries the *source*), and the binaries it produced.
//! That file is the only correct basis for `uninstall` and `update`, for three
//! reasons:
//!
//! 1. **`uninstall` must not need the network.** The user asks by the name they
//!    dispatch with (`github`), but `cargo uninstall` takes a *package* name, and
//!    the two differ routinely — in this very repo `clove-plugin-echo` builds a
//!    binary called `clove-echo`. Resolving that mapping by probing crates.io
//!    would make an offline, local operation depend on the internet.
//! 2. **`update` must know whether to re-resolve or re-clone.** A plugin
//!    installed from a git URL cannot be updated by looking it up on crates.io;
//!    doing so would silently convert a git install into a registry one, or fail
//!    outright for a crate that was never published.
//! 3. **A plugin clove did not install must be reported, not mangled.** Every
//!    instruction predating the install command says `cargo install
//!    clove-sync-github`, which lands in `~/.cargo/bin` — outside our root. Those
//!    are invisible here, and the commands say so rather than failing opaquely.

use camino::Utf8Path;
use serde::Deserialize;

/// Where an installed package came from, parsed from its cargo package id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// `registry+https://github.com/rust-lang/crates.io-index`
    Registry,
    /// `git+<url>` with the optional `?tag=`/`?rev=`/`?branch=` and `#<sha>`.
    Git {
        url: String,
        reference: Option<String>,
    },
    /// `path+file://…` — a local build, e.g. `cargo install --path`.
    Path(String),
    /// A source shape this build does not recognize; carried verbatim so the
    /// command can report it instead of guessing.
    Other(String),
}

impl Source {
    /// A short human label for the install listing.
    pub fn label(&self) -> String {
        match self {
            Source::Registry => "crates.io".to_owned(),
            Source::Git { url, reference } => match reference {
                Some(r) => format!("git {url} ({r})"),
                None => format!("git {url}"),
            },
            Source::Path(path) => format!("path {path}"),
            Source::Other(raw) => raw.clone(),
        }
    }
}

/// One package installed under the clove home.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Installed {
    /// The cargo **package** name — what `cargo uninstall` takes, which is not
    /// necessarily any of the binary names.
    pub package: String,
    pub version: String,
    pub source: Source,
    /// The binaries the package installed, as filenames on disk.
    pub bins: Vec<String>,
}

impl Installed {
    /// Does this package own a binary that dispatches as `name`?
    ///
    /// Compared against the *stripped* subcommand name (`sync-github`, not
    /// `clove-sync-github`), and suffix-insensitively: cargo records
    /// `clove-sync-github.exe` on Windows while the resolver reports the
    /// subcommand as `sync-github` on every platform.
    pub fn provides_subcommand(&self, name: &str) -> bool {
        self.bins
            .iter()
            .any(|bin| bare_subcommand(bin) == Some(name))
    }
}

/// The subcommand a binary filename dispatches as: `clove-sync-github.exe` →
/// `sync-github`. `None` when the file is not a clove plugin binary at all.
pub fn bare_subcommand(bin: &str) -> Option<&str> {
    bare_subcommand_with(bin, std::env::consts::EXE_SUFFIX)
}

/// [`bare_subcommand`] with the executable suffix injected — see
/// [`super::install::installed_binary_path_with`] for why.
///
/// The suffix is tolerated rather than required: cargo's bookkeeping is the only
/// place the suffixed name appears, and a hand-written or older entry may carry
/// the bare name.
pub fn bare_subcommand_with<'a>(bin: &'a str, exe_suffix: &str) -> Option<&'a str> {
    let stem = if exe_suffix.is_empty() {
        bin
    } else {
        bin.strip_suffix(exe_suffix).unwrap_or(bin)
    };
    stem.strip_prefix("clove-").filter(|rest| !rest.is_empty())
}

#[derive(Debug, Deserialize)]
struct CratesFile {
    #[serde(default)]
    installs: std::collections::BTreeMap<String, InstallEntry>,
}

#[derive(Debug, Deserialize)]
struct InstallEntry {
    #[serde(default)]
    bins: Vec<String>,
}

/// The bookkeeping file cargo maintains under an install root.
const CRATES2: &str = ".crates2.json";

/// Read every package installed under `home`.
///
/// An absent or unreadable file means "nothing installed by clove" — that is the
/// state before the first install, not an error.
pub fn installed_under(home: &Utf8Path) -> Vec<Installed> {
    let path = home.join(CRATES2);
    // The same ceiling `cache::read` applies, for the same stated reason: this
    // module already treats `$CLOVE_HOME` as attacker-writable, so an unbounded
    // read of a file living there is a self-inflicted OOM waiting to happen. The
    // asymmetry with the cache was unintended.
    const MAX_CRATES2_BYTES: u64 = 16 * 1024 * 1024;
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > MAX_CRATES2_BYTES) {
        return Vec::new();
    }
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    let Ok(parsed) = serde_json::from_str::<CratesFile>(&raw) else {
        return Vec::new();
    };

    let mut out: Vec<Installed> = parsed
        .installs
        .into_iter()
        .filter_map(|(pkgid, entry)| {
            parse_pkgid(&pkgid).map(|(p, v, s)| Installed {
                package: p,
                version: v,
                source: s,
                bins: entry.bins,
            })
        })
        .collect();
    out.sort_by(|a, b| a.package.cmp(&b.package));
    out
}

/// Find the package owning the binary that dispatches as `name`.
pub fn find_by_subcommand(home: &Utf8Path, name: &str) -> Option<Installed> {
    installed_under(home)
        .into_iter()
        .find(|i| i.provides_subcommand(name))
}

/// Parse a cargo package id: `"<name> <version> (<source>)"`.
///
/// Cargo has used this shape for `.crates2.json` keys across the versions clove
/// supports. An unparseable key is skipped rather than guessed at — a wrong
/// package name here would be passed to `cargo uninstall`.
fn parse_pkgid(pkgid: &str) -> Option<(String, String, Source)> {
    let (name, rest) = pkgid.split_once(' ')?;
    let (version, source) = rest.split_once(' ')?;
    let source = source.strip_prefix('(')?.strip_suffix(')')?;
    if name.is_empty() || version.is_empty() {
        return None;
    }
    // The package name is handed to `cargo uninstall`/`cargo install` as argv,
    // and this file lives in `$CLOVE_HOME`, which the rest of this module already
    // treats as attacker-writable. An entry whose name is not a legal crate name
    // is dropped rather than passed on — a name like
    // `--config=build.rustc-wrapper=…` would otherwise be read by cargo as an
    // option. Same discipline as "an unparseable key is skipped, not guessed at".
    if super::validate_crate_name(name).is_err() {
        return None;
    }
    Some((name.to_owned(), version.to_owned(), parse_source(source)))
}

fn parse_source(source: &str) -> Source {
    if source.starts_with("registry+") || source.starts_with("sparse+") {
        return Source::Registry;
    }
    if let Some(rest) = source.strip_prefix("git+") {
        // `git+<url>[?tag=v1|?rev=abc|?branch=main][#<sha>]`
        let (url_and_query, _sha) = rest.split_once('#').unwrap_or((rest, ""));
        let (url, query) = url_and_query
            .split_once('?')
            .map_or((url_and_query, None), |(u, q)| (u, Some(q)));
        return Source::Git {
            url: url.to_owned(),
            reference: query.map(|q| q.to_owned()),
        };
    }
    if let Some(path) = source.strip_prefix("path+") {
        return Source::Path(path.to_owned());
    }
    Source::Other(source.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use camino::Utf8PathBuf;

    fn home_with(contents: &str) -> (tempfile::TempDir, Utf8PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let home = Utf8PathBuf::from_path_buf(dir.path().to_owned()).unwrap();
        std::fs::write(home.join(CRATES2), contents).unwrap();
        (dir, home)
    }

    /// A real `.crates2.json`, captured from `cargo install --root` in this repo.
    const REAL: &str = r#"{
      "installs": {
        "clove-plugin-echo 0.1.0 (path+file:///home/user/clove/crates/clove-plugin-echo)": {
          "version_req": null,
          "bins": ["clove-echo"],
          "features": [],
          "all_features": false,
          "no_default_features": false,
          "profile": "release",
          "target": "x86_64-unknown-linux-gnu",
          "rustc": "rustc 1.94.1"
        }
      }
    }"#;

    #[test]
    fn a_real_crates2_file_parses() {
        let (_dir, home) = home_with(REAL);
        let installed = installed_under(&home);
        assert_eq!(installed.len(), 1);
        assert_eq!(installed[0].package, "clove-plugin-echo");
        assert_eq!(installed[0].version, "0.1.0");
        assert_eq!(installed[0].bins, vec!["clove-echo"]);
        assert!(matches!(installed[0].source, Source::Path(_)));
    }

    #[test]
    fn the_package_name_is_not_the_binary_name() {
        // The whole reason this module exists: `cargo uninstall` takes the
        // package, the user asks by the subcommand, and in this repo they differ.
        let (_dir, home) = home_with(REAL);
        let found = find_by_subcommand(&home, "echo").expect("resolves by subcommand");
        assert_eq!(found.package, "clove-plugin-echo");
        assert_ne!(found.package, "clove-echo");
    }

    #[test]
    fn sources_are_classified() {
        let (_dir, home) = home_with(
            r#"{"installs":{
              "clove-sync-gitlab 0.2.0 (registry+https://github.com/rust-lang/crates.io-index)":{"bins":["clove-sync-gitlab"]},
              "clove-sync-x 0.1.0 (git+https://example.com/x?tag=v1#abc123)":{"bins":["clove-sync-x"]},
              "clove-sync-y 0.1.0 (sparse+https://index.crates.io/)":{"bins":["clove-sync-y"]}
            }}"#,
        );
        let installed = installed_under(&home);
        assert_eq!(installed.len(), 3);

        let gitlab = installed
            .iter()
            .find(|i| i.package == "clove-sync-gitlab")
            .unwrap();
        assert_eq!(gitlab.source, Source::Registry);

        let x = installed
            .iter()
            .find(|i| i.package == "clove-sync-x")
            .unwrap();
        assert_eq!(
            x.source,
            Source::Git {
                url: "https://example.com/x".to_owned(),
                reference: Some("tag=v1".to_owned()),
            },
            "a git install must be recognizable, so `update` re-clones instead of \
             looking the crate up on crates.io"
        );

        // A sparse registry is still crates.io.
        let y = installed
            .iter()
            .find(|i| i.package == "clove-sync-y")
            .unwrap();
        assert_eq!(y.source, Source::Registry);
    }

    #[test]
    fn an_absent_or_corrupt_file_means_nothing_installed() {
        let dir = tempfile::tempdir().unwrap();
        let home = Utf8PathBuf::from_path_buf(dir.path().to_owned()).unwrap();
        assert!(installed_under(&home).is_empty(), "absent");

        let (_d, corrupt) = home_with("{ not json");
        assert!(installed_under(&corrupt).is_empty(), "corrupt");
    }

    #[test]
    fn an_unparseable_pkgid_is_skipped_not_guessed() {
        // A wrong package name here would be handed to `cargo uninstall`.
        let (_dir, home) = home_with(
            r#"{"installs":{
              "nonsense-without-source":{"bins":["clove-x"]},
              "clove-good 1.0.0 (registry+https://x)":{"bins":["clove-good"]}
            }}"#,
        );
        let installed = installed_under(&home);
        assert_eq!(installed.len(), 1);
        assert_eq!(installed[0].package, "clove-good");
    }

    #[test]
    fn subcommand_extraction_ignores_the_platform_suffix() {
        // cargo records `clove-sync-github.exe` on Windows; the resolver reports
        // the subcommand as `sync-github` everywhere.
        let bin = format!("clove-sync-github{}", std::env::consts::EXE_SUFFIX);
        assert_eq!(bare_subcommand(&bin), Some("sync-github"));
        assert_eq!(bare_subcommand("clove-echo"), Some("echo"));
        // Not a clove plugin binary.
        assert_eq!(bare_subcommand("ripgrep"), None);
        assert_eq!(bare_subcommand("clove-"), None);
    }

    #[test]
    fn source_labels_are_human_readable() {
        assert_eq!(Source::Registry.label(), "crates.io");
        assert_eq!(
            Source::Git {
                url: "https://e.com/x".to_owned(),
                reference: Some("tag=v1".to_owned())
            }
            .label(),
            "git https://e.com/x (tag=v1)"
        );
    }
}
