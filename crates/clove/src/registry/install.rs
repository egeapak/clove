//! Installing, removing and updating plugin binaries.
//!
//! # The trust model, stated plainly
//!
//! Installing a plugin **builds and runs third-party code as the user**. The
//! checks below are *shape* checks, not trust checks, and the wording the user
//! sees says so. Every one of them is forgeable at zero cost by whoever publishes
//! the crate:
//!
//! - depending on `clove-plugin` is one line in a `Cargo.toml`;
//! - a matching `bin_names` is just naming your `[[bin]]`;
//! - the `--clove-plugin-info` reply is a JSON string to print — and it is
//!   observed only *after* `cargo install` has already executed the crate's
//!   `build.rs` and proc macros.
//!
//! So they establish "this looks like a clove plugin", never "this is safe" or
//! "clove vetted this". What actually protects the user is the prompt: an
//! explicit, per-install decision with the crate, version, owner and source in
//! front of them. That is why the prompt is not skippable by being non-interactive
//! (§ [`Consent`]) and why no allowlist exists — a blessed path is a thing to
//! socially engineer.

use camino::{Utf8Path, Utf8PathBuf};
use clove_types::CloveError;

use super::provenance::{self, Installed, Source};
use super::RegistryPlugin;

/// The crate whose reverse dependencies define the registry.
pub const REGISTRY_ROOT_CRATE: &str = "clove-plugin";

/// How the "does it depend on `clove-plugin`" check came out.
///
/// Three-valued on purpose. "The registry says no" and "the registry could not
/// tell us" must not collapse: the first is a reason to refuse, the second is a
/// reason to say so and let the user decide. Collapsing them either blocks every
/// install whenever discovery is down, or silently accepts an unverified crate —
/// and the second is the wrong direction to fail in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DependsOnPlugin {
    /// Present in `clove-plugin`'s reverse dependencies.
    Verified,
    /// The registry answered, and this crate is not among its dependents.
    NotADependent,
    /// The registry could not be consulted (offline, or `clove-plugin` itself
    /// is not published).
    Unverifiable,
}

/// The outcome of the pre-install checks, as reported to the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Gates {
    pub depends_on_plugin: DependsOnPlugin,
    /// The crate builds a binary the resolver can actually dispatch to.
    pub dispatchable_bin: Option<String>,
    /// The version that will be installed.
    pub version: Option<String>,
    /// True when the resolved version is yanked.
    pub yanked: bool,
}

impl Gates {
    /// Can the install proceed at all, before asking the user?
    ///
    /// Only two conditions are *fatal* here: the registry positively saying this
    /// is not a plugin, and the crate building nothing dispatchable. An
    /// unverifiable dependency is reported and left to the prompt, unless the
    /// caller asked for `--strict`.
    pub fn blocking_reason(&self, strict: bool) -> Option<String> {
        if self.depends_on_plugin == DependsOnPlugin::NotADependent {
            return Some(format!(
                "this crate does not depend on `{REGISTRY_ROOT_CRATE}`, so it is not a \
                 clove plugin"
            ));
        }
        if strict && self.depends_on_plugin == DependsOnPlugin::Unverifiable {
            return Some(format!(
                "could not verify that this crate depends on `{REGISTRY_ROOT_CRATE}` \
                 (registry unavailable), and --strict was given"
            ));
        }
        let Some(bin) = &self.dispatchable_bin else {
            return Some(
                "this crate builds no `clove-*` binary, so nothing it installs could be \
                 run as a clove subcommand"
                    .to_owned(),
            );
        };
        if bin.is_empty() {
            return Some("this crate builds no usable binary name".to_owned());
        }
        None
    }
}

