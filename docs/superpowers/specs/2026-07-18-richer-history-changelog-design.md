# Richer history / changelog — design (gh-29)

**Status:** design only (implementation deferred). Proposes a per-item **change
history** ("what changed, when, by whom") beyond today's `created`/`updated`
timestamps and append-only comments.

## Problem

clove records *state*, not *transitions*. You can see an item is `closed` and
was `updated: 2026-07-18`, but not *when* it moved open→in_progress→closed, when
its priority was bumped, or who reassigned it. Comments are the only append-only
record, and they are manual. `clove stats --history` captures *aggregate*
snapshots but nothing per-item. This item adds a per-item timeline of field
changes, surfaced in the CLI, web detail, and TUI.

## Two approaches

### A. Derive from git history (recommended for v1)

Item files are the source of truth and already versioned in git. Reconstruct
history by walking `git log -p --follow -- .clove/issues/<id>.md`: for each
commit that touched the file, diff the frontmatter between parent and commit and
emit a `Change { at, author, field, from, to }` per changed field.

- **Pros:** zero new state, zero new files, no write-path change, works
  retroactively on all existing history, author/timestamp come free from the
  commit. Fully aligned with "files are truth, they travel with the repo."
- **Cons:** only as granular as commits (several edits in one commit collapse to
  one entry); requires the repo to be a git repo (clove doesn't strictly require
  git); parsing two frontmatter versions per commit is O(commits for the file) —
  fine per-item, not for a whole-store scan. Two limitations to state up front:
  (a) **uncommitted working-tree edits are invisible** — a change shows up only
  once committed, so `updated:` can lead the newest history entry; (b) **rename
  follow is not free in libgit2** (`--follow` is a git-CLI feature, not a git2
  API — it needs manual diff-rename detection when walking), but this is **moot in
  practice** since item files are id-named `<id>.md` and never renamed, so the walk
  can drop `--follow` entirely.
- **Author quality:** commit author is the git identity. Note this differs from
  the gh-26 comment-author chain (which resolves `CLOVE_AUTHOR`→`GIT_AUTHOR_EMAIL`
  →git config→`$USER`) and the commit author isn't necessarily the field-editor
  (batch/bot commits) — directionally aligned, not identical.

### B. Append-only events log

On every write, append a structured event to
`.clove/issues/<id>/history.jsonl` (sibling to `comments/`): `{ts, author,
field, from, to}`. The unified write path (`apply_edit`, `ops::*`) is the single
choke point, so one hook covers every surface (CLI/web/TUI/MCP/daemon).

- **Pros:** exact per-edit granularity; independent of git; queryable without
  shelling out; captures the *intent* of each op (e.g. `dep_add`, `set_parent`)
  not just the field diff.
- **Cons:** new on-disk state that must stay consistent with the item file (a
  hand-edit to the `.md` bypasses it), a new gitignore/merge consideration, and
  it grows unbounded. It also duplicates what git already stores.

### Recommendation

**Ship A (git-derived) first** — it delivers the feature with no new state and no
write-path risk, and it matches clove's "files + git are the truth" ethos. Keep
B in reserve for if/when sub-commit granularity or git-independence becomes a
real need; the two are not exclusive (B could later enrich A). The rest of this
spec designs A.

## Design (git-derived)

New pure module `clove_core::history`. **Git access: prefer shelling out to the
`git` CLI, matching clove-core's existing precedent** — `repo.rs` deliberately
does *not* use `git2`/libgit2 in core because vendored libgit2 blocks the
macOS→Windows cross-compile, so worktree detection already shells out to `git`.
`git2` is isolated to `cloved` under its optional `git-sync` feature; putting it
in core/CLI would pull libgit2 into that build for the first time. (A `clove-core
history` *cargo feature* with its own optional `git2` dep would be the only valid
way to use git2 here — cloved's feature can't gate a clove-core module — but the
CLI-shell approach is lighter and also matches the `git log -p` phrasing below.)
The module degrades gracefully when git is absent:

```rust
pub struct Change {
    pub at: DateTime<Utc>,
    pub author: String,          // git commit author
    pub commit: String,          // short sha, for reference
    pub kind: ChangeKind,        // Created | FieldSet | StatusChange | …
    pub field: String,           // "status", "priority", "assignee", "labels", …
    pub from: Option<String>,
    pub to: Option<String>,
}

/// Walk the file's git log newest→oldest, diff frontmatter per commit.
pub fn item_history(repo_root: &Utf8Path, id: &CloveId)
    -> Result<Vec<Change>, HistoryError>;
```

- Diff scope: the **frontmatter scalar/label fields** (status, type, priority,
  assignee, labels, parent, deps, title). Body changes collapse to a single
  "edited description" entry (no line-diff in v1).
- Graceful degradation: not a git repo, or the file has no history →
  `Ok(vec![])` plus the synthetic `Created` entry from the frontmatter
  `created`/`author`, so the feature never errors.
- Performance: per-item only (never whole-store); bounded by that file's commit
  count. `--limit N` caps it.

## Surfaces

- **CLI:** `clove history <id> [--limit N] [--format json]` → a reverse-chron
  table (`when · who · change`) and a JSON envelope (new
  `docs/json-schema/v1/history.json`, extending the gh-20 schema set).
- **Web:** a **History** tab on the item detail page (beside Overview / Dep tree /
  Comments), served by `GET /api/v1/items/:id/history`. Renders a vertical
  timeline; reuses the detail-pane tab machinery.
- **TUI:** a fourth detail sub-view (`H` = history — **not** `h`, which is already
  bound to list-pane focus in `lib.rs`) alongside `o`/`t`/`c`,
  rendered like the comments list.
- **agent-doc / MCP:** a `clove_history` read tool (optional follow-up) so agents
  can ask "how did this get here?".

## Testing

- Unit: build a temp git repo, commit an item through several edits, assert
  `item_history` reconstructs the expected `Change` sequence (status open→closed,
  priority bump, label add/remove); assert graceful empty history for a
  non-git-repo store.
- Schema round-trip test for `history.json` (mirrors gh-20).
- Web integration + a TUI snapshot for the new sub-view.

## Non-goals

Body line-level diffs, cross-item "what changed in the repo this week" (that is
what `stats --history` + the timeline already cover in aggregate), and blame-style
attribution beyond the git commit author.
