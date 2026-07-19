//! `clove setup`: wire clove into Claude Code. Mirrors `engramdb setup` but
//! clove has no hooks, so it only (1) registers the `clove mcp` server + its
//! tool permissions in `settings.json`, (2) writes a `CLOVE.md` agent-directives
//! file, and (3) appends an `@CLOVE.md` reference to `CLAUDE.md`. Idempotent, with
//! `--dry-run` and `--global` (`~/.claude/`) vs project (`<cwd>/.claude/`) scope.

use camino::{Utf8Path, Utf8PathBuf};
use clove_core::OutputFormat;
use clove_types::CloveError;
use serde_json::{json, Value};

use crate::context::current_dir;
use crate::output::print_json_success;

/// The agent-directives file, kept byte-identical to the repo-root `CLOVE.md`
/// (a test pins this). Inlined rather than `include_str!`d because `CLOVE.md`
/// lives above this crate's directory and would not travel in the published
/// `.crate` tarball.
const CLOVE_MD_CONTENT: &str = r#"# clove

This repository uses **clove** — a git-native, dependency-aware work-item
tracker — as the source of truth for tasks, bugs, and features. Prefer it over
ad-hoc TODO lists or scratch notes for anything that spans more than a single
step.

- **Check before starting** — at the start of any multi-step task, run
  `clove ready` (unblocked work) and `clove search <text>` / `clove list` to find
  related items *before* creating new ones. The `clove_ready` / `clove_search` /
  `clove_list` MCP tools do the same.
- **File work as items** — capture new tasks/bugs/features with
  `clove new <title> [--type bug|feature|chore|docs|epic] [-p 0-4] [--dep ID]
  [--parent ID]` instead of loose notes, so the work is tracked and shareable.
- **Record progress** — transition items with
  `clove status <id> <open|in_progress|closed>` (aliases `start` / `close`) and
  capture findings with `clove comment <id> <message>` as you go.
- **Respect the graph** — wire blocking relationships with
  `clove dep add <id> <dep-id>`; use `clove blocked` and `clove dep tree <id>` to
  see what is waiting on what. An item is *ready* when it is open and every hard
  dependency is closed.
- **Full reference** — run `clove agent-doc` for the complete command surface,
  the `{ v, ok, data, _meta }` JSON envelope, and exit codes.
"#;

/// The line appended to `CLAUDE.md` to import the directives above.
const CLOVE_MD_REF: &str = "@CLOVE.md";

/// The `settings.json` `mcpServers` key. Matches the installed command name so
/// the resulting `mcp__clove__*` tool-permission strings read naturally. This is
/// a *separate* registration from the marketplace plugin's `tracker` key (which
/// resolves under `mcp__plugin_clove_tracker__*`); the two never collide.
const MCP_SERVER_KEY: &str = "clove";

/// Prefix for a permission entry: `mcp__` + [`MCP_SERVER_KEY`] + `__`.
const SETTINGS_MCP_PREFIX: &str = "mcp__clove__";

/// Every tool the `clove mcp` server exposes. A test pins this to the live
/// clove-mcp router (under the `mcp` feature) so a renamed/added tool can't
/// silently drift and reintroduce per-call permission prompts.
const MCP_TOOL_NAMES: &[&str] = &[
    "clove_ready",
    "clove_blocked",
    "clove_list",
    "clove_show",
    "clove_search",
    "clove_dep_tree",
    "clove_stats",
    "clove_new",
    "clove_status",
    "clove_edit",
    "clove_comment",
    "clove_dep_add",
    "clove_dep_remove",
    "clove_set_parent",
];

/// Run `clove setup`.
pub fn run(
    format: OutputFormat,
    global: bool,
    dry_run: bool,
    claude_dir_override: Option<&Utf8Path>,
    quiet: bool,
) -> Result<(), CloveError> {
    let project_dir = current_dir()?;
    let claude_dir = resolve_claude_dir(&project_dir, global, claude_dir_override)?;

    let mut actions: Vec<String> = Vec::new();
    let settings_changed = write_settings(&claude_dir, dry_run, &mut actions)?;
    let clove_md_changed = write_clove_md(&claude_dir, dry_run, &mut actions)?;
    let claude_md_changed = update_claude_md(&claude_dir, dry_run, &mut actions)?;
    let changed = settings_changed || clove_md_changed || claude_md_changed;

    match format {
        OutputFormat::Json | OutputFormat::Jsonl => print_json_success(
            json!({
                "changed": changed,
                "dry_run": dry_run,
                "claude_dir": claude_dir.as_str(),
                "actions": actions,
            }),
            json!({ "warnings": [] }),
        ),
        OutputFormat::Human => {
            if !quiet {
                for line in &actions {
                    println!("{line}");
                }
                if dry_run {
                    println!("Dry run — nothing was written.");
                } else if !changed {
                    println!("Everything is already set up. Nothing to do.");
                }
            }
        }
    }
    Ok(())
}