/// Evaluate the pre-install checks for `candidate`.
///
/// `dependents` is the registry's answer: `None` when it could not be consulted.
/// It is passed in rather than fetched here so the caller controls freshness —
/// install evidence must come from a live fetch, never from the TTL cache, which
/// is a file anyone able to set `$CLOVE_HOME` can write.
pub fn evaluate(candidate: &RegistryPlugin, dependents: Option<&[String]>) -> Gates {
    let depends_on_plugin = match dependents {
        Some(names) => {
            if names.iter().any(|n| n == &candidate.crate_name) {
                DependsOnPlugin::Verified
            } else {
                DependsOnPlugin::NotADependent
            }
        }
        None => DependsOnPlugin::Unverifiable,
    };

    Gates {
        depends_on_plugin,
        dispatchable_bin: dispatchable_bin(candidate).map(str::to_owned),
        version: candidate.display_version(),
        yanked: candidate.fully_yanked(),
    }
}

/// The first binary the resolver could dispatch to (`clove-<something>`).
pub fn dispatchable_bin(candidate: &RegistryPlugin) -> Option<&str> {
    candidate.bin_names.iter().find_map(|bin| {
        provenance::bare_subcommand(bin)?;
        Some(bin.as_str())
    })
}

/// Whether the user has consented to an install.
///
/// The rule the old design got backwards: **a non-interactive run refuses**.
/// `PLUGIN_REGISTRY.md` §5 originally said non-TTY/JSON "proceeds (scriptable)",
/// which is precisely the CI-and-agent case where building an unvetted crate does
/// the most damage and nobody is watching. Automation states its intent with
/// `--yes`; silence is not consent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Consent {
    /// `--yes` was given: proceed without asking.
    Granted,
    /// Interactive and human-readable: ask.
    Ask,
    /// No TTY, or a machine-readable format, and no `--yes`: refuse.
    Refuse,
}

/// Decide how consent must be obtained.
///
/// `human` is whether the output format is the human one — a prompt written into
/// a JSON envelope would corrupt it, so a `--format json` run is treated as
/// automation regardless of the terminal.
pub fn consent_policy(yes: bool, human: bool, stdin_tty: bool, stderr_tty: bool) -> Consent {
    if yes {
        return Consent::Granted;
    }
    if human && stdin_tty && stderr_tty {
        return Consent::Ask;
    }
    Consent::Refuse
}

/// The message shown when consent cannot be obtained.
pub fn refusal_message() -> String {
    "installing builds and runs third-party code, so it needs an explicit decision; \
     re-run with --yes to confirm, or run it in a terminal"
        .to_owned()
}

/// Render the confirmation the user is asked to approve.
///
/// Deliberately free of any "verified" claim. The design's mock-up showed
/// `✓ verified clove plugin`, which reads as "clove vetted this" — see the module
/// docs for why nothing here can support that.
pub fn prompt_text(candidate: &RegistryPlugin, gates: &Gates) -> String {
    let mut out = String::new();
    let version = gates.version.as_deref().unwrap_or("?");
    out.push_str(&format!("  {} {version}\n", candidate.crate_name));

    let shape = match gates.depends_on_plugin {
        DependsOnPlugin::Verified => {
            format!("depends on {REGISTRY_ROOT_CRATE}; matches the clove plugin convention")
        }
        DependsOnPlugin::Unverifiable => {
            format!("could NOT verify it depends on {REGISTRY_ROOT_CRATE} (registry unavailable)")
        }
        DependsOnPlugin::NotADependent => "not a clove plugin".to_owned(),
    };
    out.push_str(&format!("  checks:    {shape} — not audited\n"));

    if let Some(bin) = &gates.dispatchable_bin {
        out.push_str(&format!("  installs:  {bin}\n"));
    }
    if let Some(owner) = &candidate.published_by {
        out.push_str(&format!("  owner:     {owner}\n"));
    }
    if let Some(repo) = &candidate.repository {
        out.push_str(&format!("  repo:      {repo}\n"));
    }
    out.push_str(&format!("  downloads: {}\n", candidate.downloads));
    out.push('\n');
    out.push_str("  Installing builds and runs third-party code as you. Continue? [y/N] ");
    out
}

