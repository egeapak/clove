//! Two-way GitHub sync: the pure reconciliation engine (T-M06).
//!
//! `clove import github` and `clove export github` are one-way mirrors. This
//! module adds the missing piece — a single reconciled pass (`clove sync github`)
//! that pulls remote changes *and* pushes local changes, detecting the case where
//! the same issue changed on both sides since the last sync and resolving it by a
//! configurable [`ConflictPolicy`] (default: newest edit wins, every conflict
//! reported).
//!
//! ## Layering (mirrors `github.rs`)
//!
//! Everything here is **pure and always compiled** so the whole decision matrix
//! is offline-unit-tested with no network and no token:
//!
//! - [`SyncState`] — the per-repo last-sync fingerprint store (`external_ref →
//!   {gh_updated_at, local_updated}`), persisted as JSON under
//!   `.clove/sync/github/`. This is what makes "changed since last sync"
//!   decidable in *both* directions.
//! - [`plan_sync`] — the reconciliation planner. Given the fetched remote issues,
//!   the local item objects (the `export_object` §7.4 shape), the prior
//!   [`SyncState`], and a [`ConflictPolicy`], it produces a write-free
//!   [`SyncPlan`]. `--dry-run` serializes the plan and stops here.
//!
//! The network apply (octocrab create/update + local writes + state persistence)
//! lives behind the `github` feature in [`crate::sync_net`].
//!
//! ## Decision matrix (per linked issue)
//!
//! With a prior sync recorded, `remote_changed = issue.updated_at >
//! state.gh_updated_at` and `local_changed = item.updated > state.local_updated`:
//!
//! | remote_changed | local_changed | action |
//! |---|---|---|
//! | no  | no  | in sync (skip) |
//! | yes | no  | pull (update local from remote) |
//! | no  | yes | push (update remote from local) |
//! | yes | yes | **conflict** → resolved per [`ConflictPolicy`] |
//!
//! With *no* prior sync for an already-linked pair (e.g. it was imported then
//! exported by the old one-way commands), the planner falls back to a content
//! comparison: identical → in sync; divergent → treated as a both-sides change
//! and routed through the conflict policy. This guarantees a first `sync` never
//! silently clobbers one side.

use std::collections::{HashMap, HashSet};

use camino::{Utf8Path, Utf8PathBuf};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use clove_types::{CloveId, ItemStatus, Priority};

use crate::error::ImportError;
use crate::github::{
    build_export_item, external_ref_for, map_issue, parse_gh_number, ExportItem, GitHubIssue,
    StagedIssue,
};

/// How to resolve an issue that changed on **both** sides since the last sync.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConflictPolicy {
    /// The most recently edited side wins (compare GitHub `updated_at` vs the
    /// local item's `updated`). The default. Every conflict is still reported.
    #[default]
    Newer,
    /// The local item always wins (push local → remote).
    PreferLocal,
    /// The remote issue always wins (pull remote → local).
    PreferRemote,
    /// Apply neither side; report the conflict and leave both untouched.
    Manual,
}

impl ConflictPolicy {
    /// Parse the `--prefer` flag value. Accepts `newer|local|remote|manual`.
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_lowercase().as_str() {
            "newer" | "newest" => Some(Self::Newer),
            "local" | "prefer-local" | "prefer_local" => Some(Self::PreferLocal),
            "remote" | "prefer-remote" | "prefer_remote" => Some(Self::PreferRemote),
            "manual" | "none" | "skip" => Some(Self::Manual),
            _ => None,
        }
    }
}

/// The last-synced fingerprint of one linked issue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncEntry {
    /// The issue's GitHub `updated_at` at the moment of the last successful sync.
    /// `None` when GitHub did not report one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gh_updated_at: Option<DateTime<Utc>>,
    /// The local item's `updated` at the moment of the last successful sync.
    pub local_updated: DateTime<Utc>,
    /// GitHub comment ids already represented locally (pull-dedup; also holds the
    /// ids of comments clove itself posted, so they are never pulled back).
    #[serde(default, skip_serializing_if = "HashSet::is_empty")]
    pub gh_comment_ids: HashSet<u64>,
    /// Body hashes of local comments already on GitHub (push-dedup; also holds the
    /// hashes of pulled-in comments, so they are never pushed back).
    #[serde(default, skip_serializing_if = "HashSet::is_empty")]
    pub local_comment_hashes: HashSet<u64>,
    /// The local item's assignee at the last sync. Lets a push distinguish the
    /// assignee clove "owns" from extra GitHub assignees a human added, so the
    /// push can replace the former without clobbering the latter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub synced_assignee: Option<String>,
}

impl SyncEntry {
    /// A fresh entry with the given fingerprint and no comment bookkeeping yet.
    pub fn new(gh_updated_at: Option<DateTime<Utc>>, local_updated: DateTime<Utc>) -> Self {
        Self {
            gh_updated_at,
            local_updated,
            gh_comment_ids: HashSet::new(),
            local_comment_hashes: HashSet::new(),
            synced_assignee: None,
        }
    }
}

/// The current schema version of the persisted sync-state file.
const SYNC_STATE_VERSION: u32 = 1;