/// Resolve the `.claude/` directory: an explicit override wins; otherwise
/// `~/.claude` under `--global`, else `<project>/.claude`.
fn resolve_claude_dir(
    project_dir: &Utf8Path,
    global: bool,
    override_dir: Option<&Utf8Path>,
) -> Result<Utf8PathBuf, CloveError> {
    if let Some(dir) = override_dir {
        return Ok(dir.to_owned());
    }
    if global {
        return Ok(home_dir()?.join(".claude"));
    }
    Ok(project_dir.join(".claude"))
}

/// The user's home directory (`$HOME`, or `%USERPROFILE%` on Windows). clove has
/// no `dirs`-style dependency, so this is resolved by hand.
fn home_dir() -> Result<Utf8PathBuf, CloveError> {
    let var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    let raw = std::env::var_os(var).ok_or_else(|| CloveError::InvalidField {
        field: "--global".to_owned(),
        reason: format!("cannot resolve the home directory: ${var} is not set"),
    })?;
    Utf8PathBuf::from_path_buf(std::path::PathBuf::from(raw)).map_err(|path| {
        CloveError::InvalidField {
            field: "--global".to_owned(),
            reason: format!("home directory is not valid UTF-8: {}", path.display()),
        }
    })
}

/// Register the `clove mcp` server and its tool permissions in `settings.json`,
/// merging into (never clobbering) any existing config. Returns whether anything
/// changed.
fn write_settings(
    claude_dir: &Utf8Path,
    dry_run: bool,
    actions: &mut Vec<String>,
) -> Result<bool, CloveError> {
    let settings_path = claude_dir.join("settings.json");
    let mut settings = read_settings(&settings_path)?;
    let mut changed = false;

    // MCP server entry.
    {
        let root = settings_object_mut(&mut settings, &settings_path)?;
        let servers = ensure_object_entry(root, "mcpServers", "mcpServers", &settings_path)?;
        if !servers.contains_key(MCP_SERVER_KEY) {
            servers.insert(
                MCP_SERVER_KEY.to_owned(),
                json!({ "command": "clove", "args": ["mcp"] }),
            );
            changed = true;
        }
    }

    // Tool permissions (one explicit allow entry per tool).
    {
        let root = settings_object_mut(&mut settings, &settings_path)?;
        let permissions = ensure_object_entry(root, "permissions", "permissions", &settings_path)?;
        let allow = ensure_array_entry(permissions, "allow", "permissions.allow", &settings_path)?;
        let missing: Vec<String> = {
            let existing: std::collections::HashSet<&str> =
                allow.iter().filter_map(Value::as_str).collect();
            MCP_TOOL_NAMES
                .iter()
                .map(|tool| format!("{SETTINGS_MCP_PREFIX}{tool}"))
                .filter(|perm| !existing.contains(perm.as_str()))
                .collect()
        };
        if !missing.is_empty() {
            for perm in missing {
                allow.push(json!(perm));
            }
            changed = true;
        }
    }

    if !changed {
        actions
            .push("settings.json already registers the clove MCP server and permissions.".into());
        return Ok(false);
    }
    if dry_run {
        actions.push(
            "Would register the clove MCP server + tool permissions in settings.json.".into(),
        );
        return Ok(true);
    }

    create_dir_all(claude_dir)?;
    let formatted =
        serde_json::to_string_pretty(&settings).map_err(|e| CloveError::InvalidField {
            field: "settings.json".to_owned(),
            reason: format!("could not serialize settings.json: {e}"),
        })?;
    write_atomic(&settings_path, &formatted)?;
    actions.push("Registered the clove MCP server + tool permissions in settings.json.".into());
    Ok(true)
}