/// Read a yes/no answer from the controlling terminal.
///
/// Reads `/dev/tty` rather than stdin so a piped script (`curl … | clove …`)
/// cannot feed the answer, and treats EOF as **No** — an empty stdin must never
/// read as consent.
pub fn ask_confirmation(prompt: &str) -> bool {
    use std::io::{BufRead, BufReader, Write};

    eprint!("{prompt}");
    let _ = std::io::stderr().flush();

    #[cfg(unix)]
    let source = std::fs::File::open("/dev/tty").ok();
    #[cfg(not(unix))]
    let source: Option<std::fs::File> = None;

    let mut line = String::new();
    let read = match source {
        Some(tty) => BufReader::new(tty).read_line(&mut line),
        None => std::io::stdin().read_line(&mut line),
    };
    match read {
        Ok(0) | Err(_) => false, // EOF or unreadable → No.
        Ok(_) => matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes"),
    }
}

/// The `cargo install` argv for a crates.io install.
///
/// Two details are load-bearing:
///
/// - **`--bin`**: without it `cargo install` installs *every* binary the crate
///   declares. A crate could then ship an extra `clove-sync-github` alongside the
///   one being installed and have it land in the search path — where it would
///   receive the full inherited environment, `GITHUB_TOKEN` included, on the next
///   `clove sync github`. Restricting to the one binary the user agreed to is the
///   difference between installing a plugin and granting a crate a name.
/// - **`--version =X.Y.Z`**: an exact pin. Without it cargo re-resolves and may
///   pick a different version than the one whose metadata the user just approved,
///   which would make the whole prompt advisory.
pub fn cargo_install_argv(
    package: &str,
    version: Option<&str>,
    bin: &str,
    root: &Utf8Path,
    force: bool,
) -> Vec<String> {
    let mut argv = vec![
        "install".to_owned(),
        "--locked".to_owned(),
        "--root".to_owned(),
        root.to_string(),
        "--bin".to_owned(),
        bin.to_owned(),
    ];
    if let Some(v) = version {
        argv.push("--version".to_owned());
        argv.push(format!("={v}"));
    }
    if force {
        argv.push("--force".to_owned());
    }
    argv.push(package.to_owned());
    argv
}

/// The `cargo uninstall` argv. Takes the **package** name, which
/// [`provenance`] resolves from the subcommand the user typed.
pub fn cargo_uninstall_argv(package: &str, root: &Utf8Path) -> Vec<String> {
    vec![
        "uninstall".to_owned(),
        "--root".to_owned(),
        root.to_string(),
        package.to_owned(),
    ]
}

/// Undo an install whose post-build probe rejected it.
///
/// Gate 3 runs *after* `cargo install` has already placed the binary in
/// `<clove-home>/bin`, which is on the plugin search path. Refusing without
/// removing it would leave the rejected binary resolvable by the next dispatch —
/// a gate that reports but does not gate.
pub fn rollback_argv(package: &str, root: &Utf8Path) -> Vec<String> {
    cargo_uninstall_argv(package, root)
}

/// A plugin that resolves from outside the clove-managed root.
///
/// Everything written before the install command told users to
/// `cargo install clove-sync-github`, which lands in `~/.cargo/bin`. Those
/// installs are real and working, but `uninstall`/`update` cannot touch them, so
/// the commands name the situation instead of failing with cargo's "package is
/// not installed".
pub fn foreign_install_message(name: &str, path: &Utf8Path, root: &Utf8Path) -> String {
    format!(
        "`{name}` resolves from {path}, which is outside the clove-managed root \
         ({root}). clove can only manage what it installed; remove it with the tool \
         that installed it (for a `cargo install`, `cargo uninstall`)."
    )
}

/// Summarize what `update` would change for one installed package.
pub fn update_line(installed: &Installed, latest: Option<&str>) -> String {
    match latest {
        Some(v) if v != installed.version => {
            format!("  {} {} → {v}", installed.package, installed.version)
        }
        Some(_) => format!("  {} {} (up to date)", installed.package, installed.version),
        None => format!(
            "  {} {} ({} — no newer version known)",
            installed.package,
            installed.version,
            installed.source.label()
        ),
    }
}

/// Is this package one `update` can re-resolve from crates.io?
pub fn updatable_from_registry(installed: &Installed) -> bool {
    installed.source == Source::Registry
}

/// The install root's `bin/` directory.
pub fn bin_dir(root: &Utf8Path) -> Utf8PathBuf {
    root.join("bin")
}

