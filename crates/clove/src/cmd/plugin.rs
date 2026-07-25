//! `clove plugin list` / `clove plugin search` — inspect installed subcommand
//! plugins and discover published ones.
//!
//! `plugin list` (no flags) is a read-only view over [`crate::plugin::list_enriched`]
//! (the pure `stat` walk of the §5 search path, plus a bounded
//! `--clove-plugin-info` probe of each binary, `PLUGIN_REGISTRY.md` §3). It
//! **never touches the network**.
//!
//! `plugin list --all` and `plugin search` additionally consult the registry —
//! crates.io, via the reverse dependencies of `clove-plugin`. Discovery is
//! strictly additive: if it fails for any reason (offline, rate-limited,
//! `clove-plugin` not yet published) the installed set still prints and the
//! reason is reported as `_meta.registry_error`. Dispatch is never affected.
//!
//! Human mode renders a `NAME / VERSION / RUN AS / ABOUT` table (with an
//! *Available* section under `--all`); JSON/JSONL emit the standard
//! `{ v, ok, data, _meta }` envelope with one additive object per plugin.
//! Needs no repository.

use camino::Utf8PathBuf;
use chrono::Utc;
use clove_core::OutputFormat;
use clove_types::CloveError;
use serde_json::{json, Value};

use crate::cli::{PluginListArgs, PluginSearchArgs};
use crate::output::{print_json_list, print_jsonl_items_with_meta};
use crate::plugin::{self, EnrichedPlugin, PluginStatus};
use crate::registry::{self, crates_io::CratesIo, http::UreqFetch, RegistryPlugin};

/// The crate whose reverse dependencies *are* the plugin registry: depending on
/// `clove-plugin` is what makes a crate a clove plugin.
const REGISTRY_ROOT_CRATE: &str = "clove-plugin";

/// The outcome of a discovery attempt.
enum Discovery {
    /// The registry answered. `None` means `clove-plugin` is not published yet,
    /// so there is nothing to discover *and nothing is wrong*.
    Available(Option<Vec<RegistryPlugin>>),
    /// Discovery failed; the message is surfaced as a warning, never an error.
    Failed(String),
}

impl Discovery {
    fn plugins(&self) -> &[RegistryPlugin] {
        match self {
            Discovery::Available(Some(plugins)) => plugins,
            _ => &[],
        }
    }

    /// The user-facing warning, if this attempt did not yield a usable registry.
    fn warning(&self) -> Option<String> {
        match self {
            Discovery::Available(Some(_)) => None,
            Discovery::Available(None) => Some(format!(
                "the plugin registry is not available yet: `{REGISTRY_ROOT_CRATE}` is not \
                 published to crates.io, so no plugins can be discovered"
            )),
            Discovery::Failed(message) => Some(message.clone()),
        }
    }
}

/// Render the installed plugins in the requested `format`.
pub fn run_list(format: OutputFormat, args: &PluginListArgs) -> Result<(), CloveError> {
    let installed = plugin::list_enriched();

    // `--refresh` only means anything against the registry, so it implies `--all`.
    if !args.all && !args.refresh {
        return render(format, &installed, &[], None);
    }

    let discovery = discover(args.refresh);
    let available = not_installed(discovery.plugins(), &installed);
    render(format, &installed, &available, discovery.warning())
}

/// Filter the discovered set by `query` and render it.
pub fn run_search(format: OutputFormat, args: &PluginSearchArgs) -> Result<(), CloveError> {
    let installed = plugin::list_enriched();
    let discovery = discover(args.refresh);

    let needle = args.query.to_lowercase();
    let mut matches: Vec<RegistryPlugin> = discovery
        .plugins()
        .iter()
        .filter(|p| {
            p.crate_name.to_lowercase().contains(&needle)
                || p.description
                    .as_deref()
                    .is_some_and(|d| d.to_lowercase().contains(&needle))
        })
        .cloned()
        .collect();

    // Nothing matched the discovered set, but the query can still be answered
    // directly: the naming convention is total, so the candidate crate names are
    // *constructible*.
    //
    // Probe whenever the filter found nothing — *not* only when discovery failed.
    // Gating on the warning would mean a stale-but-valid cache (up to 24h old)
    // silently answers "no published plugins matched" for a plugin published an
    // hour ago, with no indication the answer came from a snapshot. Perversely,
    // the same query would work if the network were down. The probe is exact-name
    // and cheap (at most four requests), so it costs little to always try.
    let mut warning = discovery.warning();
    if matches.is_empty() {
        match probe_by_name(&args.query) {
            // The probe answered the question, so the registry's absence is no
            // longer something the user needs to act on.
            Ok(found) if !found.is_empty() => {
                matches = found;
                warning = None;
            }
            // Probed, nothing published under any candidate name.
            Ok(_) => {}
            // Report the probe's own failure when nothing else is being reported.
            // Swallowing it would print a bare "no published plugins matched" for
            // what is actually "we could not ask" — the exact conflation the
            // `Fetch` return type exists to prevent.
            Err(error) => warning = warning.or(Some(error.to_string())),
        }
    }

    // A search reports only what matched — the installed set is not the subject
    // of the query — but an installed plugin that matches is still marked as
    // installed so the reader knows it is already available locally.
    render_search(format, &matches, &installed, warning)
}