/// Write `CLOVE.md` (create or refresh stale content). Returns whether it changed.
fn write_clove_md(
    claude_dir: &Utf8Path,
    dry_run: bool,
    actions: &mut Vec<String>,
) -> Result<bool, CloveError> {
    let path = claude_dir.join("CLOVE.md");
    let exists = path.exists();
    if exists && read_to_string(&path)? == CLOVE_MD_CONTENT {
        actions.push("CLOVE.md is already up to date.".into());
        return Ok(false);
    }
    if dry_run {
        actions.push(if exists {
            "Would update CLOVE.md.".into()
        } else {
            "Would create CLOVE.md.".into()
        });
        return Ok(true);
    }
    create_dir_all(claude_dir)?;
    write_file(&path, CLOVE_MD_CONTENT)?;
    actions.push(if exists {
        "Updated CLOVE.md.".into()
    } else {
        "Created CLOVE.md.".into()
    });
    Ok(true)
}

/// Append `@CLOVE.md` to `CLAUDE.md` if it is not already imported. Returns
/// whether it changed.
fn update_claude_md(
    claude_dir: &Utf8Path,
    dry_run: bool,
    actions: &mut Vec<String>,
) -> Result<bool, CloveError> {
    let path = claude_dir.join("CLAUDE.md");
    let existing = read_to_string_opt(&path)?;

    if existing.lines().any(|line| line.trim() == CLOVE_MD_REF) {
        actions.push("CLAUDE.md already imports @CLOVE.md.".into());
        return Ok(false);
    }
    if dry_run {
        actions.push(if existing.is_empty() {
            "Would create CLAUDE.md importing @CLOVE.md.".into()
        } else {
            "Would add the @CLOVE.md import to CLAUDE.md.".into()
        });
        return Ok(true);
    }

    create_dir_all(claude_dir)?;
    let new_content = if existing.is_empty() {
        format!("{CLOVE_MD_REF}\n")
    } else {
        let separator = if existing.ends_with('\n') {
            "\n"
        } else {
            "\n\n"
        };
        format!("{existing}{separator}{CLOVE_MD_REF}\n")
    };
    write_file(&path, &new_content)?;
    actions.push(if existing.is_empty() {
        "Created CLAUDE.md importing @CLOVE.md.".into()
    } else {
        "Added the @CLOVE.md import to CLAUDE.md.".into()
    });
    Ok(true)
}

// --- settings.json JSON helpers (ported from engramdb, adapted to CloveError) ---

fn json_type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// Error for a `settings.json` key with the wrong JSON type. Names the file and
/// the offending key so it can be fixed by hand.
fn shape_error(path: &Utf8Path, key_path: &str, expected: &str, found: &Value) -> CloveError {
    CloveError::InvalidField {
        field: format!("settings.json:{key_path}"),
        reason: format!(
            "{path}: expected \"{key_path}\" to be a JSON {expected}, found {} — \
             fix or remove the key and re-run `clove setup`",
            json_type_name(found)
        ),
    }
}

/// True if a value is semantically empty (`null`, `[]`, or `{}`) and so safe to
/// replace with a freshly shaped container without losing user data.
fn is_semantically_empty(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::Array(arr) => arr.is_empty(),
        Value::Object(obj) => obj.is_empty(),
        _ => false,
    }
}

/// Read and parse `settings.json`. A missing file yields `{}`; a non-object top
/// level (or invalid JSON) is a hard error rather than a silent clobber.
fn read_settings(path: &Utf8Path) -> Result<Value, CloveError> {
    let settings: Value = if path.exists() {
        let content = read_to_string(path)?;
        serde_json::from_str(&content).map_err(|e| CloveError::InvalidField {
            field: "settings.json".to_owned(),
            reason: format!("{path}: not valid JSON ({e}) — fix the file and re-run `clove setup`"),
        })?
    } else {
        json!({})
    };
    if !settings.is_object() {
        return Err(CloveError::InvalidField {
            field: "settings.json".to_owned(),
            reason: format!(
                "{path}: expected the top-level value to be a JSON object, found {} — \
                 fix the file and re-run `clove setup`",
                json_type_name(&settings)
            ),
        });
    }
    Ok(settings)
}

/// Borrow the top-level object, erroring (not panicking) if it is not one.
fn settings_object_mut<'a>(
    settings: &'a mut Value,
    path: &Utf8Path,
) -> Result<&'a mut serde_json::Map<String, Value>, CloveError> {
    match settings {
        Value::Object(map) => Ok(map),
        other => Err(shape_error(path, "<root>", "object", other)),
    }
}

