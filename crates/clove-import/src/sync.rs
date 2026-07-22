//! Two-way GitHub sync: the pure reconciliation engine (T-M06).
//!
//! `clove sync github` is the single GitHub path: one reconciled pass that pulls
//! remote changes *and* pushes local changes, detecting the case where the same
//! issue changed on both sides since the last sync and resolving it by a
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

/// Which side(s) of a reconcile to apply (`PLUGIN_SYSTEM.md` §4.2 composition
/// model). The full two-way `clove sync github` uses [`Direction::Both`]; the
/// one-way views expose the same reconcile planner through a single binary:
/// `clove import github` is [`Direction::PullOnly`] and `clove export github` is
/// [`Direction::PushOnly`].
///
/// A direction gates only the *apply* of the item plan (via
/// [`SyncPlan::restrict_to`]): the plan is computed identically, then the
/// irrelevant side is dropped. Item fingerprints stay accurate because each side
/// is recorded only when its action is applied. Comment sync is bidirectional and
/// its fingerprints are coupled to the skip optimization, so it runs only under
/// [`Direction::Both`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Direction {
    /// Two-way reconcile (pull + push). `clove sync`.
    #[default]
    Both,
    /// Remote → local only (create/update local items). `clove import`.
    PullOnly,
    /// Local → remote only (create/update GitHub issues). `clove export`.
    PushOnly,
}

impl Direction {
    /// Whether this direction applies the pull (remote → local) side.
    pub fn pulls(self) -> bool {
        matches!(self, Direction::Both | Direction::PullOnly)
    }

    /// Whether this direction applies the push (local → remote) side.
    pub fn pushes(self) -> bool {
        matches!(self, Direction::Both | Direction::PushOnly)
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
    ///
    /// Legacy representation: a bare set cannot count *occurrences*, so it is
    /// superseded by [`SyncEntry::local_comment_hash_counts`] (a set-only hash
    /// counts as one occurrence). Still written for forward/backward state-file
    /// compatibility.
    #[serde(default, skip_serializing_if = "HashSet::is_empty")]
    pub local_comment_hashes: HashSet<u64>,
    /// Occurrence counts of local comment bodies already represented on GitHub,
    /// keyed by [`body_hash`]. Authoritative when a hash is present; see
    /// [`SyncEntry::comment_hash_counts`]. Without counts, a user commenting
    /// the same text twice (months apart) would have the second comment
    /// silently dropped from the push plan forever.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub local_comment_hash_counts: HashMap<u64, u32>,
    /// The local item's assignee at the last sync. Lets a push distinguish the
    /// assignee clove "owns" from extra GitHub assignees a human added, so the
    /// push can replace the former without clobbering the latter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub synced_assignee: Option<String>,
    /// The remote comment count and the local comment count at the end of the
    /// last comment sync. When BOTH still match, the per-issue comments GET is
    /// skipped — on a repo with many linked-but-idle issues this removes
    /// nearly all per-sync API calls. `None` (pre-tracking state files, or an
    /// issue whose comment sync errored mid-way) always re-fetches.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub synced_gh_comments: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub synced_local_comments: Option<u64>,
    /// Fingerprint of the remote issue's *synced content* at the last sync
    /// (see [`staged_fingerprint`]). With it, "remote changed" requires the
    /// mapped content to actually differ, not just `updated_at` to move — a
    /// third-party comment/reaction bumps the clock without changing any
    /// field, and treating that as a change (combined with a real local edit)
    /// used to surface a conflict whose `Newer` resolution reverted the local
    /// edit with stale remote content. `None` (pre-fingerprint state files)
    /// falls back to clock-only detection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gh_content_hash: Option<u64>,
}

impl SyncEntry {
    /// A fresh entry with the given fingerprint and no comment bookkeeping yet.
    pub fn new(gh_updated_at: Option<DateTime<Utc>>, local_updated: DateTime<Utc>) -> Self {
        Self {
            gh_updated_at,
            local_updated,
            gh_comment_ids: HashSet::new(),
            local_comment_hashes: HashSet::new(),
            local_comment_hash_counts: HashMap::new(),
            synced_gh_comments: None,
            synced_local_comments: None,
            synced_assignee: None,
            gh_content_hash: None,
        }
    }

