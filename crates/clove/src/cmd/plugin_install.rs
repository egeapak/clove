//! `clove plugin install` / `uninstall` / `update`.
//!
//! The policy — the gates, the consent rule, the exact cargo argv, the rollback —
//! lives in [`crate::registry::install`], which is pure and unit-tested. This
//! module is the orchestration: resolve a name against the registry, ask, shell
//! out to cargo, probe the result, and report.
//!
//! **Install evidence is always fetched live.** The discovery cache is never
//! consulted here: it is a file anyone able to set `$CLOVE_HOME` can write, and
//! it must not be able to decide what counts as a plugin.

use camino::{Utf8Path, Utf8PathBuf};
use clove_core::OutputFormat;
use clove_types::CloveError;
use serde_json::{json, Value};

use crate::cli::{PluginInstallArgs, PluginUninstallArgs, PluginUpdateArgs};
use crate::output::print_json_success;
use crate::registry::crates_io::CratesIo;
use crate::registry::http::UreqFetch;
use crate::registry::install::{
    self, cargo_failure, cargo_install_argv, cargo_missing, cargo_uninstall_argv, consent_policy,
    rollback_argv, Consent, Gates, REGISTRY_ROOT_CRATE,
};
use crate::registry::provenance::{self, Installed};
use crate::registry::RegistryPlugin;

/// The plugin binary this install would shadow, if any.
///
/// `<clove-home>/bin` outranks `$PATH` in dispatch, so writing a binary there
/// silently takes over a subcommand that currently resolves elsewhere — most
/// obviously a `clove-sync-github` the user installed with `cargo install`,
/// which lives in `~/.cargo/bin`. The next `clove sync github` would exec the
/// new one with the full inherited environment, `GITHUB_TOKEN` included.
///
/// `dispatchable_bin` closes the *different* door — crate A installing crate B's
/// binary. It does nothing about a crate that is simply *named*
/// `clove-sync-github`, which is the whole point of `--git` against an arbitrary
/// forge. So the user is told, and must pass `--force` to proceed.
fn shadowed_binary(bin: &str, installed_path: &Utf8Path) -> Option<Utf8PathBuf> {
    let subcommand = provenance::bare_subcommand(bin)?;
    let existing = crate::plugin::resolve(&[subcommand])?;
    (existing != installed_path).then_some(existing)
}

/// The refusal shown when an install would shadow a plugin already on the path.
fn shadow_refusal(bin: &str, existing: &Utf8Path) -> String {
    format!(
        "installing `{bin}` would shadow the `{bin}` already resolving from \
         {existing}: clove's install root is searched before $PATH, so that copy \
         would stop being used. Pass --force to install it anyway, or remove the \
         existing one first."
    )
}

/// `clove plugin install <name>` or `--git <url>`.
pub fn run_install(format: OutputFormat, args: &PluginInstallArgs) -> Result<(), CloveError> {
    match (&args.git, &args.name) {
        (Some(url), _) => install_from_git(format, args, url),
        (None, Some(name)) => install_from_registry(format, args, name),
        (None, None) => Err(CloveError::InvalidField {
            field: "name".to_owned(),
            reason: "name a plugin to install, or pass --git <url>".to_owned(),
        }),
    }
}

