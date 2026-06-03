//! `clove merge-driver <ancestor> <ours> <theirs> <marker-size>` (T-M05).
//!
//! Invoked by git via the `[merge "clove-item"]` driver that `clove init
//! --merge-driver` installs (`driver = clove merge-driver %O %A %B %L`). git
//! hands us three temp file paths (`%O` ancestor / merge base, `%A` ours, `%B`
//! theirs) and the conflict marker size (`%L`), and reads the merged result back
//! from the `%A` path. The git contract is: **exit 0 = clean** (git stages the
//! file), **nonzero = conflict** (git leaves the path conflicted with whatever
//! we wrote).
//!
//! Algorithm (DESIGN.md §9.2):
//! 1. Read all three files (ancestor may be empty/absent → add/add, base=None).
//! 2. Parse each into [`ItemFrontmatter`] + body, *leniently* (git temp paths do
//!    not match the item id). If `ours` or `theirs` fails to parse as a clove
//!    item, do **not** clobber `ours` — exit nonzero so git falls back to its
//!    default conflict behavior.
//! 3. Merge the frontmatter via [`clove_import::merge::merge_frontmatter`].
//! 4. Merge the body by shelling out to `git merge-file`.
//! 5. Write the merged frontmatter (canonical [`FrontmatterWriter`]) + merged
//!    body to the `%A` path. On any field/body conflict embed git-style markers
//!    and exit nonzero.

use std::io::Write as _;
use std::process::Command;

use camino::{Utf8Path, Utf8PathBuf};
use clove_core::write::FrontmatterWriter;
use clove_core::{parse_item_lenient, CloveError, Item, ItemFrontmatter, OutputFormat};
use clove_import::merge::{merge_frontmatter, FieldConflict, MergeOutcome};
use serde_json::json;

use crate::cli::MergeDriverArgs;
use crate::exit::ExitCode;
use crate::output::print_json_success;

/// Run the merge driver. Returns the process exit code: [`ExitCode::Success`]
/// (0) on a fully clean merge, [`ExitCode::Usage`] (1) on any conflict — the
/// git merge-driver "unmerged" signal. `Err` is reserved for catastrophic I/O
/// (git itself missing, the ours path unwritable).
pub fn run(format: OutputFormat, args: MergeDriverArgs) -> Result<ExitCode, CloveError> {
    let marker_size = args.marker_size.max(7);

    // Read raw bytes for all three sides. Ancestor may be absent/empty.
    let ours_bytes = read_file(&args.ours)?;
    let theirs_bytes = read_opt(&args.theirs)?;
    let base_bytes = read_opt(&args.ancestor)?;

    // Parse ours/theirs leniently. If either is unparseable as a clove item, we
    // must not clobber ours: leave it untouched and report a conflict so git
    // falls back to its default behavior.
    let ours_item = match parse_item_lenient(&ours_bytes, &args.ours) {
        Ok(item) => item,
        Err(_) => {
            return finish(
                format,
                ExitCode::Usage,
                &args.ours,
                false,
                "unparseable-ours",
            )
        }
    };
    let theirs_item = match theirs_bytes
        .as_ref()
        .map(|b| parse_item_lenient(b, &args.theirs))
    {
        Some(Ok(item)) => item,
        // Missing/unparseable theirs → cannot semantic-merge; leave ours, conflict.
        Some(Err(_)) | None => {
            return finish(
                format,
                ExitCode::Usage,
                &args.ours,
                false,
                "unparseable-theirs",
            )
        }
    };
    // Base is optional (add/add). A present-but-unparseable base is treated as
    // absent (best effort: still try a 2-way-ish merge against no ancestor).
    let base_item: Option<Item> = base_bytes
        .as_ref()
        .filter(|b| !b.is_empty())
        .and_then(|b| parse_item_lenient(b, &args.ancestor).ok());

    let base_fm: Option<&ItemFrontmatter> = base_item.as_ref().map(|i| &i.frontmatter);
    let outcome = merge_frontmatter(base_fm, &ours_item.frontmatter, &theirs_item.frontmatter);

    // Merge the body via `git merge-file`.
    let body_merge = merge_body(
        base_item.as_ref().map(|i| i.body.as_str()),
        &ours_item.body,
        &theirs_item.body,
        marker_size,
    )?;

    let (merged_fm, field_conflicts) = match &outcome {
        MergeOutcome::Clean(fm) => (fm.as_ref(), Vec::new()),
        MergeOutcome::Conflict { merged, conflicts } => (merged.as_ref(), conflicts.clone()),
    };

    let conflicted = !outcome.is_clean() || body_merge.conflicted;

    // Serialize canonical frontmatter, then append conflict-marked field blocks
    // (if any) and the (possibly conflict-marked) body.
    let mut out: Vec<u8> = Vec::new();
    {
        let mut writer = FrontmatterWriter::new(&mut out);
        writer
            .write_item(merged_fm)
            .map_err(|source| CloveError::Io {
                path: args.ours.clone(),
                source,
            })?;
    }
    if !field_conflicts.is_empty() {
        append_field_conflict_block(&mut out, &field_conflicts, marker_size);
    }
    out.extend_from_slice(body_merge.text.as_bytes());

    // Write the merged result to the %A (ours) path. Plain overwrite (git owns
    // this temp/worktree file) — not the atomic sibling-rename path.
    std::fs::write(args.ours.as_std_path(), &out).map_err(|source| CloveError::Io {
        path: args.ours.clone(),
        source,
    })?;

    let code = if conflicted {
        ExitCode::Usage
    } else {
        ExitCode::Success
    };
    finish(format, code, &args.ours, !conflicted, "merged")
}

