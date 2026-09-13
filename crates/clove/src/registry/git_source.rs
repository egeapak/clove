//! Installing a plugin from a git repository.
//!
//! `clove plugin install --git <url>` clones the repository shallowly, works out
//! which package in it is a clove plugin, and hands that package to
//! `cargo install --git`. Everything here runs **plain `git`**, never `gh`, so a
//! non-GitHub forge works exactly the same.
//!
//! # Why the URL is validated before it is used
//!
//! `git` takes a great deal of its configuration from its own argv, and several
//! of those options execute code:
//!
//! - `--upload-pack=<cmd>` runs `<cmd>` for a local or ssh remote;
//! - `--template=<dir>` installs hooks from a directory into the new clone, and
//!   `git` then runs them;
//! - `--config protocol.ext.allow=always` re-enables the `ext::` transport that
//!   modern git denies precisely because it executes a shell command.
//!
//! A URL beginning with `-` is parsed as one of those, not as a URL. So the
//! argument is checked against an explicit scheme allow-list *before* it reaches
//! any subprocess — the same discipline `plugin::is_valid_segment` applies to
//! dispatch, applied here to argv.

use std::process::{Command, Stdio};
use std::time::Duration;

use camino::{Utf8Path, Utf8PathBuf};
use clove_types::CloveError;

/// How long any single git subprocess may run.
///
/// The `--clove-plugin-info` probe is bounded at 500ms and the HTTP client at 8s;
/// an unbounded `git clone` against an attacker-supplied host would be the one
/// step that can hang forever. Generous enough for a real clone over a slow link,
/// finite enough that a black hole does not wedge the command.
const GIT_TIMEOUT: Duration = Duration::from_secs(120);

/// A package in a cloned repository that is installable as a clove plugin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitPlugin {
    /// The cargo package name — what `cargo install --git … <package>` takes.
    pub package: String,
    /// The dispatchable binary it builds (`clove-sync-gitlab`).
    pub bin: String,
}

/// Validate a `--git` argument.
///
/// Only the transports a plugin repository plausibly uses are accepted, and a
/// leading `-` is rejected outright — see the module docs for what git would do
/// with it. `ext::` is refused explicitly rather than relying on git's own
/// default, so the refusal does not silently depend on the host's git config.
pub fn validate_git_url(url: &str) -> Result<(), CloveError> {
    let invalid = |reason: &str| CloveError::InvalidField {
        field: "--git".to_owned(),
        reason: reason.to_owned(),
    };

    if url.is_empty() {
        return Err(invalid("a git URL cannot be empty"));
    }
    if url.starts_with('-') {
        return Err(invalid(
            "a git URL cannot start with `-`: git would read it as an option, and \
             several of those (--upload-pack, --template, --config) execute code",
        ));
    }
    if url.contains('\n') || url.contains('\r') {
        return Err(invalid("a git URL cannot contain a newline"));
    }
    let lower = url.to_ascii_lowercase();
    if lower.starts_with("ext::") {
        return Err(invalid(
            "the `ext::` transport runs an arbitrary command and is not accepted",
        ));
    }

    // scp-style `user@host:path` is how ssh remotes are usually written.
    let scp_style = !url.contains("://") && url.contains(':') && url.contains('@');
    let allowed_scheme = ["https://", "http://", "ssh://", "git://", "file://"]
        .iter()
        .any(|s| lower.starts_with(s));

    if !allowed_scheme && !scp_style {
        return Err(invalid(&format!(
            "unsupported git URL `{url}` — expected https://, ssh://, git://, \
             file://, or user@host:path"
        )));
    }
    Ok(())
}

/// Validate a `--tag`/`--rev`/`--branch` value.
///
/// These also land in git's argv, so the same leading-`-` rule applies.
pub fn validate_git_ref(kind: &str, value: &str) -> Result<(), CloveError> {
    if value.is_empty() || value.starts_with('-') || value.contains(char::is_whitespace) {
        return Err(CloveError::InvalidField {
            field: kind.to_owned(),
            reason: format!("invalid {kind} `{value}`"),
        });
    }
    Ok(())
}