/// Install from a git repository.
fn install_from_git(
    format: OutputFormat,
    args: &PluginInstallArgs,
    url: &str,
) -> Result<(), CloveError> {
    use crate::registry::git_source::{self, GitRef};

    let home = crate::clove_home::clove_home()?;
    git_source::validate_git_url(url)?;

    let reference = match (&args.tag, &args.rev, &args.branch) {
        (Some(t), _, _) => {
            git_source::validate_git_ref("--tag", t)?;
            Some(GitRef::Tag(t.clone()))
        }
        (_, Some(r), _) => {
            git_source::validate_git_ref("--rev", r)?;
            Some(GitRef::Rev(r.clone()))
        }
        (_, _, Some(b)) => {
            git_source::validate_git_ref("--branch", b)?;
            Some(GitRef::Branch(b.clone()))
        }
        _ => None,
    };
    if let Some(p) = &args.package {
        crate::registry::validate_crate_name(p)?;
    }

    git_source::probe_remote(url)?;

    // Clone into a temp dir purely to read the manifests; the actual build is
    // done by `cargo install --git`, which does its own clone. Inspecting first
    // is what lets the user be told *what* they are about to build.
    let (_guard, clone) = git_source::temp_clone_dir()?;
    git_source::shallow_clone(url, reference.as_ref(), &clone)?;
    let found = git_source::find_plugins(&clone)?;
    let chosen = git_source::select(&found, args.package.as_deref())?;

    // Build exactly the commit that was just inspected. cargo does its own
    // clone, so without this the user approves one tree and cargo builds
    // whatever the host serves the second time; a tag or branch is a mutable
    // name, not a commitment.
    let pinned = GitRef::Rev(git_source::resolve_head(&clone)?);

    let installed_path = install::installed_binary_path(&home, &chosen.bin);
    let shadowed = shadowed_binary(&chosen.bin, &installed_path);
    if let Some(existing) = &shadowed {
        if !args.force {
            return Err(CloveError::InvalidField {
                field: "--git".to_owned(),
                reason: shadow_refusal(&chosen.bin, existing),
            });
        }
    }

    let mut warnings = Vec::new();
    if reference.is_none() {
        warnings.push(git_source::unpinned_warning(url));
    }
    if let Some(existing) = &shadowed {
        warnings.push(format!(
            "`{}` now shadows the copy at {existing}",
            chosen.bin
        ));
    }

    // The same consent rule as the crates.io path — this builds and runs
    // third-party code too, and from a source clove knows even less about.
    match consent_policy(
        args.yes,
        format == OutputFormat::Human,
        is_tty_stdin(),
        is_tty_stderr(),
    ) {
        Consent::Granted => {}
        Consent::Ask => {
            // Not printed here: the same warnings are printed after the install
            // below, and doing both showed the unpinned-branch warning twice.
            let prompt = install::git_prompt_text(url, &chosen.package, &chosen.bin);
            if !install::ask_confirmation(&prompt) {
                emit(
                    format,
                    json!({ "installed": false, "declined": true }),
                    "not installed",
                );
                return Ok(());
            }
        }
        Consent::Refuse => {
            return Err(CloveError::InvalidField {
                field: "--yes".to_owned(),
                reason: install::refusal_message(),
            })
        }
    }

    let argv = git_source::cargo_install_argv(
        url,
        Some(&pinned),
        &chosen.package,
        &chosen.bin,
        &home,
        args.force,
    );
    run_cargo(&argv, "install")?;

    let probe = crate::plugin::probe_info(&installed_path);
    if let Some(reason) = incompatible_reason(&chosen.package, probe.as_ref()) {
        return Err(CloveError::InvalidField {
            field: "--git".to_owned(),
            reason: roll_back(&chosen.package, &home, &installed_path, reason),
        });
    }

    for warning in &warnings {
        if format == OutputFormat::Human {
            eprintln!("warning: {warning}");
        }
    }
    emit_with_warnings(
        format,
        json!({
            "installed": true,
            "package": chosen.package,
            "binary": chosen.bin,
            "source": url,
            "commit": match &pinned { GitRef::Rev(sha) => sha.as_str(), _ => "" },
            "path": installed_path.as_str(),
        }),
        &format!("installed {} from {url} as {}", chosen.package, chosen.bin),
        warnings,
    );
    Ok(())
}