fn default_state_version() -> u32 {
    SYNC_STATE_VERSION
}

/// The per-repo sync-state store: `external_ref → SyncEntry`, persisted as JSON.
///
/// This is *local* bookkeeping (not part of the item schema), so it lives under
/// `.clove/sync/` and is git-ignored — two clones keep their own sync clocks.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SyncState {
    /// File schema version (forward-compat).
    #[serde(default = "default_state_version")]
    pub version: u32,
    /// The `owner/repo` this state belongs to (informational / sanity check).
    #[serde(default)]
    pub repo: String,
    /// `external_ref` (`"gh-<number>"`) → last-sync fingerprint.
    #[serde(default)]
    pub entries: HashMap<String, SyncEntry>,
}

impl SyncState {
    /// An empty state stamped with `repo`.
    pub fn new(repo: &str) -> Self {
        Self {
            version: SYNC_STATE_VERSION,
            repo: repo.to_owned(),
            entries: HashMap::new(),
        }
    }

    /// The on-disk path for a repo's sync state under `repo_root`.
    ///
    /// `owner/repo` is flattened to `owner__repo.json` so it is a single,
    /// filesystem-safe file name (a `/` would otherwise be a subdirectory).
    pub fn path_for(repo_root: &Utf8Path, spec: &str) -> Utf8PathBuf {
        let safe: String = spec
            .trim()
            .chars()
            .map(|c| if c == '/' { '_' } else { c })
            .collect();
        repo_root
            .join(".clove")
            .join("sync")
            .join("github")
            .join(format!("{safe}.json"))
    }

    /// Load the state at `path`. A missing file yields a fresh empty state; a
    /// present-but-unparseable file is also treated as empty (never an error) so
    /// corrupt bookkeeping degrades to "re-examine everything", not a hard stop.
    pub fn load(path: &Utf8Path, repo: &str) -> Self {
        match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_else(|_| SyncState::new(repo)),
            Err(_) => SyncState::new(repo),
        }
    }

    /// Persist the state to `path` (creating parent directories), pretty-printed
    /// and atomically (temp file + rename) so a crash never leaves a half-written
    /// state.
    pub fn save(&self, path: &Utf8Path) -> Result<(), ImportError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| ImportError::Source {
                path: parent.to_owned(),
                message: format!("failed to create sync-state dir: {source}"),
            })?;
        }
        let json = serde_json::to_string_pretty(self).map_err(|err| ImportError::Record {
            message: format!("failed to serialize sync state: {err}"),
        })?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, json.as_bytes()).map_err(|source| ImportError::Source {
            path: tmp.clone(),
            message: format!("failed to write sync state: {source}"),
        })?;
        std::fs::rename(&tmp, path).map_err(|source| ImportError::Source {
            path: path.to_owned(),
            message: format!("failed to commit sync state: {source}"),
        })
    }

    /// Record the fingerprint for `external_ref`, preserving any existing comment
    /// bookkeeping (only the `{gh_updated_at, local_updated}` clock is refreshed).
    pub fn record(
        &mut self,
        external_ref: &str,
        gh_updated_at: Option<DateTime<Utc>>,
        local_updated: DateTime<Utc>,
    ) {
        let entry = self
            .entries
            .entry(external_ref.to_owned())
            .or_insert_with(|| SyncEntry::new(gh_updated_at, local_updated));
        entry.gh_updated_at = gh_updated_at;
        entry.local_updated = local_updated;
    }
}

/// A remote issue to be created as a brand-new local item (pull).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullCreate {
    /// The fully mapped issue (ready to write as an [`clove_types::Item`]).
    pub staged: StagedIssue,
}

/// A remote issue whose changes should overwrite an existing local item (pull).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullUpdate {
    /// The existing local item to update.
    pub clove_id: CloveId,
    /// The mapped remote fields to apply.
    pub staged: StagedIssue,
}

/// A new local item to be created on GitHub (push).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushCreate {
    /// The clove id (for the write-back of the new `external_ref`).
    pub clove_id: String,
    /// The GitHub payload (body already carries the `clove-meta` comment).
    pub item: ExportItem,
    /// The local item's `updated`, recorded as the sync fingerprint after push.
    pub local_updated: DateTime<Utc>,
}

/// A local item whose changes should overwrite an existing GitHub issue (push).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushUpdate {
    /// The target GitHub issue number.
    pub number: u64,
    /// The clove id (for reporting / state).
    pub clove_id: String,
    /// The GitHub payload.
    pub item: ExportItem,
    /// The local item's `updated`, recorded as the sync fingerprint after push.
    pub local_updated: DateTime<Utc>,
    /// The issue's current GitHub assignee logins (so the push can preserve the
    /// extras a human added rather than reset them to clove's single assignee).
    pub gh_assignees: Vec<String>,
    /// The issue's current `state_reason` (preserved when clove pushes a close).
    pub gh_state_reason: Option<String>,
}

