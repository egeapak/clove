//! `clove init` (T-CLI02): create the `.clove/` layout. Idempotent.

use camino::{Utf8Path, Utf8PathBuf};
use clove_core::config::derive_prefix;
use clove_core::{OutputFormat, GITIGNORE_ENTRIES};
use clove_types::CloveError;
use serde_json::json;

use crate::cli::InitArgs;
use crate::context::current_dir;
use crate::output::print_json_success;

pub fn run(
    format: OutputFormat,
    clove_dir_override: Option<&Utf8Path>,
    args: InitArgs,
    quiet: bool,
) -> Result<(), CloveError> {
    let (root, clove_dir) = match clove_dir_override {
        Some(dir) => {
            let root = dir
                .parent()
                .map(Utf8Path::to_owned)
                .unwrap_or_else(|| Utf8PathBuf::from("."));
            (root, dir.to_owned())
        }
        None => {
            let cwd = current_dir()?;
            let clove_dir = cwd.join(".clove");
            (cwd, clove_dir)
        }
    };

    let issues_dir = clove_dir.join("issues");
    mkdir_all(&issues_dir)?;

    // config.toml — never overwrite an existing one (idempotency).
    let config_path = clove_dir.join("config.toml");
    let created_config = !config_path.exists();
    if created_config {
        let prefix = args.prefix.unwrap_or_else(|| derive_prefix(&root));
        write_file(&config_path, &render_config(&prefix))?;
    }

    // .gitignore — fixed content, LF endings; safe to rewrite each run.
    let gitignore = clove_dir.join(".gitignore");
    write_file(&gitignore, &format!("{}\n", GITIGNORE_ENTRIES.join("\n")))?;

    if args.merge_driver {
        install_merge_driver(&root)?;
    }

    match format {
        OutputFormat::Json | OutputFormat::Jsonl => print_json_success(
            json!({
                "root": root.as_str(),
                "clove_dir": clove_dir.as_str(),
                "created_config": created_config,
            }),
            json!({ "warnings": [] }),
        ),
        OutputFormat::Human => {
            if !quiet {
                println!("Initialized clove repository in {clove_dir}");
                println!("run 'clove agent-doc' to generate an AGENTS.md snippet");
                println!("run 'clove setup' to register clove's MCP server with Claude Code");
            }
        }
    }
    Ok(())
}

/// Render a default `config.toml` with the chosen id prefix.
fn render_config(prefix: &str) -> String {
    format!(
        "# clove configuration. See `clove agent-doc` for the full reference.\n\
         config_schema = 1\n\
         id_prefix = \"{prefix}\"\n\
         default_type = \"feature\"\n\
         default_format = \"human\"\n\
         \n\
         [index]\n\
         auto_refresh = true\n\
         \n\
         [daemon]\n\
         git_sync = false\n\
         watch_debounce_ms = 200\n\
         # Idle minutes before the daemon self-terminates (0 = never). Each clove\n\
         # command resets this, so it only fires after a long gap of no activity.\n\
         idle_shutdown_min = 240\n"
    )
}

/// Install the 3-way merge driver: a `.gitattributes` line plus a
/// `[merge "clove-item"]` stanza in `.git/config`. Both are idempotent.
fn install_merge_driver(root: &Utf8Path) -> Result<(), CloveError> {
    let attributes = root.join(".gitattributes");
    let line = ".clove/issues/*.md merge=clove-item";
    let existing = read_to_string_opt(&attributes)?;
    if !existing.lines().any(|l| l.trim() == line) {
        let mut next = existing;
        if !next.is_empty() && !next.ends_with('\n') {
            next.push('\n');
        }
        next.push_str(line);
        next.push('\n');
        write_file(&attributes, &next)?;
    }

    let git_config = root.join(".git").join("config");
    if git_config.exists() {
        let existing = read_to_string_opt(&git_config)?;
        if !existing.contains("[merge \"clove-item\"]") {
            let stanza = "\n[merge \"clove-item\"]\n\
                 \tname = clove item 3-way merge\n\
                 \tdriver = clove merge-driver %O %A %B %L\n";
            let mut next = existing;
            next.push_str(stanza);
            write_file(&git_config, &next)?;
        }
    }
    Ok(())
}

fn mkdir_all(path: &Utf8Path) -> Result<(), CloveError> {
    std::fs::create_dir_all(path).map_err(|source| CloveError::Io {
        path: path.to_owned(),
        source,
    })
}

fn write_file(path: &Utf8Path, contents: &str) -> Result<(), CloveError> {
    std::fs::write(path, contents).map_err(|source| CloveError::Io {
        path: path.to_owned(),
        source,
    })
}

fn read_to_string_opt(path: &Utf8Path) -> Result<String, CloveError> {
    match std::fs::read_to_string(path) {
        Ok(s) => Ok(s),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(source) => Err(CloveError::Io {
            path: path.to_owned(),
            source,
        }),
    }
}