/// Get-or-create `parent[key]` as an object; repair a semantically empty value
/// to `{}`; any other non-object type is a hard error naming the key.
fn ensure_object_entry<'a>(
    parent: &'a mut serde_json::Map<String, Value>,
    key: &str,
    key_path: &str,
    path: &Utf8Path,
) -> Result<&'a mut serde_json::Map<String, Value>, CloveError> {
    let slot = parent.entry(key).or_insert_with(|| json!({}));
    if !slot.is_object() && is_semantically_empty(slot) {
        *slot = json!({});
    }
    match slot {
        Value::Object(map) => Ok(map),
        other => Err(shape_error(path, key_path, "object", other)),
    }
}

/// Get-or-create `parent[key]` as an array; repair a semantically empty value to
/// `[]`; any other non-array type is a hard error naming the key.
fn ensure_array_entry<'a>(
    parent: &'a mut serde_json::Map<String, Value>,
    key: &str,
    key_path: &str,
    path: &Utf8Path,
) -> Result<&'a mut Vec<Value>, CloveError> {
    let slot = parent.entry(key).or_insert_with(|| json!([]));
    if !slot.is_array() && is_semantically_empty(slot) {
        *slot = json!([]);
    }
    match slot {
        Value::Array(arr) => Ok(arr),
        other => Err(shape_error(path, key_path, "array", other)),
    }
}

/// Atomically replace `path`: write a sibling temp file, then rename over the
/// target, so a crash mid-write can never leave a truncated `settings.json`.
fn write_atomic(path: &Utf8Path, contents: &str) -> Result<(), CloveError> {
    let dir = match path.parent() {
        Some(parent) if !parent.as_str().is_empty() => parent.to_owned(),
        _ => Utf8PathBuf::from("."),
    };
    let file_name = path.file_name().unwrap_or("settings.json");
    let tmp_path = dir.join(format!(".{file_name}.{}.tmp", std::process::id()));
    let result =
        std::fs::write(&tmp_path, contents).and_then(|()| std::fs::rename(&tmp_path, path));
    result.map_err(|source| {
        let _ = std::fs::remove_file(&tmp_path);
        CloveError::Io {
            path: path.to_owned(),
            source,
        }
    })
}

// --- small filesystem helpers (CloveError-mapped) ---