/// Resolve a bare query to published crates by **constructing** the candidate
/// names rather than searching for them.
///
/// crates.io has no prefix search — `?q=` is fuzzy full-text, and `?q=clove-sync`
/// returns zero results — so searching for the convention cannot work. Since the
/// convention is total, the names can simply be built and probed exactly.
///
/// Every candidate is probed and **all** hits are reported. This is deliberately
/// not a first-match ladder: a ladder has to decide which mux wins, and that
/// decision belongs to `install` (where it must agree with dispatch), not to a
/// read-only search that can just show the user everything.
fn probe_by_name(query: &str) -> Result<Vec<RegistryPlugin>, CloveError> {
    // A search query is matched against descriptions too, so it may legitimately
    // be a phrase ("two-way sync") that is not a valid crate name. That is not an
    // error — it simply cannot be probed, so there are no name hits. It also
    // means a traversal- or flag-shaped query never reaches a URL.
    if registry::validate_crate_name(query).is_err() {
        return Ok(Vec::new());
    }

    let candidates: Vec<String> = if query.starts_with("clove-") {
        vec![query.to_owned()]
    } else {
        ["sync", "import", "export"]
            .iter()
            .map(|mux| format!("clove-{mux}-{query}"))
            .chain(std::iter::once(format!("clove-{query}")))
            .collect()
    };

    let fetch = UreqFetch::new();
    let client = CratesIo::new(&fetch);
    let mut found = Vec::new();
    for candidate in candidates {
        // A 404 means "not this one"; a transport failure aborts, so a flaky
        // network can never be reported as "no such plugin".
        match client.crate_exists(&candidate) {
            Ok(Some(plugin)) => found.push(plugin),
            Ok(None) => {}
            Err(error) => return Err(CloveError::from(error)),
        }
    }
    Ok(found)
}

/// Consult the registry, preferring a fresh cache entry unless `refresh`.
fn discover(refresh: bool) -> Discovery {
    let home = match crate::clove_home::clove_home() {
        Ok(home) => home,
        // No resolvable home: skip the cache entirely and just fetch.
        Err(_) => return fetch_uncached(),
    };
    let now = Utc::now();

    if !refresh {
        if let Some(cached) = registry::cache::read(&home, now, registry::cache::TTL) {
            return Discovery::Available(cached);
        }
    }

    match fetch(&home, now) {
        Ok(result) => Discovery::Available(result),
        Err(error) => Discovery::Failed(error),
    }
}

/// Fetch and cache the registry result.
fn fetch(
    home: &Utf8PathBuf,
    now: chrono::DateTime<Utc>,
) -> Result<Option<Vec<RegistryPlugin>>, String> {
    let fetch = UreqFetch::new();
    let client = CratesIo::new(&fetch);
    match client.reverse_dependents(REGISTRY_ROOT_CRATE) {
        Ok(result) => {
            registry::cache::write(home, now, result.as_deref());
            Ok(result)
        }
        Err(error) => Err(error.to_string()),
    }
}

/// Fetch without touching the cache (no resolvable clove home).
fn fetch_uncached() -> Discovery {
    let fetch = UreqFetch::new();
    let client = CratesIo::new(&fetch);
    match client.reverse_dependents(REGISTRY_ROOT_CRATE) {
        Ok(result) => Discovery::Available(result),
        Err(error) => Discovery::Failed(error.to_string()),
    }
}

