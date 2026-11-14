//! `clove edit` (T-CLI05) and the shared non-interactive field application used
//! by `clove set`.

use clove_core::{parse_item_file, ItemStore, OutputFormat};
use clove_types::{CloveError, CloveId};
use serde_json::Map;

use crate::cli::EditArgs;
use crate::context::Ctx;
use crate::item_json::print_item;
use crate::util::{now_millis, parse_id};

/// Apply `KEY=VALUE` (and `labels+=`/`labels-=`) assignments to `id` atomically,
/// returning the saved item. Shared by `clove edit --field` and `clove set`.
///
/// The read-modify-write runs under one store-wide lock (`update_with`), not a
/// lock-free `get` followed by a locking `update`: the latter leaves a window in
/// which a concurrent writer (web, MCP, daemon) can commit between the read and
/// the write, and have its update silently clobbered (DESIGN §4).
pub fn apply_assignments_to(
    store: &ItemStore,
    id: &CloveId,
    assignments: &[String],
) -> Result<clove_types::Item, CloveError> {
    let now = now_millis();
    store.update_with(id, now, |item| {
        clove_types::apply_assignments(&mut item.frontmatter, assignments, now)
    })
}

pub fn run(ctx: &Ctx, format: OutputFormat, args: EditArgs) -> Result<(), CloveError> {
    let id = parse_id(&args.id)?;
    let body = body_argument(&args)?;

    if args.fields.is_empty() && body.is_none() {
        let saved = open_in_editor(ctx, &id)?;
        print_item(format, &saved, Map::new());
        return Ok(());
    }

    // One `EditRequest` through `apply_edit`, the write path every surface
    // shares: the field tokens and the body land in a single locked write.
    let mut request = clove_types::EditRequest::from_tokens(&args.fields)?;
    request.body = body;
    clove_core::apply_edit(&ctx.store, &id, &request, now_millis())?;
    print_item(format, &ctx.store.get(&id)?, Map::new());
    Ok(())
}

/// The replacement body from `--body` or `--body-file` (`-` is stdin), if any.
fn body_argument(args: &EditArgs) -> Result<Option<String>, CloveError> {
    let Some(path) = &args.body_file else {
        return Ok(args.body.clone());
    };
    let read = if path.as_str() == "-" {
        std::io::read_to_string(std::io::stdin())
    } else {
        std::fs::read_to_string(path)
    };
    read.map(Some).map_err(|source| CloveError::Io {
        path: path.clone(),
        source,
    })
}

/// Open the item file in `$EDITOR`/`$VISUAL`, then save the result through the
/// store and return the saved item.
///
/// The editor works on the item file itself, with no lock held (an editor
/// session has no bound). What it leaves behind is then treated as a requested
/// edit, not as a finished write:
///
/// - Unchanged bytes: nothing is written.
/// - A file that no longer parses, or fails validation: the original bytes are
///   put back and the edited text is kept beside the store's temp files, so an
///   invalid item never stays in the store (where it would vanish from lists
///   and exports).
/// - Otherwise the item is rewritten by [`ItemStore::update_with`]: validated,
///   `updated` stamped, the `closed` timestamp kept in step with a changed
///   status, and renamed into place — the rename is what the index's staleness
///   check sees, which an editor that writes in place does not trigger.
fn open_in_editor(ctx: &Ctx, id: &clove_types::CloveId) -> Result<clove_types::Item, CloveError> {
    let path = ctx.store.path_for(id);
    if !path.exists() {
        return Err(CloveError::NotFound { id: id.to_string() });
    }
    let io_err = |source| CloveError::Io {
        path: path.clone(),
        source,
    };
    let original = std::fs::read(&path).map_err(io_err)?;
    let before = parse_item_file(&path).ok();

    let editor = std::env::var("CLOVE_EDITOR")
        .or_else(|_| std::env::var("VISUAL"))
        .or_else(|_| std::env::var("EDITOR"))
        .unwrap_or_else(|_| "vi".to_owned());

    let status = editor_command(&editor, path.as_str())
        .status()
        .map_err(|source| CloveError::Io {
            path: path.clone(),
            source: std::io::Error::new(
                source.kind(),
                format!("could not run editor `{editor}`: {source}"),
            ),
        })?;
    if !status.success() {
        return Err(CloveError::Io {
            path: path.clone(),
            source: std::io::Error::other(format!("editor `{editor}` exited with {status}")),
        });
    }

    let edited = std::fs::read(&path).map_err(io_err)?;
    if edited == original {
        return parse_item_file(&path);
    }

    let now = now_millis();
    let old_status = before.as_ref().map(|item| item.frontmatter.status);
    let saved = ctx.store.update_with(id, now, |item| {
        // A status typed into the file carries no `closed` timestamp (or a
        // stale one); keep the status↔`closed` invariant the way every other
        // status change does.
        if old_status.is_some_and(|old| old != item.frontmatter.status) {
            clove_types::set_status(&mut item.frontmatter, item.frontmatter.status, now);
        }
        Ok(())
    });
    saved.map_err(|error| reject_edit(&path, &original, &edited, error))
}

