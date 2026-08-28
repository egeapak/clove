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

use super::provenance::{Installed, Source};
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
    /// The crate these gates were evaluated for, so the messages can name it.
    pub crate_name: Option<String>,
    pub depends_on_plugin: DependsOnPlugin,
    /// The binary that will be installed — the crate's own, validated name.
    pub dispatchable_bin: Option<String>,
    /// Why no binary qualified, when none did.
    pub bin_problem: Option<String>,
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
        // Name the crate. A bare name resolves through a ladder, so "this crate"
        // left the user unable to tell *which* candidate was rejected — while
        // the neighbouring yanked message names it.
        let crate_name = self
            .crate_name
            .as_deref()
            .map(|n| format!("`{}`", super::display_safe(n)))
            .unwrap_or_else(|| "this crate".to_owned());
        if self.depends_on_plugin == DependsOnPlugin::NotADependent {
            return Some(format!(
                "{crate_name} does not depend on `{REGISTRY_ROOT_CRATE}`, so it is not a \
                 clove plugin"
            ));
        }
        if strict && self.depends_on_plugin == DependsOnPlugin::Unverifiable {
            return Some(format!(
                "could not verify that {crate_name} depends on `{REGISTRY_ROOT_CRATE}` \
                 (the registry could not be consulted), and --strict was given"
            ));
        }
        if self.dispatchable_bin.is_none() {
            return Some(self.bin_problem.clone().unwrap_or_else(|| {
                "this crate builds no `clove-*` binary, so nothing it installs could be \
                 run as a clove subcommand"
                    .to_owned()
            }));
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

    let (dispatchable, bin_problem) = match dispatchable_bin(candidate) {
        Ok(bin) => (Some(bin.to_owned()), None),
        Err(e) => (None, Some(e.to_string())),
    };

    Gates {
        crate_name: Some(candidate.crate_name.clone()),
        depends_on_plugin,
        dispatchable_bin: dispatchable,
        bin_problem,
        version: candidate.display_version(),
        yanked: candidate.fully_yanked(),
    }
}

