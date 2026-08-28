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
//! reason is reported in `_meta.warnings`. Dispatch is never affected.
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
use crate::output::{print_json_list, print_jsonl_items};
use crate::plugin::{self, EnrichedPlugin, PluginStatus};
use crate::registry::{self, crates_io::CratesIo, http::UreqFetch, RegistryPlugin};

// The crate whose reverse dependencies *are* the plugin registry: depending on
// `clove-plugin` is what makes a crate a clove plugin. Imported from
// `registry::install` rather than redeclared here — the two copies held the same
// value, and a registry whose root differed between `list` and `install` would
// discover one set of plugins and refuse another.
use crate::registry::install::REGISTRY_ROOT_CRATE;
use crate::registry::provenance;

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
    fn warning(&self, fresh: bool) -> Option<String> {
        match self {
            Discovery::Available(Some(_)) => None,
            // This may be served from a cache up to 24h old, so it is phrased as
            // what was observed rather than as a fact about right now, and it
            // names the flag that re-checks.
            // The "as of the last check … --refresh" tail is only true of a
            // cached answer. Printing it after `--refresh` told the user to do
            // the thing they had just done.
            Discovery::Available(None) => Some(if fresh {
                format!(
                    "no plugin registry found: `{REGISTRY_ROOT_CRATE}` is not published to \
                     crates.io, so no plugins can be discovered"
                )
            } else {
                format!(
                    "no plugin registry found: `{REGISTRY_ROOT_CRATE}` was not published to \
                     crates.io as of the last check, so no plugins can be discovered \
                     (re-check with --refresh)"
                )
            }),
            Discovery::Failed(message) => Some(message.clone()),
        }
    }
}

/// Render the installed plugins in the requested `format`.
pub fn run_list(format: OutputFormat, args: &PluginListArgs) -> Result<(), CloveError> {
    let installed = plugin::list_enriched();

    // `--refresh` only means anything against the registry, so it implies `--all`.
    if !args.all && !args.refresh {
        return render(format, &installed, &[], &[], None, false);
    }

    let discovery = discover(args.refresh);
    let available = not_installed(discovery.plugins(), &installed);
    render(
        format,
        &installed,
        &available,
        discovery.plugins(),
        discovery.warning(args.refresh),
        true,
    )
}

