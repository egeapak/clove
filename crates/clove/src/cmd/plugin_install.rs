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

use camino::Utf8Path;
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

/// `clove plugin install <name>`.
pub fn run_install(format: OutputFormat, args: &PluginInstallArgs) -> Result<(), CloveError> {
    let home = crate::clove_home::clove_home()?;
    let fetch = UreqFetch::new();
    let client = CratesIo::new(&fetch);

    let candidate = resolve_candidate(&client, &args.name)?;

    // Gate 1 evidence, fetched live. `None` = the registry could not be
    // consulted, which is reported rather than silently treated as a pass.
    let dependents: Option<Vec<String>> = client
        .reverse_dependents(REGISTRY_ROOT_CRATE)
        .ok()
        .flatten()
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

    match consent_policy(
        args.yes,
        format == OutputFormat::Human,
        is_tty_stdin(),
        is_tty_stderr(),
    ) {
        Consent::Granted => {}
        Consent::Ask => {
            let prompt = install::prompt_text(&candidate, &gates);
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

    let bin = gates
        .dispatchable_bin
        .as_deref()
        .expect("a blocking reason would have fired");
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
    let installed_path = install::bin_dir(&home).join(bin);
    let probe = crate::plugin::probe_info(&installed_path);
    if let Some(info) = &probe {
        if info.min_clove_plugin_api > clove_plugin::CLOVE_PLUGIN_API {
            let _ = run_cargo(&rollback_argv(&candidate.crate_name, &home), "uninstall");
            return Err(CloveError::InvalidField {
                field: "name".to_owned(),
                reason: format!(
                    "`{}` needs a newer clove (it requires plugin API {}, this clove \
                     provides {}); the install has been rolled back",
                    candidate.crate_name,
                    info.min_clove_plugin_api,
                    clove_plugin::CLOVE_PLUGIN_API
                ),
            });
        }
    }

    let warnings = match &probe {
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

/// `clove plugin uninstall <name>` — offline, resolved from cargo's bookkeeping.
pub fn run_uninstall(format: OutputFormat, args: &PluginUninstallArgs) -> Result<(), CloveError> {
    let home = crate::clove_home::clove_home()?;
    let name = args.name.trim_start_matches("clove-");

    let Some(installed) = provenance::find_by_subcommand(&home, name) else {
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

    let targets: Vec<Installed> = match &args.name {
        Some(name) => {
            let bare = name.trim_start_matches("clove-");
            match all.into_iter().find(|i| i.provides_subcommand(bare)) {
                Some(found) => vec![found],
                None => {
                    return Err(CloveError::InvalidField {
                        field: "name".to_owned(),
                        reason: format!("no plugin `{bare}` was installed by clove"),
                    })
                }
            }
        }
        None => all,
    };

    if targets.is_empty() {
        emit(
            format,
            json!({ "updated": [], "checked": 0 }),
            "no clove-installed plugins to update",
        );
        return Ok(());
    }

    let fetch = UreqFetch::new();
    let client = CratesIo::new(&fetch);

    // Resolve what each would move to, and show it before doing anything.
    let mut plans: Vec<(Installed, Option<String>)> = Vec::new();
    for installed in targets {
        let latest = if install::updatable_from_registry(&installed) {
            client
                .crate_exists(&installed.package)
                .ok()
                .flatten()
                .and_then(|c| c.display_version())
        } else {
            None
        };
        plans.push((installed, latest));
    }

    let upgradable: Vec<&(Installed, Option<String>)> = plans
        .iter()
        .filter(|(i, latest)| latest.as_deref().is_some_and(|v| v != i.version))
        .collect();

    if format == OutputFormat::Human {
        for (installed, latest) in &plans {
            println!("{}", install::update_line(installed, latest.as_deref()));
        }
    }
    if upgradable.is_empty() {
        emit(
            format,
            json!({ "updated": [], "checked": plans.len() }),
            "everything is up to date",
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
                    json!({ "updated": [], "declined": true }),
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
    for (installed, latest) in &upgradable {
        let Some(bin) = installed.bins.first() else {
            continue;
        };
        let argv = cargo_install_argv(&installed.package, latest.as_deref(), bin, &home, true);
        run_cargo(&argv, "install")?;
        updated.push(json!({
            "package": installed.package,
            "from": installed.version,
            "to": latest,
        }));
    }

    let count = updated.len();
    emit(
        format,
        json!({ "updated": updated, "checked": plans.len() }),
        &format!("updated {count} plugin(s)"),
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

    let candidates: Vec<String> = if name.starts_with("clove-") {
        vec![name.to_owned()]
    } else {
        ["sync", "import", "export"]
            .iter()
            .map(|mux| format!("clove-{mux}-{name}"))
            .chain(std::iter::once(format!("clove-{name}")))
            .collect()
    };

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

    match found.len() {
        0 => Err(CloveError::InvalidField {
            field: "name".to_owned(),
            reason: format!(
                "no published plugin matches `{name}` (looked for {})",
                candidates.join(", ")
            ),
        }),
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
        OutputFormat::Json | OutputFormat::Jsonl => {
            print_json_success(data, json!({ "warnings": warnings }))
        }
    }
}

/// The install root, for messages that need to name it.
#[allow(dead_code)]
fn root_label(home: &Utf8Path) -> String {
    home.to_string()
}