/// Run a git subprocess with the hardening every call here needs.
///
/// `GIT_TERMINAL_PROMPT=0` matters for more than tidiness: without it a URL that
/// answers 401 makes git print `Username for 'https://…':` and block on stdin
/// mid-install — a credential prompt the user would reasonably attribute to
/// clove, on a host clove did not vouch for.
fn git(args: &[&str], cwd: Option<&Utf8Path>) -> Result<std::process::Output, CloveError> {
    let mut cmd = Command::new("git");
    cmd.args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_ASKPASS", "")
        .env("SSH_ASKPASS", "")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }

    let mut child = cmd.spawn().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            CloveError::Registry {
                message: "`git` was not found on PATH; installing from a repository \
                          requires it"
                    .to_owned(),
            }
        } else {
            CloveError::Registry {
                message: format!("could not run git: {e}"),
            }
        }
    })?;

    // The pipes must be drained *while* git runs, not after it exits.
    //
    // Polling `try_wait` and only then calling `wait_with_output` deadlocks the
    // moment a child fills the 64 KiB pipe buffer: git blocks in `write`, so it
    // never exits, so `try_wait` never reports it, and the loop burns the whole
    // 120s timeout before killing a process that was working fine. `ls-remote`
    // writes one line per ref, so any repository with a few thousand refs — a
    // fork of a large upstream, a monorepo with per-release tags — hit it and
    // reported "could not reach" for a perfectly reachable repo.
    //
    // A reader thread per pipe is the standard answer, and it is what
    // `Command::output()` does internally (which is why `find_plugins` never had
    // this bug). The threads end when git closes its ends, so they cannot
    // outlive the kill below.
    let reader = |pipe: Option<std::process::ChildStdout>| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut pipe) = pipe {
                use std::io::Read;
                let _ = pipe.read_to_end(&mut buf);
            }
            buf
        })
    };
    let stdout = reader(child.stdout.take());
    let stderr = {
        let pipe = child.stderr.take();
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut pipe) = pipe {
                use std::io::Read;
                let _ = pipe.read_to_end(&mut buf);
            }
            buf
        })
    };

    let start = std::time::Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if start.elapsed() >= GIT_TIMEOUT {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(CloveError::Registry {
                        message: format!(
                            "git timed out after {}s (`git {}`)",
                            GIT_TIMEOUT.as_secs(),
                            args.first().copied().unwrap_or("?")
                        ),
                    });
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => {
                return Err(CloveError::Registry {
                    message: format!("git failed: {e}"),
                })
            }
        }
    };

    Ok(std::process::Output {
        status,
        stdout: stdout.join().unwrap_or_default(),
        stderr: stderr.join().unwrap_or_default(),
    })
}

