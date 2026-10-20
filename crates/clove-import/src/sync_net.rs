//! Two-way GitHub sync: the network apply layer (T-M06, `github` feature only).
//!
//! The pure reconciliation lives in [`crate::sync`]; this module performs the
//! side effects the plan describes:
//!
//! - **push** — create / update GitHub issues via octocrab, then write the new
//!   `external_ref` back onto the local item so the link is durable (closing the
//!   idempotency gap the old one-way `export` had);
//! - **pull** — create new local items and apply remote edits through the unified
//!   write path (`apply_edit`);
//! - **state** — record every touched issue's `{gh_updated_at, local_updated}`
//!   fingerprint and persist the [`SyncState`] so the next run is incremental.
//!
//! Network calls go through [`with_retry`] (bounded exponential backoff) so a
//! transient blip or rate-limit doesn't abort a whole sync. `--dry-run` never
//! enters this module's apply path — it stops at the plan.

use chrono::{DateTime, Utc};
use serde_json::{Map, Value};

use clove_types::model::CURRENT_SCHEMA_VERSION;
use clove_types::{CloveId, EditRequest, Item, ItemFrontmatter, ItemStatus, LabelEdit};

use octocrab::models::issues::Issue;
use octocrab::Octocrab;

use crate::error::ImportError;
use crate::github::net::{build_client, fetch_all, net_err, parse_repo_spec};
use crate::github::{parse_gh_number, StagedIssue};
use crate::map::build_external_ref_index;
use crate::store::Store;
use crate::sync::{
    comment_thread_idle, link_hash, plan_comments, plan_sync, CommentConflictRef, CommentLink,
    CommentPlan, CommentRef, ConflictPolicy, Direction, GhComment, LocalComment, PullUpdate,
    PushCreate, PushUpdate, SkippedItem, SyncPlan, SyncReport, SyncState, SyncSummary,
};

/// The marker `source_system` value stamped on synced items.
const SOURCE_GITHUB: &str = "github";

/// Run a full two-way sync of `store` against `spec` (`owner/repo`).
///
/// Returns the plan summary always, and the apply report when not a dry run. The
/// sync state under `.clove/sync/github/<owner>__<repo>.json` is loaded before
/// planning and persisted after a successful apply.
pub fn sync_github(
    spec: &str,
    store: &dyn Store,
    policy: ConflictPolicy,
    sync_comments: bool,
    dry_run: bool,
    direction: Direction,
) -> Result<(SyncSummary, Option<SyncReport>), ImportError> {
    let (owner, repo) = parse_repo_spec(spec)?;

    let state_path = SyncState::path_for(store.repo_root(), spec);

    // Serialize concurrent syncs of the same repo (e.g. a daemon timer overlapping
    // a manual `clove sync`): two in flight could both push-create the same
    // unlinked item and mint duplicate GitHub issues. A dry run reads only, so it
    // needs no lock. The guard borrows `sync_lock_rw`, so both are held (and
    // released on drop) for the whole run.
    let mut sync_lock_rw;
    let _sync_lock = if dry_run {
        None
    } else {
        sync_lock_rw = open_sync_lock(&state_path)?;
        Some(sync_lock_rw.try_write().map_err(|_| ImportError::Source {
            path: state_path.clone(),
            message: format!("another sync for `{spec}` is already in progress"),
        })?)
    };

    // The local side of the diff: each item's frontmatter plus its body, which is
    // all the planner reads. Built here (not by the caller) so the CLI and the
    // daemon share one path.
    let local = local_objects(store)?;

    let mut state = SyncState::load(&state_path, spec);
    // Taken before anything is fetched, so a comment edited while this run is in
    // flight is still "since" the next one.
    let sync_started = Utc::now();
    let sync_comments = sync_comments && direction == Direction::Both;

    // Fetch remote issues. Even a dry run needs the current remote state to plan
    // a meaningful diff.
    let rt = tokio::runtime::Runtime::new().map_err(|err| ImportError::Source {
        path: camino::Utf8PathBuf::from("<github>"),
        message: format!("failed to start async runtime: {err}"),
    })?;
    let issues = rt.block_on(async {
        let crab = build_client()?;
        fetch_all(&crab, &owner, &repo).await
    })?;

    let mut plan: SyncPlan = plan_sync(&issues, &local, &state, policy)
        .map_err(|message| ImportError::Record { message })?;
    // Restrict the plan to the requested direction (§4.2): `import github` applies
    // only pulls, `export github` only pushes, `sync github` both. Done before
    // `summary()` so a directional `--dry-run` reports only its own side.
    plan.restrict_to(direction);
    let mut summary = plan.summary();

    // Remote comment counts from the issues list, for the idle-thread check.
    let remote_comment_counts: std::collections::HashMap<String, u64> = issues
        .iter()
        .filter(|issue| issue.pull_request.is_none())
        .map(|issue| {
            (
                crate::github::external_ref_for(issue.number),
                issue.comments,
            )
        })
        .collect();

    if dry_run {
        // Comment threads are planned against the items linked today; a thread
        // of an issue this run would create or link has nothing to reconcile yet.
        if sync_comments {
            let threads = rt.block_on(async {
                let crab = build_client()?;
                plan_comment_threads(
                    &crab,
                    &owner,
                    &repo,
                    store,
                    &state,
                    &remote_comment_counts,
                    policy,
                )
                .await
            })?;
            for thread in &threads {
                summary.comments.add_plan(
                    thread.clove_id.as_str(),
                    &thread.external_ref,
                    &thread.plan,
                );
            }
        }
        return Ok((summary, None));
    }

    let apply_result = rt.block_on(async {
        let crab = build_client()?;
        let outcome = apply_plan(&crab, &owner, &repo, plan, store, &mut state).await?;
        let mut report = outcome.report;
        // Comment sync is inherently two-way and its per-issue fingerprints are
        // coupled to the idle-thread check, so it runs only for a full `Both`
        // reconcile. A one-way `import`/`export github` syncs items only (documented
        // in PLUGIN_SYSTEM.md §4.2); use `clove sync github` for comments.
        if sync_comments {
            let threads = plan_comment_threads(
                &crab,
                &owner,
                &repo,
                store,
                &state,
                &remote_comment_counts,
                policy,
            )
            .await?;
            let all_applied = apply_comment_threads(
                &crab,
                &owner,
                &repo,
                store,
                &mut state,
                threads,
                &mut report,
            )
            .await?;
            if all_applied {
                state.comments_checked_at = Some(sync_started);
            }
        }
        Ok::<(SyncReport, std::collections::HashSet<String>), ImportError>((
            report,
            outcome.reconciled,
        ))
    });

    match apply_result {
        Ok((report, reconciled)) => {
            // Record each reconciled item's post-apply assignee, so the next push
            // can tell clove's assignee apart from human-added extras. Done in one
            // pass (not per action) and after apply, so the pushes above still saw
            // the *previous* value.
            record_synced_assignees(store, &mut state, &reconciled)?;
            state.save(&state_path)?;
            Ok((summary, Some(report)))
        }
        Err(err) => {
            // A mid-run failure (e.g. an API call exhausting its retries) must not
            // discard the bookkeeping for actions already applied: the comment ids,
            // `external_ref` links, and fingerprints accumulated in `state` are what
            // stop the next run from re-pulling / re-pushing them as duplicates.
            // Persist what we have (best-effort), then surface the original error.
            let _ = state.save(&state_path);
            Err(err)
        }
    }
}