/// Undo an editor session whose result the store refused: restore the original
/// item file and keep the rejected text in a temp file named in the error.
fn reject_edit(
    path: &camino::Utf8Path,
    original: &[u8],
    edited: &[u8],
    error: CloveError,
) -> CloveError {
    let kept = tempfile::Builder::new()
        .prefix("clove-edit-")
        .suffix(".md")
        .tempfile()
        .and_then(|mut file| {
            std::io::Write::write_all(&mut file, edited)?;
            file.keep().map_err(|e| e.error)
        })
        .map(|(_, kept)| kept.display().to_string());
    // Only restore once the rejected text is safe somewhere: losing the user's
    // edit would be worse than leaving the invalid file for them to fix.
    let Ok(kept) = kept else {
        return error;
    };
    if std::fs::write(path, original).is_err() {
        return error;
    }
    CloveError::InvalidField {
        field: "edit".to_owned(),
        reason: format!("{error}; the item was left unchanged and your text is in {kept}"),
    }
}

/// Build the command that opens `file` in `editor`. Like git, the editor value
/// may carry arguments (`code --wait`, `emacsclient -t`): on Unix it is run
/// through `sh` with the file passed as `"$@"`, so shell quoting works as it
/// does in git.
#[cfg(unix)]
fn editor_command(editor: &str, file: &str) -> std::process::Command {
    let mut command = std::process::Command::new("sh");
    command
        .arg("-c")
        .arg(format!("{editor} \"$@\""))
        .arg(editor)
        .arg(file);
    command
}

/// Windows has no `sh`: split the editor value on whitespace outside double
/// quotes (`"C:\Program Files\Editor\editor.exe" --wait`).
#[cfg(not(unix))]
fn editor_command(editor: &str, file: &str) -> std::process::Command {
    let mut words = split_editor_words(editor).into_iter();
    let program = words.next().unwrap_or_else(|| editor.to_owned());
    let mut command = std::process::Command::new(program);
    command.args(words).arg(file);
    command
}

#[cfg_attr(unix, allow(dead_code))]
fn split_editor_words(editor: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    let mut in_word = false;
    for character in editor.chars() {
        match character {
            '"' => {
                in_quotes = !in_quotes;
                in_word = true;
            }
            c if c.is_whitespace() && !in_quotes => {
                if in_word {
                    words.push(std::mem::take(&mut current));
                    in_word = false;
                }
            }
            c => {
                current.push(c);
                in_word = true;
            }
        }
    }
    if in_word {
        words.push(current);
    }
    words
}

#[cfg(test)]
mod tests {
    use super::split_editor_words;

    #[test]
    fn split_editor_words_handles_arguments_and_quoted_paths() {
        assert_eq!(split_editor_words("vi"), ["vi"]);
        assert_eq!(split_editor_words("code --wait"), ["code", "--wait"]);
        assert_eq!(
            split_editor_words(r#""C:\Program Files\Ed\ed.exe"  -w"#),
            [r"C:\Program Files\Ed\ed.exe", "-w"]
        );
        assert_eq!(split_editor_words("  "), Vec::<String>::new());
    }
}