/// Install from crates.io.
fn install_from_registry(
    format: OutputFormat,
    args: &PluginInstallArgs,
    name: &str,
) -> Result<(), CloveError> {
    let home = crate::clove_home::clove_home()?;
    let fetch = UreqFetch::new();
    let client = CratesIo::new(&fetch);

    let candidate = resolve_candidate(&client, name)?;
    // The name that reaches cargo's argv comes from the registry response, not
    // from the validated user input, so it is re-checked here. `validate_crate_name`
    // documents itself as the single choke point for anything entering a URL or a
    // subprocess argv; this is the call that makes that true on the way back.
    crate::registry::validate_crate_name(&candidate.crate_name)?;

    // Gate 1 evidence, fetched live.
    //
    // A *transport* failure aborts. `.ok()` here was the exact conflation the
    // `Fetch` return type exists to prevent: a 429 or a TLS error became
    // `Unverifiable`, which `blocking_reason` deliberately does not block on, so
    // the gate silently did not run and the crate installed anyway. `--yes`
    // skips the prompt, which is the only place an `Unverifiable` verdict is
    // spoken, so nothing told the user. `update` already got this right; this is
    // the same rule.
    //
    // `Ok(None)` is different and still proceeds: it means `clove-plugin` itself
    // is not published, so there is no registry to be a member of yet.
    let dependents: Option<Vec<String>> = client
        .reverse_dependents(REGISTRY_ROOT_CRATE)
        .map_err(CloveError::from)?
        .map(|plugins| plugins.into_iter().map(|p| p.crate_name).collect());

    let gates = install::evaluate(&candidate, dependents.as_deref());

    if let Some(reason) = gates.blocking_reason(args.strict) {
        return Err(CloveError::InvalidField {
            field: "name".to_owned(),
            reason,
        });
    }
    if gates.yanked && !args.allow_yanked {
        return Err(CloveError::InvalidField {
            field: "name".to_owned(),
            reason: format!(
                "every published version of `{}` is yanked; pass --allow-yanked to \
                 install one anyway",
                candidate.crate_name
            ),
        });
    }

    // Already installed?
    if let Some(existing) = provenance::find_by_subcommand(&home, subcommand_of(&gates)) {
        if !args.force {
            emit(
                format,
                json!({
                    "installed": false,
                    "already_installed": true,
                    "package": existing.package,
                    "version": existing.version,
                }),
                &format!(
                    "{} {} is already installed (--force to reinstall)",
                    existing.package, existing.version
                ),
            );
            return Ok(());
        }
    }

    // Would this take over a subcommand that already resolves elsewhere?
    // `find_by_subcommand` above only knows what *clove* installed, so it says
    // nothing about a `cargo install`ed copy in `~/.cargo/bin`.
    let bin_name = gates
        .dispatchable_bin
        .clone()
        .expect("a blocking reason would have fired");
    let installed_path = install::installed_binary_path(&home, &bin_name);
    let shadowed = shadowed_binary(&bin_name, &installed_path);
    if let Some(existing) = &shadowed {
        if !args.force {
            return Err(CloveError::InvalidField {
                field: "name".to_owned(),
                reason: shadow_refusal(&bin_name, existing),
            });
        }
    }

    match consent_policy(
        args.yes,
        format == OutputFormat::Human,
        is_tty_stdin(),
        is_tty_stderr(),
    ) {
        Consent::Granted => {}
        Consent::Ask => {
            let prompt = install::prompt_text(&candidate, &gates, shadowed.as_deref());
            if !install::ask_confirmation(&prompt) {
                emit(
                    format,
                    json!({ "installed": false, "declined": true }),
                    "not installed",
                );
                return Ok(());
            }
        }
        Consent::Refuse => {
            return Err(CloveError::InvalidField {
                field: "--yes".to_owned(),
                reason: install::refusal_message(),
            })
        }
    }

    let bin = bin_name.as_str();
    let argv = cargo_install_argv(
        &candidate.crate_name,
        gates.version.as_deref(),
        bin,
        &home,
        args.force,
    );
    run_cargo(&argv, "install")?;

    // Gate 3: probe the *built artifact*. This runs after cargo has already
    // executed the crate's build scripts, so it is not a safety check — it is a
    // compatibility check, and an incompatible plugin must be removed rather than
    // left resolvable on the search path.
    let probe = crate::plugin::probe_info(&installed_path);
    if let Some(reason) = incompatible_reason(&candidate.crate_name, probe.as_ref()) {
        return Err(CloveError::InvalidField {
            field: "name".to_owned(),
            reason: roll_back(&candidate.crate_name, &home, &installed_path, reason),
        });
    }

    let mut warnings = match &probe {
        Some(info) if info.max_clove_plugin_api < clove_plugin::CLOVE_PLUGIN_API => vec![format!(
            "`{}` predates this clove (plugin API {} < {}); it will run, but may not \
             understand everything this version sends",
            candidate.crate_name,
            info.max_clove_plugin_api,
            clove_plugin::CLOVE_PLUGIN_API
        )],
        None => vec![format!(
            "`{}` did not answer --clove-plugin-info; it is installed, but clove cannot \
             report what it provides",
            candidate.crate_name
        )],
        Some(_) => Vec::new(),
    };

    // An unverified gate 1 must survive into the result. The prompt says so, but
    // `--yes` and `--format json` never render the prompt — so without this, the
    // one run that most needs the caveat (unattended, machine-read) was the one
    // that never saw it.
    if let Some(existing) = &shadowed {
        warnings.push(format!("`{bin}` now shadows the copy at {existing}"));
    }
    if gates.depends_on_plugin == install::DependsOnPlugin::Unverifiable {
        warnings.push(format!(
            "could not verify that `{}` depends on `{REGISTRY_ROOT_CRATE}`: it is \
             installed, but the check that distinguishes a clove plugin from any \
             other crate did not run (pass --strict to make this fatal)",
            candidate.crate_name
        ));
    }

    for warning in &warnings {
        if format == OutputFormat::Human {
            eprintln!("warning: {warning}");
        }
    }
    emit_with_warnings(
        format,
        json!({
            "installed": true,
            "package": candidate.crate_name,
            "version": gates.version,
            "binary": bin,
            "path": installed_path.as_str(),
        }),
        &format!(
            "installed {} {} as {bin}",
            candidate.crate_name,
            gates.version.as_deref().unwrap_or("?")
        ),
        warnings,
    );
    Ok(())
}