/// Open (creating if needed) the per-repo sync lock file next to the state file,
/// returning an [`fd_lock::RwLock`] the caller locks for the run. The advisory
/// lock releases when the underlying file handle is dropped.
fn open_sync_lock(
    state_path: &camino::Utf8Path,
) -> Result<fd_lock::RwLock<std::fs::File>, ImportError> {
    let lock_path = state_path.with_extension("lock");
    if let Some(parent) = lock_path.parent() {
        let root = crate::sync::store_root(&lock_path);
        clove_core::fs_safe::create_dirs(root, parent).map_err(|source| ImportError::Source {
            path: parent.to_owned(),
            message: format!("failed to create sync dir: {source}"),
        })?;
    }
    let file =
        clove_core::fs_safe::open_lock_file(&lock_path).map_err(|source| ImportError::Source {
            path: lock_path.clone(),
            message: format!("failed to open sync lock: {source}"),
        })?;
    Ok(fd_lock::RwLock::new(file))
}

/// Stamp the current local assignee onto the sync entry of every item that was
/// actually reconciled this run, so the next push can tell clove's assignee apart
/// from human-added extras.
///
/// Only `reconciled` refs are stamped: an item whose reconciliation was skipped
/// (a `Manual`-policy conflict) or whose remote issue is missing was *not* pushed,
/// so its GitHub assignee still reflects the *previous* local assignee. Restamping
/// the baseline to the new local value there would make the next push treat the
/// stale GitHub assignee as a human-added extra and keep it, leaving both the old
/// and new assignee on the issue.
fn record_synced_assignees(
    store: &dyn Store,
    state: &mut SyncState,
    reconciled: &std::collections::HashSet<String>,
) -> Result<(), ImportError> {
    let frontmatters = store.scan_frontmatter()?;
    for fm in frontmatters {
        if let Some(external_ref) = &fm.external_ref {
            if external_ref.starts_with("gh-") && reconciled.contains(external_ref) {
                if let Some(entry) = state.entries.get_mut(external_ref) {
                    entry.synced_assignee = fm.assignee.clone();
                }
            }
        }
    }
    Ok(())
}

/// Build the local side of the diff: one object per item, its serialized
/// frontmatter plus `body` (the exact fields [`plan_sync`] / `build_export_item`
/// read). Errors if any item file fails to parse (see below).
fn local_objects(store: &dyn Store) -> Result<Vec<Map<String, Value>>, ImportError> {
    let crate::store::Scan { items, errors } = store.scan()?;
    // Refuse to sync when any local item file fails to parse. A dropped item is
    // invisible to the planner, so its linked remote issue would be seen as having
    // no local counterpart and pull-created as a DUPLICATE local item carrying the
    // same `external_ref` — permanently forking the link. Fail loudly instead so
    // the user repairs the file first (`clove doctor` lists the broken ones).
    if let Some(err) = errors.first() {
        return Err(ImportError::Source {
            path: camino::Utf8PathBuf::from(&err.path),
            message: format!(
                "{} local item file(s) failed to parse; fix them before syncing \
                 (see `clove doctor`): {}",
                errors.len(),
                err.message
            ),
        });
    }
    Ok(items
        .iter()
        .map(|item| {
            let mut obj = clove_core::frontmatter_object(&item.frontmatter);
            obj.insert("body".to_owned(), Value::String(item.body.clone()));
            obj
        })
        .collect())
}

/// The result of an apply pass: the [`SyncReport`] plus the set of
/// `external_ref`s whose item was actually reconciled this run (in-sync,
/// pulled, or pushed). Manual-skipped conflicts and remote-missing refs are
/// deliberately absent — their local side was not touched, so their assignee
/// baseline must not be restamped (see [`record_synced_assignees`]).
struct ApplyOutcome {
    report: SyncReport,
    reconciled: std::collections::HashSet<String>,
}