/// The discovered plugins that are not already installed here.
///
/// A published crate is "installed" when any binary it builds is already
/// resolvable on the search path — matched suffix-insensitively, because
/// crates.io records `bin_names` without the platform executable suffix while
/// the resolver looks for `clove-sync-github.exe` on Windows.
fn not_installed(
    discovered: &[RegistryPlugin],
    installed: &[EnrichedPlugin],
) -> Vec<RegistryPlugin> {
    discovered
        .iter()
        .filter(|candidate| is_dispatchable(candidate) && !is_installed(candidate, installed))
        .cloned()
        .collect()
}

/// Does this crate build a binary clove could actually dispatch to?
///
/// The registry client already drops dependents that build no binary at all; this
/// is the clove-specific half — only a `clove-`-prefixed binary is resolvable
/// (`PLUGIN_SYSTEM.md` §5), so a crate that depends on `clove-plugin` but ships a
/// differently-named binary could never be run as `clove <something>`. Offering
/// it under "Available" would promise a command that cannot exist.
///
/// The naming convention lives here rather than in the crates.io client so that
/// client stays a general-purpose registry reader.
fn is_dispatchable(candidate: &RegistryPlugin) -> bool {
    candidate.bin_names.iter().any(|bin| {
        bin.strip_prefix("clove-")
            .is_some_and(|rest| !rest.is_empty())
    })
}

fn is_installed(candidate: &RegistryPlugin, installed: &[EnrichedPlugin]) -> bool {
    installed_match(candidate, installed).is_some()
}

/// The installed plugin corresponding to `candidate`, if any.
///
/// Only a `clove-`-prefixed binary can match. Falling back to the raw name would
/// compare a bin literally named `gitlab` against the installed set's *stripped*
/// names and match an unrelated `clove-gitlab` — filtering a genuinely available
/// plugin out of `list --all`. A false positive hides a plugin from the user,
/// while a false negative merely shows a duplicate row, so this errs toward
/// showing. A binary without the prefix is not dispatch-resolvable anyway, so it
/// can never legitimately mean "installed".
fn installed_match<'a>(
    candidate: &RegistryPlugin,
    installed: &'a [EnrichedPlugin],
) -> Option<&'a EnrichedPlugin> {
    candidate.bin_names.iter().find_map(|bin| {
        let bare = bin.strip_prefix("clove-")?;
        installed.iter().find(|p| p.info.name == bare)
    })
}

/// Render the list output for `format`.
fn render(
    format: OutputFormat,
    installed: &[EnrichedPlugin],
    available: &[RegistryPlugin],
    warning: Option<String>,
) -> Result<(), CloveError> {
    let mut items: Vec<Value> = installed.iter().map(installed_json).collect();
    items.extend(available.iter().map(available_json));

    match format {
        OutputFormat::Human => render_human(installed, available, warning.as_deref()),
        OutputFormat::Json => {
            print_json_list(items, meta(installed.len(), available.len(), warning))
        }
        OutputFormat::Jsonl => print_jsonl_items_with_meta(
            &items,
            warning.map_or(Value::Null, |w| json!({ "registry_error": w })),
        ),
    }
    Ok(())
}

/// Render the search output for `format`.
fn render_search(
    format: OutputFormat,
    matches: &[RegistryPlugin],
    installed: &[EnrichedPlugin],
    warning: Option<String>,
) -> Result<(), CloveError> {
    let mut installed_matches = 0usize;
    let items: Vec<Value> = matches
        .iter()
        .map(|p| {
            let mut value = available_json(p);
            // Carry the *real* compat verdict, path and probed version from the
            // installed plugin. Synthesizing `status: "ok"` here would contradict
            // the field's documented meaning and could report a plugin that
            // dispatch actively refuses to run (`needs_newer_clove`) as healthy.
            if let Some(local) = installed_match(p, installed) {
                installed_matches += 1;
                value["installed"] = json!(true);
                value["status"] = json!(local.status.as_str());
                value["path"] = json!(local.info.path.as_str());
                value["version"] = json!(local.probed.as_ref().map(|probe| probe.version.as_str()));
            }
            value
        })
        .collect();

    match format {
        OutputFormat::Human => {
            if let Some(warning) = &warning {
                eprintln!("warning: {warning}");
            }
            if matches.is_empty() {
                println!("no published plugins matched");
            } else {
                render_available_table(matches, installed);
            }
        }
        // A search's matches are counted by whether each row is already installed,
        // so the envelope's counts agree with the rows' own `installed` flags
        // instead of reporting `installed_count: 0` next to `"installed": true`.
        OutputFormat::Json => print_json_list(
            items,
            meta(
                installed_matches,
                matches.len() - installed_matches,
                warning,
            ),
        ),
        OutputFormat::Jsonl => print_jsonl_items_with_meta(
            &items,
            warning.map_or(Value::Null, |w| json!({ "registry_error": w })),
        ),
    }
    Ok(())
}

