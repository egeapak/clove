# GitLab vendor bridge — design (gh-19)

**Status:** design only (implementation deferred). Scopes `clove sync gitlab
<project>` as a second two-way vendor bridge, and introduces the **`VendorSync`
trait** that lets Jira (gh-25) and any future tracker reuse the same
reconciliation core.

## Goal

`clove sync gitlab <group/project>` reconciles clove items ↔ GitLab issues in one
two-way pass, with the exact semantics `clove sync github` already ships:
pull+push in a single pass, per-project last-sync fingerprints, conflict policy
(`--prefer newer|local|remote|manual`), bidirectional comment sync, `--dry-run`,
and `external_ref` write-back. No new user-facing concepts — just a new tracker
name.

## What generalizes — and what must actually change (do not overstate)

The *reconciliation algorithm* (change-detection matrix, conflict policy, comment
dedup) is tracker-independent and worth reusing. But — correcting an earlier draft
of this spec — the core is **not yet vendor-neutral in its types**; the extraction
is a real refactor, not a zero-behavior-change wrapper. Verified against the code:

- `plan_sync(remote: &[GitHubIssue], …)` (`sync.rs`) takes **`&[GitHubIssue]`**, not
  a neutral type, and calls `map_issue` / `external_ref_for` / `parse_gh_number`
  and reads `issue.pull_request` / `issue.updated_at` **internally**. The planner
  is GitHub-coupled today.
- The plan **output** types are GitHub-shaped: `PushUpdate.number: u64`,
  `PushUpdate.gh_assignees`, `PushUpdate.gh_state_reason` (`sync.rs`), and
  `ExportItem.gh_number` (`github.rs`). These must be de-GitHub'd → `RemoteId`, a
  generic "extra assignees" carrier, and a vendor passthrough for `state_reason`.
- `SyncState::path_for` is hardcoded `.clove/sync/github/<owner__repo>.json` and
  `SyncEntry` fields are `gh_`-prefixed on disk (`gh_updated_at`, `gh_comment_ids`,
  `gh_content_hash`, `synced_gh_comments`). Reusable, but must be parameterized by
  vendor (and the `gh_` field names are cosmetically wrong for a `gl-` state file).

So the honest boundary work is: **change `plan_sync`'s input to a neutral
`StagedIssue`, generalize the plan/`ExportItem`/`SyncState` types, keep the logic
identical.** `ConflictPolicy`, `body_hash`, `staged_fingerprint`, `plan_comments`,
and `SyncSummary` are genuinely logic-neutral and carry over.

The GitHub-specific pieces the `VendorSync` trait must absorb:

1. **Transport** — the octocrab client + fetch (behind the `github` feature).
2. **Field mapping** — `map_issue` (remote → `StagedIssue`), `build_export_item`
   (clove → payload), the `<!-- clove-meta -->` body codec, and the `gh-<number>`
   ref format.
3. **The apply loop** — `sync_net::sync_github`'s create/update/comment calls +
   `apply_edit` write-back.

**Two completeness constraints the trait must respect (not obvious):**

- **Fetch must stay whole-set, not incremental.** The planner detects a deleted
  remote by "a locally-linked `external_ref` absent from the fetched set" and
  records `in_sync` by iterating all remote issues. So `fetch_since` returning only
  *changed* issues would misclassify every unchanged-but-linked item as
  `remote_missing`. `VendorSync::fetch` must return the **full** current remote set
  (or the planner must be reworked to distinguish absent-unchanged from
  absent-deleted). This design keeps full fetch.
- **`StagedIssue` needs `comment_count`.** The comment-sync skip optimization reads
  `issue.comments` off the raw `GitHubIssue`; a neutral `Vec<StagedIssue>` drops
  it unless `StagedIssue` carries a `comment_count`. Add that field.

## The `VendorSync` trait

Introduce a trait that captures exactly those three vendor-specific concerns, so
the apply loop becomes generic. Sketch (in a new `clove_import::vendor` module):

```rust
/// A remote issue tracker clove can two-way sync against. One impl per vendor.
pub trait VendorSync {
    /// Stable short name used in `external_ref` (`gh`, `gl`, `jira`) and the
    /// sync-state filename.
    fn slug(&self) -> &'static str;

    /// `external_ref` for a remote issue id (e.g. `gl-42`), and its inverse.
    fn external_ref(&self, remote_id: &RemoteId) -> String;
    fn parse_external_ref(&self, external_ref: &str) -> Option<RemoteId>;

    /// Fetch the **full** current remote issue set, mapped into the vendor-neutral
    /// staged form the planner consumes. Whole-set (not incremental) is required:
    /// the planner infers remote deletions from absence, so a partial fetch would
    /// misclassify unchanged-but-linked items as deleted (see completeness note
    /// above). Each `StagedIssue` carries `comment_count` for the comment-skip
    /// optimization.
    fn fetch(&self) -> Result<Vec<StagedIssue>, VendorError>;

    /// Create / update a remote issue from a clove item payload; return the new
    /// (or unchanged) remote id + updated_at for `external_ref` write-back.
    fn create(&self, item: &ExportItem) -> Result<RemoteRef, VendorError>;
    fn update(&self, id: &RemoteId, item: &ExportItem) -> Result<RemoteRef, VendorError>;

    /// Comment reconciliation primitives (used by `plan_comments`).
    fn list_comments(&self, id: &RemoteId) -> Result<Vec<GhComment>, VendorError>;
    fn add_comment(&self, id: &RemoteId, body: &str) -> Result<(), VendorError>;
}
```