/// Apply every action in `plan`, mutating both GitHub and the local store and
/// recording fresh fingerprints into `state`.
async fn apply_plan(
    crab: &Octocrab,
    owner: &str,
    repo: &str,
    plan: SyncPlan,
    store: &dyn Store,
    state: &mut SyncState,
) -> Result<ApplyOutcome, ImportError> {
    let mut report = SyncReport {
        conflicts: plan.conflicts.len(),
        remote_missing: plan.remote_missing.len(),
        ..SyncReport::default()
    };
    let mut reconciled: std::collections::HashSet<String> = std::collections::HashSet::new();

    // Already-in-sync pairs: just (re)record their fingerprint so a first sync
    // over an old one-way link becomes incremental next time.
    for entry in &plan.in_sync {
        state.record(
            &entry.external_ref,
            entry.gh_updated_at,
            entry.local_updated,
        );
        state.record_content_hash(&entry.external_ref, entry.gh_content_hash);
        reconciled.insert(entry.external_ref.clone());
        report.in_sync += 1;
    }

    // --- Pulls (remote → local), through the unified write path. ---
    for pull in &plan.pull_create {
        pull_create(store, &pull.staged, state)?;
        reconciled.insert(pull.staged.external_ref.clone());
        report.pulled_created += 1;
    }
    for pull in &plan.pull_update {
        pull_update(store, pull, state)?;
        reconciled.insert(pull.staged.external_ref.clone());
        report.pulled_updated += 1;
    }

    // --- Pushes (local → remote), writing the link back locally. ---
    // GitHub rejecting one item (a 4xx about it) skips that item, not the run: it
    // is left out of `reconciled` and its fingerprint is not recorded, so the next
    // run retries it.
    for push in &plan.push_create {
        match push_create(crab, owner, repo, push, store, state, &mut report).await? {
            PushOutcome::Done(external_ref) => {
                reconciled.insert(external_ref);
                report.pushed_created += 1;
            }
            PushOutcome::Skipped { item, created } => {
                report.pushed_created += usize::from(created);
                report.skipped.push(item);
            }
        }
    }
    for push in &plan.push_update {
        match push_update(crab, owner, repo, push, state, &mut report).await? {
            PushOutcome::Done(external_ref) => {
                reconciled.insert(external_ref);
                report.pushed_updated += 1;
            }
            PushOutcome::Skipped { item, .. } => report.skipped.push(item),
        }
    }

    Ok(ApplyOutcome { report, reconciled })
}

/// How a push to GitHub ended for one item.
enum PushOutcome {
    /// Pushed; carries the item's `external_ref`.
    Done(String),
    /// GitHub rejected the item. `created` is whether the issue nevertheless
    /// exists (a create whose follow-up close was rejected).
    Skipped { item: SkippedItem, created: bool },
}

/// Create a brand-new local item from a remote issue (mirrors the importer's
/// write path), then record the sync fingerprint.
fn pull_create(
    store: &dyn Store,
    staged: &StagedIssue,
    state: &mut SyncState,
) -> Result<(), ImportError> {
    let now = truncate(Utc::now());
    let id = store.new_id()?;
    let frontmatter = ItemFrontmatter {
        schema: CURRENT_SCHEMA_VERSION,
        id: id.clone(),
        title: staged.title.clone(),
        status: staged.status,
        item_type: staged.item_type.unwrap_or_default(),
        priority: staged.priority,
        created: now,
        updated: now,
        closed: staged.closed.or(if staged.status == ItemStatus::Closed {
            Some(now)
        } else {
            None
        }),
        assignee: staged.assignee.clone(),
        parent: None,
        labels: staged.labels.clone(),
        deps: staged.deps.clone().unwrap_or_default(),
        relates: Vec::new(),
        duplicates: Vec::new(),
        supersedes: Vec::new(),
        source_system: Some(SOURCE_GITHUB.to_owned()),
        external_ref: Some(staged.external_ref.clone()),
    };
    let item = Item {
        frontmatter,
        body: staged.body.clone(),
    };
    store.create_item(&item)?;
    state.record(&staged.external_ref, staged.updated_at, now);
    state.record_content_hash(
        &staged.external_ref,
        crate::sync::staged_fingerprint(staged),
    );
    Ok(())
}

/// Apply remote edits to an existing local item via the unified `apply_edit`
/// path, then record the post-edit fingerprint.
fn pull_update(
    store: &dyn Store,
    pull: &PullUpdate,
    state: &mut SyncState,
) -> Result<(), ImportError> {
    let staged = &pull.staged;

    // GitHub only distinguishes closed vs. not-closed, and `map_issue` collapses
    // every non-closed state to `Open` (there is no `in_progress` on GitHub, nor
    // does the `clove-meta` codec round-trip it). So only touch `status` when the
    // *closed-ness* actually differs — otherwise a remote non-status edit (a title
    // tweak, a pushed comment bumping `updated_at`, …) would silently demote a
    // local `in_progress` back to `open`. This mirrors `sync::content_equal`'s
    // `in_progress == open` equivalence.
    let current = store.get(&pull.clove_id)?;
    let local_closed = current.frontmatter.status == ItemStatus::Closed;
    let remote_closed = staged.status == ItemStatus::Closed;
    let status = (local_closed != remote_closed).then_some(staged.status);

    // Only touch the type when the remote meta actually declared one that
    // differs — a body with no `clove-meta` says nothing about the type, and a
    // pull must not reset a local `bug` to the default.
    let item_type = staged
        .item_type
        .filter(|t| *t != current.frontmatter.item_type);

    let req = EditRequest {
        title: Some(staged.title.clone()),
        body: Some(staged.body.clone()),
        status,
        priority: Some(staged.priority),
        item_type,
        // `Some(None)` clears the assignee when GitHub has none, `Some(Some(x))`
        // sets it — exactly the tri-state `apply_edit` expects.
        assignee: Some(staged.assignee.clone()),
        labels: Some(LabelEdit::Set(staged.labels.clone())),
    };
    let now = Utc::now();

    // Remote dep edits ride the dedicated cycle-validated graph ops (deps are
    // round-tripped through `clove-meta`, so without this a dep added on one
    // clone is never applied on another — and the other clone's next push then
    // deletes it from the remote record). `None` means the body carried no
    // meta at all: dep ownership unknown, leave local deps alone. Applied
    // BEFORE `apply_edit` so the fingerprint recorded from `apply_edit`'s
    // returned `updated` covers these writes too. A single edge that fails
    // (target not present locally, would-cycle) is skipped rather than
    // aborting the whole sync.
    if let Some(remote_deps) = &staged.deps {
        let want: std::collections::BTreeSet<&CloveId> = remote_deps.iter().collect();
        let have: std::collections::BTreeSet<&CloveId> = current.frontmatter.deps.iter().collect();
        for dep in want.difference(&have) {
            let _ = store.dep_add(&pull.clove_id, dep);
        }
        for dep in have.difference(&want) {
            let _ = store.dep_remove(&pull.clove_id, dep);
        }
    }

    let value = store.apply_edit(&pull.clove_id, &req)?;
    let local_updated = value_updated(&value).unwrap_or_else(|| truncate(now));
    state.record(&staged.external_ref, staged.updated_at, local_updated);
    state.record_content_hash(
        &staged.external_ref,
        crate::sync::staged_fingerprint(staged),
    );
    Ok(())
}