/// Map a cargo invocation failure onto a clove error.
pub fn cargo_failure(command: &str, code: Option<i32>) -> CloveError {
    CloveError::Registry {
        message: match code {
            Some(c) => format!("`cargo {command}` failed (exit {c})"),
            None => format!("`cargo {command}` was terminated by a signal"),
        },
    }
}

/// `cargo` is missing from the machine.
pub fn cargo_missing() -> CloveError {
    CloveError::Registry {
        message: "`cargo` was not found on PATH; installing a plugin builds it from \
                  source, so a Rust toolchain is required (see https://rustup.rs)"
            .to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(name: &str, bins: &[&str]) -> RegistryPlugin {
        RegistryPlugin {
            crate_name: name.to_owned(),
            latest: Some(semver::Version::new(0, 2, 0)),
            latest_yanked: None,
            description: Some("a plugin".to_owned()),
            repository: Some("https://example.com/p".to_owned()),
            bin_names: bins.iter().map(|b| (*b).to_owned()).collect(),
            published_by: Some("someone".to_owned()),
            downloads: 41,
        }
    }

    #[test]
    fn a_crate_the_registry_disowns_is_refused() {
        let c = candidate("clove-sync-evil", &["clove-sync-evil"]);
        let gates = evaluate(&c, Some(&["clove-sync-gitlab".to_owned()]));
        assert_eq!(gates.depends_on_plugin, DependsOnPlugin::NotADependent);
        assert!(gates.blocking_reason(false).is_some());
    }

    #[test]
    fn an_unverifiable_dependency_is_not_fatal_unless_strict() {
        // The registry being unreachable must not block every install — but it
        // must be visible, and --strict must be able to make it fatal.
        let c = candidate("clove-sync-gitlab", &["clove-sync-gitlab"]);
        let gates = evaluate(&c, None);
        assert_eq!(gates.depends_on_plugin, DependsOnPlugin::Unverifiable);
        assert_eq!(gates.blocking_reason(false), None);
        assert!(gates.blocking_reason(true).is_some());

        // And the prompt says so out loud rather than implying a clean check.
        let text = prompt_text(&c, &gates);
        assert!(text.contains("could NOT verify"), "{text}");
    }

    #[test]
    fn a_crate_with_no_dispatchable_binary_is_refused() {
        let c = candidate("clove-sync-odd", &["helper"]);
        let gates = evaluate(&c, Some(&["clove-sync-odd".to_owned()]));
        assert_eq!(gates.dispatchable_bin, None);
        assert!(gates
            .blocking_reason(false)
            .unwrap()
            .contains("no `clove-*` binary"));
    }

    #[test]
    fn the_prompt_never_claims_the_plugin_is_verified_or_safe() {
        // The design's mock-up read "✓ verified clove plugin". Every gate is
        // forgeable by the publisher, so that string must not ship.
        let c = candidate("clove-sync-gitlab", &["clove-sync-gitlab"]);
        let gates = evaluate(&c, Some(&["clove-sync-gitlab".to_owned()]));
        let text = prompt_text(&c, &gates);

        assert!(!text.contains('✓'), "{text}");
        assert!(
            !text.to_lowercase().contains("verified clove plugin"),
            "{text}"
        );
        assert!(text.contains("not audited"), "{text}");
        assert!(
            text.contains("builds and runs third-party code"),
            "the prompt must state what it is actually authorizing: {text}"
        );
        // The facts a person needs to decide.
        assert!(text.contains("someone"), "owner: {text}");
        assert!(text.contains("https://example.com/p"), "repo: {text}");
    }

    #[test]
    fn a_non_interactive_run_refuses_rather_than_proceeding() {
        // The rule the superseded design had backwards. Proceeding here is
        // unattended execution of third-party code in exactly the CI/agent case
        // where nobody is watching.
        assert_eq!(
            consent_policy(false, true, false, true),
            Consent::Refuse,
            "no stdin tty"
        );
        assert_eq!(
            consent_policy(false, true, true, false),
            Consent::Refuse,
            "no stderr tty"
        );
        assert_eq!(
            consent_policy(false, false, true, true),
            Consent::Refuse,
            "a JSON run is automation; a prompt would corrupt the envelope"
        );
        // Automation states its intent explicitly.
        assert_eq!(consent_policy(true, false, false, false), Consent::Granted);
        // A human at a terminal is asked.
        assert_eq!(consent_policy(false, true, true, true), Consent::Ask);
    }

    #[test]
    fn install_argv_pins_the_version_and_the_single_binary() {
        let root = Utf8PathBuf::from("/home/u/.local/share/clove");
        let argv = cargo_install_argv(
            "clove-sync-gitlab",
            Some("0.2.0"),
            "clove-sync-gitlab",
            &root,
            false,
        );

        // `--bin` stops a crate installing extra binaries into the search path.
        let bin_at = argv
            .iter()
            .position(|a| a == "--bin")
            .expect("--bin present");
        assert_eq!(argv[bin_at + 1], "clove-sync-gitlab");

        // An exact pin, or the prompt the user just answered was advisory.
        let ver_at = argv
            .iter()
            .position(|a| a == "--version")
            .expect("--version");
        assert_eq!(argv[ver_at + 1], "=0.2.0");

        assert!(argv.contains(&"--locked".to_owned()));
        assert_eq!(argv.last().unwrap(), "clove-sync-gitlab");
        assert!(!argv.contains(&"--force".to_owned()));
    }

    #[test]
    fn uninstall_takes_the_package_name_not_the_binary() {
        // `cargo uninstall clove-echo` fails: the package is clove-plugin-echo.
        let root = Utf8PathBuf::from("/root");
        let argv = cargo_uninstall_argv("clove-plugin-echo", &root);
        assert_eq!(argv.last().unwrap(), "clove-plugin-echo");
        assert!(argv.contains(&"--root".to_owned()));
    }

    #[test]
    fn rollback_removes_the_package_that_failed_its_probe() {
        let root = Utf8PathBuf::from("/root");
        assert_eq!(
            rollback_argv("clove-sync-bad", &root),
            cargo_uninstall_argv("clove-sync-bad", &root),
            "a refused plugin must be removed, not left on the search path"
        );
    }

    #[test]
    fn update_only_re_resolves_registry_installs() {
        let git = Installed {
            package: "clove-sync-x".to_owned(),
            version: "0.1.0".to_owned(),
            source: Source::Git {
                url: "https://e.com/x".to_owned(),
                reference: None,
            },
            bins: vec!["clove-sync-x".to_owned()],
        };
        assert!(!updatable_from_registry(&git));
        // …and says why, rather than silently converting it to a registry install.
        assert!(update_line(&git, None).contains("git https://e.com/x"));

        let reg = Installed {
            package: "clove-sync-gitlab".to_owned(),
            version: "0.1.0".to_owned(),
            source: Source::Registry,
            bins: vec!["clove-sync-gitlab".to_owned()],
        };
        assert!(updatable_from_registry(&reg));
        assert_eq!(
            update_line(&reg, Some("0.2.0")),
            "  clove-sync-gitlab 0.1.0 → 0.2.0",
            "update must show what it would change before doing it"
        );
        assert!(update_line(&reg, Some("0.1.0")).contains("up to date"));
    }

    #[test]
    fn a_plugin_installed_outside_the_root_is_named_not_mangled() {
        let msg = foreign_install_message(
            "sync-github",
            Utf8Path::new("/home/u/.cargo/bin/clove-sync-github"),
            Utf8Path::new("/home/u/.local/share/clove"),
        );
        assert!(msg.contains(".cargo/bin"));
        assert!(msg.contains("cargo uninstall"), "must say how to remove it");
    }

    #[test]
    fn a_yanked_resolution_is_visible_in_the_gates() {
        let mut c = candidate("clove-sync-gone", &["clove-sync-gone"]);
        c.latest = None;
        c.latest_yanked = Some(semver::Version::new(0, 3, 0));
        let gates = evaluate(&c, Some(&["clove-sync-gone".to_owned()]));
        assert!(gates.yanked);
        assert_eq!(gates.version.as_deref(), Some("0.3.0"));
    }
}