/// Remove a rejected install, and report honestly whether that worked.
///
/// The previous code discarded the uninstall's result while telling the user
/// "the install has been rolled back" unconditionally. If the rollback fails the
/// rejected binary is still in `<clove-home>/bin`, which outranks `$PATH` — so
/// the message must not assert an action that did not happen, and must say what
/// to delete.
fn roll_back(package: &str, home: &Utf8Path, installed_path: &Utf8Path, reason: String) -> String {
    let uninstalled = run_cargo(&rollback_argv(package, home), "uninstall").is_ok();
    // cargo's exit code is not proof the file is gone. `cargo uninstall` exits 0
    // while leaving a binary another `.crates2.json` entry also claims — verified
    // against cargo 1.94 with two packages sharing a bin name under one --root.
    // That state is reachable here: installing the same package from crates.io
    // and then from `--git --force` writes two entries. Reporting "rolled back"
    // on the strength of the exit code would tell the user a rejected binary was
    // removed while it still sits in a directory that outranks `$PATH`.
    if uninstalled && !installed_path.exists() {
        return format!("{reason}; the install has been rolled back");
    }
    format!(
        "{reason}. Rolling the install back FAILED — `{installed_path}` is still \
         present and is on the plugin search path. Remove it manually."
    )
}

/// Why the freshly-installed binary is unusable with this clove, if it is.
///
/// This is gate 3, and it runs *after* cargo has built and therefore already
/// executed the crate's build script — so it is a compatibility check, not a
/// safety one. A failure must roll the install back: the binary is already in
/// `<clove-home>/bin`, which is on the plugin search path.
///
/// The probe is taken as an argument rather than performed here so the caller
/// spawns the just-downloaded third-party binary exactly **once**. Probing again
/// internally meant two executions of an unvetted program, and two places whose
/// verdicts could disagree if the second one answered differently.
fn incompatible_reason(package: &str, probe: Option<&crate::plugin::ProbedInfo>) -> Option<String> {
    let info = probe?;
    (info.min_clove_plugin_api > clove_plugin::CLOVE_PLUGIN_API).then(|| {
        format!(
            "`{package}` needs a newer clove (it requires plugin API {}, this clove \
             provides {}); the install has been rolled back",
            info.min_clove_plugin_api,
            clove_plugin::CLOVE_PLUGIN_API
        )
    })
}