/// Create the issue on GitHub, write the new `external_ref` back onto the local
/// item, and record the fingerprint. The write-back is what makes a subsequent
/// sync/export an UPDATE rather than a duplicate CREATE.
async fn push_create(
    crab: &Octocrab,
    owner: &str,
    repo: &str,
    push: &PushCreate,
    store: &dyn Store,
    state: &mut SyncState,
    report: &mut SyncReport,
) -> Result<PushOutcome, ImportError> {
    let item = &push.item;
    let handler = crab.issues(owner, repo);
    let create_operation = format!("issue create for {}", push.clove_id);
    let mut send_assignee = item.assignee.is_some();
    let created: Issue = loop {
        let attempt = call_item(false, &create_operation, || {
            let mut builder = handler.create(&item.title).body(item.body.clone());
            if !item.labels.is_empty() {
                builder = builder.labels(item.labels.clone());
            }
            if let (true, Some(assignee)) = (send_assignee, &item.assignee) {
                builder = builder.assignees(vec![assignee.clone()]);
            }
            builder.send()
        })
        .await;
        match attempt {
            Ok(created) => break created,
            // Nothing was created, so retrying without the assignee is safe.
            Err(CallFailure::Rejected {
                assignees: Some(_), ..
            }) if send_assignee => {
                send_assignee = false;
                report.warnings.push(format!(
                    "GitHub rejected the assignee `{}` of {}; created the issue without it",
                    item.assignee.as_deref().unwrap_or_default(),
                    push.clove_id
                ));
            }
            Err(CallFailure::Rejected { error, .. }) => {
                return Ok(PushOutcome::Skipped {
                    item: skipped_item(&push.clove_id, None, &error),
                    created: false,
                });
            }
            Err(CallFailure::Fatal(error)) => return Err(error),
        }
    };

    // Link the local item to the new issue *before* anything else can fail: the
    // write-back of `external_ref` (plus the recorded fingerprint) is what makes
    // every later step idempotent. If it only happened after the close PATCH
    // below, a failed close would leave the created issue unlinked and the next
    // sync would create a duplicate on both sides.
    let external_ref = crate::github::external_ref_for(created.number);
    let local_updated = link_local(store, &push.clove_id, &external_ref)?;
    state.record(&external_ref, Some(created.updated_at), local_updated);
    // The issue GitHub just created is OPEN regardless of `item.closed` — hash
    // the open variant until the close below actually lands.
    let open_item = crate::github::ExportItem {
        closed: false,
        ..item.clone()
    };
    state.record_content_hash(&external_ref, crate::sync::export_fingerprint(&open_item));

    // GitHub created the issue OPEN. If the local item is closed, close it too so
    // the two sides actually match after a create-of-a-closed-item. The close is a
    // second PATCH that bumps `updated_at` past the create response's — record the
    // *close* response's timestamp as the fingerprint, or the next sync would see
    // a spurious remote change and pull-update the just-created item.
    if item.closed {
        let operation = format!(
            "issue close of {} for {}",
            issue_label(created.number),
            push.clove_id
        );
        match call_item(true, &operation, || {
            handler
                .update(created.number)
                .state(octocrab::models::IssueState::Closed)
                .send()
        })
        .await
        {
            Ok(closed) => {
                let closed: Issue = closed;
                state.record(&external_ref, Some(closed.updated_at), local_updated);
                state.record_content_hash(&external_ref, crate::sync::export_fingerprint(item));
            }
            Err(failure) => {
                // The pair is linked but the remote is still OPEN while the
                // local item is closed. With accurate clocks the next sync
                // would call that pair in_sync forever — backdate the recorded
                // local clock so it re-examines and re-pushes the close as an
                // update.
                state.record(
                    &external_ref,
                    Some(created.updated_at),
                    crate::sync::epoch(),
                );
                return match failure {
                    CallFailure::Rejected { error, .. } => Ok(PushOutcome::Skipped {
                        item: skipped_item(&push.clove_id, Some(&external_ref), &error),
                        created: true,
                    }),
                    CallFailure::Fatal(error) => Err(error),
                };
            }
        }
    }

    Ok(PushOutcome::Done(external_ref))
}

