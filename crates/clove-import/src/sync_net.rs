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

use chrono::{DateTime, Timelike, Utc};
use serde_json::{Map, Value};

use clove_core::write::write_item_file;
use clove_core::{apply_edit, ItemStore};
use clove_types::id::new_id;
use clove_types::model::CURRENT_SCHEMA_VERSION;
use clove_types::{CloveId, EditRequest, Item, ItemFrontmatter, ItemStatus, ItemType, LabelEdit};

use octocrab::models::issues::Issue;
use octocrab::Octocrab;

use clove_core::comments::{add_comment_at, list_comments};

use crate::error::ImportError;
use crate::github::net::{build_client, fetch_all, net_err, parse_repo_spec};
use crate::github::{parse_gh_number, StagedIssue};
use crate::map::build_external_ref_index;
use crate::sync::{
    body_hash, plan_comments, plan_sync, ConflictPolicy, GhComment, LocalComment, PullUpdate,
    PushCreate, PushUpdate, SyncPlan, SyncReport, SyncState, SyncSummary,
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
    store: &ItemStore,
    prefix: &str,
    policy: ConflictPolicy,
    sync_comments: bool,
    dry_run: bool,
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

    let plan: SyncPlan = plan_sync(&issues, &local, &state, policy)
        .map_err(|message| ImportError::Record { message })?;
    let summary = plan.summary();

    if dry_run {
        return Ok((summary, None));
    }

    let apply_result = rt.block_on(async {
        let crab = build_client()?;
        let outcome = apply_plan(&crab, &owner, &repo, plan, store, prefix, &mut state).await?;
        let mut report = outcome.report;
        if sync_comments {
            let (pulled, pushed) =
                sync_all_comments(&crab, &owner, &repo, store, &mut state).await?;
            report.comments_pulled = pulled;
            report.comments_pushed = pushed;
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
        std::fs::create_dir_all(parent).map_err(|source| ImportError::Source {
            path: parent.to_owned(),
            message: format!("failed to create sync dir: {source}"),
        })?;
    }
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(lock_path.as_std_path())
        .map_err(|source| ImportError::Source {
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
    store: &ItemStore,
    state: &mut SyncState,
    reconciled: &std::collections::HashSet<String>,
) -> Result<(), ImportError> {
    let (frontmatters, _errors) = store.scan_frontmatter()?;
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
fn local_objects(store: &ItemStore) -> Result<Vec<Map<String, Value>>, ImportError> {
    let (items, errors) = store.scan()?;
    // Refuse to sync when any local item file fails to parse. A dropped item is
    // invisible to the planner, so its linked remote issue would be seen as having
    // no local counterpart and pull-created as a DUPLICATE local item carrying the
    // same `external_ref` — permanently forking the link. Fail loudly instead so
    // the user repairs the file first (`clove doctor` lists the broken ones).
    if let Some(err) = errors.first() {
        return Err(ImportError::Source {
            path: err.path().to_owned(),
            message: format!(
                "{} local item file(s) failed to parse; fix them before syncing \
                 (see `clove doctor`): {err}",
                errors.len()
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
    store: &ItemStore,
    prefix: &str,
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
        reconciled.insert(entry.external_ref.clone());
        report.in_sync += 1;
    }

    // --- Pulls (remote → local), through the unified write path. ---
    for pull in &plan.pull_create {
        pull_create(store, prefix, &pull.staged, state)?;
        reconciled.insert(pull.staged.external_ref.clone());
        report.pulled_created += 1;
    }
    for pull in &plan.pull_update {
        pull_update(store, pull, state)?;
        reconciled.insert(pull.staged.external_ref.clone());
        report.pulled_updated += 1;
    }

    // --- Pushes (local → remote), writing the link back locally. ---
    for push in &plan.push_create {
        let external_ref = push_create(crab, owner, repo, push, store, state).await?;
        reconciled.insert(external_ref);
        report.pushed_created += 1;
    }
    for push in &plan.push_update {
        push_update(crab, owner, repo, push, state).await?;
        reconciled.insert(crate::github::external_ref_for(push.number));
        report.pushed_updated += 1;
    }

    Ok(ApplyOutcome { report, reconciled })
}

/// Create a brand-new local item from a remote issue (mirrors the importer's
/// write path), then record the sync fingerprint.
fn pull_create(
    store: &ItemStore,
    prefix: &str,
    staged: &StagedIssue,
    state: &mut SyncState,
) -> Result<(), ImportError> {
    let now = truncate(Utc::now());
    let id = new_id(prefix, store.issues_dir())?;
    let frontmatter = ItemFrontmatter {
        schema: CURRENT_SCHEMA_VERSION,
        id: id.clone(),
        title: staged.title.clone(),
        status: staged.status,
        item_type: ItemType::default(),
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
        deps: staged.deps.clone(),
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
    write_item_file(&item, &store.path_for(&id))?;
    state.record(&staged.external_ref, staged.updated_at, now);
    Ok(())
}

/// Apply remote edits to an existing local item via the unified `apply_edit`
/// path, then record the post-edit fingerprint.
fn pull_update(
    store: &ItemStore,
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

    let req = EditRequest {
        title: Some(staged.title.clone()),
        body: Some(staged.body.clone()),
        status,
        priority: Some(staged.priority),
        item_type: None,
        // `Some(None)` clears the assignee when GitHub has none, `Some(Some(x))`
        // sets it — exactly the tri-state `apply_edit` expects.
        assignee: Some(staged.assignee.clone()),
        labels: Some(LabelEdit::Set(staged.labels.clone())),
    };
    let now = Utc::now();
    let value = apply_edit(store, &pull.clove_id, &req, now)?;
    let local_updated = value_updated(&value).unwrap_or_else(|| truncate(now));
    state.record(&staged.external_ref, staged.updated_at, local_updated);
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
    store: &ItemStore,
    state: &mut SyncState,
) -> Result<String, ImportError> {
    let item = &push.item;
    let handler = crab.issues(owner, repo);
    let created: Issue = with_retry(false, || {
        let mut builder = handler.create(&item.title).body(item.body.clone());
        if !item.labels.is_empty() {
            builder = builder.labels(item.labels.clone());
        }
        if let Some(assignee) = &item.assignee {
            builder = builder.assignees(vec![assignee.clone()]);
        }
        builder.send()
    })
    .await?;

    // GitHub created the issue OPEN. If the local item is closed, close it too so
    // the two sides actually match after a create-of-a-closed-item. The close is a
    // second PATCH that bumps `updated_at` past the create response's — record the
    // *close* response's timestamp as the fingerprint, or the next sync would see
    // a spurious remote change and pull-update the just-created item.
    let mut gh_updated = created.updated_at;
    if item.closed {
        let closed: Issue = with_retry(true, || {
            handler
                .update(created.number)
                .state(octocrab::models::IssueState::Closed)
                .send()
        })
        .await?;
        gh_updated = closed.updated_at;
    }

    let external_ref = crate::github::external_ref_for(created.number);
    let local_updated = link_local(store, &push.clove_id, &external_ref)?;
    state.record(&external_ref, Some(gh_updated), local_updated);
    Ok(external_ref)
}

/// Update an existing GitHub issue from local fields, then record the fingerprint.
async fn push_update(
    crab: &Octocrab,
    owner: &str,
    repo: &str,
    push: &PushUpdate,
    state: &mut SyncState,
) -> Result<(), ImportError> {
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

    let updated: Issue = with_retry(true, || {
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
    .await?;

    state.record(&external_ref, Some(updated.updated_at), push.local_updated);
    Ok(())
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

/// Reconcile comment threads for every issue linked to a local item, after the
/// main item apply has run. Iterates the *post-apply* `external_ref` index (a
/// fresh store scan), so issues just created on either side — which are absent
/// from the pre-apply fetch — are still covered. Returns `(pulled, pushed)`.
async fn sync_all_comments(
    crab: &Octocrab,
    owner: &str,
    repo: &str,
    store: &ItemStore,
    state: &mut SyncState,
) -> Result<(usize, usize), ImportError> {
    // Re-scan so freshly pull-created / push-created items are included. Sort by
    // ref for a deterministic order of API calls.
    let mut linked: Vec<(String, clove_types::CloveId)> = build_external_ref_index(store)?
        .into_iter()
        .filter_map(|(ext, existing)| ext.starts_with("gh-").then_some((ext, existing.id)))
        .collect();
    linked.sort_by(|a, b| a.0.cmp(&b.0));

    let mut pulled = 0;
    let mut pushed = 0;

    for (external_ref, id) in &linked {
        let Some(number) = parse_gh_number(external_ref) else {
            continue;
        };

        let gh = fetch_comments(crab, owner, repo, number).await?;
        let local: Vec<LocalComment> = list_comments(store.issues_dir(), id)?
            .into_iter()
            .map(|c| LocalComment {
                author: c.author,
                body: c.body,
            })
            .collect();

        let entry = state
            .entries
            .entry(external_ref.clone())
            .or_insert_with(default_entry);
        let plan = plan_comments(&gh, &local, entry);

        for comment in &plan.pull {
            let when = comment.created_at.unwrap_or_else(Utc::now);
            let author = if comment.author.trim().is_empty() {
                "github".to_owned()
            } else {
                comment.author.clone()
            };
            add_comment_at(store.issues_dir(), id, &author, &comment.body, when)?;
            entry.gh_comment_ids.insert(comment.id);
            entry.local_comment_hashes.insert(body_hash(&comment.body));
            pulled += 1;
        }
        for comment in &plan.push {
            let handler = crab.issues(owner, repo);
            let body = comment.body.clone();
            // A comment create is non-idempotent — a retry on a lost response would
            // post a duplicate — so it is never retried (see `with_retry`).
            let created =
                with_retry(false, || handler.create_comment(number, body.clone())).await?;
            entry.gh_comment_ids.insert(created.id.into_inner());
            entry.local_comment_hashes.insert(body_hash(&comment.body));
            // Posting a comment bumps the issue's `updated_at` on GitHub. Advance
            // the recorded remote fingerprint to the new comment's timestamp so the
            // next sync doesn't see a spurious remote change and pull-update (or
            // false-conflict) the item whose only "remote change" was clove's own
            // comment push.
            let created_at = created.created_at;
            if entry.gh_updated_at.is_none_or(|cur| created_at > cur) {
                entry.gh_updated_at = Some(created_at);
            }
            pushed += 1;
        }
    }
    Ok((pulled, pushed))
}

/// A default [`crate::sync::SyncEntry`] for an issue that somehow lacks a
/// fingerprint entry by comment-sync time (defensive — every linked issue gets
/// one during the item apply).
fn default_entry() -> crate::sync::SyncEntry {
    crate::sync::SyncEntry::new(None, truncate(Utc::now()))
}

/// Fetch every comment on an issue (paginated), reduced to [`GhComment`].
async fn fetch_comments(
    crab: &Octocrab,
    owner: &str,
    repo: &str,
    number: u64,
) -> Result<Vec<GhComment>, ImportError> {
    let handler = crab.issues(owner, repo);
    let first = with_retry(true, || {
        handler.list_comments(number).per_page(100u8).send()
    })
    .await?;
    let all = crab.all_pages(first).await.map_err(net_err)?;
    Ok(all
        .into_iter()
        .map(|c| GhComment {
            id: c.id.into_inner(),
            author: c.user.login,
            body: c.body.unwrap_or_default(),
            created_at: Some(c.created_at),
        })
        .collect())
}

/// Stamp `source_system = github` + `external_ref` onto a local item and persist
/// it, returning the item's new `updated` (used as the sync fingerprint). This
/// goes through `store.update`, so it shares the atomic-write + validation path.
fn link_local(
    store: &ItemStore,
    clove_id: &str,
    external_ref: &str,
) -> Result<DateTime<Utc>, ImportError> {
    let id = CloveId::new(clove_id).map_err(|e| ImportError::Record {
        message: format!("invalid local id `{clove_id}`: {e}"),
    })?;
    let mut item = store.get(&id)?;
    item.frontmatter.source_system = Some(SOURCE_GITHUB.to_owned());
    item.frontmatter.external_ref = Some(external_ref.to_owned());
    let saved = store.update(&item, Utc::now())?;
    Ok(saved.frontmatter.updated)
}

/// Parse the `updated` field out of an `apply_edit` result object.
fn value_updated(value: &Value) -> Option<DateTime<Utc>> {
    value
        .get("updated")
        .and_then(Value::as_str)
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&Utc))
}

/// Truncate to whole seconds (the canonical on-disk timestamp precision).
fn truncate(ts: DateTime<Utc>) -> DateTime<Utc> {
    ts.with_nanosecond(0).expect("zero nanos is valid")
}

/// The base backoff delay between network retries. Overridable via
/// `CLOVE_GITHUB_RETRY_MS` so the test-suite can keep retries instant.
fn retry_base_ms() -> u64 {
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
async fn with_retry<T, F, Fut>(idempotent: bool, mut op: F) -> Result<T, ImportError>
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
                    return Err(net_err(err));
                }
                tokio::time::sleep(delay).await;
                delay = delay.saturating_mul(2);
            }
        }
    }
}