/// Emit an optional JSON envelope (humans/tests may pass `--format json`; git
/// never does) and return the resolved exit code.
fn finish(
    format: OutputFormat,
    code: ExitCode,
    ours: &Utf8Path,
    clean: bool,
    note: &str,
) -> Result<ExitCode, CloveError> {
    if matches!(format, OutputFormat::Json | OutputFormat::Jsonl) {
        print_json_success(
            json!({
                "ours": ours.as_str(),
                "clean": clean,
                "conflict": !clean,
                "note": note,
            }),
            json!({ "warnings": [] }),
        );
    }
    Ok(code)
}

/// The result of a body three-way merge.
struct BodyMerge {
    text: String,
    conflicted: bool,
}

/// Three-way merge the body by delegating to the `git merge-file` binary.
///
/// Invocation:
/// `git merge-file -p -L ours -L base -L theirs --marker-size=<L> <ours> <base> <theirs>`
/// — `-p` writes the merged result to stdout (we capture it), the three `-L`
/// labels name the conflict-marker sides, and a nonzero exit means the body had
/// conflicts (git embeds the markers in the captured output).
fn merge_body(
    base: Option<&str>,
    ours: &str,
    theirs: &str,
    marker_size: usize,
) -> Result<BodyMerge, CloveError> {
    let dir = tempfile::tempdir().map_err(|source| CloveError::Io {
        path: Utf8PathBuf::from("<tmp>"),
        source,
    })?;
    let dir_path = Utf8Path::from_path(dir.path()).ok_or_else(|| CloveError::Io {
        path: Utf8PathBuf::from("<tmp>"),
        source: std::io::Error::new(std::io::ErrorKind::InvalidInput, "non-utf8 temp dir"),
    })?;

    let write_part = |name: &str, contents: &str| -> Result<Utf8PathBuf, CloveError> {
        let p = dir_path.join(name);
        let mut f = std::fs::File::create(p.as_std_path()).map_err(|source| CloveError::Io {
            path: p.clone(),
            source,
        })?;
        f.write_all(contents.as_bytes())
            .map_err(|source| CloveError::Io {
                path: p.clone(),
                source,
            })?;
        Ok(p)
    };

    let ours_path = write_part("ours", ours)?;
    let base_path = write_part("base", base.unwrap_or(""))?;
    let theirs_path = write_part("theirs", theirs)?;

    let output = Command::new("git")
        .arg("merge-file")
        .arg("-p")
        .arg("-L")
        .arg("ours")
        .arg("-L")
        .arg("base")
        .arg("-L")
        .arg("theirs")
        .arg(format!("--marker-size={marker_size}"))
        .arg(ours_path.as_str())
        .arg(base_path.as_str())
        .arg(theirs_path.as_str())
        .output()
        .map_err(|source| CloveError::Io {
            path: Utf8PathBuf::from("git"),
            source,
        })?;

    // `git merge-file` exit code: 0 = clean, N>0 = N conflicts, <0 = error.
    let status = output.status.code().unwrap_or(-1);
    if status < 0 {
        return Err(CloveError::Io {
            path: Utf8PathBuf::from("git merge-file"),
            source: std::io::Error::other(format!(
                "git merge-file failed: {}",
                String::from_utf8_lossy(&output.stderr)
            )),
        });
    }
    let text = String::from_utf8_lossy(&output.stdout).into_owned();
    Ok(BodyMerge {
        text,
        conflicted: status > 0,
    })
}

/// Append a human-resolvable conflict block describing every conflicting
/// frontmatter field, fenced with git-style markers sized per `marker_size`.
/// The canonical frontmatter (written above this block) already holds our value
/// for each conflicting field, so the file still round-trips for tooling; this
/// block makes the divergence explicit for a human to resolve.
fn append_field_conflict_block(out: &mut Vec<u8>, conflicts: &[FieldConflict], marker_size: usize) {
    let lt = "<".repeat(marker_size);
    let eq = "=".repeat(marker_size);
    let gt = ">".repeat(marker_size);

    let mut block = String::new();
    block.push_str("\n<!-- clove: frontmatter merge conflict, resolve and remove this block\n");
    for c in conflicts {
        block.push_str(&format!("{lt} ours\n"));
        block.push_str(&format!("{}: {}\n", c.field, c.ours));
        block.push_str(&format!("{eq}\n"));
        block.push_str(&format!("{}: {}\n", c.field, c.theirs));
        block.push_str(&format!("{gt} theirs\n"));
    }
    block.push_str("-->\n");
    out.extend_from_slice(block.as_bytes());
}

/// Read a required file's bytes.
fn read_file(path: &Utf8Path) -> Result<Vec<u8>, CloveError> {
    std::fs::read(path.as_std_path()).map_err(|source| CloveError::Io {
        path: path.to_owned(),
        source,
    })
}

/// Read an optional file: missing → `None`, present → its bytes.
fn read_opt(path: &Utf8Path) -> Result<Option<Vec<u8>>, CloveError> {
    match std::fs::read(path.as_std_path()) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(CloveError::Io {
            path: path.to_owned(),
            source,
        }),
    }
}