/// Update an existing GitHub issue from local fields, then record the fingerprint.
async fn push_update(
    crab: &Octocrab,
    owner: &str,
    repo: &str,
    push: &PushUpdate,
    state: &mut SyncState,
    report: &mut SyncReport,
) -> Result<PushOutcome, ImportError> {
    let item = &push.item;
    let external_ref = crate::github::external_ref_for(push.number);
    let handler = crab.issues(owner, repo);
    let want_closed = item.closed;

    // Preserve extra GitHub assignees a human added: keep every current assignee
    // except the one clove previously owned (recorded `synced_assignee`), and put
    // clove's current assignee first. This replaces clove's primary on
    // reassignment without dropping the others.
    let prev_owned = state
        .entries
        .get(&external_ref)
        .and_then(|e| e.synced_assignee.clone());
    let mut dropped_assignee = false;
    let mut assignees: Vec<String> = Vec::new();
    if let Some(primary) = &item.assignee {
        assignees.push(primary.clone());
    }
    for login in &push.gh_assignees {
        if Some(login.as_str()) != prev_owned.as_deref() && !assignees.contains(login) {
            assignees.push(login.clone());
        }
    }

    // Preserve a human's close reason (`not_planned`) rather than resetting it to
    // `completed` when clove pushes a close.
    let preserved_reason = want_closed
        .then(|| push.gh_state_reason.as_deref().and_then(parse_state_reason))
        .flatten();

    // Send the reconciled assignee list whenever clove is involved in assignment
    // now or was at the last sync — so an unassign locally actually clears the
    // assignee on GitHub (possibly an empty list). When clove never owned an
    // assignee, leave GitHub's untouched (don't disturb purely-human assignees).
    let touch_assignees = item.assignee.is_some() || prev_owned.is_some();

    let operation = format!(
        "issue update of {} for {}",
        issue_label(push.number),
        push.clove_id
    );
    let updated: Issue = loop {
        let attempt = call_item(true, &operation, || {
            let state_param = if want_closed {
                octocrab::models::IssueState::Closed
            } else {
                octocrab::models::IssueState::Open
            };
            let mut builder = handler
                .update(push.number)
                .title(&item.title)
                .body(&item.body)
                .state(state_param);
            // Always send labels on update — clove's label set is authoritative on
            // push (a pull mirrors GitHub's labels down via `LabelEdit::Set`). Sending
            // an empty list is what clears the last label; skipping the field would
            // leave a removed label stranded on GitHub forever, and the fresh
            // fingerprint would then mark the pair as synced so it is never re-examined.
            builder = builder.labels(&item.labels);
            if touch_assignees {
                builder = builder.assignees(&assignees);
            }
            if let Some(reason) = &preserved_reason {
                builder = builder.state_reason(reason.clone());
            }
            builder.send()
        })
        .await;
        match attempt {
            Ok(updated) => break updated,
            // Retry once without the logins GitHub named (clove's own assignee
            // when it named none), keeping the rest of the assignee list.
            Err(CallFailure::Rejected {
                assignees: Some(refused),
                ..
            }) if touch_assignees && !dropped_assignee => {
                dropped_assignee = true;
                let before = assignees.len();
                assignees.retain(|login| !refused.contains(login));
                if assignees.len() == before {
                    if let Some(primary) = &item.assignee {
                        assignees.retain(|login| login != primary);
                    }
                }
                report.warnings.push(format!(
                    "GitHub rejected an assignee of {} on {}; updated the issue without {}",
                    push.clove_id,
                    issue_label(push.number),
                    if refused.is_empty() {
                        "it".to_owned()
                    } else {
                        format!("`{}`", refused.join("`, `"))
                    }
                ));
            }
            Err(CallFailure::Rejected { error, .. }) => {
                return Ok(PushOutcome::Skipped {
                    item: skipped_item(&push.clove_id, Some(&external_ref), &error),
                    created: false,
                });
            }
            Err(CallFailure::Fatal(error)) => return Err(error),
        }
    };

    state.record(&external_ref, Some(updated.updated_at), push.local_updated);
    state.record_content_hash(&external_ref, crate::sync::export_fingerprint(item));
    Ok(PushOutcome::Done(external_ref))
}

/// How an error names a GitHub issue: `#3 (gh-3)`, the number and the
/// `external_ref` the local item carries.
fn issue_label(number: u64) -> String {
    format!("#{number} ({})", crate::github::external_ref_for(number))
}

/// Map a GitHub `state_reason` string onto octocrab's enum (`None` for the
/// open-issue `reopened`, which a close never sets, or an unknown value).
fn parse_state_reason(raw: &str) -> Option<octocrab::models::issues::IssueStateReason> {
    use octocrab::models::issues::IssueStateReason;
    match raw.trim().to_lowercase().as_str() {
        "completed" => Some(IssueStateReason::Completed),
        "not_planned" | "not planned" => Some(IssueStateReason::NotPlanned),
        "duplicate" => Some(IssueStateReason::Duplicate),
        _ => None,
    }
}

/// The linked GitHub issues of the store as `(external_ref, local id)`, sorted by
/// ref for a deterministic order of API calls. A fresh scan, so items a run just
/// created on either side are included.
fn linked_issues(store: &dyn Store) -> Result<Vec<(String, CloveId)>, ImportError> {
    let mut linked: Vec<(String, CloveId)> = build_external_ref_index(store)?
        .into_iter()
        .filter_map(|(ext, existing)| ext.starts_with("gh-").then_some((ext, existing.id)))
        .collect();
    linked.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(linked)
}

/// One linked issue's fetched comment thread and what the sync would do to it.
struct ThreadPlan {
    clove_id: CloveId,
    external_ref: String,
    number: u64,
    gh: Vec<GhComment>,
    local_count: usize,
    plan: CommentPlan,
}

/// Plan the comment thread of every linked issue that is not provably idle (see
/// [`comment_thread_idle`]), fetching GitHub's side but writing nothing. Shared
/// by `--dry-run` and the apply.
async fn plan_comment_threads(
    crab: &Octocrab,
    owner: &str,
    repo: &str,
    store: &dyn Store,
    state: &SyncState,
    remote_comment_counts: &std::collections::HashMap<String, u64>,
    policy: ConflictPolicy,
) -> Result<Vec<ThreadPlan>, ImportError> {
    let touched = touched_issue_numbers(crab, owner, repo, state).await?;
    let mut threads = Vec::new();
    for (external_ref, id) in linked_issues(store)? {
        let Some(number) = parse_gh_number(&external_ref) else {
            continue;
        };
        let local: Vec<LocalComment> = store
            .comments(&id)?
            .into_iter()
            .map(|c| LocalComment {
                key: c.key,
                author: c.author,
                body: c.body,
                modified: c.modified,
            })
            .collect();

        let entry = state.entries.get(&external_ref);
        if let Some(entry) = entry {
            let remote_touched = touched.as_ref().is_none_or(|set| set.contains(&number));
            let remote_count = remote_comment_counts.get(&external_ref).copied();
            if comment_thread_idle(entry, &local, remote_count, remote_touched) {
                continue;
            }
        }

        let gh = fetch_comments(crab, owner, repo, number, &id).await?;
        let fallback;
        let entry = match entry {
            Some(entry) => entry,
            None => {
                fallback = default_entry();
                &fallback
            }
        };
        let plan = plan_comments(&gh, &local, entry, policy);
        threads.push(ThreadPlan {
            clove_id: id,
            external_ref,
            number,
            gh,
            local_count: local.len(),
            plan,
        });
    }
    Ok(threads)
}