/// Find the clove-installed plugin a user-typed name refers to.
///
/// `install` resolves a bare name through a candidate ladder (`gitlab` →
/// `clove-sync-gitlab`), so `uninstall` and `update` must resolve it the same
/// way or the name that installed a plugin cannot remove or update it. Matching
/// only the literal subcommand meant `clove plugin install gitlab` succeeded and
/// `clove plugin uninstall gitlab` answered "no plugin `gitlab` was installed by
/// clove" — about a plugin clove had just installed.
///
/// An exact subcommand match wins outright (`uninstall sync-gitlab` is
/// unambiguous by construction). Only when there is none does the mux ladder
/// apply, and a name that expands to more than one installed plugin is refused
/// rather than guessed at — the same rule `install` applies to an ambiguous
/// name, and removing the wrong plugin is not recoverable by re-running.
fn find_installed(home: &Utf8Path, name: &str) -> Result<Option<Installed>, CloveError> {
    let bare = name.strip_prefix("clove-").unwrap_or(name);
    if let Some(found) = provenance::find_by_subcommand(home, bare) {
        return Ok(Some(found));
    }

    // Read cargo's bookkeeping once. Probing per candidate re-parsed the file up
    // to four more times and opened a window for it to change between probes.
    let installed = provenance::installed_under(home);

    // A package may own several binaries, so the same `Installed` can match more
    // than one candidate name — dedup by package, or a single plugin reports
    // itself as ambiguous with itself ("clove-tools, clove-tools are all
    // installed"), which names no way out.
    let mut matches: Vec<Installed> = Vec::new();
    for crate_name in crate::registry::candidate_crate_names(bare) {
        let Some(subcommand) = crate_name.strip_prefix("clove-") else {
            continue;
        };
        for candidate in &installed {
            // The package name is a legitimate way to name a plugin — it is what
            // `plugin list` prints — and it is not always the binary name.
            let matched =
                candidate.provides_subcommand(subcommand) || candidate.package == crate_name;
            if matched && !matches.iter().any(|m| m.package == candidate.package) {
                matches.push(candidate.clone());
            }
        }
    }

    match matches.len() {
        0 => Ok(None),
        1 => Ok(Some(matches.remove(0))),
        _ => Err(CloveError::InvalidField {
            field: "name".to_owned(),
            reason: format!(
                "`{bare}` is ambiguous — {} are all installed. Name one exactly.",
                matches
                    .iter()
                    .map(|i| i.package.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }),
    }
}

/// `clove plugin uninstall <name>` — offline, resolved from cargo's bookkeeping.
pub fn run_uninstall(format: OutputFormat, args: &PluginUninstallArgs) -> Result<(), CloveError> {
    let home = crate::clove_home::clove_home()?;
    let name = args.name.strip_prefix("clove-").unwrap_or(&args.name);

    let Some(installed) = find_installed(&home, &args.name)? else {
        // Distinguish "clove did not install this" from "nothing by that name":
        // an existing plugin from `cargo install` resolves, but not from our root.
        if let Some(path) = crate::plugin::resolve(&[name]) {
            return Err(CloveError::InvalidField {
                field: "name".to_owned(),
                reason: install::foreign_install_message(name, &path, &home),
            });
        }
        return Err(CloveError::InvalidField {
            field: "name".to_owned(),
            reason: format!("no plugin `{name}` was installed by clove"),
        });
    };

    run_cargo(
        &cargo_uninstall_argv(&installed.package, &home),
        "uninstall",
    )?;

    // Same rule as `roll_back`: cargo exiting 0 is not proof the binaries are
    // gone. Saying "uninstalled" over a binary that still resolves is the worst
    // possible answer here — the user removes a plugin precisely because they no
    // longer trust it, and `<clove-home>/bin` outranks `$PATH`, so it would keep
    // receiving the full inherited environment on the next dispatch.
    let left_behind: Vec<String> = installed
        .bins
        .iter()
        .map(|bin| install::installed_binary_path(&home, bin))
        .filter(|path| path.exists())
        .map(|path| path.to_string())
        .collect();
    if !left_behind.is_empty() {
        return Err(CloveError::Registry {
            message: format!(
                "cargo reported success but {} still present after uninstalling `{}`: {}. \
                 It is still on the plugin search path — remove it manually.",
                if left_behind.len() == 1 {
                    "this binary is"
                } else {
                    "these binaries are"
                },
                installed.package,
                left_behind.join(", ")
            ),
        });
    }

    emit(
        format,
        json!({
            "uninstalled": true,
            "package": installed.package,
            "version": installed.version,
        }),
        &format!("uninstalled {} {}", installed.package, installed.version),
    );
    Ok(())
}

/// `clove plugin update [<name>] [--all]`.
pub fn run_update(format: OutputFormat, args: &PluginUpdateArgs) -> Result<(), CloveError> {
    let home = crate::clove_home::clove_home()?;
    let all = provenance::installed_under(&home);

    // `--all` is the explicit spelling of the no-name default — clap rejects it
    // alongside a name, so it never changes *which* plugins are checked. It is
    // read rather than ignored so the scope reaches the JSON: a caller can tell
    // "checked everything" from "checked the one I named" without re-parsing
    // argv, and `checked: 0` stops being ambiguous between the two.
    let scope = if args.all || args.name.is_none() {
        "all"
    } else {
        "one"
    };

    let targets: Vec<Installed> = match &args.name {
        Some(name) => match find_installed(&home, name)? {
            Some(found) => vec![found],
            None => {
                return Err(CloveError::InvalidField {
                    field: "name".to_owned(),
                    reason: format!(
                        "no plugin `{}` was installed by clove",
                        name.strip_prefix("clove-").unwrap_or(name)
                    ),
                })
            }
        },
        None => all,
    };

    if targets.is_empty() {
        emit(
            format,
            json!({ "updated": [], "checked": 0, "scope": scope }),
            "no clove-installed plugins to update",
        );
        return Ok(());
    }

    let fetch = UreqFetch::new();
    let client = CratesIo::new(&fetch);

    // Resolve what each would move to, and show it before doing anything.
    //
    // A *transport* failure aborts rather than being folded into "no newer
    // version known". Swallowing it made `update` print "everything is up to
    // date" while offline — the exact conflation `resolve_candidate` refuses to
    // make, and the worst possible answer for a scheduled job whose whole purpose
    // is picking up security fixes.
    let mut plans: Vec<(Installed, Option<RegistryPlugin>)> = Vec::new();
    for installed in targets {
        let latest = if install::updatable_from_registry(&installed) {
            crate::registry::validate_crate_name(&installed.package)?;
            client
                .crate_exists(&installed.package)
                .map_err(CloveError::from)?
        } else {
            None
        };
        plans.push((installed, latest));
    }

    // An update installs *newer* third-party code, so every pre-install gate is
    // re-run per package. Skipping them meant a crate that had since stopped
    // depending on `clove-plugin` — the shape of an account takeover publishing a
    // non-plugin follow-up — was refused by `install` and accepted by `update`.
    let dependents: Option<Vec<String>> = client
        .reverse_dependents(REGISTRY_ROOT_CRATE)
        .map_err(CloveError::from)?
        .map(|plugins| plugins.into_iter().map(|p| p.crate_name).collect());

    let mut upgradable: Vec<(&Installed, String, String)> = Vec::new();
    let mut blocked: Vec<String> = Vec::new();
    let mut verdicts: Vec<install::UpdateVerdict> = Vec::new();
    for (installed, latest) in &plans {
        // Ordering, not string inequality. `!=` moved to a pre-release and even
        // *downgraded* against a lagging mirror, rendering `1.2.0 → 1.1.0` with
        // an arrow implying forward motion.
        let verdict = install::update_verdict(installed, latest.as_ref());
        let install::UpdateVerdict::Newer(version) = verdict.clone() else {
            verdicts.push(verdict);
            continue;
        };
        verdicts.push(verdict);
        let Some(candidate) = latest else { continue };
        let gates = install::evaluate(candidate, dependents.as_deref());
        if let Some(reason) = gates.blocking_reason(args.strict) {
            blocked.push(format!("  {} is not updated: {reason}", installed.package));
            continue;
        }
        // A yank is the standard response to a compromised release, so it must
        // never be *offered* as an upgrade.
        if gates.yanked && !args.allow_yanked {
            blocked.push(format!(
                "  {} is not updated: version {version} is yanked",
                installed.package
            ));
            continue;
        }
        let Some(bin) = gates.dispatchable_bin.clone() else {
            continue;
        };
        upgradable.push((installed, version, bin));
    }

    // The plan goes to stderr, the same stream the question is asked on, so
    // `clove plugin update > log` cannot show a bare [y/N] with the facts
    // redirected away.
    if format == OutputFormat::Human {
        for ((installed, _), verdict) in plans.iter().zip(&verdicts) {
            eprintln!("{}", install::update_line(installed, verdict));
        }
    }
    // A blocked package is *not* human-mode narrative. "`clove-sync-x` stopped
    // depending on clove-plugin" is the account-takeover signal these re-run
    // gates exist to catch, and "version 2.0.0 is yanked" is the standard shape
    // of a compromised release — reporting them only under `--format human`
    // meant the automation path, which is the one that runs unattended, saw
    // `{"updated":[],"checked":3}` with `ok: true` and an empty warnings list.
    let mut warnings: Vec<String> = blocked.iter().map(|line| line.trim().to_owned()).collect();
    for line in &blocked {
        eprintln!("{line}");
    }
    if upgradable.is_empty() {
        emit_with_warnings(
            format,
            json!({ "updated": [], "checked": plans.len(), "scope": scope }),
            "everything is up to date",
            warnings,
        );
        return Ok(());
    }

    // An update replaces running third-party code with *newer* third-party code,
    // possibly from a compromised account. That is the same decision as the
    // original install, so it is asked the same way.
    match consent_policy(
        args.yes,
        format == OutputFormat::Human,
        is_tty_stdin(),
        is_tty_stderr(),
    ) {
        Consent::Granted => {}
        Consent::Ask => {
            if !install::ask_confirmation("  Update these plugins? [y/N] ") {
                emit(
                    format,
                    json!({ "updated": [], "declined": true, "scope": scope }),
                    "not updated",
                );
                return Ok(());
            }
        }
        Consent::Refuse => {
            return Err(CloveError::InvalidField {
                field: "--yes".to_owned(),
                reason: install::refusal_message(),
            })
        }
    }

    let mut updated = Vec::new();
    for (installed, version, bin) in &upgradable {
        let argv = cargo_install_argv(&installed.package, Some(version), bin, &home, true);
        // One package failing must not discard the record of those already
        // updated, so the loop reports what it did rather than aborting.
        if let Err(error) = run_cargo(&argv, "install") {
            let message = format!("{} was not updated: {error}", installed.package);
            eprintln!("warning: {message}");
            warnings.push(message);
            continue;
        }
        // The same post-install compatibility gate as a fresh install, with the
        // same rollback — an update can just as easily land an incompatible
        // build, and leaving it resolvable is what the gate exists to prevent.
        let installed_path = install::installed_binary_path(&home, bin);
        let probe = crate::plugin::probe_info(&installed_path);
        if let Some(reason) = incompatible_reason(&installed.package, probe.as_ref()) {
            // This message can say "rolling the install back FAILED — the binary
            // is still on the plugin search path". Losing that to a format flag
            // is the worst of the three.
            let message = roll_back(&installed.package, &home, &installed_path, reason);
            eprintln!("warning: {message}");
            warnings.push(message);
            continue;
        }
        updated.push(json!({
            "package": installed.package,
            "from": installed.version,
            "to": version,
        }));
    }

    let count = updated.len();
    emit_with_warnings(
        format,
        json!({ "updated": updated, "checked": plans.len(), "scope": scope }),
        &format!("updated {count} plugin(s)"),
        warnings,
    );
    Ok(())
}

/// Resolve the user's argument to exactly one published crate.
///
/// Every candidate name is probed and **a unique hit is required**. A
/// first-match ladder would have to decide which multiplexer wins, and that
/// decision belongs to dispatch (`plugin::mux_candidates`) — if install picked a
/// different one, `clove plugin install beads` and `clove import beads` would
/// disagree about which binary is authoritative, and a squatted
/// `clove-sync-<name>` could shadow a real `clove-import-<name>`. Requiring the
/// user to disambiguate is the honest resolution.
fn resolve_candidate(client: &CratesIo, name: &str) -> Result<RegistryPlugin, CloveError> {
    crate::registry::validate_crate_name(name)?;

    // The same ladder `plugin search` probes, from the same function — the two
    // must agree about what a bare name could mean.
    let candidates = crate::registry::candidate_crate_names(name);

    let mut found: Vec<RegistryPlugin> = Vec::new();
    for candidate in &candidates {
        // A transport failure aborts: a flaky network must never be reported as
        // "no such plugin", which would send the user hunting for a typo.
        match client.crate_exists(candidate) {
            Ok(Some(plugin)) => found.push(plugin),
            Ok(None) => {}
            Err(error) => return Err(CloveError::from(error)),
        }
    }

    // Ambiguity is decided over the *installable* hits only.
    //
    // Counting every hit let one $0 publish deny any provider name: squatting
    // `clove-gitlab` with `bin_names = ["clove-helper"]` — installable by
    // nobody, since `dispatchable_bin` requires bin == crate name — made
    // `install gitlab` answer "ambiguous" forever, against a crate that is not
    // a valid answer to the question.
    //
    // The non-installable hits are still kept, because when *nothing* is
    // installable they carry the reason. Dropping them at the probe reported
    // "no plugin named `gitlab` is published" for a crate that plainly is —
    // trading one wrong answer for another.
    let installable: Vec<RegistryPlugin> = found
        .iter()
        .filter(|p| install::dispatchable_bin(p).is_ok())
        .cloned()
        .collect();
    let mut found = if installable.is_empty() {
        found
    } else {
        installable
    };

    match found.len() {
        // "Not found" is the *expected* answer for most names: the plugin
        // ecosystem is young, so a miss is far more often "nobody has published
        // that yet" than "you typed it wrong". The message says so, and points
        // at the two things that actually help — the list of what does exist,
        // and the escape hatch for something that exists but isn't published.
        0 => Err(CloveError::InvalidField {
            field: "name".to_owned(),
            reason: format!(
                "no plugin named `{name}` is published on crates.io (looked for {}). \
                 Run `clove plugin list --all` to see what is published, or install \
                 from source with `clove plugin install --git <url>`.",
                candidates.join(", ")
            ),
        }),
        // Exactly one meaning. If it is not installable, `evaluate` produces the
        // specific reason ("does not build a binary of its own name") — a better
        // answer than anything this function could say.
        1 => Ok(found.remove(0)),
        _ => Err(CloveError::InvalidField {
            field: "name".to_owned(),
            reason: format!(
                "`{name}` is ambiguous — {} all exist. Install one by its exact crate \
                 name.",
                found
                    .iter()
                    .map(|p| p.crate_name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }),
    }
}

/// The subcommand the resolved binary will dispatch as.
fn subcommand_of(gates: &Gates) -> &str {
    gates
        .dispatchable_bin
        .as_deref()
        .and_then(provenance::bare_subcommand)
        .unwrap_or_default()
}

/// Run `cargo` with `argv`, streaming its output.
fn run_cargo(argv: &[String], what: &str) -> Result<(), CloveError> {
    let status = std::process::Command::new("cargo").args(argv).status();
    match status {
        Ok(s) if s.success() => Ok(()),
        Ok(s) => Err(cargo_failure(what, s.code())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(cargo_missing()),
        Err(e) => Err(CloveError::Registry {
            message: format!("could not run cargo: {e}"),
        }),
    }
}

fn is_tty_stdin() -> bool {
    use std::io::IsTerminal;
    std::io::stdin().is_terminal()
}

fn is_tty_stderr() -> bool {
    use std::io::IsTerminal;
    std::io::stderr().is_terminal()
}

fn emit(format: OutputFormat, data: Value, human: &str) {
    emit_with_warnings(format, data, human, Vec::new());
}

fn emit_with_warnings(format: OutputFormat, data: Value, human: &str, warnings: Vec<String>) {
    match format {
        OutputFormat::Human => println!("{human}"),
        OutputFormat::Json => print_json_success(data, json!({ "warnings": warnings })),
        // jsonl carries no `_meta` (DESIGN §7.3) — `cmd/plugin.rs` routes its
        // list output the same way. Collapsing `Json | Jsonl` into one arm here
        // re-introduced, two files away, the exact trailing-metadata shape this
        // branch removed from `plugin list`.
        OutputFormat::Jsonl => {
            for warning in &warnings {
                eprintln!("warning: {warning}");
            }
            crate::output::print_jsonl_items(std::slice::from_ref(&data))
        }
    }
}