/// The `_meta` object: counts, plus the discovery warning when there is one.
fn meta(installed: usize, available: usize, warning: Option<String>) -> Value {
    let mut meta = json!({
        "count": installed + available,
        "installed_count": installed,
        "available_count": available,
    });
    if let Some(warning) = warning {
        meta["registry_error"] = json!(warning);
    }
    meta
}

/// The JSON object for one installed plugin (§3): today's `{ name, path }` plus
/// `binary`, the probed `version`/`about`/`provides`, the derived `commands`,
/// `installed: true`, and the compat `status`.
fn installed_json(plugin: &EnrichedPlugin) -> Value {
    let binary = format!("clove-{}", plugin.info.name);
    let version = plugin.probed.as_ref().map(|p| p.version.as_str());
    let about = plugin.probed.as_ref().map(|p| p.about.as_str());
    let provides: Vec<&str> = plugin
        .probed
        .as_ref()
        .map(|p| p.provides.iter().map(String::as_str).collect())
        .unwrap_or_default();

    json!({
        "name": plugin.info.name,
        "binary": binary,
        "path": plugin.info.path.as_str(),
        "version": version,
        "about": about,
        "provides": provides,
        "commands": plugin.commands,
        "installed": true,
        "status": plugin.status.as_str(),
    })
}

/// The JSON object for a discovered-but-not-installed plugin.
///
/// `status` stays the host↔plugin **compat** verdict everywhere, so it can never
/// be confused with registry freshness: an uninstalled plugin has no compat
/// verdict yet and reports `available`, while "a newer release exists" is the
/// separate `latest_version` field.
fn available_json(plugin: &RegistryPlugin) -> Value {
    json!({
        "name": plugin
            .crate_name
            .strip_prefix("clove-")
            .unwrap_or(&plugin.crate_name),
        "binary": plugin.bin_names.first(),
        "path": Value::Null,
        "version": Value::Null,
        "latest_version": plugin.display_version(),
        "about": plugin.description,
        "provides": Vec::<String>::new(),
        "commands": plugin
            .bin_names
            .iter()
            .map(|bin| plugin::run_as(&[], bin.strip_prefix("clove-").unwrap_or(bin)))
            .collect::<Vec<_>>()
            .concat(),
        "crate": plugin.crate_name,
        "repository": plugin.repository,
        "published_by": plugin.published_by,
        "downloads": plugin.downloads,
        "yanked": plugin.fully_yanked(),
        "installed": false,
        "status": "available",
    })
}

/// Render the human `NAME / VERSION / RUN AS / ABOUT` table (§3).
///
/// A plugin that failed the probe (`no_info`) shows `—` for version and
/// `(no metadata)` for about; an out-of-range plugin (`outdated` /
/// `needs_newer_clove`) gets a trailing compat note on its row.
fn render_human(installed: &[EnrichedPlugin], available: &[RegistryPlugin], warning: Option<&str>) {
    if let Some(warning) = warning {
        eprintln!("warning: {warning}");
    }

    // Only the bare `plugin list` stays silent on an empty machine. Once the
    // registry is in play there are sections to label, so printing nothing at
    // all would read as a broken command.
    let show_sections = !available.is_empty() || warning.is_some();

    if installed.is_empty() && !show_sections {
        return;
    }

    if show_sections {
        println!("Installed");
        if installed.is_empty() {
            println!("  (none)");
        }
    }
    if !installed.is_empty() {
        render_installed_table(installed, show_sections);
    }

    if !available.is_empty() {
        println!();
        println!("Available");
        render_available_table(available, installed);
    }
}