fn create_dir_all(path: &Utf8Path) -> Result<(), CloveError> {
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

fn read_to_string(path: &Utf8Path) -> Result<String, CloveError> {
    std::fs::read_to_string(path).map_err(|source| CloveError::Io {
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

#[cfg(test)]
mod tests {
    use super::*;
    use camino::Utf8Path;
    use tempfile::TempDir;

    fn dir(tmp: &TempDir) -> Utf8PathBuf {
        Utf8Path::from_path(tmp.path()).unwrap().to_owned()
    }

    fn settings_at(dir: &Utf8Path) -> Value {
        serde_json::from_str(&std::fs::read_to_string(dir.join("settings.json")).unwrap()).unwrap()
    }

    // --- CLOVE.md ---

    #[test]
    fn clove_md_creates() {
        let tmp = TempDir::new().unwrap();
        let d = dir(&tmp);
        let mut a = Vec::new();
        assert!(write_clove_md(&d, false, &mut a).unwrap());
        assert_eq!(
            read_to_string(&d.join("CLOVE.md")).unwrap(),
            CLOVE_MD_CONTENT
        );
    }

    #[test]
    fn clove_md_idempotent() {
        let tmp = TempDir::new().unwrap();
        let d = dir(&tmp);
        let mut a = Vec::new();
        write_clove_md(&d, false, &mut a).unwrap();
        assert!(!write_clove_md(&d, false, &mut a).unwrap());
    }

    #[test]
    fn clove_md_updates_stale() {
        let tmp = TempDir::new().unwrap();
        let d = dir(&tmp);
        write_file(&d.join("CLOVE.md"), "old content").unwrap();
        let mut a = Vec::new();
        assert!(write_clove_md(&d, false, &mut a).unwrap());
        assert_eq!(
            read_to_string(&d.join("CLOVE.md")).unwrap(),
            CLOVE_MD_CONTENT
        );
    }

    #[test]
    fn clove_md_dry_run_writes_nothing() {
        let tmp = TempDir::new().unwrap();
        let d = dir(&tmp);
        let mut a = Vec::new();
        assert!(write_clove_md(&d, true, &mut a).unwrap());
        assert!(!d.join("CLOVE.md").exists());
    }

    // --- CLAUDE.md ---

    #[test]
    fn claude_md_creates() {
        let tmp = TempDir::new().unwrap();
        let d = dir(&tmp);
        let mut a = Vec::new();
        assert!(update_claude_md(&d, false, &mut a).unwrap());
        assert_eq!(read_to_string(&d.join("CLAUDE.md")).unwrap(), "@CLOVE.md\n");
    }

    #[test]
    fn claude_md_appends_with_blank_line_when_no_trailing_newline() {
        let tmp = TempDir::new().unwrap();
        let d = dir(&tmp);
        write_file(&d.join("CLAUDE.md"), "# existing").unwrap();
        let mut a = Vec::new();
        assert!(update_claude_md(&d, false, &mut a).unwrap());
        let content = read_to_string(&d.join("CLAUDE.md")).unwrap();
        assert!(content.starts_with("# existing"));
        assert!(content.contains("\n\n@CLOVE.md\n"));
    }

    #[test]
    fn claude_md_idempotent_and_detects_existing_ref() {
        let tmp = TempDir::new().unwrap();
        let d = dir(&tmp);
        let mut a = Vec::new();
        update_claude_md(&d, false, &mut a).unwrap();
        assert!(!update_claude_md(&d, false, &mut a).unwrap());
    }

    #[test]
    fn claude_md_dry_run_writes_nothing() {
        let tmp = TempDir::new().unwrap();
        let d = dir(&tmp);
        let mut a = Vec::new();
        assert!(update_claude_md(&d, true, &mut a).unwrap());
        assert!(!d.join("CLAUDE.md").exists());
    }

    // --- settings.json ---

    #[test]
    fn settings_creates_server_and_permissions() {
        let tmp = TempDir::new().unwrap();
        let d = dir(&tmp);
        let mut a = Vec::new();
        assert!(write_settings(&d, false, &mut a).unwrap());
        let s = settings_at(&d);
        assert_eq!(s["mcpServers"]["clove"]["command"], "clove");
        assert_eq!(s["mcpServers"]["clove"]["args"], json!(["mcp"]));
        let allow = s["permissions"]["allow"].as_array().unwrap();
        for tool in MCP_TOOL_NAMES {
            assert!(allow
                .iter()
                .any(|v| v == &json!(format!("{SETTINGS_MCP_PREFIX}{tool}"))));
        }
    }

    #[test]
    fn settings_idempotent() {
        let tmp = TempDir::new().unwrap();
        let d = dir(&tmp);
        let mut a = Vec::new();
        write_settings(&d, false, &mut a).unwrap();
        let before = std::fs::read_to_string(d.join("settings.json")).unwrap();
        assert!(!write_settings(&d, false, &mut a).unwrap());
        let after = std::fs::read_to_string(d.join("settings.json")).unwrap();
        assert_eq!(before, after);
    }

    #[test]
    fn settings_merges_preserving_foreign_entries() {
        let tmp = TempDir::new().unwrap();
        let d = dir(&tmp);
        write_file(
            &d.join("settings.json"),
            r#"{"mcpServers":{"other":{"command":"x"}},"permissions":{"allow":["Bash(ls:*)"]}}"#,
        )
        .unwrap();
        let mut a = Vec::new();
        assert!(write_settings(&d, false, &mut a).unwrap());
        let s = settings_at(&d);
        assert_eq!(s["mcpServers"]["other"]["command"], "x");
        assert_eq!(s["mcpServers"]["clove"]["command"], "clove");
        let allow = s["permissions"]["allow"].as_array().unwrap();
        assert!(allow.iter().any(|v| v == &json!("Bash(ls:*)")));
        assert_eq!(allow.len(), 1 + MCP_TOOL_NAMES.len());
    }

    #[test]
    fn settings_dry_run_writes_nothing() {
        let tmp = TempDir::new().unwrap();
        let d = dir(&tmp);
        let mut a = Vec::new();
        assert!(write_settings(&d, true, &mut a).unwrap());
        assert!(!d.join("settings.json").exists());
    }

    #[test]
    fn settings_repairs_semantically_empty_containers() {
        let tmp = TempDir::new().unwrap();
        let d = dir(&tmp);
        write_file(
            &d.join("settings.json"),
            r#"{"mcpServers":null,"permissions":{"allow":[]}}"#,
        )
        .unwrap();
        let mut a = Vec::new();
        assert!(write_settings(&d, false, &mut a).unwrap());
        let s = settings_at(&d);
        assert_eq!(s["mcpServers"]["clove"]["command"], "clove");
        assert_eq!(
            s["permissions"]["allow"].as_array().unwrap().len(),
            MCP_TOOL_NAMES.len()
        );
    }

    #[test]
    fn settings_rejects_bad_shape_without_writing() {
        let tmp = TempDir::new().unwrap();
        let d = dir(&tmp);
        write_file(&d.join("settings.json"), r#"{"mcpServers":"nope"}"#).unwrap();
        let mut a = Vec::new();
        let err = write_settings(&d, false, &mut a).unwrap_err();
        assert!(matches!(err, CloveError::InvalidField { .. }));
        // The original file is untouched.
        assert_eq!(
            std::fs::read_to_string(d.join("settings.json")).unwrap(),
            r#"{"mcpServers":"nope"}"#
        );
    }

    #[test]
    fn settings_rejects_invalid_json() {
        let tmp = TempDir::new().unwrap();
        let d = dir(&tmp);
        write_file(&d.join("settings.json"), "{ not json").unwrap();
        let mut a = Vec::new();
        let err = write_settings(&d, false, &mut a).unwrap_err();
        match err {
            CloveError::InvalidField { reason, .. } => assert!(reason.contains("not valid JSON")),
            other => panic!("expected InvalidField, got {other:?}"),
        }
    }

    // --- scope resolution ---

    #[test]
    fn resolve_claude_dir_variants() {
        let project = Utf8Path::new("/tmp/proj");
        assert_eq!(
            resolve_claude_dir(project, false, None).unwrap(),
            Utf8PathBuf::from("/tmp/proj/.claude")
        );
        let override_dir = Utf8Path::new("/custom/.claude");
        assert_eq!(
            resolve_claude_dir(project, false, Some(override_dir)).unwrap(),
            override_dir
        );
        // `--global` with an override still honors the override (no home lookup).
        assert_eq!(
            resolve_claude_dir(project, true, Some(override_dir)).unwrap(),
            override_dir
        );
    }

    // --- full run ---

    #[test]
    fn run_creates_all_three_and_is_idempotent() {
        let tmp = TempDir::new().unwrap();
        let d = dir(&tmp);
        run(OutputFormat::Json, false, false, Some(&d), true).unwrap();
        assert!(d.join("settings.json").exists());
        assert!(d.join("CLOVE.md").exists());
        assert!(d.join("CLAUDE.md").exists());

        let settings_before = std::fs::read_to_string(d.join("settings.json")).unwrap();
        let clove_before = std::fs::read_to_string(d.join("CLOVE.md")).unwrap();
        let claude_before = std::fs::read_to_string(d.join("CLAUDE.md")).unwrap();

        run(OutputFormat::Json, false, false, Some(&d), true).unwrap();
        assert_eq!(
            std::fs::read_to_string(d.join("settings.json")).unwrap(),
            settings_before
        );
        assert_eq!(
            std::fs::read_to_string(d.join("CLOVE.md")).unwrap(),
            clove_before
        );
        assert_eq!(
            std::fs::read_to_string(d.join("CLAUDE.md")).unwrap(),
            claude_before
        );
    }

    #[test]
    fn run_dry_run_writes_nothing() {
        let tmp = TempDir::new().unwrap();
        let d = dir(&tmp);
        run(OutputFormat::Json, false, true, Some(&d), true).unwrap();
        assert!(!d.join("settings.json").exists());
        assert!(!d.join("CLOVE.md").exists());
        assert!(!d.join("CLAUDE.md").exists());
    }

    // --- drift guards ---

    #[test]
    fn clove_md_content_matches_repo_file() {
        // CARGO_MANIFEST_DIR is `crates/clove`; the canonical file is at the
        // workspace root.
        let repo_file = concat!(env!("CARGO_MANIFEST_DIR"), "/../../CLOVE.md");
        let on_disk = std::fs::read_to_string(repo_file).unwrap();
        assert_eq!(
            on_disk, CLOVE_MD_CONTENT,
            "CLOVE.md drifted from the embedded CLOVE_MD_CONTENT; update the const"
        );
    }

    #[cfg(feature = "mcp")]
    #[test]
    fn mcp_tool_names_match_server() {
        let mut server = clove_mcp::CloveServer::tool_names();
        server.sort();
        let mut ours: Vec<String> = MCP_TOOL_NAMES.iter().map(|s| (*s).to_owned()).collect();
        ours.sort();
        assert_eq!(
            ours, server,
            "MCP_TOOL_NAMES drifted from the clove-mcp tool router; update the list"
        );
    }
}