/// The numbers of issues carrying a comment that someone other than this sync
/// created or edited since the last comment sync, from one repo-wide listing of
/// recently updated comments. `None` when there is no earlier sync to measure
/// from (or GitHub omitted an issue url), meaning every thread must be checked.
async fn touched_issue_numbers(
    crab: &Octocrab,
    owner: &str,
    repo: &str,
    state: &SyncState,
) -> Result<Option<std::collections::HashSet<u64>>, ImportError> {
    const PAGE_SIZE: usize = 100;
    let Some(checked_at) = state.comments_checked_at else {
        return Ok(None);
    };
    // Generous: re-checking a thread is cheap, missing an edit is not.
    let since = (checked_at - chrono::Duration::minutes(5))
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let route = format!("/repos/{owner}/{repo}/issues/comments");
    let operation = format!("recent comment listing of {owner}/{repo}");
    let mut touched = std::collections::HashSet::new();
    for page in 1u32.. {
        let query = serde_json::json!({ "since": since, "per_page": PAGE_SIZE, "page": page });
        let batch: Vec<Value> =
            with_retry(true, &operation, || crab.get(&route, Some(&query))).await?;
        for comment in &batch {
            let number = comment
                .get("issue_url")
                .and_then(Value::as_str)
                .and_then(|url| url.rsplit('/').next())
                .and_then(|tail| tail.parse::<u64>().ok());
            let (Some(comment_id), Some(number)) =
                (comment.get("id").and_then(Value::as_u64), number)
            else {
                return Ok(None);
            };
            let updated_at = comment
                .get("updated_at")
                .and_then(Value::as_str)
                .and_then(clove_types::parse_rfc3339);
            let ours = state
                .entries
                .get(&crate::github::external_ref_for(number))
                .is_some_and(|entry| {
                    entry.comment_links.iter().any(|link| {
                        link.gh_id == comment_id
                            && updated_at.is_some()
                            && link.gh_updated_at == updated_at
                    })
                });
            if !ours {
                touched.insert(number);
            }
        }
        if batch.len() < PAGE_SIZE {
            break;
        }
    }
    Ok(Some(touched))
}

/// Apply the planned comment writes of every thread, recording each pair as it
/// is made so a mid-run failure never loses a link. A write GitHub rejects skips
/// that comment (listed in `report.skipped`, retried next sync) instead of
/// aborting. Returns whether every write went through.
async fn apply_comment_threads(
    crab: &Octocrab,
    owner: &str,
    repo: &str,
    store: &dyn Store,
    state: &mut SyncState,
    threads: Vec<ThreadPlan>,
    report: &mut SyncReport,
) -> Result<bool, ImportError> {
    let mut all_applied = true;
    for thread in threads {
        let ThreadPlan {
            clove_id: id,
            external_ref,
            number,
            gh,
            local_count,
            plan,
        } = thread;
        let handler = crab.issues(owner, repo);
        let entry = state
            .entries
            .entry(external_ref.clone())
            .or_insert_with(default_entry);
        let gh_updated = |gh_id: u64| gh.iter().find(|c| c.id == gh_id).and_then(|c| c.updated_at);
        let skip = |report: &mut SyncReport, error: &ImportError| {
            report
                .skipped
                .push(skipped_item(id.as_str(), Some(&external_ref), error));
        };
        let mut pulled = 0usize;
        let mut pushed = 0usize;
        let mut deleted_local = 0usize;
        let mut deleted_remote = 0usize;
        let mut thread_applied = true;

        for conflict in &plan.conflicts {
            report.comments.conflicts.push(CommentConflictRef {
                clove_id: id.to_string(),
                external_ref: external_ref.clone(),
                gh_comment_id: conflict.gh_id,
                comment: conflict.key.clone(),
                resolution: conflict.resolution.clone(),
            });
        }
        for relink in &plan.relink {
            if let Some(hash) = relink.legacy_hash {
                entry.release_comment_hash(hash);
            }
            entry.set_comment_link(relink.link.clone());
        }
        for gh_id in &plan.unlink {
            entry.drop_comment_link(*gh_id);
        }

        for edit in &plan.update_local {
            store.update_comment(&id, &edit.key, &edit.body)?;
            entry.set_comment_link(CommentLink {
                gh_id: edit.gh_id,
                local_key: edit.key.clone(),
                base_hash: link_hash(&edit.body),
                gh_updated_at: gh_updated(edit.gh_id),
            });
            report.comments.update_local += 1;
        }
        for edit in &plan.update_remote {
            let operation = format!("comment edit on {} for {id}", issue_label(number));
            // octocrab's `update_comment` POSTs; the edit endpoint is a PATCH.
            let route = format!("/repos/{owner}/{repo}/issues/comments/{}", edit.gh_id);
            let payload = serde_json::json!({ "body": edit.body });
            let sent = call_item(true, &operation, || {
                crab.patch::<octocrab::models::issues::Comment, _, _>(&route, Some(&payload))
            })
            .await;
            match sent {
                Ok(updated) => {
                    entry.set_comment_link(CommentLink {
                        gh_id: edit.gh_id,
                        local_key: edit.key.clone(),
                        base_hash: link_hash(&edit.body),
                        gh_updated_at: updated.updated_at,
                    });
                    advance_remote_clock(entry, updated.updated_at);
                    report.comments.update_remote += 1;
                }
                Err(CallFailure::Rejected { error, .. }) => {
                    skip(report, &error);
                    thread_applied = false;
                }
                Err(CallFailure::Fatal(error)) => return Err(error),
            }
        }

        for removal in &plan.delete_local {
            store.delete_comment(&id, &removal.key)?;
            entry.drop_comment_link(removal.gh_id);
            report
                .comments
                .delete_local
                .push(CommentRef::new(id.as_str(), &external_ref, removal));
            deleted_local += 1;
        }
        for removal in &plan.delete_remote {
            let operation = format!("comment delete on {} for {id}", issue_label(number));
            let sent = call_item(true, &operation, || {
                handler.delete_comment(removal.gh_id.into())
            })
            .await;
            match sent {
                // A retry after a lost response finds the comment already gone.
                Ok(()) | Err(CallFailure::Rejected { status: 404, .. }) => {
                    entry.drop_comment_link(removal.gh_id);
                    report.comments.delete_remote.push(CommentRef::new(
                        id.as_str(),
                        &external_ref,
                        removal,
                    ));
                    deleted_remote += 1;
                }
                Err(CallFailure::Rejected { error, .. }) => {
                    skip(report, &error);
                    thread_applied = false;
                }
                Err(CallFailure::Fatal(error)) => return Err(error),
            }
        }

        for comment in &plan.pull {
            let when = comment.created_at.unwrap_or_else(Utc::now);
            let author = if comment.author.trim().is_empty() {
                "github".to_owned()
            } else {
                comment.author.clone()
            };
            let key = store.add_comment(&id, &author, &comment.body, when)?;
            entry.set_comment_link(CommentLink {
                gh_id: comment.id,
                local_key: key,
                base_hash: link_hash(&comment.body),
                gh_updated_at: comment.updated_at,
            });
            pulled += 1;
        }
        for comment in &plan.push {
            let body = comment.body.clone();
            // A comment create is non-idempotent — a retry on a lost response would
            // post a duplicate — so it is never retried (see `with_retry`).
            let operation = format!("comment post on {} for {id}", issue_label(number));
            let sent = call_item(false, &operation, || {
                handler.create_comment(number, body.clone())
            })
            .await;
            match sent {
                Ok(created) => {
                    entry.set_comment_link(CommentLink {
                        gh_id: created.id.into_inner(),
                        local_key: comment.key.clone(),
                        base_hash: link_hash(&comment.body),
                        gh_updated_at: created.updated_at.or(Some(created.created_at)),
                    });
                    // Posting a comment bumps the issue's `updated_at` on GitHub.
                    // Advance the recorded remote fingerprint to the new comment's
                    // timestamp so the next sync doesn't see a spurious remote
                    // change and pull-update (or false-conflict) the item whose
                    // only "remote change" was clove's own comment push.
                    advance_remote_clock(entry, Some(created.created_at));
                    pushed += 1;
                }
                Err(CallFailure::Rejected { error, .. }) => {
                    skip(report, &error);
                    thread_applied = false;
                }
                Err(CallFailure::Fatal(error)) => return Err(error),
            }
        }

        report.comments.pull += pulled;
        report.comments.push += pushed;
        report.comments_pulled += pulled;
        report.comments_pushed += pushed;

        // Set only when every write landed (an early `?` above leaves them stale
        // too): the next sync's idle check compares against what each side now
        // holds, and a skipped write must make it look again.
        if thread_applied {
            entry.synced_gh_comments = Some((gh.len() + pushed - deleted_remote) as u64);
            entry.synced_local_comments = Some((local_count + pulled - deleted_local) as u64);
        } else {
            entry.synced_gh_comments = None;
            entry.synced_local_comments = None;
            all_applied = false;
        }
    }
    Ok(all_applied)
}