/// An issue found already in sync — no action, but its fingerprint is (re)recorded
/// so a first `sync` over a pair the old one-way commands linked still establishes
/// fast-path state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InSyncEntry {
    /// The `external_ref` (`"gh-<number>"`).
    pub external_ref: String,
    /// The remote `updated_at` to record.
    pub gh_updated_at: Option<DateTime<Utc>>,
    /// The local `updated` to record.
    pub local_updated: DateTime<Utc>,
}

/// A reported both-sides conflict and how it was (or was not) resolved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SyncConflict {
    /// The `external_ref` (`"gh-<number>"`) in conflict.
    pub external_ref: String,
    /// The clove id of the local item.
    pub clove_id: String,
    /// The local item title (for a readable report).
    pub title: String,
    /// `"remote_wins"`, `"local_wins"`, or `"skipped"` (manual policy).
    pub resolution: String,
    /// The remote `updated_at` that drove the decision (if known).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_updated: Option<DateTime<Utc>>,
    /// The local `updated` that drove the decision.
    pub local_updated: DateTime<Utc>,
}

/// The write-free reconciliation plan produced by [`plan_sync`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SyncPlan {
    /// New remote issues to create locally.
    pub pull_create: Vec<PullCreate>,
    /// Remote changes to apply to existing local items.
    pub pull_update: Vec<PullUpdate>,
    /// New local items to create on GitHub.
    pub push_create: Vec<PushCreate>,
    /// Local changes to apply to existing GitHub issues.
    pub push_update: Vec<PushUpdate>,
    /// Both-sides conflicts (each also routed into pull/push_update unless the
    /// policy is [`ConflictPolicy::Manual`]).
    pub conflicts: Vec<SyncConflict>,
    /// Issues found already in sync (no action; fingerprint re-recorded).
    pub in_sync: Vec<InSyncEntry>,
    /// `external_ref`s present locally whose GitHub issue was not found (deleted
    /// remotely, or wrong repo) — reported, never auto-applied.
    pub remote_missing: Vec<String>,
}

impl SyncPlan {
    /// Whether the plan would change nothing on either side.
    pub fn is_noop(&self) -> bool {
        self.pull_create.is_empty()
            && self.pull_update.is_empty()
            && self.push_create.is_empty()
            && self.push_update.is_empty()
    }

    /// A serializable, human/JSON-friendly summary (the `--dry-run` payload and
    /// the post-apply report share this shape).
    pub fn summary(&self) -> SyncSummary {
        let entry_pull = |s: &StagedIssue| SyncSummaryItem {
            id: s.source_id.clone(),
            title: s.title.clone(),
        };
        SyncSummary {
            pull_create: self
                .pull_create
                .iter()
                .map(|p| entry_pull(&p.staged))
                .collect(),
            pull_update: self
                .pull_update
                .iter()
                .map(|p| entry_pull(&p.staged))
                .collect(),
            push_create: self
                .push_create
                .iter()
                .map(|p| SyncSummaryItem {
                    id: p.clove_id.clone(),
                    title: p.item.title.clone(),
                })
                .collect(),
            push_update: self
                .push_update
                .iter()
                .map(|p| SyncSummaryItem {
                    id: p.clove_id.clone(),
                    title: p.item.title.clone(),
                })
                .collect(),
            conflicts: self.conflicts.clone(),
            in_sync: self.in_sync.len(),
            remote_missing: self.remote_missing.clone(),
        }
    }
}

/// One entry in a [`SyncSummary`] direction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SyncSummaryItem {
    /// The clove id (push) or `gh-<number>` source id (pull).
    pub id: String,
    /// The item title.
    pub title: String,
}

/// The serializable shape of a [`SyncPlan`] (and the apply report).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct SyncSummary {
    pub pull_create: Vec<SyncSummaryItem>,
    pub pull_update: Vec<SyncSummaryItem>,
    pub push_create: Vec<SyncSummaryItem>,
    pub push_update: Vec<SyncSummaryItem>,
    pub conflicts: Vec<SyncConflict>,
    pub in_sync: usize,
    pub remote_missing: Vec<String>,
}

/// What a sync `apply` run actually pushed/pulled.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct SyncReport {
    /// Local items created from new remote issues.
    pub pulled_created: usize,
    /// Local items updated from remote changes.
    pub pulled_updated: usize,
    /// GitHub issues created from new local items.
    pub pushed_created: usize,
    /// GitHub issues updated from local changes.
    pub pushed_updated: usize,
    /// Both-sides conflicts encountered (reported).
    pub conflicts: usize,
    /// Issues left untouched because they were already in sync.
    pub in_sync: usize,
    /// Local refs whose remote issue was missing.
    pub remote_missing: usize,
    /// GitHub comments pulled into local items.
    pub comments_pulled: usize,
    /// Local comments pushed to GitHub.
    pub comments_pushed: usize,
}

// ---------------------------------------------------------------------------
// Comment reconciliation (pure)
// ---------------------------------------------------------------------------

/// A GitHub issue comment, reduced to what the sync needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GhComment {
    /// The GitHub comment id (the pull-dedup key).
    pub id: u64,
    /// The commenter's login.
    pub author: String,
    /// The comment body.
    pub body: String,
    /// When GitHub recorded the comment.
    pub created_at: Option<DateTime<Utc>>,
}

