//! Dynamic, plugin-aware `--help` for the `import`/`export`/`sync` multiplexers
//! (`PLUGIN_REGISTRY.md` §6).
//!
//! clap's derive `after_help` is a compile-time string, so it cannot list the
//! *installed* provider plugins. Instead the host intercepts a bare `<mux>
//! --help` in argv **before** `Cli::try_parse()`: [`detect`] recognizes it
//! (reusing the "globals precede the provider" rule), and [`render`] rebuilds
//! clap's help for that subcommand with a runtime `after_help` that probes the
//! `clove-<mux>-*` plugins on the search path. Every other argv is untouched — it
//! falls through to the normal parser.

use clap::CommandFactory;

use crate::cli::Cli;
use crate::plugin;

/// The global flags that take a value (`PLUGIN_REGISTRY.md` §6). These MUST mirror
/// the `global = true` value-taking args on [`Cli`] exactly — the drift-guard test
/// [`global_flag_lists_match_the_cli`] pins that. [`detect`] consumes the flag's
/// value token (or handles the `--flag=value` form) when skipping past them.
const GLOBAL_VALUE_FLAGS: &[&str] = &["-f", "--format", "--color", "--clove-dir"];

/// The global boolean flags (`PLUGIN_REGISTRY.md` §6). MUST mirror the boolean
/// `global = true` args on [`Cli`] exactly (drift-guarded). [`detect`] skips these
/// without consuming a following token.
const GLOBAL_BOOL_FLAGS: &[&str] = &["--no-index", "--deep", "--quiet"];

/// The multiplexers whose `--help` is dynamically rendered.
const MULTIPLEXERS: &[&str] = &["import", "export", "sync"];

/// The flag name of a token, stripping a `=value` suffix (`--format=json` →
/// `--format`) so a value flag matches in either spelling.
fn flag_name(token: &str) -> &str {
    token.split_once('=').map(|(name, _)| name).unwrap_or(token)
}

/// True if `token` is a global **short** value flag with its value attached in
/// the same token, e.g. `-fjson` for `-f`. clap accepts this spelling, so
/// [`detect`] must treat the whole token as a self-contained value flag (it does
/// *not* consume a following token). Only the two-char short entries in
/// [`GLOBAL_VALUE_FLAGS`] (`-f`) qualify; long flags carry their value via `=`.
fn is_attached_short_value(token: &str) -> bool {
    GLOBAL_VALUE_FLAGS.iter().any(|flag| {
        flag.len() == 2 && !flag.starts_with("--") && token.len() > 2 && token.starts_with(flag)
    })
}

/// Detect a `<mux> --help` invocation to intercept (`PLUGIN_REGISTRY.md` §6).
///
/// `argv` is the full process argv (argv[0] is the program name). Skips the
/// leading global flags (a value flag consumes its value token unless written
/// `--flag=value`), takes the first non-flag token, and — if it is one of
/// `import`/`export`/`sync` **and** the next token is `-h`/`--help` — returns that
/// multiplexer. So `import --help` intercepts, but `import tk --help` does not
/// (the `--help` is past the provider and is forwarded to the plugin), and
/// `clove --help` does not (the first token is not a multiplexer).
pub fn detect(argv: &[String]) -> Option<&'static str> {
    let mut iter = argv.iter().skip(1);

    // Skip the leading global flags to reach the first positional token.
    let first = loop {
        let token = iter.next()?;
        let name = flag_name(token);
        if GLOBAL_VALUE_FLAGS.contains(&name) {
            // A separate value token (`-f json`) is consumed; the `--flag=value`
            // form already carries its value in the same token.
            if !token.contains('=') {
                iter.next();
            }
            continue;
        }
        // Attached short-value form (`-fjson`): a self-contained value flag —
        // don't misread it as the first positional.
        if is_attached_short_value(token) {
            continue;
        }
        if GLOBAL_BOOL_FLAGS.contains(&name) {
            continue;
        }
        break token;
    };

    // The first positional must be a known multiplexer.
    let mux = MULTIPLEXERS.iter().copied().find(|m| *m == first)?;

    // …and the very next token must be the help flag (i.e. the provider slot).
    match iter.next() {
        Some(next) if next == "-h" || next == "--help" => Some(mux),
        _ => None,
    }
}

/// Render the dynamic `--help` for `mux` and print it to stdout
/// (`PLUGIN_REGISTRY.md` §6). Rebuilds clap's help for that subcommand with a
/// runtime `after_help` = [`render_provider_section`], so clap stays the single
/// source for usage/args and only the provider trailer is dynamic.
pub fn render(mux: &str) {
    let section = render_provider_section(mux);
    let mut cmd = Cli::command().mut_subcommand(mux, move |c| c.after_help(section));
    cmd.build();
    if let Some(sub) = cmd.find_subcommand_mut(mux) {
        let _ = sub.print_long_help();
    }
}