/// Filter the discovered set by `query` and render it.
pub fn run_search(format: OutputFormat, args: &PluginSearchArgs) -> Result<(), CloveError> {
    let installed = plugin::list_enriched();
    let discovery = discover(args.refresh);

    let needle = args.query.to_lowercase();
    // `is_dispatchable` is applied here for the same reason `list --all` applies
    // it: a crate that builds no `clove-`-prefixed binary can never be run as a
    // clove subcommand, so listing it promises a command that cannot exist.
    let mut matches: Vec<RegistryPlugin> = discovery
        .plugins()
        .iter()
        .filter(|p| {
            is_dispatchable(p)
                && (p.crate_name.to_lowercase().contains(&needle)
                    || p.description
                        .as_deref()
                        .is_some_and(|d| d.to_lowercase().contains(&needle)))
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
    let mut warning = discovery.warning(args.refresh);
    // Whether a successful probe *answers* the warning depends on which warning
    // it is. An absent registry has nothing to report, so a hit resolves it. A
    // discovery *failure* is different: the probe checked a handful of
    // constructed names, not the registry, so every other published plugin is
    // still missing from a result that would otherwise look complete.
    let probe_can_answer = matches!(discovery, Discovery::Available(None));
    if matches.is_empty() {
        match probe_by_name(&args.query) {
            Ok(found) if !found.is_empty() => {
                matches = found;
                if probe_can_answer {
                    warning = None;
                }
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

    let candidates = registry::candidate_crate_names(query);

    let fetch = UreqFetch::new();
    let client = CratesIo::new(&fetch);
    let mut found = Vec::new();
    for candidate in candidates {
        // A 404 means "not this one"; a transport failure aborts, so a flaky
        // network can never be reported as "no such plugin".
        match client.crate_exists(&candidate) {
            // A probe hit still has to be dispatchable to be worth offering.
            Ok(Some(plugin)) if is_dispatchable(&plugin) => found.push(plugin),
            Ok(Some(_)) | Ok(None) => {}
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
/// The newest published version of an installed plugin, when discovery saw it
/// and it differs from what is installed.
fn latest_for(plugin: &EnrichedPlugin, discovered: &[RegistryPlugin]) -> Option<String> {
    let probed = plugin.probed.as_ref()?.version.as_str();
    let candidate = discovered
        .iter()
        .find(|c| installed_match(c, std::slice::from_ref(plugin)).is_some())?;
    let latest = candidate.latest.as_ref()?;
    // Only a strictly greater *stable* release is an update — the same rule
    // `update` itself applies, so the two cannot disagree about whether there is
    // one.
    let current = semver::Version::parse(probed).ok()?;
    (*latest > current).then(|| latest.to_string())
}

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
/// The naming convention lives in [`provenance::subcommand_of_file`] rather than
/// in the crates.io client, so that client stays a general-purpose registry
/// reader — and rather than inline here, so every "is this binary dispatchable?"
/// answer comes from one body. The inline copies this replaced were not
/// suffix-aware, so they disagreed with the installed-side check about a
/// `clove-x.exe`.
///
/// One deliberate difference remains: enumerating files on disk requires the
/// platform suffix, while reading cargo's bookkeeping tolerates its absence.
/// That is a parameter of the shared function, documented where it lives.
pub fn is_dispatchable(candidate: &RegistryPlugin) -> bool {
    candidate
        .bin_names
        .iter()
        .any(|bin| provenance::bare_subcommand(bin).is_some())
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
        let bare = provenance::bare_subcommand(bin)?;
        installed.iter().find(|p| p.info.name == bare)
    })
}

/// Render the list output for `format`.
fn render(
    format: OutputFormat,
    installed: &[EnrichedPlugin],
    available: &[RegistryPlugin],
    // Everything discovery returned, including plugins already installed —
    // which `available` excludes, and which is exactly what an "update
    // available" marker needs.
    discovered: &[RegistryPlugin],
    warning: Option<String>,
    consulted_registry: bool,
) -> Result<(), CloveError> {
    let mut items: Vec<Value> = installed
        .iter()
        .map(|p| installed_json_with_latest(p, latest_for(p, discovered).as_deref()))
        .collect();
    items.extend(available.iter().map(available_json));

    match format {
        OutputFormat::Human => render_human(
            installed,
            available,
            discovered,
            warning.as_deref(),
            consulted_registry,
        ),
        OutputFormat::Json => {
            print_json_list(items, meta(installed.len(), available.len(), warning))
        }
        // jsonl is "one envelope per line, `data` is a single item"
        // (DESIGN §7.3), so it carries no `_meta` and a discovery warning goes
        // to stderr — where human mode already prints it. A trailing
        // `_meta`-only line would make `jq -r .data.name` emit a spurious
        // `null` on the last record, and this was the only jsonl surface in the
        // repo emitting one.
        OutputFormat::Jsonl => {
            warn_on_stderr(warning.as_deref());
            print_jsonl_items(&items)
        }
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
                // "Nothing matched" is a claim about the registry. When the
                // warning above says we could not reach it, that claim was not
                // checked — the `Ok(None)`/`Err` distinction the whole read path
                // preserves was being collapsed one line later, in prose.
                if warning.is_some() {
                    println!("could not check crates.io — no results");
                } else {
                    println!("no published plugins matched");
                }
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
        // See the note in `render`: jsonl lines are items only.
        OutputFormat::Jsonl => {
            warn_on_stderr(warning.as_deref());
            print_jsonl_items(&items)
        }
    }
    Ok(())
}

/// Report a discovery warning on stderr.
///
/// Used by the jsonl path, whose lines are items only. Stdout stays parseable —
/// DESIGN §7.3: stderr is narrative, stdout is JSON.
fn warn_on_stderr(warning: Option<&str>) {
    if let Some(warning) = warning {
        eprintln!("warning: {warning}");
    }
}

/// The `_meta` object: counts, plus the discovery warning when there is one.
///
/// A discovery failure is reported through **`warnings`**, the repo-wide channel
/// for a non-fatal problem (`item-list.json`'s `listMeta.warnings`, and how
/// `clove setup` already reports). Inventing a parallel `registry_error` key for
/// the same purpose would mean a consumer had to learn a second convention to
/// notice that a command partially failed.
fn meta(installed: usize, available: usize, warning: Option<String>) -> Value {
    let mut meta = json!({
        "count": installed + available,
        "installed_count": installed,
        "available_count": available,
        "warnings": Vec::<String>::new(),
    });
    if let Some(warning) = warning {
        meta["warnings"] = json!([warning]);
    }
    meta
}

/// The JSON object for one installed plugin (§3): today's `{ name, path }` plus
/// `binary`, the probed `version`/`about`/`provides`, the derived `commands`,
/// `installed: true`, and the compat `status`.
fn installed_json_with_latest(plugin: &EnrichedPlugin, latest: Option<&str>) -> Value {
    let mut value = installed_json(plugin);
    // The plan deferred this "with install", on the grounds that a version the
    // user cannot act on is noise. Install has shipped, so the premise is gone:
    // `update` is now the action, and `list --all` is the only place a user
    // would learn they need it.
    if let Some(latest) = latest {
        value["latest_version"] = json!(latest);
    }
    value
}

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
    // Every identifier is derived from the *dispatchable* binaries, so `name`,
    // `binary` and `commands` agree with each other and with what the resolver
    // would actually find. Deriving `name` from the crate name instead would
    // report `gitlab` for a crate `clove-gitlab` that ships only
    // `clove-sync-gitlab` — a subcommand that does not exist; and mapping
    // `commands` over *every* bin would emit `clove helper` for a bin named
    // `helper`, which can never be invoked.
    let dispatchable: Vec<&str> = plugin
        .bin_names
        .iter()
        .filter_map(|bin| provenance::bare_subcommand(bin))
        .collect();

    json!({
        "name": dispatchable
            .first()
            .copied()
            .unwrap_or_else(|| plugin
                .crate_name
                .strip_prefix("clove-")
                .unwrap_or(&plugin.crate_name)),
        "binary": dispatchable.first().map(|bare| format!("clove-{bare}")),
        "path": Value::Null,
        "version": Value::Null,
        "latest_version": plugin.display_version(),
        "about": plugin.description,
        "provides": Vec::<String>::new(),
        "commands": dispatchable
            .iter()
            .map(|bare| plugin::run_as(&[], bare))
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
fn render_human(
    installed: &[EnrichedPlugin],
    available: &[RegistryPlugin],
    discovered: &[RegistryPlugin],
    warning: Option<&str>,
    consulted_registry: bool,
) {
    if let Some(warning) = warning {
        eprintln!("warning: {warning}");
    }

    // Only the bare `plugin list` stays silent on an empty machine. Once the
    // registry is in play there are sections to label, so printing nothing at
    // all would read as a broken command.
    //
    // Derived from `--all`/`--refresh` having been asked for, not from whether
    // the answer happened to be non-empty: a *successful* discovery that found
    // nothing is exactly when a bare empty screen is least defensible, and it is
    // reachable for real in the window between `clove-plugin` being published
    // and its first dependent.
    let show_sections = consulted_registry;

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
        render_installed_table(installed, discovered, show_sections);
    }

    if !available.is_empty() {
        println!();
        println!("Available");
        render_available_table(available, installed);
    }
}

fn render_installed_table(plugins: &[EnrichedPlugin], discovered: &[RegistryPlugin], indent: bool) {
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
                // These state what the *plugin declares*, not what dispatch
                // does. Dispatch is a probe-free `stat` walk by design, so it
                // neither warns nor refuses; the old strings ("runs with a
                // warning", "[needs a newer clove]") described enforcement that
                // does not exist anywhere in the codebase.
                PluginStatus::Outdated => {
                    about.push_str("  [built for an older clove]");
                }
                PluginStatus::NeedsNewerClove => {
                    about.push_str("  [declares it needs a newer clove]");
                }
                PluginStatus::Ok | PluginStatus::NoInfo => {}
            }
            if let Some(latest) = latest_for(p, discovered) {
                about.push_str(&format!("  [update available: {latest}]"));
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
            latest_prerelease: None,
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
        let warning = discovery.warning(false).expect("absent registry warns");
        assert!(warning.contains("not published"));
    }

    #[test]
    fn a_published_but_empty_registry_does_not_warn() {
        // Published with no dependents yet is a *complete, correct* answer.
        let discovery = Discovery::Available(Some(vec![]));
        assert_eq!(discovery.warning(false), None);
    }

    #[test]
    fn a_failed_discovery_reports_its_cause() {
        let discovery = Discovery::Failed("could not reach crates.io: dns".to_owned());
        assert!(discovery.plugins().is_empty());
        assert!(discovery.warning(false).unwrap().contains("dns"));
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
    fn meta_reports_a_discovery_failure_through_warnings() {
        // `warnings` is the repo-wide non-fatal channel; a parallel key would
        // make a consumer learn a second convention to notice a partial failure.
        let with_error = meta(1, 0, Some("offline".to_owned()));
        assert_eq!(with_error["warnings"], json!(["offline"]));
        assert_eq!(with_error["installed_count"], 1);

        let clean = meta(1, 2, None);
        assert_eq!(clean["warnings"], json!([]));
        assert_eq!(clean["count"], 3);
    }
}