/// The binary this crate is allowed to install.
///
/// It must be the crate's **own** name. Taking "the first `clove-*` binary"
/// instead is a plugin-shadowing hole: a crate `clove-sync-gitlab` — a genuine
/// `clove-plugin` dependent, so gate 1 passes — can declare
/// `bin_names = ["clove-sync-github", "clove-sync-gitlab"]`, and installing
/// `gitlab` would then place a `clove-sync-github` into `<clove-home>/bin`. That
/// directory outranks `$PATH`, so it shadows a legitimately installed
/// `clove-sync-github`, and the next `clove sync github` execs it with the full
/// inherited environment — `GITHUB_TOKEN` included. `--bin` restricts how *many*
/// binaries are installed; only this restricts *which*.
///
/// The name is validated as well as matched, because `--bin` is a glob: a crate
/// naming a binary `clove-[a-z]*` would otherwise install every binary matching
/// it (verified against cargo 1.94).
///
/// Requiring package == binary is stricter than cargo needs, and deliberately so:
/// that equality is the convention dispatch itself is built on, so a plugin that
/// breaks it could not be dispatched under its own crate name anyway.
pub fn dispatchable_bin(candidate: &RegistryPlugin) -> Result<&str, CloveError> {
    let expected = candidate.crate_name.as_str();
    super::validate_bin_name(expected)?;

    candidate
        .bin_names
        .iter()
        .find(|bin| bin.as_str() == expected)
        .map(String::as_str)
        .ok_or_else(|| CloveError::InvalidField {
            field: "name".to_owned(),
            reason: format!(
                "`{expected}` does not build a binary of its own name (it declares \
                 {}), so clove cannot tell which binary you would be installing",
                if candidate.bin_names.is_empty() {
                    "none".to_owned()
                } else {
                    candidate
                        .bin_names
                        .iter()
                        .map(|b| super::display_safe(b))
                        .collect::<Vec<_>>()
                        .join(", ")
                }
            ),
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

/// Render the confirmation for `plugin install --git <url>`.
///
/// Same shape and same disclaimers as [`prompt_text`], but the evidence is
/// different: there is no registry, no owner, and no download count — only the
/// URL the user typed and what the cloned manifest claims about itself.
///
/// `package` and `bin` are read out of a *third-party* `Cargo.toml`, so they get
/// the same [`super::display_safe`] treatment as registry strings: a package
/// name carrying a newline would otherwise forge a prompt line, and CR/CSI could
/// overwrite the sentence that says third-party code is about to run.
pub fn git_prompt_text(
    url: &str,
    package: &str,
    bin: &str,
    commit: &str,
    shadows: Option<&Utf8Path>,
) -> String {
    let safe = super::display_safe;
    let mut out = String::new();
    out.push_str(&format!("  {}\n", safe(package)));
    out.push_str(&format!(
        "  checks:    depends on {REGISTRY_ROOT_CRATE}; matches the clove plugin \
         convention — not audited\n"
    ));
    out.push_str(&format!("  installs:  {}\n", safe(bin)));
    if let Some(existing) = shadows {
        out.push_str(&format!("  REPLACES:  {}\n", safe(existing.as_str())));
    }
    out.push_str(&format!("  source:    {}\n", safe(url)));
    // "The tree clove inspected is the tree cargo builds" is the whole point of
    // pinning, and it is the mitigation for the moving-branch warning printed
    // just above this. A prompt that will not say *which* tree cannot support
    // either claim.
    out.push_str(&format!("  commit:    {}\n", safe(commit)));
    out.push('\n');
    out.push_str("  Installing builds and runs third-party code as you. Continue? [y/N] ");
    out
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
pub fn prompt_text(
    candidate: &RegistryPlugin,
    gates: &Gates,
    shadows: Option<&Utf8Path>,
) -> String {
    // Every value below comes from the registry response, and this prompt *is*
    // the security decision. Left raw, a `repository` containing a newline forges
    // extra prompt lines ("checks: audited by clove"), and CR/CSI sequences
    // overwrite the line saying third-party code is about to run — before the
    // user answers. See `super::display_safe`.
    let safe = super::display_safe;

    let mut out = String::new();
    let version = safe(gates.version.as_deref().unwrap_or("?"));
    out.push_str(&format!("  {} {version}\n", safe(&candidate.crate_name)));

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
        out.push_str(&format!("  installs:  {}\n", safe(bin)));
    }
    // The single most consequential fact when it applies: this install silently
    // takes over a subcommand that currently runs a different binary.
    if let Some(existing) = shadows {
        out.push_str(&format!("  REPLACES:  {}\n", safe(existing.as_str())));
    }
    if let Some(owner) = &candidate.published_by {
        out.push_str(&format!("  owner:     {}\n", safe(owner)));
    }
    if let Some(repo) = &candidate.repository {
        out.push_str(&format!("  repo:      {}\n", safe(repo)));
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
    // `--` before the positional: the package name reaches here from a registry
    // *response*, not from the validated user input, so a hostile mirror could
    // otherwise return a name like `--config=build.rustc-wrapper=…` and have
    // cargo read it as an option — arbitrary code during the build, bypassing
    // every gate. Callers re-validate the name too; this is the second layer.
    argv.push("--".to_owned());
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
        "--".to_owned(),
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

/// What `update` should do about one installed package.
///
/// This exists because the question is **"is the candidate greater?"**, and the
/// code used to ask "is the string different?". Two real cases that gets wrong:
///
/// - A pre-release. `RegistryPlugin::latest` now excludes them, but a crate
///   whose only releases are pre-releases still resolves to one, and moving to
///   it is not something an unattended `update` should decide.
/// - A *downgrade*. `$CLOVE_REGISTRY_URL` is a documented mirror seam, and a
///   lagging mirror hands back an older `latest`. The string differs, so the old
///   code rendered `1.2.0 → 1.1.0` with an arrow implying forward motion and ran
///   `cargo install --version =1.1.0 --force`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateVerdict {
    /// Move to this version.
    Newer(String),
    /// Do nothing, for this reason.
    Hold(Hold),
}

/// Why an installed package is not being updated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Hold {
    /// The newest published version is the installed one.
    UpToDate,
    /// The registry's newest is older than what is installed.
    RegistryBehind(String),
    /// Only a pre-release is available, and `update` does not move to those.
    PrereleaseOnly(String),
    /// `installed.version` is not a semver version, so nothing is comparable.
    LocalUnparseable,
    /// Nothing published under this name to compare against.
    NoCandidate,
    /// Not installed from the registry, so `update` never looked. The string is
    /// the source label, which names how to update it instead.
    NotChecked(String),
}

/// Decide whether `candidate` is an update for `installed`.
pub fn update_verdict(installed: &Installed, candidate: Option<&RegistryPlugin>) -> UpdateVerdict {
    // "We did not look" is not "there is nothing newer". A git-installed plugin
    // reported "no newer version known", and the summary then said "everything
    // is up to date" — a green light, forever, over a plugin that was never
    // checked. It is also the case where the user most needs to be told what to
    // do instead, since `update` cannot do it for them.
    if !updatable_from_registry(installed) {
        return UpdateVerdict::Hold(Hold::NotChecked(installed.source.label()));
    }
    let Some(candidate) = candidate else {
        return UpdateVerdict::Hold(Hold::NoCandidate);
    };
    // `installed.version` is a raw string out of `.crates2.json`. If it will not
    // parse, say so rather than falling back to a string compare — a bogus local
    // version is a reason to report, not to guess a direction.
    let Ok(current) = semver::Version::parse(&installed.version) else {
        return UpdateVerdict::Hold(Hold::LocalUnparseable);
    };

    match &candidate.latest {
        Some(latest) if *latest > current => UpdateVerdict::Newer(latest.to_string()),
        Some(latest) if *latest == current => UpdateVerdict::Hold(Hold::UpToDate),
        Some(latest) => UpdateVerdict::Hold(Hold::RegistryBehind(latest.to_string())),
        // No stable release. A pre-release is reported but never moved to.
        None => match &candidate.latest_prerelease {
            Some(pre) if *pre > current => {
                UpdateVerdict::Hold(Hold::PrereleaseOnly(pre.to_string()))
            }
            _ => UpdateVerdict::Hold(Hold::NoCandidate),
        },
    }
}

/// Summarize what `update` would change for one installed package.
///
/// Every value is `display_safe`'d, for the same reason `prompt_text` is: these
/// lines *are* the evidence for the `Update these plugins? [y/N]` question, and
/// they come out of `.crates2.json` — a file this module's own threat model
/// treats as attacker-writable. `parse_pkgid` validates the crate *name* but
/// carries the version and the source string through verbatim, so a version of
/// `1.0.0\u{1b}[2K\r  clove-sync-github 0.1.0 (up to date)` forges the plan the
/// user is approving. That is the injection this branch already closed for the
/// install prompt.
pub fn update_line(installed: &Installed, verdict: &UpdateVerdict) -> String {
    let package = super::display_safe(&installed.package);
    let version = super::display_safe(&installed.version);
    let (package, version) = (&package, &version);
    match verdict {
        UpdateVerdict::Newer(v) => format!("  {package} {version} → {v}"),
        UpdateVerdict::Hold(Hold::UpToDate) => format!("  {package} {version} (up to date)"),
        UpdateVerdict::Hold(Hold::RegistryBehind(v)) => {
            format!("  {package} {version} (kept — the registry's newest is {v}, which is older)")
        }
        UpdateVerdict::Hold(Hold::PrereleaseOnly(v)) => format!(
            "  {package} {version} (kept — {v} is a pre-release; install it explicitly to \
             move to it)"
        ),
        UpdateVerdict::Hold(Hold::LocalUnparseable) => format!(
            "  {package} {version} (kept — the installed version is not a semver version, \
             so nothing can be compared)"
        ),
        UpdateVerdict::Hold(Hold::NoCandidate) => {
            format!("  {package} {version} (nothing published under this name)")
        }
        UpdateVerdict::Hold(Hold::NotChecked(label)) => format!(
            "  {package} {version} ({} — not checked; reinstall to update it)",
            super::display_safe(label)
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

/// The on-disk path of an installed plugin binary.
///
/// The executable suffix is **not** optional: cargo writes
/// `clove-sync-github.exe` on Windows, so a path built without it finds nothing —
/// which would make `probe_info` return `None`, the compatibility gate pass
/// vacuously, and the rollback never fire, on the one platform where the whole
/// `bare_subcommand` suffix dance exists.
pub fn installed_binary_path(root: &Utf8Path, bin: &str) -> Utf8PathBuf {
    installed_binary_path_with(root, bin, std::env::consts::EXE_SUFFIX)
}

/// [`installed_binary_path`] with the executable suffix injected.
///
/// The suffix is a compile-time constant, so the other platform's behaviour is
/// unreachable from a test on this one — which is exactly how the missing
/// suffix shipped and sat unnoticed through a Windows CI leg. Taking it as a
/// parameter makes both cases testable everywhere.
pub fn installed_binary_path_with(root: &Utf8Path, bin: &str, exe_suffix: &str) -> Utf8PathBuf {
    bin_dir(root).join(format!("{bin}{exe_suffix}"))
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
            latest_prerelease: None,
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
        let text = prompt_text(&c, &gates, None);
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
            .contains("does not build a binary of its own name"));
    }

    #[test]
    fn a_crate_may_only_install_its_own_binary() {
        // The shadowing hole: a genuine clove-plugin dependent listing someone
        // else's binary first. `--bin` limits how many binaries are installed;
        // only this limits which.
        let c = candidate(
            "clove-sync-gitlab",
            &["clove-sync-github", "clove-sync-gitlab"],
        );
        assert_eq!(dispatchable_bin(&c).unwrap(), "clove-sync-gitlab");

        // And when the crate builds *only* another plugin's binary, there is
        // nothing safe to install.
        let hostile = candidate("clove-sync-gitlab", &["clove-sync-github"]);
        assert!(dispatchable_bin(&hostile).is_err());
    }

    #[test]
    fn a_glob_shaped_binary_name_is_rejected() {
        // `cargo install --bin` is a GLOB (verified against cargo 1.94):
        // `--bin 'clove-[a-z]*'` installs every matching binary, which defeats
        // the single-binary restriction entirely.
        for bad in [
            "clove-[a-z]*",
            "clove-*",
            "clove-a?b",
            "clove-../../etc/passwd",
            "clove-a b",
        ] {
            assert!(
                super::super::validate_bin_name(bad).is_err(),
                "{bad} must be rejected"
            );
        }
        assert!(super::super::validate_bin_name("clove-sync-gitlab").is_ok());
        assert!(super::super::validate_bin_name("clove-").is_err());
        assert!(super::super::validate_bin_name("ripgrep").is_err());
    }

    #[test]
    fn prompt_fields_cannot_forge_extra_lines() {
        // `repository` is free-form publisher text rendered into the consent
        // prompt. A newline forges a line; CR overwrites the warning that
        // third-party code is about to run.
        let mut c = candidate("clove-sync-gitlab", &["clove-sync-gitlab"]);
        c.repository = Some("https://x\n  checks:    audited by clove\r hidden".to_owned());
        c.published_by = Some("someone\u{1b}[2K".to_owned());
        let gates = evaluate(&c, Some(&["clove-sync-gitlab".to_owned()]));
        let text = prompt_text(&c, &gates, None);

        // The prompt has exactly the lines it builds — no injected ones.
        let repo_lines = text
            .lines()
            .filter(|l| l.contains("audited by clove"))
            .count();
        assert_eq!(
            repo_lines, 1,
            "forged text must stay on its own field: {text}"
        );
        assert!(!text.contains('\r'), "{text:?}");
        assert!(!text.contains('\u{1b}'), "no escape sequences: {text:?}");
    }

    #[test]
    fn the_prompt_never_claims_the_plugin_is_verified_or_safe() {
        // The design's mock-up read "✓ verified clove plugin". Every gate is
        // forgeable by the publisher, so that string must not ship.
        let c = candidate("clove-sync-gitlab", &["clove-sync-gitlab"]);
        let gates = evaluate(&c, Some(&["clove-sync-gitlab".to_owned()]));
        let text = prompt_text(&c, &gates, None);

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
    fn the_git_prompt_is_aligned_and_carries_the_same_disclaimers() {
        let text = git_prompt_text(
            "https://github.com/someone/clove-sync-gitlab",
            "clove-sync-gitlab",
            "clove-sync-gitlab",
            "0123456789abcdef0123456789abcdef01234567",
            None,
        );

        // Regression: the git prompt used to be one long `format!` whose folded
        // source lines leaked their indentation into the output, rendering as
        // runs of 18 spaces mid-sentence. Every line is a two-space indent, a
        // padded label, and a value — nothing else.
        // The widest legitimate gap is the four spaces padding the `source:` /
        // `checks:` labels, so anything longer is leaked source indentation.
        assert!(
            !text.contains("     "),
            "stray indentation inside the prompt: {text:?}"
        );

        assert!(
            text.contains("  source:    https://github.com/someone/"),
            "{text}"
        );
        assert!(
            text.contains("  commit:    0123456789abcdef"),
            "the prompt must name the tree cargo will build: {text}"
        );
        assert!(text.contains("  installs:  clove-sync-gitlab"), "{text}");
        assert!(text.contains("not audited"), "{text}");
        assert!(
            text.contains("builds and runs third-party code"),
            "the git path authorizes the same thing the registry path does: {text}"
        );
        assert!(!text.contains('✓'), "{text}");
    }

    #[test]
    fn the_git_prompt_sanitizes_names_read_from_a_foreign_manifest() {
        // `package`/`bin` come out of a Cargo.toml in someone else's repo, so
        // they are exactly as untrusted as a registry string.
        let text = git_prompt_text(
            "https://example.com/r",
            "evil\n  checks:    audited by clove",
            "b\r\u{1b}[2K",
            "0123456789abcdef0123456789abcdef01234567",
            None,
        );
        assert_eq!(
            text.lines()
                .filter(|l| l.contains("audited by clove"))
                .count(),
            1,
            "forged text must stay on its own field: {text}"
        );
        assert!(!text.contains('\r'), "{text:?}");
        assert!(!text.contains('\u{1b}'), "{text:?}");
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
    fn the_consent_matrix_is_total() {
        // This function decides whether unvetted third-party code gets built and
        // run. Sampling five of its sixteen inputs is not enough for that — the
        // rule is stated once here and checked against every combination, so a
        // future edit cannot widen it in a corner nobody sampled.
        for yes in [false, true] {
            for human in [false, true] {
                for stdin_tty in [false, true] {
                    for stderr_tty in [false, true] {
                        let got = consent_policy(yes, human, stdin_tty, stderr_tty);
                        let want = if yes {
                            // An explicit decision, whatever the environment.
                            Consent::Granted
                        } else if human && stdin_tty && stderr_tty {
                            // Someone is there to answer, on a stream that can
                            // carry the question.
                            Consent::Ask
                        } else {
                            // Silence is not consent.
                            Consent::Refuse
                        };
                        assert_eq!(
                            got, want,
                            "consent_policy(yes={yes}, human={human}, \
                             stdin_tty={stdin_tty}, stderr_tty={stderr_tty})"
                        );

                        // Relied on by the three call sites, which print a plain
                        // line rather than an envelope when the user declines:
                        // asking is only ever reachable in human format.
                        if got == Consent::Ask {
                            assert!(human, "a prompt would corrupt a machine-readable envelope");
                        }
                    }
                }
            }
        }
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
        assert!(update_line(&git, &update_verdict(&git, None)).contains("git https://e.com/x"));

        let reg = Installed {
            package: "clove-sync-gitlab".to_owned(),
            version: "0.1.0".to_owned(),
            source: Source::Registry,
            bins: vec!["clove-sync-gitlab".to_owned()],
        };
        assert!(updatable_from_registry(&reg));
        let newer = candidate("clove-sync-gitlab", &["clove-sync-gitlab"]);
        assert_eq!(
            update_line(&reg, &update_verdict(&reg, Some(&newer))),
            "  clove-sync-gitlab 0.1.0 → 0.2.0",
            "update must show what it would change before doing it"
        );
    }

    /// A candidate at an explicit version, for the ordering tests.
    fn candidate_at(name: &str, version: &str) -> RegistryPlugin {
        let mut c = candidate(name, &[name]);
        let parsed = semver::Version::parse(version).unwrap();
        if parsed.pre.is_empty() {
            c.latest = Some(parsed);
            c.latest_prerelease = None;
        } else {
            c.latest = None;
            c.latest_prerelease = Some(parsed);
        }
        c
    }

    fn installed_at(version: &str) -> Installed {
        Installed {
            package: "clove-sync-gitlab".to_owned(),
            version: version.to_owned(),
            source: Source::Registry,
            bins: vec!["clove-sync-gitlab".to_owned()],
        }
    }

    #[test]
    fn an_update_moves_only_to_a_strictly_greater_stable_version() {
        let installed = installed_at("1.2.0");

        // The case a string compare gets right by luck.
        assert_eq!(
            update_verdict(
                &installed,
                Some(&candidate_at("clove-sync-gitlab", "1.3.0"))
            ),
            UpdateVerdict::Newer("1.3.0".to_owned())
        );
        assert_eq!(
            update_verdict(
                &installed,
                Some(&candidate_at("clove-sync-gitlab", "1.2.0"))
            ),
            UpdateVerdict::Hold(Hold::UpToDate)
        );

        // A lagging mirror hands back an *older* `latest`. The strings differ, so
        // `!=` rendered `1.2.0 → 1.1.0` and ran `--version =1.1.0 --force`.
        assert_eq!(
            update_verdict(
                &installed,
                Some(&candidate_at("clove-sync-gitlab", "1.1.0"))
            ),
            UpdateVerdict::Hold(Hold::RegistryBehind("1.1.0".to_owned()))
        );

        // Semver ordering, not lexical: "0.10.0" < "0.9.0" as strings.
        let ten = installed_at("0.10.0");
        assert_eq!(
            update_verdict(&ten, Some(&candidate_at("clove-sync-gitlab", "0.9.0"))),
            UpdateVerdict::Hold(Hold::RegistryBehind("0.9.0".to_owned()))
        );

        // A pre-release outranks a stable release by semver precedence, so
        // folding it into `latest` offered an alpha as an upgrade. It is
        // reported and held.
        assert_eq!(
            update_verdict(
                &installed,
                Some(&candidate_at("clove-sync-gitlab", "2.0.0-alpha.1"))
            ),
            UpdateVerdict::Hold(Hold::PrereleaseOnly("2.0.0-alpha.1".to_owned()))
        );

        // A local version cargo did not write is reported, never guessed at.
        let odd = installed_at("not-a-version");
        assert_eq!(
            update_verdict(&odd, Some(&candidate_at("clove-sync-gitlab", "1.3.0"))),
            UpdateVerdict::Hold(Hold::LocalUnparseable)
        );
    }

    #[test]
    fn the_update_plan_cannot_forge_its_own_lines() {
        // `.crates2.json` is attacker-writable per this module's threat model,
        // and `parse_pkgid` validates only the name — so the version reaches
        // here raw, and these lines are the evidence for the [y/N] question.
        let forged = Installed {
            package: "clove-sync-gitlab".to_owned(),
            version: "1.0.0\u{1b}[2K\r  clove-sync-github 9.9.9 (up to date)".to_owned(),
            source: Source::Registry,
            bins: vec!["clove-sync-gitlab".to_owned()],
        };
        let line = update_line(&forged, &UpdateVerdict::Hold(Hold::UpToDate));
        assert!(!line.contains('\r'), "{line:?}");
        assert!(!line.contains('\u{1b}'), "{line:?}");
        assert_eq!(line.lines().count(), 1, "one package, one line: {line:?}");
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