/// A local sidecar comment, reduced to what the sync needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalComment {
    /// The comment author.
    pub author: String,
    /// The comment body.
    pub body: String,
}

/// Which comments to pull (GitHub → local) and push (local → GitHub).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommentPlan {
    /// GitHub comments to add as local sidecar comments.
    pub pull: Vec<GhComment>,
    /// Local comments to post on the GitHub issue.
    pub push: Vec<LocalComment>,
}

/// A stable, dependency-free hash of a comment body (the push-dedup key). Uses
/// the fixed-seed [`DefaultHasher`](std::collections::hash_map::DefaultHasher) so
/// the value is reproducible across runs and machines. The body is trimmed first
/// so a trailing-newline difference never forks the identity.
pub fn body_hash(body: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    body.trim().hash(&mut hasher);
    hasher.finish()
}

/// Reconcile an issue's GitHub comments against its local sidecar comments,
/// given the prior per-issue bookkeeping in `entry`.
///
/// Dedup is symmetric and stateless-per-comment: a GitHub comment is pulled
/// unless its id was already seen; a local comment is pushed unless its body hash
/// was already seen. Pulling a comment also marks its body hash seen (so the
/// freshly-created local copy is never pushed back), so a clean first pass over a
/// shared thread converges without duplication.
pub fn plan_comments(gh: &[GhComment], local: &[LocalComment], entry: &SyncEntry) -> CommentPlan {
    let mut seen_gh = entry.gh_comment_ids.clone();
    let mut seen_local = entry.local_comment_hashes.clone();
    let mut plan = CommentPlan::default();

    for comment in gh {
        if seen_gh.insert(comment.id) {
            seen_local.insert(body_hash(&comment.body));
            plan.pull.push(comment.clone());
        }
    }
    for comment in local {
        if seen_local.insert(body_hash(&comment.body)) {
            plan.push.push(comment.clone());
        }
    }
    plan
}

/// The Unix epoch as a UTC datetime — the "infinitely old" fallback for a
/// missing timestamp, so an item with no recorded clock always looks unchanged
/// rather than spuriously newer.
fn epoch() -> DateTime<Utc> {
    DateTime::<Utc>::from_timestamp(0, 0).expect("epoch is valid")
}

/// Parse a local item object's `updated` field into a datetime (epoch on miss).
fn obj_updated(obj: &Map<String, Value>) -> DateTime<Utc> {
    obj.get("updated")
        .and_then(Value::as_str)
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&Utc))
        .unwrap_or_else(epoch)
}

/// Whether the remote side changed since `last` was recorded. An unknown current
/// `updated_at` is treated as "changed" (re-examine), as is a missing baseline.
fn remote_changed(current: Option<DateTime<Utc>>, last: Option<DateTime<Utc>>) -> bool {
    match (current, last) {
        (Some(cur), Some(prev)) => cur > prev,
        _ => true,
    }
}

/// Compare a mapped remote issue against the local item object for the fields the
/// sync round-trips. Used only as the no-prior-state fallback. GitHub has no
/// `in_progress`, so a local `in_progress` is considered equal to a remote
/// `open`; only the closed/not-closed distinction matters.
fn content_equal(staged: &StagedIssue, obj: &Map<String, Value>) -> bool {
    let s = |k: &str| obj.get(k).and_then(Value::as_str).unwrap_or_default();

    if staged.title != s("title") {
        return false;
    }
    let local_closed = s("status").eq_ignore_ascii_case("closed");
    let remote_closed = staged.status == ItemStatus::Closed;
    if local_closed != remote_closed {
        return false;
    }
    let local_priority = obj
        .get("priority")
        .and_then(Value::as_u64)
        .map(|p| p as u8)
        .unwrap_or(Priority::DEFAULT.get());
    if staged.priority.get() != local_priority {
        return false;
    }
    let local_assignee = obj
        .get("assignee")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty());
    if staged.assignee.as_deref() != local_assignee {
        return false;
    }
    let mut local_labels: Vec<&str> = obj
        .get("labels")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    local_labels.sort_unstable();
    let staged_labels: Vec<&str> = staged.labels.iter().map(String::as_str).collect();
    if staged_labels != local_labels {
        return false;
    }
    let local_body = s("body").trim();
    staged.body.trim() == local_body
}