/// Move the recorded remote clock forward to `at`: a comment write bumps the
/// issue's `updated_at` on GitHub.
fn advance_remote_clock(entry: &mut crate::sync::SyncEntry, at: Option<DateTime<Utc>>) {
    if let Some(at) = at {
        if entry.gh_updated_at.is_none_or(|cur| at > cur) {
            entry.gh_updated_at = Some(at);
        }
    }
}

/// A default [`crate::sync::SyncEntry`] for an issue that somehow lacks a
/// fingerprint entry by comment-sync time (defensive — every linked issue gets
/// one during the item apply).
fn default_entry() -> crate::sync::SyncEntry {
    crate::sync::SyncEntry::new(None, truncate(Utc::now()))
}

/// Fetch every comment on an issue (paginated), reduced to [`GhComment`].
/// `clove_id` is the linked local item, named in a failure.
async fn fetch_comments(
    crab: &Octocrab,
    owner: &str,
    repo: &str,
    number: u64,
    clove_id: &CloveId,
) -> Result<Vec<GhComment>, ImportError> {
    let operation = format!("comment fetch of {} for {clove_id}", issue_label(number));
    let handler = crab.issues(owner, repo);
    let first = with_retry(true, &operation, || {
        handler.list_comments(number).per_page(100u8).send()
    })
    .await?;
    let all = crab
        .all_pages(first)
        .await
        .map_err(|err| net_err(&operation, &err))?;
    Ok(all
        .into_iter()
        .map(|c| GhComment {
            id: c.id.into_inner(),
            author: c.user.login,
            body: c.body.unwrap_or_default(),
            created_at: Some(c.created_at),
            updated_at: c.updated_at,
        })
        .collect())
}

/// Stamp `source_system = github` + `external_ref` onto a local item and persist
/// it, returning the item's new `updated` (used as the sync fingerprint).
///
/// Uses `update_with`, which holds the store-wide write lock across the whole
/// read-modify-write. A lock-free `get` followed by a locking `update` would let
/// a concurrent CLI/web/MCP/daemon write land in between and be silently
/// clobbered — and because the write persists a whole-frontmatter snapshot, the
/// clobbered edit is lost entirely. The per-repo `.clove/sync/<repo>.lock` this
/// runs under does not help: it excludes other *syncs*, not other writers.
fn link_local(
    store: &dyn Store,
    clove_id: &str,
    external_ref: &str,
) -> Result<DateTime<Utc>, ImportError> {
    let id = CloveId::new(clove_id).map_err(|e| ImportError::Record {
        message: format!("invalid local id `{clove_id}`: {e}"),
    })?;
    store.link_external(&id, SOURCE_GITHUB, external_ref)
}

/// Parse the `updated` field out of an `apply_edit` result object.
fn value_updated(value: &Value) -> Option<DateTime<Utc>> {
    value
        .get("updated")
        .and_then(Value::as_str)
        .and_then(clove_types::parse_rfc3339)
}