    /// Effective synced-occurrence count per comment body hash: the count map,
    /// seeded with one occurrence for every legacy set-only hash (state files
    /// written before the multiset existed).
    pub fn comment_hash_counts(&self) -> HashMap<u64, u32> {
        let mut counts = self.local_comment_hash_counts.clone();
        for hash in &self.local_comment_hashes {
            counts.entry(*hash).or_insert(1);
        }
        counts
    }

    /// Record one more synced occurrence of a comment body hash (a pull that
    /// created the local copy, or a push that created the GitHub copy).
    pub fn record_comment_hash(&mut self, hash: u64) {
        // Materialize the legacy-set migration first so the increment composes
        // with pre-multiset bookkeeping; keep the set updated too so an older
        // binary reading this state file still dedups (at set precision).
        self.local_comment_hash_counts = self.comment_hash_counts();
        *self.local_comment_hash_counts.entry(hash).or_insert(0) += 1;
        self.local_comment_hashes.insert(hash);
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

    /// Record the last-synced remote *content* fingerprint for `external_ref`
    /// (see [`SyncEntry::gh_content_hash`]). Pair with [`SyncState::record`],
    /// which refreshes the clocks and creates the entry.
    pub fn record_content_hash(&mut self, external_ref: &str, hash: u64) {
        if let Some(entry) = self.entries.get_mut(external_ref) {
            entry.gh_content_hash = Some(hash);
        }
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
    /// The remote content fingerprint to record ([`staged_fingerprint`]).
    pub gh_content_hash: u64,
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
    /// `external_ref`s owned by a *different* external system (`tk:…`,
    /// `beads:…`) — not GitHub links, so they take no part in a GitHub sync.
    /// Reported so the skip is visible, never auto-applied.
    pub foreign: Vec<String>,
}

impl SyncPlan {
    /// Drop the action lists this [`Direction`] does not apply, so both the
    /// dry-run summary and the apply pass reflect the requested one-way view
    /// (`PLUGIN_SYSTEM.md` §4.2). [`Direction::PullOnly`] clears the push side and
    /// [`Direction::PushOnly`] the pull side; [`Direction::Both`] is a no-op.
    ///
    /// `in_sync`, `conflicts`, `remote_missing`, and `foreign` are left intact —
    /// they record no writes (they are informational or re-record a fingerprint),
    /// so surfacing them in either direction is harmless and more useful than
    /// hiding them.
    pub fn restrict_to(&mut self, direction: Direction) {
        if !direction.pulls() {
            self.pull_create.clear();
            self.pull_update.clear();
        }
        if !direction.pushes() {
            self.push_create.clear();
            self.push_update.clear();
        }
    }

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
            foreign: self.foreign.clone(),
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub foreign: Vec<String>,
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

/// A stable hash of a comment body (the push-dedup key), reproducible across
/// runs, machines, **and toolchain upgrades**.
///
/// These `u64`s are persisted in [`SyncEntry::local_comment_hashes`] and compared
/// against values written by (potentially) a different build on a later run, so
/// the algorithm must be fixed forever. std's `DefaultHasher` is explicitly
/// documented as unstable across Rust releases ("should not be relied upon over
/// releases"), so it must not back a persisted fingerprint — a toolchain bump
/// would silently invalidate every stored hash and re-push every comment as a
/// duplicate. We use blake3 (the project's content hasher), truncated to 64 bits;
/// truncation is fine because this is a dedup key, not a security boundary. The
/// body is trimmed first so a trailing-newline difference never forks the identity.
pub fn body_hash(body: &str) -> u64 {
    let digest = blake3::hash(body.trim().as_bytes());
    let head: [u8; 8] = digest.as_bytes()[..8]
        .try_into()
        .expect("blake3 digest is 32 bytes");
    u64::from_le_bytes(head)
}

/// A stable fingerprint of one side's *synced content* — the fields the sync
/// round-trips, in a canonical form both sides can produce (blake3, truncated
/// to 64 bits; persisted in [`SyncEntry::gh_content_hash`], so like
/// [`body_hash`] the algorithm must stay fixed).
///
/// The assignee is deliberately excluded: GitHub does not guarantee the order
/// of the `assignees` array, and a phantom mismatch there would only re-open
/// the clock-based path this fingerprint exists to narrow.
#[allow(clippy::too_many_arguments)]
fn content_fingerprint(
    title: &str,
    closed: bool,
    priority: u8,
    item_type: Option<&str>,
    deps: Option<Vec<&str>>,
    labels: &[String],
    body: &str,
) -> u64 {
    let mut hasher = blake3::Hasher::new();
    let mut push = |part: &str| {
        // Length-prefixed so adjacent fields can never alias each other.
        hasher.update(&(part.len() as u64).to_le_bytes());
        hasher.update(part.as_bytes());
    };
    push(title);
    push(if closed { "1" } else { "0" });
    push(&priority.to_string());
    push(item_type.unwrap_or(""));
    match deps {
        // No clove-meta at all (dep ownership unknown) hashes differently
        // from meta owning an empty dep set.
        None => push("<no-meta>"),
        Some(mut d) => {
            d.sort_unstable();
            push(&d.join(","));
        }
    }
    let mut sorted_labels: Vec<&str> = labels.iter().map(String::as_str).collect();
    sorted_labels.sort_unstable();
    push(&sorted_labels.join(","));
    push(body.trim());

    let digest = hasher.finalize();
    let head: [u8; 8] = digest.as_bytes()[..8]
        .try_into()
        .expect("blake3 digest is 32 bytes");
    u64::from_le_bytes(head)
}

/// [`content_fingerprint`] of a mapped remote issue (the pull-side view).
pub fn staged_fingerprint(staged: &StagedIssue) -> u64 {
    content_fingerprint(
        &staged.title,
        staged.status == ItemStatus::Closed,
        staged.priority.get(),
        staged.item_type.map(clove_types::ItemType::as_str),
        staged
            .deps
            .as_ref()
            .map(|deps| deps.iter().map(CloveId::as_str).collect()),
        &staged.labels,
        &staged.body,
    )
}

/// [`content_fingerprint`] of a local item's push payload — i.e. what the
/// remote issue will map to *after* this payload is pushed. Decodes the
/// `clove-meta` marker exactly the way [`map_issue`] will on the next fetch,
/// so `export_fingerprint(pushed) == staged_fingerprint(map_issue(fetched))`.
pub fn export_fingerprint(item: &ExportItem) -> u64 {
    use crate::github::{decode_clove_meta, strip_clove_meta};
    let meta = decode_clove_meta(&item.body);
    let has_meta = meta.is_some();
    let meta = meta.unwrap_or_default();
    let priority = meta
        .priority
        .map(|p| crate::map::coerce_priority(i64::from(p)))
        .unwrap_or(Priority::DEFAULT)
        .get();
    let item_type = meta
        .item_type
        .as_deref()
        .and_then(|t| clove_types::ItemType::parse(t).ok());
    let deps: Option<Vec<String>> = has_meta.then(|| {
        meta.deps
            .iter()
            .filter_map(|d| CloveId::new(d.trim()).ok())
            .map(|id| id.to_string())
            .collect()
    });
    content_fingerprint(
        &item.title,
        item.closed,
        priority,
        item_type.map(clove_types::ItemType::as_str),
        deps.as_ref()
            .map(|d| d.iter().map(String::as_str).collect()),
        &item.labels,
        &strip_clove_meta(&item.body),
    )
}

/// Reconcile an issue's GitHub comments against its local sidecar comments,
/// given the prior per-issue bookkeeping in `entry`.
///
/// A GitHub comment is pulled unless its id was already seen. Local comments
/// are pushed by a **multiset** diff on body hashes: each already-synced
/// occurrence of a body (previously pushed, or just pulled — the fresh local
/// copy must not echo back) consumes one matching local comment, and only the
/// surplus is pushed. A plain set would collapse repeats: a user commenting
/// the same text twice would never get the second one pushed.
pub fn plan_comments(gh: &[GhComment], local: &[LocalComment], entry: &SyncEntry) -> CommentPlan {
    let mut seen_gh = entry.gh_comment_ids.clone();
    let mut synced = entry.comment_hash_counts();
    let mut plan = CommentPlan::default();

    for comment in gh {
        if seen_gh.insert(comment.id) {
            *synced.entry(body_hash(&comment.body)).or_insert(0) += 1;
            plan.pull.push(comment.clone());
        }
    }
    for comment in local {
        let remaining = synced.entry(body_hash(&comment.body)).or_insert(0);
        if *remaining > 0 {
            *remaining -= 1;
        } else {
            plan.push.push(comment.clone());
        }
    }
    plan
}

/// The Unix epoch as a UTC datetime — the "infinitely old" fallback for a
/// missing timestamp, so an item with no recorded clock always looks unchanged
/// rather than spuriously newer. (Also used by the apply layer to *backdate* a
/// recorded local clock so the next sync is forced to re-examine a pair.)
pub(crate) fn epoch() -> DateTime<Utc> {
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
    // Type/deps are only comparable when the remote body carried clove-meta
    // (`None` = unknown, which never counts as a difference).
    if let Some(t) = staged.item_type {
        if !s("type").eq_ignore_ascii_case(t.as_str()) {
            return false;
        }
    }
    if let Some(remote_deps) = &staged.deps {
        let mut local_deps: Vec<&str> = obj
            .get("deps")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        local_deps.sort_unstable();
        let mut remote: Vec<&str> = remote_deps.iter().map(CloveId::as_str).collect();
        remote.sort_unstable();
        if remote != local_deps {
            return false;
        }
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
            // Linked to a *different* external system (`tk:…`, `beads:…`) — not
            // a GitHub link at all, so it neither matches a remote issue nor
            // counts as "deleted remotely". Skipped, reported.
            Some(ext) if parse_gh_number(&ext).is_none() => plan.foreign.push(ext),
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
    let remote_hash = staged_fingerprint(&staged);
    let (remote_chg, local_chg) = match state.entries.get(ext) {
        Some(entry) => {
            let clock_newer = remote_changed(issue.updated_at, entry.gh_updated_at);
            // With a recorded content fingerprint, "remote changed" requires
            // the mapped content to actually differ: a comment/reaction bumps
            // `updated_at` without changing any synced field, and a phantom
            // change here (combined with a real local edit) would resolve as a
            // conflict that reverts the local edit with stale remote content.
            let remote = match entry.gh_content_hash {
                Some(prev) => clock_newer && prev != remote_hash,
                None => clock_newer, // pre-fingerprint state file
            };
            (remote, local_updated > entry.local_updated)
        }
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
            gh_content_hash: remote_hash,
        }),
        (true, false) => push_pull_update(plan, clove_id, staged)?,
        (false, true) => push_push_update(plan, ext, clove_id, item, local_updated, issue),
        // Both clocks moved but the synced content agrees — e.g. a third-party
        // GitHub comment bumped the issue's `updated_at` without changing any
        // field, alongside a local edit that was already pushed (or vice
        // versa). Not a real conflict: refresh the fingerprint instead of
        // letting the policy overwrite one side with identical/stale content.
        (true, true) if content_equal(&staged, obj) => plan.in_sync.push(InSyncEntry {
            external_ref: ext.to_owned(),
            gh_updated_at: issue.updated_at,
            local_updated,
            gh_content_hash: remote_hash,
        }),
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
    fn direction_predicates() {
        assert!(Direction::Both.pulls() && Direction::Both.pushes());
        assert!(Direction::PullOnly.pulls() && !Direction::PullOnly.pushes());
        assert!(!Direction::PushOnly.pulls() && Direction::PushOnly.pushes());
        assert_eq!(Direction::default(), Direction::Both);
    }

    #[test]
    fn restrict_to_drops_the_other_side() {
        // A plan carrying both a pull-create (new remote issue) and a push-create
        // (new local item) — restrict_to keeps only the requested direction's side.
        let build = || {
            plan_sync(
                &[issue(7, "Remote", "open", "2026-06-01T00:00:00Z")],
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
            .unwrap()
        };

        let mut both = build();
        both.restrict_to(Direction::Both);
        assert_eq!(both.pull_create.len(), 1);
        assert_eq!(both.push_create.len(), 1);

        let mut pull = build();
        pull.restrict_to(Direction::PullOnly);
        assert_eq!(pull.pull_create.len(), 1, "pull side kept");
        assert!(pull.push_create.is_empty(), "push side dropped");

        let mut push = build();
        push.restrict_to(Direction::PushOnly);
        assert!(push.pull_create.is_empty(), "pull side dropped");
        assert_eq!(push.push_create.len(), 1, "push side kept");
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
    fn foreign_external_ref_is_skipped_not_remote_missing() {
        // An item imported from tk/beads carries a non-GitHub external_ref: it
        // takes no part in a GitHub sync — neither "deleted remotely" nor a
        // push-create candidate.
        let plan = plan_sync(
            &[],
            &[local(
                "proj-AAAA1111",
                "From tk",
                "open",
                Some("tk:abc-123"),
                "2026-06-03T00:00:00Z",
            )],
            &SyncState::default(),
            ConflictPolicy::Newer,
        )
        .unwrap();
        assert!(plan.remote_missing.is_empty());
        assert!(plan.push_create.is_empty());
        assert_eq!(plan.foreign, vec!["tk:abc-123".to_owned()]);
        assert!(plan.is_noop());
    }

    #[test]
    fn invalid_meta_dep_id_does_not_abort_the_plan() {
        // Anyone who can edit an issue body can plant a malformed clove-meta;
        // a bad dep id must be dropped, not abort the whole sync.
        let mut bad = issue(7, "Foreign meta", "open", "2026-06-04T00:00:00Z");
        bad.body = Some("Body.\n\n<!-- clove-meta: {\"deps\":[\"lol\"]} -->".to_owned());
        let plan = plan_sync(&[bad], &[], &SyncState::default(), ConflictPolicy::Newer)
            .expect("a bad dep id in a foreign body must not fail the plan");
        assert_eq!(plan.pull_create.len(), 1);
        assert_eq!(plan.pull_create[0].staged.deps, Some(Vec::new()));
    }

    #[test]
    fn phantom_remote_bump_with_local_edit_is_a_push_not_a_conflict() {
        // Last sync recorded the remote content fingerprint. A third-party
        // comment then bumps the remote `updated_at` (content unchanged) and
        // the user edits the local title. Clock-only detection called this a
        // both-sides conflict and `Newer` reverted the local edit with stale
        // remote content; the fingerprint proves the remote did not change.
        let remote = issue(7, "Same title", "open", "2026-06-04T00:00:00Z");
        let hash = staged_fingerprint(&map_issue(&remote).unwrap());
        let mut state = SyncState::default();
        state.record(
            "gh-7",
            Some(ts("2026-06-01T00:00:00Z")),
            ts("2026-06-01T00:00:00Z"),
        );
        state.record_content_hash("gh-7", hash);

        let plan = plan_sync(
            &[remote],
            &[local(
                "proj-AAAA1111",
                "Locally edited title",
                "open",
                Some("gh-7"),
                "2026-06-03T00:00:00Z",
            )],
            &state,
            ConflictPolicy::Newer,
        )
        .unwrap();
        assert!(plan.conflicts.is_empty(), "no phantom conflict");
        assert_eq!(plan.push_update.len(), 1, "local edit pushes");
        assert!(plan.pull_update.is_empty());
    }

    #[test]
    fn real_remote_change_with_local_edit_is_still_a_conflict() {
        // Same clocks as above, but the remote content genuinely differs from
        // the recorded fingerprint → the conflict machinery still engages.
        let old_remote = issue(7, "Old remote title", "open", "2026-06-01T00:00:00Z");
        let hash = staged_fingerprint(&map_issue(&old_remote).unwrap());
        let mut state = SyncState::default();
        state.record(
            "gh-7",
            Some(ts("2026-06-01T00:00:00Z")),
            ts("2026-06-01T00:00:00Z"),
        );
        state.record_content_hash("gh-7", hash);

        let plan = plan_sync(
            &[issue(7, "New remote title", "open", "2026-06-04T00:00:00Z")],
            &[local(
                "proj-AAAA1111",
                "Locally edited title",
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
    }

    #[test]
    fn export_and_staged_fingerprints_agree_after_a_push() {
        // What we hash when pushing must equal what we hash when the pushed
        // payload is fetched back and mapped — otherwise every push would look
        // like a remote change on the next sync.
        let obj: Map<String, Value> = json!({
            "id": "proj-AAAA1111",
            "title": "Round trip",
            "body": "The body.",
            "priority": 1,
            "type": "bug",
            "deps": ["proj-BBBB2222"],
            "labels": ["area:core", "bug"],
            "status": "open",
            "updated": "2026-06-03T00:00:00Z",
        })
        .as_object()
        .unwrap()
        .clone();
        let item = build_export_item(&obj);

        // Simulate GitHub echoing the pushed payload back on the next fetch.
        let echoed = GitHubIssue {
            number: 7,
            title: item.title.clone(),
            state: "open".to_owned(),
            body: Some(item.body.clone()),
            labels: item
                .labels
                .iter()
                .map(|l| crate::github::GitHubLabel { name: l.clone() })
                .collect(),
            updated_at: Some(ts("2026-06-04T00:00:00Z")),
            ..Default::default()
        };
        let staged = map_issue(&echoed).unwrap();
        assert_eq!(export_fingerprint(&item), staged_fingerprint(&staged));
    }

    #[test]
    fn both_clocks_moved_but_identical_content_is_in_sync() {
        // A third-party GitHub comment bumps `updated_at` without changing any
        // synced field; with a concurrent local clock bump but identical
        // content this must not surface as a conflict (which would overwrite a
        // side with identical/stale content).
        let mut state = SyncState::default();
        state.record(
            "gh-7",
            Some(ts("2026-06-01T00:00:00Z")),
            ts("2026-06-01T00:00:00Z"),
        );
        let plan = plan_sync(
            &[issue(7, "Same", "open", "2026-06-04T00:00:00Z")],
            &[local(
                "proj-AAAA1111",
                "Same",
                "open",
                Some("gh-7"),
                "2026-06-03T00:00:00Z",
            )],
            &state,
            ConflictPolicy::Newer,
        )
        .unwrap();
        assert!(plan.conflicts.is_empty());
        assert_eq!(plan.in_sync.len(), 1);
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
    fn repeat_identical_local_comment_still_pushes() {
        // "done." was pushed months ago (recorded once); the user comments
        // "done." again. The surplus occurrence must push — a plain hash set
        // dropped it forever.
        let mut entry = SyncEntry::new(None, ts("2026-06-01T00:00:00Z"));
        entry.record_comment_hash(body_hash("done."));
        let plan = plan_comments(
            &[],
            &[local_comment("done."), local_comment("done.")],
            &entry,
        );
        assert_eq!(plan.push.len(), 1, "only the surplus occurrence pushes");
    }

    #[test]
    fn two_new_identical_local_comments_both_push() {
        let entry = SyncEntry::new(None, ts("2026-06-01T00:00:00Z"));
        let plan = plan_comments(&[], &[local_comment("same"), local_comment("same")], &entry);
        assert_eq!(plan.push.len(), 2);
    }

    #[test]
    fn legacy_set_only_state_counts_as_one_occurrence() {
        // A pre-multiset state file has the hash only in the legacy set: it
        // must satisfy exactly one local occurrence, not all of them.
        let mut entry = SyncEntry::new(None, ts("2026-06-01T00:00:00Z"));
        entry.local_comment_hashes.insert(body_hash("hi"));
        let plan = plan_comments(&[], &[local_comment("hi"), local_comment("hi")], &entry);
        assert_eq!(plan.push.len(), 1);

        // And recording on top of the legacy set composes (1 legacy + 1 new).
        entry.record_comment_hash(body_hash("hi"));
        assert_eq!(entry.comment_hash_counts()[&body_hash("hi")], 2);
    }

    #[test]
    fn body_hash_ignores_trailing_whitespace() {
        assert_eq!(body_hash("hello"), body_hash("hello\n"));
        assert_ne!(body_hash("hello"), body_hash("world"));
    }

    #[test]
    fn body_hash_is_a_fixed_stable_vector() {
        // These hashes are persisted and compared across toolchain upgrades, so
        // the algorithm must never drift. Pin known vectors: a change here (e.g. a
        // regression back to the unstable std `DefaultHasher`) breaks this test
        // and forces a deliberate, documented migration.
        assert_eq!(body_hash("hello"), 10557148580892020714);
        assert_eq!(body_hash("A local comment"), 1232548354274388200);
    }
}