`sync_net::sync_github` is refactored to `sync_net::sync_vendor<V: VendorSync>(v,
…)` containing the entire apply loop (create/update/comment + `apply_edit`
write-back + `SyncState` persistence + bounded retry). `sync_github` becomes a
thin `sync_vendor(GitHubVendor::new(repo, token))`; `sync_gitlab` is
`sync_vendor(GitLabVendor::new(project, token))`. **The planner never changes.**

`RemoteId` is an enum-free newtype (`String` inside) so numeric (GitHub/GitLab
issue iid) and string (Jira key) ids both fit; the codec `<!-- clove-meta -->`
stays vendor-neutral (it already only carries clove fields).

## GitLab field mapping (`gitlab.rs`)

| clove | GitLab issue | Notes |
|---|---|---|
| `title` | `title` | direct |
| `body` | `description` + `<!-- clove-meta -->` | same codec as GitHub |
| `status` open/closed | `state` opened/closed | GitLab has no `in_progress` state → keep clove's `in_progress` locally; map to `opened` remotely (record in clove-meta so a round-trip doesn't demote it — the same guard `sync_github` uses) |
| `labels` | `labels` (CSV) | canonicalized both ways |
| `assignee` | `assignees[0]` | GitLab multi-assignee; take/set the first, preserve the rest the way GitHub already does — via `SyncEntry.synced_assignee` + `PushUpdate.gh_assignees` (NOT clove-meta). Generalizing that extra-assignee carrier is exactly the `gh_assignees`/`synced_assignee` de-GitHub'ing noted above |
| `external_ref` | `gl-<iid>` | project-scoped **iid**, not global id |
| comments | issue **notes** | filter out system notes (`system: true`) — only user notes sync |

GitLab specifics to encode once, in `gitlab.rs`:
- **iid vs id:** issues are addressed by project-scoped `iid` in the REST path
  (`/projects/:id/issues/:iid`) but the API returns both; `external_ref` uses `iid`.
- **`updated_at`** drives change detection exactly like GitHub.
- **Pagination:** `?updated_after=<since>&per_page=100` + `X-Next-Page` header.
- **state_reason:** GitLab lacks GitHub's `state_reason`; closed is just closed.

## Transport & auth

- `gitlab` cargo feature (mirrors `github`; opt-in per gh-18) pulling a REST
  client (`reqwest` is already in the tree via octocrab, or the `gitlab` crate).
- Token: `GITLAB_TOKEN`, then `glab auth token` if present. Base URL configurable
  (`--host`, default `https://gitlab.com`) for self-hosted instances.
- Same rustls ring-provider init hazard as octocrab (see `github.rs`).

## CLI & config

- `clove sync gitlab <group/project> [--prefer …] [--no-comments] [--dry-run]
  [--host URL]`. `<TRACKER>` enum gains `gitlab`.
- Daemon timer: `[daemon] gitlab_sync_interval_min` + `gitlab_sync_project`
  (mirrors the GitHub keys).

## Testing

Mirror the GitHub approach exactly: a deterministic **in-process mock GitLab
server** (base URL overridden via `CLOVE_GITLAB_API_URL`, like
`CLOVE_GITHUB_API_URL`) driving the real `clove sync gitlab` binary through the
REST client. Port the 13 `sync_github.rs` scenarios (push/pull create+update,
conflict resolution, idempotency/write-back, comments, assignee/state
preservation, dry-run). The pure `map_issue`/`content_equal` mapping gets
offline unit tests. The `VendorSync` refactor is covered by re-running the
existing GitHub suite unchanged (proving the extraction is behavior-preserving).

## Rollout

1. **De-GitHub the core**: change `plan_sync`'s input to a neutral `StagedIssue`
   (add `comment_count`); generalize the plan output, `ExportItem`, and `SyncState`
   types (`RemoteId`, generic extra-assignees, vendor-parameterized state path);
   extract `VendorSync` + `sync_vendor` and re-point `sync_github` at it. This is a
   type/signature refactor with the **logic preserved** — not a no-op wrapper — so
   the existing GitHub suite (incl. the 13 `sync_github` scenarios) passing
   unchanged is the proof it's behavior-preserving. **Ship this first, on its own**
   to de-risk the boundary before any GitLab code.
2. Add `gitlab.rs` (mapping) + `GitLabVendor` (transport) behind the `gitlab`
   feature; wire the CLI/daemon; port the mock-server suite.

## Non-goals

Merge requests, epics-as-GitLab-epics, milestones, and cross-project references
are out of scope for v1 (issues only), matching the GitHub bridge.