/// Reconcile fetched `remote` issues against the local item objects (the
/// `export_object` §7.4 shape) given the prior `state` and a `policy`.
///
/// Pure: no network, no writes. Errors only on a malformed remote issue (a bad
/// `clove-meta` dep id), mirroring the file importers' record-level errors.
pub fn plan_sync(
    remote: &[GitHubIssue],
    local: &[Map<String, Value>],
    state: &SyncState,
    policy: ConflictPolicy,
) -> Result<SyncPlan, String> {
    let mut plan = SyncPlan::default();

    // Index real remote issues (skip PRs) by their external_ref.
    let mut remote_by_ref: HashMap<String, &GitHubIssue> = HashMap::new();
    for issue in remote {
        if issue.pull_request.is_some() {
            continue;
        }
        remote_by_ref.insert(external_ref_for(issue.number), issue);
    }

    let mut matched: HashSet<String> = HashSet::new();

    for obj in local {
        let item = build_export_item(obj);
        let clove_id = item.clove_id.clone();
        let local_updated = obj_updated(obj);
        let external_ref = obj
            .get("external_ref")
            .and_then(Value::as_str)
            .map(str::to_owned);

        match external_ref {
            // Brand-new local item — create it on GitHub.
            None => plan.push_create.push(PushCreate {
                clove_id,
                item,
                local_updated,
            }),
            Some(ext) => match remote_by_ref.get(ext.as_str()) {
                // Linked locally but the remote issue is gone.
                None => plan.remote_missing.push(ext),
                Some(issue) => {
                    matched.insert(ext.clone());
                    let staged = map_issue(issue)?;
                    decide_linked(
                        &mut plan,
                        &ext,
                        &clove_id,
                        obj,
                        issue,
                        staged,
                        item,
                        local_updated,
                        state,
                        policy,
                    )?;
                }
            },
        }
    }

    // Remote issues with no local counterpart — create them locally.
    for issue in remote {
        if issue.pull_request.is_some() {
            continue;
        }
        let ext = external_ref_for(issue.number);
        if matched.contains(&ext) {
            continue;
        }
        let staged = map_issue(issue)?;
        plan.pull_create.push(PullCreate { staged });
    }

    Ok(plan)
}

/// Decide the action for one issue linked to a local item.
#[allow(clippy::too_many_arguments)]
fn decide_linked(
    plan: &mut SyncPlan,
    ext: &str,
    clove_id: &str,
    obj: &Map<String, Value>,
    issue: &GitHubIssue,
    staged: StagedIssue,
    item: ExportItem,
    local_updated: DateTime<Utc>,
    state: &SyncState,
    policy: ConflictPolicy,
) -> Result<(), String> {
    let (remote_chg, local_chg) = match state.entries.get(ext) {
        Some(entry) => (
            remote_changed(issue.updated_at, entry.gh_updated_at),
            local_updated > entry.local_updated,
        ),
        // No prior sync for an already-linked pair: compare content so a first
        // `sync` can never silently clobber a side.
        None => {
            let differ = !content_equal(&staged, obj);
            (differ, differ)
        }
    };

    match (remote_chg, local_chg) {
        (false, false) => plan.in_sync.push(InSyncEntry {
            external_ref: ext.to_owned(),
            gh_updated_at: issue.updated_at,
            local_updated,
        }),
        (true, false) => push_pull_update(plan, clove_id, staged)?,
        (false, true) => push_push_update(plan, ext, clove_id, item, local_updated, issue),
        (true, true) => resolve_conflict(
            plan,
            ext,
            clove_id,
            issue,
            staged,
            item,
            local_updated,
            policy,
        )?,
    }
    Ok(())
}

/// Queue a pull-update (remote → local).
fn push_pull_update(
    plan: &mut SyncPlan,
    clove_id: &str,
    staged: StagedIssue,
) -> Result<(), String> {
    let id = CloveId::new(clove_id).map_err(|e| format!("invalid local id `{clove_id}`: {e}"))?;
    plan.pull_update.push(PullUpdate {
        clove_id: id,
        staged,
    });
    Ok(())
}

/// Queue a push-update (local → remote), capturing the issue's current GitHub
/// assignees and `state_reason` so the apply can preserve them.
fn push_push_update(
    plan: &mut SyncPlan,
    ext: &str,
    clove_id: &str,
    item: ExportItem,
    local_updated: DateTime<Utc>,
    issue: &GitHubIssue,
) {
    // The number is parsed from the external_ref; it is guaranteed `Some` here
    // because the ref matched a fetched issue keyed by `gh-<number>`.
    if let Some(number) = parse_gh_number(ext) {
        plan.push_update.push(PushUpdate {
            number,
            clove_id: clove_id.to_owned(),
            item,
            local_updated,
            gh_assignees: issue
                .assignees
                .iter()
                .map(|u| u.login.clone())
                .filter(|l| !l.trim().is_empty())
                .collect(),
            gh_state_reason: issue.state_reason.clone(),
        });
    }
}