/// Build the dynamic provider trailer for `mux`'s help (`PLUGIN_REGISTRY.md` §6):
/// the built-in providers, then an `Installed providers:` list, then the "globals
/// precede the provider" note.
///
/// Installed providers are bucketed by the **`<mux>:<provider>` capability token**
/// each plugin advertises (`--clove-plugin-info`-probed), not by binary name — so a
/// multi-capability binary like `clove-sync-github` (`provides: sync/import/export
/// :github`) lists under all three of `import`/`export`/`sync --help` from one
/// install (`PLUGIN_SYSTEM.md` §4.2). A legacy plugin that answers no probe falls
/// back to its name (`clove-<mux>-<provider>` ⇒ it serves `<mux>:<provider>`), so
/// it still lists under its home mux. When two binaries serve the same provider, a
/// dedicated `clove-<mux>-<provider>` is preferred in the row (it wins dispatch).
fn render_provider_section(mux: &str) -> String {
    use std::collections::BTreeMap;

    let mut out = String::new();

    match mux {
        "import" | "export" => {
            out.push_str("Built-in providers:\n");
            out.push_str(
                "  json   clove's native restore/dump — a single JSON envelope of all items\n",
            );
            out.push_str("  jsonl  one item per line (NDJSON)\n");
        }
        "sync" => {
            out.push_str("Built-in providers: none (every provider is a plugin)\n");
        }
        _ => {}
    }

    // provider → (row text, is-dedicated). Dedup by provider; a dedicated
    // `clove-<mux>-<provider>` binary's row replaces an umbrella binary's.
    let want = format!("{mux}:");
    let dedicated_name = |provider: &str| format!("{mux}-{provider}");
    let mut rows: BTreeMap<String, (String, bool)> = BTreeMap::new();

    for plugin in plugin::list_enriched() {
        // Capability tokens = the UNION of the probed `provides` and the token the
        // binary's *name* implies. The name token is always included because
        // dispatch is name-based (`resolve_mux` would route `<mux> <provider>` to a
        // `clove-<mux>-<provider>` binary regardless of what it advertises), so it
        // is genuinely reachable; the probed `provides` add the extra capabilities a
        // multi-capability binary serves via the umbrella (e.g. clove-sync-github
        // under import/export). A no-probe plugin contributes only its name token.
        let mut tokens: Vec<String> = plugin
            .probed
            .as_ref()
            .map(|p| p.provides.clone())
            .unwrap_or_default();
        if let Some(name_token) = name_capability_token(&plugin.info.name) {
            if !tokens.contains(&name_token) {
                tokens.push(name_token);
            }
        }

        for token in tokens {
            let Some(provider) = token.strip_prefix(&want) else {
                continue;
            };
            let binary = format!("clove-{}", plugin.info.name);
            let descr = match &plugin.probed {
                Some(info) => format!("{binary} {} — {}", info.version, info.about),
                None => format!("{binary} (no metadata)"),
            };
            let row = format!("  {provider}  {descr}   (clove {mux} {provider})\n");
            let is_dedicated = plugin.info.name == dedicated_name(provider);
            rows.entry(provider.to_owned())
                .and_modify(|existing| {
                    // A dedicated binary's row wins over an umbrella's.
                    if is_dedicated && !existing.1 {
                        *existing = (row.clone(), true);
                    }
                })
                .or_insert((row, is_dedicated));
        }
    }

    if !rows.is_empty() {
        out.push_str("\nInstalled providers:\n");
        for (row, _) in rows.values() {
            out.push_str(row);
        }
    }

    out.push_str(
        "\nNote: clove global flags (--format, --color, --quiet, …) must come BEFORE the \
provider — everything after it is the provider's own arguments.",
    );
    out
}