fn render_installed_table(plugins: &[EnrichedPlugin], indent: bool) {
    struct Row {
        name: String,
        version: String,
        run_as: String,
        about: String,
    }

    let rows: Vec<Row> = plugins
        .iter()
        .map(|p| {
            let (version, about) = match &p.probed {
                Some(info) => (info.version.clone(), info.about.clone()),
                None => ("—".to_owned(), "(no metadata)".to_owned()),
            };
            let mut about = about;
            match p.status {
                PluginStatus::Outdated => {
                    about.push_str("  [outdated: predates this clove; runs with a warning]");
                }
                PluginStatus::NeedsNewerClove => {
                    about.push_str("  [needs a newer clove]");
                }
                PluginStatus::Ok | PluginStatus::NoInfo => {}
            }
            Row {
                name: p.info.name.clone(),
                version,
                run_as: p.commands.join(", "),
                about,
            }
        })
        .collect();

    let pad = if indent { "  " } else { "" };
    let name_w = header_width("NAME", rows.iter().map(|r| r.name.as_str()));
    let version_w = header_width("VERSION", rows.iter().map(|r| r.version.as_str()));
    let run_as_w = header_width("RUN AS", rows.iter().map(|r| r.run_as.as_str()));

    println!(
        "{pad}{:<name_w$}  {:<version_w$}  {:<run_as_w$}  ABOUT",
        "NAME", "VERSION", "RUN AS"
    );
    for row in &rows {
        println!(
            "{pad}{:<name_w$}  {:<version_w$}  {:<run_as_w$}  {}",
            row.name, row.version, row.run_as, row.about
        );
    }
}

fn render_available_table(plugins: &[RegistryPlugin], installed: &[EnrichedPlugin]) {
    struct Row {
        krate: String,
        version: String,
        about: String,
    }

    let rows: Vec<Row> = plugins
        .iter()
        .map(|p| {
            let mut about = p.description.clone().unwrap_or_else(|| "—".to_owned());
            if p.fully_yanked() {
                about.push_str("  [all versions yanked]");
            }
            if is_installed(p, installed) {
                about.push_str("  [installed]");
            }
            Row {
                krate: p.crate_name.clone(),
                version: p.display_version().unwrap_or_else(|| "—".to_owned()),
                about,
            }
        })
        .collect();

    let crate_w = header_width("CRATE", rows.iter().map(|r| r.krate.as_str()));
    let version_w = header_width("VERSION", rows.iter().map(|r| r.version.as_str()));

    println!("  {:<crate_w$}  {:<version_w$}  ABOUT", "CRATE", "VERSION");
    for row in &rows {
        println!(
            "  {:<crate_w$}  {:<version_w$}  {}",
            row.krate, row.version, row.about
        );
    }
}