/// Check that the repository exists and is reachable, without cloning it.
pub fn probe_remote(url: &str) -> Result<(), CloveError> {
    validate_git_url(url)?;
    let output = git(&["ls-remote", "--exit-code", "--", url], None)?;
    if output.status.success() {
        return Ok(());
    }
    Err(CloveError::Registry {
        message: format!(
            "could not reach `{url}`: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ),
    })
}

/// Shallow-clone `url` into `dest`.
///
/// `--filter=blob:none --depth 1` fetches the commit graph and trees but not file
/// contents until they are read, which is enough to inspect the manifests and is
/// dramatically cheaper than a full clone. Submodules are deliberately **not**
/// initialized: nothing here needs them, and fetching them would pull code from
/// further hosts the user never named.
pub fn shallow_clone(
    url: &str,
    reference: Option<&GitRef>,
    dest: &Utf8Path,
) -> Result<(), CloveError> {
    validate_git_url(url)?;

    let mut args: Vec<String> = vec![
        "clone".to_owned(),
        "--filter=blob:none".to_owned(),
        "--depth".to_owned(),
        "1".to_owned(),
        "--no-checkout".to_owned(),
        "--no-tags".to_owned(),
        "--quiet".to_owned(),
    ];
    // A tag or branch can be fetched directly; a bare rev cannot with --depth 1,
    // so that case clones the default branch and checks out afterwards.
    if let Some(GitRef::Tag(t) | GitRef::Branch(t)) = reference {
        args.push("--branch".to_owned());
        args.push(t.clone());
    }
    args.push("--".to_owned());
    args.push(url.to_owned());
    args.push(dest.to_string());

    let borrowed: Vec<&str> = args.iter().map(String::as_str).collect();
    let output = git(&borrowed, None)?;
    if !output.status.success() {
        return Err(CloveError::Registry {
            message: format!(
                "git clone of `{url}` failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        });
    }

    // `--no-checkout` above keeps the clone cheap; materialize the tree now.
    let checkout = match reference {
        Some(GitRef::Rev(rev)) => {
            let fetch = git(&["fetch", "--depth", "1", "origin", "--", rev], Some(dest))?;
            if !fetch.status.success() {
                return Err(CloveError::Registry {
                    message: format!("could not fetch rev `{rev}` from `{url}`"),
                });
            }
            // `--` disambiguates a rev from a pathspec; `probe_remote` and
            // `shallow_clone` already do this for the URL.
            git(&["checkout", "--quiet", rev, "--"], Some(dest))?
        }
        _ => git(&["checkout", "--quiet", "HEAD"], Some(dest))?,
    };
    if !checkout.status.success() {
        return Err(CloveError::Registry {
            message: format!(
                "git checkout failed: {}",
                String::from_utf8_lossy(&checkout.stderr).trim()
            ),
        });
    }
    Ok(())
}

/// The commit the working clone is actually at.
///
/// This closes the gap between "what clove inspected and showed the user" and
/// "what cargo builds". `cargo install --git` performs its **own** clone, so the
/// two fetches are independent: `--tag` and `--branch` are server-side mutable
/// names that cargo re-resolves, and a hostile or compromised host can serve a
/// different commit the second time. Only a commit id is content-addressed, so
/// the resolved sha is what gets passed on — the user's `--tag`/`--branch` selects
/// the commit, and this pins it.
pub fn resolve_head(clone: &Utf8Path) -> Result<String, CloveError> {
    let output = git(&["rev-parse", "HEAD"], Some(clone))?;
    if !output.status.success() {
        return Err(CloveError::Registry {
            message: "could not resolve the cloned commit".to_owned(),
        });
    }
    let sha = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if sha.len() < 40 || !sha.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(CloveError::Registry {
            message: format!("git returned an implausible commit id `{sha}`"),
        });
    }
    Ok(sha)
}

/// Which revision of a repository to install.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitRef {
    Tag(String),
    Branch(String),
    Rev(String),
}

impl GitRef {
    /// The `cargo install` flag pair for this reference.
    pub fn cargo_args(&self) -> [String; 2] {
        match self {
            GitRef::Tag(v) => ["--tag".to_owned(), v.clone()],
            GitRef::Branch(v) => ["--branch".to_owned(), v.clone()],
            GitRef::Rev(v) => ["--rev".to_owned(), v.clone()],
        }
    }
}

/// Find the installable clove plugins in a checked-out repository.
///
/// Resolution goes through `cargo metadata --no-deps --offline`, not a
/// hand-rolled manifest walk. cargo already knows how to expand `members`
/// globs, honor `exclude` and `default-members`, and follow path dependencies
/// that are implicit members; reimplementing that would get a repository laid
/// out even slightly unusually wrong. `--offline` keeps it from touching the
/// network, and `--no-deps` keeps it from resolving the dependency graph.
///
/// A package qualifies only if **all three** hold:
///
/// 1. it depends on `clove-plugin` — the definition of a plugin;
/// 2. it builds a `clove-<something>` binary — otherwise nothing it installs
///    could ever be dispatched;
/// 3. it is publishable (`publish` is not `false`).
///
/// Filtering on the dependency alone is not enough, and this repository is the
/// proof: five of its members depend on `clove-plugin`, including `clove-cli`
/// (the host itself, which builds `clove`) and the `publish = false` echo test
/// fixture. Rules 2 and 3 are what reduce that to the three real plugins.
pub fn find_plugins(clone: &Utf8Path) -> Result<Vec<GitPlugin>, CloveError> {
    let manifest = clone.join("Cargo.toml");
    if !manifest.exists() {
        return Err(CloveError::Registry {
            message: "the repository has no Cargo.toml at its root".to_owned(),
        });
    }

    let output = Command::new("cargo")
        .args([
            "metadata",
            "--no-deps",
            "--offline",
            "--format-version",
            "1",
            "--manifest-path",
            manifest.as_str(),
        ])
        .stdin(Stdio::null())
        .output()
        // A missing cargo is the single most likely failure here and has a
        // specific remedy, so it gets the same actionable message the install
        // path gives rather than a bare OS error. `git` is already handled this
        // way in this module; cargo was the one that was not.
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => super::install::cargo_missing(),
            _ => CloveError::Registry {
                message: format!("could not run cargo metadata: {e}"),
            },
        })?;
    if !output.status.success() {
        return Err(CloveError::Registry {
            message: format!(
                "cargo could not read the repository's manifests: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        });
    }

    let metadata: serde_json::Value =
        serde_json::from_slice(&output.stdout).map_err(|e| CloveError::Registry {
            message: format!("could not parse cargo metadata: {e}"),
        })?;

    Ok(plugins_from_metadata(&metadata))
}

/// The pure half of [`find_plugins`], over an already-parsed `cargo metadata`
/// document — so the three filter rules are unit-testable without a clone.
pub fn plugins_from_metadata(metadata: &serde_json::Value) -> Vec<GitPlugin> {
    let mut out = Vec::new();
    let Some(packages) = metadata["packages"].as_array() else {
        return out;
    };

    for package in packages {
        // 3. `publish = false` renders as an empty array.
        if package["publish"].as_array().is_some_and(|a| a.is_empty()) {
            continue;
        }
        // 1. depends on clove-plugin.
        let depends = package["dependencies"]
            .as_array()
            .is_some_and(|deps| deps.iter().any(|d| d["name"] == "clove-plugin"));
        if !depends {
            continue;
        }
        // 2. builds a dispatchable binary.
        let Some(package_name) = package["name"].as_str() else {
            continue;
        };
        // The binary must be the package's own, validated name — the same rule
        // the crates.io path applies, and for the same reason: taking "the first
        // clove-* target" lets a package install a binary belonging to a
        // different plugin into a directory that outranks `$PATH`. The name is
        // also validated because `--bin` is a glob.
        let bin = package["targets"].as_array().and_then(|targets| {
            targets.iter().find_map(|t| {
                let is_bin = t["kind"]
                    .as_array()
                    .is_some_and(|k| k.iter().any(|k| k == "bin"));
                let name = t["name"].as_str()?;
                (is_bin && name == package_name && super::validate_bin_name(name).is_ok())
                    .then(|| name.to_owned())
            })
        });
        let Some(bin) = bin else {
            continue;
        };
        out.push(GitPlugin {
            package: package_name.to_owned(),
            bin,
        });
    }
    out.sort_by(|a, b| a.package.cmp(&b.package));
    out
}

/// Choose which discovered plugin to install.
///
/// With `--package`, that one or an error naming what is available. Without it,
/// a single candidate is used and several is an error — the design's mock-up
/// offered an interactive picker, but a repository is not a menu the user has
/// already agreed to, and guessing among several packages is exactly the kind of
/// implicit choice the crates.io path also refuses to make.
pub fn select<'a>(
    found: &'a [GitPlugin],
    requested: Option<&str>,
) -> Result<&'a GitPlugin, CloveError> {
    let names = || {
        found
            .iter()
            .map(|p| p.package.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    };

    if let Some(name) = requested {
        return found
            .iter()
            .find(|p| p.package == name)
            .ok_or_else(|| CloveError::InvalidField {
                field: "--package".to_owned(),
                reason: if found.is_empty() {
                    "the repository contains no clove plugin".to_owned()
                } else {
                    format!(
                        "`{name}` is not a clove plugin in this repository; found: {}",
                        names()
                    )
                },
            });
    }

    match found.len() {
        0 => Err(CloveError::InvalidField {
            field: "--git".to_owned(),
            reason: "the repository contains no package that depends on `clove-plugin` \
                     and builds a `clove-*` binary"
                .to_owned(),
        }),
        1 => Ok(&found[0]),
        _ => Err(CloveError::InvalidField {
            field: "--package".to_owned(),
            reason: format!(
                "the repository contains several clove plugins ({}); choose one with \
                 --package",
                names()
            ),
        }),
    }
}

/// The `cargo install` argv for a git install.
pub fn cargo_install_argv(
    url: &str,
    reference: Option<&GitRef>,
    package: &str,
    bin: &str,
    root: &Utf8Path,
    force: bool,
) -> Vec<String> {
    let mut argv = vec![
        "install".to_owned(),
        "--locked".to_owned(),
        "--git".to_owned(),
        url.to_owned(),
    ];
    if let Some(r) = reference {
        argv.extend(r.cargo_args());
    }
    argv.extend([
        "--root".to_owned(),
        root.to_string(),
        // Same reason as the crates.io path: without `--bin`, cargo installs
        // every binary the package declares.
        "--bin".to_owned(),
        bin.to_owned(),
    ]);
    if force {
        argv.push("--force".to_owned());
    }
    // The same `--` the crates.io path uses: `package` comes out of a foreign
    // `Cargo.toml`, so it must not be parsable as an option. It is currently
    // forced to `clove-*` by `plugins_from_metadata`, but that is a rule two
    // functions away — this is the layer that does not depend on remembering it.
    argv.push("--".to_owned());
    argv.push(package.to_owned());
    argv
}

/// The warning shown when installing from a moving reference.
///
/// Without a tag or rev, "the same command" installs different code tomorrow.
pub fn unpinned_warning(url: &str) -> String {
    format!(
        "installing `{url}` from its default branch, which moves — the code you \
         approve now is not necessarily what a later reinstall builds. Pass --tag or \
         --rev to pin it."
    )
}

/// A temporary directory for a clone, removed when dropped.
pub fn temp_clone_dir() -> Result<(tempfile::TempDir, Utf8PathBuf), CloveError> {
    let dir = tempfile::tempdir().map_err(|e| CloveError::Registry {
        message: format!("could not create a temporary directory: {e}"),
    })?;
    let path =
        Utf8PathBuf::from_path_buf(dir.path().join("repo")).map_err(|p| CloveError::Registry {
            message: format!("temporary path is not valid UTF-8: {}", p.display()),
        })?;
    Ok((dir, path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_flag_shaped_git_url_is_refused() {
        // These are the ones that actually execute code if git parses them as
        // options rather than as a URL.
        for url in [
            "--upload-pack=/bin/sh",
            "--template=/tmp/evil",
            "--config=protocol.ext.allow=always",
            "-c",
        ] {
            let err = validate_git_url(url).unwrap_err();
            let message = err.to_string();
            assert!(
                message.contains("cannot start with `-`"),
                "{url}: {message}"
            );
        }
    }

    #[test]
    fn the_ext_transport_is_refused_explicitly() {
        // Modern git denies `ext::` by default, but a host config can re-enable
        // it; refusing here does not depend on the machine's git settings.
        assert!(validate_git_url("ext::sh -c 'id'").is_err());
        assert!(
            validate_git_url("EXT::sh -c 'id'").is_err(),
            "case-insensitive"
        );
    }

    #[test]
    fn ordinary_git_urls_are_accepted() {
        for url in [
            "https://github.com/o/r",
            "http://internal/r.git",
            "ssh://git@host/o/r.git",
            "git://host/r.git",
            "file:///srv/r.git",
            "git@github.com:o/r.git",
        ] {
            assert!(validate_git_url(url).is_ok(), "{url} should be accepted");
        }
    }

    #[test]
    fn an_unknown_scheme_is_refused() {
        assert!(validate_git_url("javascript:alert(1)").is_err());
        assert!(validate_git_url("/etc/passwd").is_err());
        assert!(validate_git_url("").is_err());
        assert!(validate_git_url("https://host/a\nb").is_err());
    }

    #[test]
    fn refs_are_validated_too() {
        assert!(validate_git_ref("--tag", "v1.0.0").is_ok());
        assert!(validate_git_ref("--rev", "abc123").is_ok());
        assert!(validate_git_ref("--tag", "--upload-pack=x").is_err());
        assert!(validate_git_ref("--tag", "").is_err());
        assert!(validate_git_ref("--tag", "a b").is_err());
    }

    /// A `cargo metadata` document shaped like this very repository: five members
    /// depend on `clove-plugin`, only three are installable plugins.
    fn clove_like_metadata() -> serde_json::Value {
        let pkg = |name: &str, bin: &str, publish_false: bool| {
            json!({
                "name": name,
                "publish": if publish_false { json!([]) } else { json!(null) },
                "dependencies": [{"name": "clove-plugin"}],
                "targets": [{"kind": ["bin"], "name": bin}]
            })
        };
        json!({
            "packages": [
                pkg("clove-sync-github", "clove-sync-github", false),
                pkg("clove-import-tk", "clove-import-tk", false),
                pkg("clove-import-beads", "clove-import-beads", false),
                // The host itself: depends on clove-plugin, builds `clove`.
                pkg("clove-cli", "clove", false),
                // The test fixture: publish = false.
                pkg("clove-plugin-echo", "clove-echo", true),
                // An unrelated library.
                json!({
                    "name": "some-lib", "publish": null,
                    "dependencies": [], "targets": [{"kind": ["lib"], "name": "some_lib"}]
                }),
            ]
        })
    }

    #[test]
    fn the_host_and_unpublishable_fixtures_are_not_offered_as_plugins() {
        // Filtering on the `clove-plugin` dependency alone matches five members
        // of this repo, including `clove-cli` — offering the user a six-minute
        // build of clove itself as a "plugin".
        let found = plugins_from_metadata(&clove_like_metadata());
        let names: Vec<&str> = found.iter().map(|p| p.package.as_str()).collect();

        assert_eq!(
            names,
            vec!["clove-import-beads", "clove-import-tk", "clove-sync-github"],
            "exactly the three real plugins"
        );
        assert!(!names.contains(&"clove-cli"), "the host is not a plugin");
        assert!(
            !names.contains(&"clove-plugin-echo"),
            "a publish = false fixture is not installable"
        );
    }

    #[test]
    fn a_package_without_a_dispatchable_binary_is_skipped() {
        let metadata = json!({"packages": [{
            "name": "clove-helper", "publish": null,
            "dependencies": [{"name": "clove-plugin"}],
            "targets": [{"kind": ["bin"], "name": "helper"}]
        }]});
        assert!(
            plugins_from_metadata(&metadata).is_empty(),
            "a bin not named clove-* can never be dispatched"
        );
    }

    #[test]
    fn selection_refuses_to_guess_between_several_plugins() {
        let found = plugins_from_metadata(&clove_like_metadata());
        let err = select(&found, None).unwrap_err().to_string();
        assert!(err.contains("several"), "{err}");
        assert!(
            err.contains("--package"),
            "must say how to disambiguate: {err}"
        );

        // …and honors an explicit choice.
        let chosen = select(&found, Some("clove-import-tk")).unwrap();
        assert_eq!(chosen.bin, "clove-import-tk");

        // A name that is not a plugin here is an error that lists what is.
        let err = select(&found, Some("clove-cli")).unwrap_err().to_string();
        assert!(err.contains("clove-sync-github"), "{err}");
    }

    #[test]
    fn a_repository_with_no_plugin_says_so() {
        let empty = json!({"packages": []});
        let found = plugins_from_metadata(&empty);
        let err = select(&found, None).unwrap_err().to_string();
        assert!(err.contains("no package"), "{err}");
    }

    #[test]
    fn install_argv_pins_the_ref_and_the_single_binary() {
        let root = Utf8PathBuf::from("/root");
        let argv = cargo_install_argv(
            "https://example.com/r",
            Some(&GitRef::Tag("v1.2.3".to_owned())),
            "clove-sync-gitlab",
            "clove-sync-gitlab",
            &root,
            false,
        );
        assert!(argv.contains(&"--git".to_owned()));
        let tag_at = argv.iter().position(|a| a == "--tag").expect("--tag");
        assert_eq!(argv[tag_at + 1], "v1.2.3");
        let bin_at = argv.iter().position(|a| a == "--bin").expect("--bin");
        assert_eq!(argv[bin_at + 1], "clove-sync-gitlab");
        assert_eq!(argv.last().unwrap(), "clove-sync-gitlab");
    }

    #[test]
    fn an_unpinned_install_warns_that_the_branch_moves() {
        let warning = unpinned_warning("https://example.com/r");
        assert!(warning.contains("--tag"), "{warning}");
        assert!(warning.contains("moves"), "{warning}");
    }
}