/// The single `<mux>:<provider>` capability token implied by a plugin's *name*
/// (`clove-<mux>-<provider>` ⇒ `<mux>:<provider>`), used only as the fallback when
/// a plugin answers no `--clove-plugin-info` probe. Returns `None` for a name that
/// is not `<one-of import|export|sync>-<provider>`.
fn name_capability_token(name: &str) -> Option<String> {
    MULTIPLEXERS.iter().find_map(|mux| {
        let prefix = format!("{mux}-");
        name.strip_prefix(&prefix)
            .filter(|provider| !provider.is_empty())
            .map(|provider| format!("{mux}:{provider}"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(items: &[&str]) -> Vec<String> {
        // Prepend the argv[0] program name the real process carries.
        std::iter::once("clove".to_owned())
            .chain(items.iter().map(|s| (*s).to_owned()))
            .collect()
    }

    #[test]
    fn intercepts_bare_mux_help() {
        assert_eq!(detect(&s(&["import", "--help"])), Some("import"));
        assert_eq!(detect(&s(&["import", "-h"])), Some("import"));
        assert_eq!(detect(&s(&["export", "--help"])), Some("export"));
        assert_eq!(detect(&s(&["sync", "--help"])), Some("sync"));
    }

    #[test]
    fn does_not_intercept_provider_help() {
        // `--help` is past the provider → forwarded to the plugin, not intercepted.
        assert_eq!(detect(&s(&["import", "tk", "--help"])), None);
        assert_eq!(detect(&s(&["sync", "github", "--help"])), None);
    }

    #[test]
    fn does_not_intercept_top_level_help() {
        assert_eq!(detect(&s(&["--help"])), None);
        assert_eq!(detect(&s(&["-h"])), None);
        assert_eq!(detect(&s(&[])), None);
    }

    #[test]
    fn name_capability_token_infers_home_mux() {
        // The no-probe fallback: a `clove-<mux>-<provider>` name implies it serves
        // `<mux>:<provider>` (used only when a plugin answers no probe).
        assert_eq!(
            name_capability_token("sync-github"),
            Some("sync:github".to_owned())
        );
        assert_eq!(
            name_capability_token("import-tk"),
            Some("import:tk".to_owned())
        );
        assert_eq!(
            name_capability_token("export-csv"),
            Some("export:csv".to_owned())
        );
        // A non-mux name (generic plugin) implies no provider capability.
        assert_eq!(name_capability_token("frobnicate"), None);
    }

    #[test]
    fn does_not_intercept_non_help_mux() {
        // `import` alone (no help flag) is a normal parse, not an interception.
        assert_eq!(detect(&s(&["import"])), None);
        assert_eq!(detect(&s(&["import", "tk"])), None);
    }

    #[test]
    fn skips_a_value_global_before_the_mux() {
        // The value flag's value token must be skipped so `import` lands in the
        // positional slot.
        assert_eq!(
            detect(&s(&["--clove-dir", "X", "import", "--help"])),
            Some("import")
        );
        assert_eq!(
            detect(&s(&["-f", "json", "import", "--help"])),
            Some("import")
        );
        assert_eq!(
            detect(&s(&["--format", "json", "sync", "--help"])),
            Some("sync")
        );
        // The `--flag=value` form carries its value inline (no token to consume).
        assert_eq!(
            detect(&s(&["--format=json", "export", "--help"])),
            Some("export")
        );
        // The attached short-value form (`-fjson`) is self-contained — clap accepts
        // it, so `detect` must not misread it as the first positional.
        assert_eq!(detect(&s(&["-fjson", "import", "--help"])), Some("import"));
    }

    #[test]
    fn skips_a_bool_global_before_the_mux() {
        assert_eq!(detect(&s(&["--quiet", "import", "--help"])), Some("import"));
        assert_eq!(
            detect(&s(&["--no-index", "--deep", "sync", "--help"])),
            Some("sync")
        );
    }

    #[test]
    fn global_flag_lists_match_the_cli() {
        // Drift-guard: the const lists MUST mirror the `global = true` flags on the
        // `Cli` derive exactly (both directions), so the argv skipper in `detect`
        // can never disagree with what clap actually treats as a global.
        use std::collections::BTreeSet;

        let cmd = Cli::command();
        // Split clap's globals by whether the flag *takes a value*, so the guard
        // catches a flag placed in the wrong const list — not just a missing/extra
        // one. A mis-categorized value flag would make `detect` fail to consume its
        // value token and shift the positional scan by one (MINOR).
        let mut actual_value: BTreeSet<String> = BTreeSet::new();
        let mut actual_bool: BTreeSet<String> = BTreeSet::new();
        for arg in cmd.get_arguments() {
            if !arg.is_global_set() {
                continue;
            }
            let id = arg.get_id().as_str();
            // clap's auto-added help/version are propagated but are not part of the
            // multiplexer-forwarding rule we mirror here.
            if id == "help" || id == "version" {
                continue;
            }
            let bucket = if arg.get_action().takes_values() {
                &mut actual_value
            } else {
                &mut actual_bool
            };
            if let Some(short) = arg.get_short() {
                bucket.insert(format!("-{short}"));
            }
            if let Some(long) = arg.get_long() {
                bucket.insert(format!("--{long}"));
            }
        }

        let declared_value: BTreeSet<String> =
            GLOBAL_VALUE_FLAGS.iter().map(|s| (*s).to_owned()).collect();
        let declared_bool: BTreeSet<String> =
            GLOBAL_BOOL_FLAGS.iter().map(|s| (*s).to_owned()).collect();

        assert_eq!(
            actual_value, declared_value,
            "GLOBAL_VALUE_FLAGS drifted from the Cli value-taking global flags"
        );
        assert_eq!(
            actual_bool, declared_bool,
            "GLOBAL_BOOL_FLAGS drifted from the Cli boolean global flags"
        );
    }
}