/// Truncate to whole milliseconds (the canonical on-disk timestamp precision).
fn truncate(ts: DateTime<Utc>) -> DateTime<Utc> {
    clove_types::truncate_to_millis(ts)
}

/// The base backoff delay between network retries. Overridable via
/// `CLOVE_GITHUB_RETRY_MS` in a `test-knobs` build, so the test-suite can keep
/// retries instant.
fn retry_base_ms() -> u64 {
    if !cfg!(feature = "test-knobs") {
        return 500;
    }
    std::env::var("CLOVE_GITHUB_RETRY_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(500)
}

/// Whether an octocrab error is worth retrying (transient), as opposed to a
/// permanent failure a retry can never fix.
///
/// A GitHub 4xx (bad token, not found, 422 validation) is permanent — retrying
/// only triples latency — so only 5xx, `408 Request Timeout`, and `429 Too Many
/// Requests` are retried. Transport-level failures (hyper / tower `Service`)
/// *may* be transient, but whether the server already committed the request is
/// unknown; the caller decides via `idempotent` whether replaying is safe.
fn is_transient(err: &octocrab::Error) -> bool {
    use octocrab::Error;
    match err {
        Error::GitHub { source, .. } => {
            let code = source.status_code.as_u16();
            code == 408 || code == 429 || (500..=599).contains(&code)
        }
        Error::Hyper { .. } | Error::Service { .. } => true,
        // Deterministic client-side failures (serde, URI, JWT, …): never transient.
        _ => false,
    }
}

/// Run an octocrab call with bounded exponential backoff (up to 3 attempts).
///
/// A transient blip (network, 5xx, secondary rate-limit) shouldn't abort a sync
/// mid-stream and leave the two sides half-reconciled, so a retryable call is
/// retried a few times before the error is surfaced. The happy path never sleeps.
///
/// `idempotent` gates retrying: an update/fetch can be safely replayed, but a
/// **create** (issue or comment) must not — if the server processed the POST but
/// the response was lost, a retry would create a duplicate. Non-idempotent calls
/// therefore make exactly one attempt. Permanent errors (4xx) are never retried
/// regardless (see [`is_transient`]).
///
/// `operation` names the call and its item for the error (see [`net_err`]).
async fn with_retry<T, F, Fut>(idempotent: bool, operation: &str, op: F) -> Result<T, ImportError>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, octocrab::Error>>,
{
    call_item(idempotent, operation, op)
        .await
        .map_err(CallFailure::into_error)
}

/// Why an item-scoped call failed for good.
enum CallFailure {
    /// GitHub refused this one request (a 4xx about the item, see
    /// [`is_item_rejection`]); the rest of the sync can proceed. `assignees` is
    /// `Some` with the logins GitHub named when the validation error is about the
    /// `assignees` field.
    Rejected {
        error: ImportError,
        status: u16,
        assignees: Option<Vec<String>>,
    },
    /// Anything that should abort the run: auth, rate limit, 5xx, network.
    Fatal(ImportError),
}

impl CallFailure {
    fn into_error(self) -> ImportError {
        match self {
            CallFailure::Rejected { error, .. } | CallFailure::Fatal(error) => error,
        }
    }
}

/// A 4xx that condemns only the one item it was about. Auth (401, 403) and
/// throttling (408, 429) say the whole run is failing, so they stay fatal.
fn is_item_rejection(status: u16) -> bool {
    (400..=499).contains(&status) && ![401, 403, 408, 429].contains(&status)
}

fn classify_failure(operation: &str, err: &octocrab::Error) -> CallFailure {
    let error = net_err(operation, err);
    let octocrab::Error::GitHub { source, .. } = err else {
        return CallFailure::Fatal(error);
    };
    if !is_item_rejection(source.status_code.as_u16()) {
        return CallFailure::Fatal(error);
    }
    let mut assignee_errors = source
        .errors
        .iter()
        .flatten()
        .filter(|entry| entry.get("field").and_then(Value::as_str) == Some("assignees"))
        .peekable();
    let assignees = assignee_errors.peek().is_some().then(|| {
        assignee_errors
            .filter_map(|entry| {
                entry
                    .get("value")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .collect()
    });
    CallFailure::Rejected {
        error,
        status: source.status_code.as_u16(),
        assignees,
    }
}

fn skipped_item(clove_id: &str, external_ref: Option<&str>, error: &ImportError) -> SkippedItem {
    SkippedItem {
        clove_id: clove_id.to_owned(),
        external_ref: external_ref.map(str::to_owned),
        reason: error.to_string(),
    }
}

/// [`with_retry`] that keeps a per-item GitHub rejection distinguishable from a
/// run-aborting failure.
async fn call_item<T, F, Fut>(
    idempotent: bool,
    operation: &str,
    mut op: F,
) -> Result<T, CallFailure>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, octocrab::Error>>,
{
    const MAX_ATTEMPTS: u32 = 3;
    let mut delay = std::time::Duration::from_millis(retry_base_ms());
    let mut attempt = 0;
    loop {
        match op().await {
            Ok(value) => return Ok(value),
            Err(err) => {
                attempt += 1;
                if attempt >= MAX_ATTEMPTS || !idempotent || !is_transient(&err) {
                    return Err(classify_failure(operation, &err));
                }
                tokio::time::sleep(delay).await;
                delay = delay.saturating_mul(2);
            }
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::open_sync_lock;

    /// A lock file planted as a symlink must not be followed — not even to
    /// create the file it points at.
    #[test]
    fn a_planted_lock_symlink_is_not_followed() {
        let dir = tempfile::tempdir().unwrap();
        let root = camino::Utf8Path::from_path(dir.path()).unwrap();
        let state = root.join(".clove/sync/github/o_r.json");
        std::fs::create_dir_all(state.parent().unwrap()).unwrap();
        let victim = root.join("created-elsewhere");
        std::os::unix::fs::symlink(&victim, state.with_extension("lock")).unwrap();
        assert!(open_sync_lock(&state).is_err());
        assert!(!victim.exists());
    }
}