/// The column width: the wider of the header and the widest cell.
fn header_width<'a>(header: &str, cells: impl Iterator<Item = &'a str>) -> usize {
    cells
        .map(str::len)
        .chain(std::iter::once(header.len()))
        .max()
        .unwrap_or(header.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn discovered(name: &str, bins: &[&str]) -> RegistryPlugin {
        RegistryPlugin {
            crate_name: name.to_owned(),
            latest: Some(semver::Version::new(0, 2, 0)),
            latest_yanked: None,
            description: Some("a plugin".to_owned()),
            repository: None,
            bin_names: bins.iter().map(|b| (*b).to_owned()).collect(),
            published_by: None,
            downloads: 0,
        }
    }

    fn installed_plugin(name: &str) -> EnrichedPlugin {
        EnrichedPlugin {
            info: plugin::PluginInfo {
                name: name.to_owned(),
                path: Utf8PathBuf::from(format!("/somewhere/clove-{name}")),
            },
            probed: None,
            status: PluginStatus::NoInfo,
            commands: vec![],
        }
    }

    #[test]
    fn discovered_plugins_already_installed_are_filtered_out() {
        let discovered_set = vec![
            discovered("clove-sync-github", &["clove-sync-github"]),
            discovered("clove-sync-gitlab", &["clove-sync-gitlab"]),
        ];
        let installed = vec![installed_plugin("sync-github")];

        let available = not_installed(&discovered_set, &installed);
        assert_eq!(available.len(), 1);
        assert_eq!(available[0].crate_name, "clove-sync-gitlab");
    }

    #[test]
    fn installed_match_ignores_the_platform_executable_suffix() {
        // crates.io records `bin_names` unsuffixed; the resolver looks for
        // `clove-sync-github.exe` on Windows, so the comparison is on the bare
        // subcommand name that `plugin::list` already strips.
        let candidate = discovered("clove-sync-github", &["clove-sync-github"]);
        let installed = vec![installed_plugin("sync-github")];
        assert!(is_installed(&candidate, &installed));
    }

    #[test]
    fn a_crate_with_no_dispatchable_binary_is_not_offered() {
        // Depending on `clove-plugin` is necessary but not sufficient: only a
        // `clove-`-prefixed binary is resolvable, so a crate shipping `helper`
        // could never be run as a clove subcommand.
        let lib_only = discovered("clove-plugin-utils", &[]);
        let wrong_name = discovered("clove-sync-odd", &["helper"]);
        let good = discovered("clove-sync-gitlab", &["clove-sync-gitlab"]);

        assert!(!is_dispatchable(&lib_only));
        assert!(!is_dispatchable(&wrong_name));
        assert!(is_dispatchable(&good));

        let available = not_installed(&[lib_only, wrong_name, good], &[]);
        assert_eq!(available.len(), 1);
        assert_eq!(available[0].crate_name, "clove-sync-gitlab");
    }

    #[test]
    fn a_bin_without_the_clove_prefix_never_counts_as_installed() {
        // `strip_prefix(..).unwrap_or(bin)` would compare a bin literally named
        // `gitlab` against the installed set's *stripped* names and match an
        // unrelated `clove-gitlab`, filtering a genuinely available plugin out of
        // `list --all`. A false positive hides a plugin; a false negative only
        // shows a duplicate row.
        let candidate = discovered("clove-sync-gitlab", &["gitlab"]);
        let installed = vec![installed_plugin("gitlab")];
        assert!(
            !is_installed(&candidate, &installed),
            "an unprefixed bin must not match an installed plugin"
        );
    }

    #[test]
    fn search_rows_carry_the_real_compat_verdict_not_a_synthesized_ok() {
        // `status` is the host<->plugin compat verdict. Hardcoding "ok" for an
        // installed match would report a plugin dispatch actively refuses to run
        // (`needs_newer_clove`) as healthy.
        let mut local = installed_plugin("sync-github");
        local.status = PluginStatus::NeedsNewerClove;
        let installed = vec![local];
        let candidate = discovered("clove-sync-github", &["clove-sync-github"]);

        let matched = installed_match(&candidate, &installed).expect("matches");
        assert_eq!(matched.status.as_str(), "needs_newer_clove");
    }

    #[test]
    fn an_absent_registry_warns_but_is_not_an_error() {
        // `clove-plugin` unpublished is the expected state today. It must read as
        // "nothing to discover yet", never as a failure.
        let discovery = Discovery::Available(None);
        assert!(discovery.plugins().is_empty());
        let warning = discovery.warning().expect("absent registry warns");
        assert!(warning.contains("not published"));
    }

    #[test]
    fn a_published_but_empty_registry_does_not_warn() {
        // Published with no dependents yet is a *complete, correct* answer.
        let discovery = Discovery::Available(Some(vec![]));
        assert_eq!(discovery.warning(), None);
    }

    #[test]
    fn a_failed_discovery_reports_its_cause() {
        let discovery = Discovery::Failed("could not reach crates.io: dns".to_owned());
        assert!(discovery.plugins().is_empty());
        assert!(discovery.warning().unwrap().contains("dns"));
    }

    #[test]
    fn available_rows_carry_the_registry_status_not_a_compat_verdict() {
        // `status` is the host<->plugin compat verdict for installed plugins
        // (`ok`/`outdated`/...). An uninstalled plugin has no compat verdict, and
        // "a newer release exists" lives in `latest_version` instead — so the two
        // meanings can never be confused in one field.
        let value = available_json(&discovered("clove-sync-gitlab", &["clove-sync-gitlab"]));
        assert_eq!(value["status"], "available");
        assert_eq!(value["installed"], false);
        assert_eq!(value["latest_version"], "0.2.0");
        assert_eq!(value["version"], Value::Null);
        assert_eq!(value["crate"], "clove-sync-gitlab");
    }

    #[test]
    fn meta_carries_the_registry_error_when_discovery_failed() {
        let with_error = meta(1, 0, Some("offline".to_owned()));
        assert_eq!(with_error["registry_error"], "offline");
        assert_eq!(with_error["installed_count"], 1);

        let clean = meta(1, 2, None);
        assert_eq!(clean.get("registry_error"), None);
        assert_eq!(clean["count"], 3);
    }
}