/// Resolve a both-sides conflict per `policy`, recording a [`SyncConflict`] and
/// (unless [`ConflictPolicy::Manual`]) routing the winning side into the plan.
#[allow(clippy::too_many_arguments)]
fn resolve_conflict(
    plan: &mut SyncPlan,
    ext: &str,
    clove_id: &str,
    issue: &GitHubIssue,
    staged: StagedIssue,
    item: ExportItem,
    local_updated: DateTime<Utc>,
    policy: ConflictPolicy,
) -> Result<(), String> {
    let remote_wins = match policy {
        ConflictPolicy::PreferRemote => true,
        ConflictPolicy::PreferLocal => false,
        ConflictPolicy::Manual => {
            plan.conflicts.push(SyncConflict {
                external_ref: ext.to_owned(),
                clove_id: clove_id.to_owned(),
                title: staged.title.clone(),
                resolution: "skipped".to_owned(),
                remote_updated: issue.updated_at,
                local_updated,
            });
            return Ok(());
        }
        // Newest edit wins. A missing remote timestamp can't beat a known local
        // one, so local wins that tie (we only override a side we can date).
        ConflictPolicy::Newer => issue.updated_at.map(|r| r > local_updated).unwrap_or(false),
    };

    plan.conflicts.push(SyncConflict {
        external_ref: ext.to_owned(),
        clove_id: clove_id.to_owned(),
        title: staged.title.clone(),
        resolution: if remote_wins {
            "remote_wins"
        } else {
            "local_wins"
        }
        .to_owned(),
        remote_updated: issue.updated_at,
        local_updated,
    });

    if remote_wins {
        push_pull_update(plan, clove_id, staged)?;
    } else {
        push_push_update(plan, ext, clove_id, item, local_updated, issue);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn issue(number: u64, title: &str, state: &str, updated: &str) -> GitHubIssue {
        GitHubIssue {
            number,
            title: title.to_owned(),
            state: state.to_owned(),
            body: Some("Body text.".to_owned()),
            updated_at: Some(updated.parse().unwrap()),
            ..Default::default()
        }
    }

    fn local(
        id: &str,
        title: &str,
        status: &str,
        external_ref: Option<&str>,
        updated: &str,
    ) -> Map<String, Value> {
        let mut obj = json!({
            "id": id,
            "title": title,
            "status": status,
            "priority": 2,
            "body": "Body text.",
            "labels": [],
            "updated": updated,
        });
        if let Some(r) = external_ref {
            obj["external_ref"] = json!(r);
        }
        obj.as_object().unwrap().clone()
    }

    fn ts(s: &str) -> DateTime<Utc> {
        s.parse().unwrap()
    }

    #[test]
    fn new_local_item_is_push_create() {
        let plan = plan_sync(
            &[],
            &[local(
                "proj-AAAA1111",
                "New",
                "open",
                None,
                "2026-06-01T00:00:00Z",
            )],
            &SyncState::default(),
            ConflictPolicy::Newer,
        )
        .unwrap();
        assert_eq!(plan.push_create.len(), 1);
        assert_eq!(plan.push_create[0].clove_id, "proj-AAAA1111");
        assert!(plan.pull_create.is_empty());
    }

    #[test]
    fn new_remote_issue_is_pull_create() {
        let plan = plan_sync(
            &[issue(7, "Remote", "open", "2026-06-01T00:00:00Z")],
            &[],
            &SyncState::default(),
            ConflictPolicy::Newer,
        )
        .unwrap();
        assert_eq!(plan.pull_create.len(), 1);
        assert_eq!(plan.pull_create[0].staged.external_ref, "gh-7");
    }

    #[test]
    fn pull_requests_are_ignored() {
        let mut pr = issue(9, "a PR", "open", "2026-06-01T00:00:00Z");
        pr.pull_request = Some(json!({"url": "x"}));
        let plan = plan_sync(&[pr], &[], &SyncState::default(), ConflictPolicy::Newer).unwrap();
        assert!(plan.pull_create.is_empty());
    }

    #[test]
    fn only_remote_changed_pulls() {
        // Last sync at T0; remote updated at T1 (> T0), local unchanged at T0.
        let mut state = SyncState::default();
        state.record(
            "gh-7",
            Some(ts("2026-06-01T00:00:00Z")),
            ts("2026-06-01T00:00:00Z"),
        );
        let plan = plan_sync(
            &[issue(7, "Remote edited", "open", "2026-06-02T00:00:00Z")],
            &[local(
                "proj-AAAA1111",
                "Old",
                "open",
                Some("gh-7"),
                "2026-06-01T00:00:00Z",
            )],
            &state,
            ConflictPolicy::Newer,
        )
        .unwrap();
        assert_eq!(plan.pull_update.len(), 1);
        assert!(plan.push_update.is_empty());
        assert!(plan.conflicts.is_empty());
    }

    #[test]
    fn only_local_changed_pushes() {
        let mut state = SyncState::default();
        state.record(
            "gh-7",
            Some(ts("2026-06-02T00:00:00Z")),
            ts("2026-06-01T00:00:00Z"),
        );
        let plan = plan_sync(
            &[issue(7, "Remote", "open", "2026-06-02T00:00:00Z")],
            &[local(
                "proj-AAAA1111",
                "Local edited",
                "open",
                Some("gh-7"),
                "2026-06-03T00:00:00Z",
            )],
            &state,
            ConflictPolicy::Newer,
        )
        .unwrap();
        assert_eq!(plan.push_update.len(), 1);
        assert_eq!(plan.push_update[0].number, 7);
        assert!(plan.pull_update.is_empty());
    }

    #[test]
    fn unchanged_both_sides_is_in_sync() {
        let mut state = SyncState::default();
        state.record(
            "gh-7",
            Some(ts("2026-06-02T00:00:00Z")),
            ts("2026-06-01T00:00:00Z"),
        );
        let plan = plan_sync(
            &[issue(7, "Remote", "open", "2026-06-02T00:00:00Z")],
            &[local(
                "proj-AAAA1111",
                "Local",
                "open",
                Some("gh-7"),
                "2026-06-01T00:00:00Z",
            )],
            &state,
            ConflictPolicy::Newer,
        )
        .unwrap();
        assert_eq!(plan.in_sync.len(), 1);
        assert_eq!(plan.in_sync[0].external_ref, "gh-7");
        assert!(plan.is_noop());
    }

    #[test]
    fn both_changed_newer_remote_wins() {
        let mut state = SyncState::default();
        state.record(
            "gh-7",
            Some(ts("2026-06-01T00:00:00Z")),
            ts("2026-06-01T00:00:00Z"),
        );
        // Remote updated 06-04 (newest), local updated 06-03.
        let plan = plan_sync(
            &[issue(7, "Remote newer", "open", "2026-06-04T00:00:00Z")],
            &[local(
                "proj-AAAA1111",
                "Local older",
                "open",
                Some("gh-7"),
                "2026-06-03T00:00:00Z",
            )],
            &state,
            ConflictPolicy::Newer,
        )
        .unwrap();
        assert_eq!(plan.conflicts.len(), 1);
        assert_eq!(plan.conflicts[0].resolution, "remote_wins");
        assert_eq!(plan.pull_update.len(), 1, "remote winner routed to pull");
        assert!(plan.push_update.is_empty());
    }

    #[test]
    fn both_changed_newer_local_wins() {
        let mut state = SyncState::default();
        state.record(
            "gh-7",
            Some(ts("2026-06-01T00:00:00Z")),
            ts("2026-06-01T00:00:00Z"),
        );
        let plan = plan_sync(
            &[issue(7, "Remote older", "open", "2026-06-03T00:00:00Z")],
            &[local(
                "proj-AAAA1111",
                "Local newer",
                "open",
                Some("gh-7"),
                "2026-06-04T00:00:00Z",
            )],
            &state,
            ConflictPolicy::Newer,
        )
        .unwrap();
        assert_eq!(plan.conflicts[0].resolution, "local_wins");
        assert_eq!(plan.push_update.len(), 1);
        assert!(plan.pull_update.is_empty());
    }

    #[test]
    fn manual_policy_skips_both_sides() {
        let mut state = SyncState::default();
        state.record(
            "gh-7",
            Some(ts("2026-06-01T00:00:00Z")),
            ts("2026-06-01T00:00:00Z"),
        );
        let plan = plan_sync(
            &[issue(7, "Remote", "open", "2026-06-04T00:00:00Z")],
            &[local(
                "proj-AAAA1111",
                "Local",
                "open",
                Some("gh-7"),
                "2026-06-03T00:00:00Z",
            )],
            &state,
            ConflictPolicy::Manual,
        )
        .unwrap();
        assert_eq!(plan.conflicts.len(), 1);
        assert_eq!(plan.conflicts[0].resolution, "skipped");
        assert!(plan.pull_update.is_empty() && plan.push_update.is_empty());
    }

    #[test]
    fn prefer_local_and_remote_force_a_side() {
        let mut state = SyncState::default();
        state.record(
            "gh-7",
            Some(ts("2026-06-01T00:00:00Z")),
            ts("2026-06-01T00:00:00Z"),
        );
        let remote = [issue(7, "R", "open", "2026-06-04T00:00:00Z")];
        let loc = [local(
            "proj-AAAA1111",
            "L",
            "open",
            Some("gh-7"),
            "2026-06-03T00:00:00Z",
        )];

        let pl = plan_sync(&remote, &loc, &state, ConflictPolicy::PreferLocal).unwrap();
        assert_eq!(pl.conflicts[0].resolution, "local_wins");
        assert_eq!(pl.push_update.len(), 1);

        let pr = plan_sync(&remote, &loc, &state, ConflictPolicy::PreferRemote).unwrap();
        assert_eq!(pr.conflicts[0].resolution, "remote_wins");
        assert_eq!(pr.pull_update.len(), 1);
    }

    #[test]
    fn no_state_identical_content_is_in_sync() {
        // Linked pair, no recorded state, identical fields → in sync, no clobber.
        let plan = plan_sync(
            &[issue(7, "Same", "open", "2026-06-04T00:00:00Z")],
            &[local(
                "proj-AAAA1111",
                "Same",
                "open",
                Some("gh-7"),
                "2026-06-03T00:00:00Z",
            )],
            &SyncState::default(),
            ConflictPolicy::Newer,
        )
        .unwrap();
        assert_eq!(plan.in_sync.len(), 1);
        assert_eq!(plan.in_sync[0].external_ref, "gh-7");
    }

    #[test]
    fn no_state_divergent_content_is_conflict() {
        let plan = plan_sync(
            &[issue(7, "Remote title", "open", "2026-06-04T00:00:00Z")],
            &[local(
                "proj-AAAA1111",
                "Local title",
                "open",
                Some("gh-7"),
                "2026-06-03T00:00:00Z",
            )],
            &SyncState::default(),
            ConflictPolicy::Newer,
        )
        .unwrap();
        assert_eq!(plan.conflicts.len(), 1, "divergent + no state → conflict");
        assert_eq!(plan.conflicts[0].resolution, "remote_wins");
    }

    #[test]
    fn local_ref_with_missing_remote_is_reported() {
        let plan = plan_sync(
            &[],
            &[local(
                "proj-AAAA1111",
                "Orphan",
                "open",
                Some("gh-99"),
                "2026-06-03T00:00:00Z",
            )],
            &SyncState::default(),
            ConflictPolicy::Newer,
        )
        .unwrap();
        assert_eq!(plan.remote_missing, vec!["gh-99".to_owned()]);
        assert!(plan.is_noop());
    }

    #[test]
    fn in_progress_local_equals_open_remote() {
        // GitHub has no in_progress; a local in_progress must not flap against a
        // remote open when no other field differs.
        let plan = plan_sync(
            &[issue(7, "Same", "open", "2026-06-04T00:00:00Z")],
            &[local(
                "proj-AAAA1111",
                "Same",
                "in_progress",
                Some("gh-7"),
                "2026-06-03T00:00:00Z",
            )],
            &SyncState::default(),
            ConflictPolicy::Newer,
        )
        .unwrap();
        assert_eq!(plan.in_sync.len(), 1);
        assert_eq!(plan.in_sync[0].external_ref, "gh-7");
    }

    #[test]
    fn policy_parse_accepts_aliases() {
        assert_eq!(ConflictPolicy::parse("newer"), Some(ConflictPolicy::Newer));
        assert_eq!(
            ConflictPolicy::parse("LOCAL"),
            Some(ConflictPolicy::PreferLocal)
        );
        assert_eq!(
            ConflictPolicy::parse("remote"),
            Some(ConflictPolicy::PreferRemote)
        );
        assert_eq!(
            ConflictPolicy::parse("manual"),
            Some(ConflictPolicy::Manual)
        );
        assert_eq!(ConflictPolicy::parse("bogus"), None);
    }

    #[test]
    fn state_round_trips_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let root = camino::Utf8Path::from_path(dir.path()).unwrap();
        let path = SyncState::path_for(root, "owner/repo");
        assert!(path.as_str().ends_with("owner_repo.json"));

        let mut state = SyncState::new("owner/repo");
        state.record(
            "gh-7",
            Some(ts("2026-06-04T00:00:00Z")),
            ts("2026-06-03T00:00:00Z"),
        );
        state.save(&path).unwrap();

        let loaded = SyncState::load(&path, "owner/repo");
        assert_eq!(loaded.entries.len(), 1);
        assert_eq!(
            loaded.entries["gh-7"].local_updated,
            ts("2026-06-03T00:00:00Z")
        );
    }

    #[test]
    fn load_missing_or_corrupt_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let root = camino::Utf8Path::from_path(dir.path()).unwrap();
        let path = SyncState::path_for(root, "o/r");
        // Missing.
        assert!(SyncState::load(&path, "o/r").entries.is_empty());
        // Corrupt.
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{not json").unwrap();
        assert!(SyncState::load(&path, "o/r").entries.is_empty());
    }

    fn gh_comment(id: u64, body: &str) -> GhComment {
        GhComment {
            id,
            author: "octocat".to_owned(),
            body: body.to_owned(),
            created_at: Some(ts("2026-06-01T00:00:00Z")),
        }
    }

    fn local_comment(body: &str) -> LocalComment {
        LocalComment {
            author: "tester".to_owned(),
            body: body.to_owned(),
        }
    }

    #[test]
    fn first_comment_sync_pulls_remote_and_pushes_local() {
        let entry = SyncEntry::new(None, ts("2026-06-01T00:00:00Z"));
        let plan = plan_comments(
            &[gh_comment(10, "from github")],
            &[local_comment("from clove")],
            &entry,
        );
        assert_eq!(plan.pull.len(), 1);
        assert_eq!(plan.pull[0].id, 10);
        assert_eq!(plan.push.len(), 1);
        assert_eq!(plan.push[0].body, "from clove");
    }

    #[test]
    fn already_synced_comments_are_skipped() {
        let mut entry = SyncEntry::new(None, ts("2026-06-01T00:00:00Z"));
        entry.gh_comment_ids.insert(10);
        entry.local_comment_hashes.insert(body_hash("from clove"));
        let plan = plan_comments(
            &[gh_comment(10, "from github")],
            &[local_comment("from clove")],
            &entry,
        );
        assert!(plan.pull.is_empty() && plan.push.is_empty());
    }

    #[test]
    fn pulled_comment_is_not_pushed_back() {
        // A GitHub comment whose body matches a not-yet-seen local comment is
        // pulled; the identical local one must NOT then be pushed (no echo).
        let entry = SyncEntry::new(None, ts("2026-06-01T00:00:00Z"));
        let plan = plan_comments(
            &[gh_comment(10, "same text")],
            &[local_comment("same text")],
            &entry,
        );
        assert_eq!(plan.pull.len(), 1);
        assert!(plan.push.is_empty(), "identical body must not echo back");
    }

    #[test]
    fn body_hash_ignores_trailing_whitespace() {
        assert_eq!(body_hash("hello"), body_hash("hello\n"));
        assert_ne!(body_hash("hello"), body_hash("world"));
    }
}
