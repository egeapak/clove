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
/// the built-in providers, then an `Installed providers:` list of the
/// `clove-<mux>-*` plugins on the search path (each `--clove-plugin-info`-probed,
/// or `(no metadata)` when the probe fails), then the "globals precede the
/// provider" note.
fn render_provider_section(mux: &str) -> String {
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

    // Only this multiplexer's plugins (`clove-<mux>-<provider>`) are probed.
    let prefix = format!("{mux}-");
    let installed: Vec<plugin::PluginInfo> = plugin::list()
        .into_iter()
        .filter(|p| p.name.starts_with(&prefix) && p.name.len() > prefix.len())
        .collect();

    if !installed.is_empty() {
        out.push_str("\nInstalled providers:\n");
        for p in &installed {
            let provider = &p.name[prefix.len()..];
            let binary = format!("clove-{}", p.name);
            match plugin::probe_info(&p.path) {
                Some(info) => out.push_str(&format!(
                    "  {provider}  {binary} {} — {}   (clove {mux} {provider})\n",
                    info.version, info.about
                )),
                None => out.push_str(&format!(
                    "  {provider}  {binary} (no metadata)   (clove {mux} {provider})\n"
                )),
            }
        }
    }

    out.push_str(
        "\nNote: clove global flags (--format, --color, --quiet, …) must come BEFORE the \
provider — everything after it is the provider's own arguments.",
    );
    out
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
        let mut actual: BTreeSet<String> = BTreeSet::new();
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
            if let Some(short) = arg.get_short() {
                actual.insert(format!("-{short}"));
            }
            if let Some(long) = arg.get_long() {
                actual.insert(format!("--{long}"));
            }
        }

        let declared: BTreeSet<String> = GLOBAL_VALUE_FLAGS
            .iter()
            .chain(GLOBAL_BOOL_FLAGS)
            .map(|s| (*s).to_owned())
            .collect();

        assert_eq!(
            actual, declared,
            "GLOBAL_*_FLAGS drifted from the Cli global flags"
        );
    }
}
