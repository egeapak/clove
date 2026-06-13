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

/// Run a full two-way sync of local items against `spec` (`owner/repo`).
///
/// `local` is the shaped `export_object` item list (built by the caller exactly
/// as `export github` does). Returns the plan summary always, and the apply
/// report when not a dry run. The sync state under
/// `.clove/sync/github/<owner>__<repo>.json` is loaded before planning and
/// persisted after a successful apply.
pub fn sync_github(
    spec: &str,
    store: &ItemStore,
    prefix: &str,
    policy: ConflictPolicy,
    sync_comments: bool,
    dry_run: bool,
) -> Result<(SyncSummary, Option<SyncReport>), ImportError> {
    let (owner, repo) = parse_repo_spec(spec)?;

    // The local side of the diff: each item's frontmatter plus its body, which is
    // all the planner reads. Built here (not by the caller) so the CLI and the
    // daemon share one path.
    let local = local_objects(store)?;

    let state_path = SyncState::path_for(store.repo_root(), spec);
    let mut state = SyncState::load(&state_path, spec);

    // Fetch remote issues. Even a dry run needs the current remote state to plan
    // a meaningful diff (mirrors `import github --dry-run`).
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

    let report = rt.block_on(async {
        let crab = build_client()?;
        let mut report = apply_plan(&crab, &owner, &repo, plan, store, prefix, &mut state).await?;
        if sync_comments {
            let (pulled, pushed) =
                sync_all_comments(&crab, &owner, &repo, store, &mut state).await?;
            report.comments_pulled = pulled;
            report.comments_pushed = pushed;
        }
        Ok::<SyncReport, ImportError>(report)
    })?;

    state.save(&state_path)?;
    Ok((summary, Some(report)))
}

/// Build the local side of the diff: one object per item, its serialized
/// frontmatter plus `body` (the exact fields [`plan_sync`] / `build_export_item`
/// read). Parse failures are dropped, matching the export path.
fn local_objects(store: &ItemStore) -> Result<Vec<Map<String, Value>>, ImportError> {
    let (items, _errors) = store.scan()?;
    Ok(items
        .iter()
        .map(|item| {
            let mut obj = clove_core::frontmatter_object(&item.frontmatter);
            obj.insert("body".to_owned(), Value::String(item.body.clone()));
            obj
        })
        .collect())
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
) -> Result<SyncReport, ImportError> {
    let mut report = SyncReport {
        conflicts: plan.conflicts.len(),
        remote_missing: plan.remote_missing.len(),
        ..SyncReport::default()
    };

    // Already-in-sync pairs: just (re)record their fingerprint so a first sync
    // over an old one-way link becomes incremental next time.
    for entry in &plan.in_sync {
        state.record(
            &entry.external_ref,
            entry.gh_updated_at,
            entry.local_updated,
        );
        report.in_sync += 1;
    }

    // --- Pulls (remote → local), through the unified write path. ---
    for pull in &plan.pull_create {
        pull_create(store, prefix, &pull.staged, state)?;
        report.pulled_created += 1;
    }
    for pull in &plan.pull_update {
        pull_update(store, pull, state)?;
        report.pulled_updated += 1;
    }

    // --- Pushes (local → remote), writing the link back locally. ---
    for push in &plan.push_create {
        push_create(crab, owner, repo, push, store, state).await?;
        report.pushed_created += 1;
    }
    for push in &plan.push_update {
        push_update(crab, owner, repo, push, state).await?;
        report.pushed_updated += 1;
    }

    Ok(report)
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
    let req = EditRequest {
        title: Some(staged.title.clone()),
        body: Some(staged.body.clone()),
        status: Some(staged.status),
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
) -> Result<(), ImportError> {
    let item = &push.item;
    let handler = crab.issues(owner, repo);
    let created: Issue = with_retry(|| {
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
    // the two sides actually match after a create-of-a-closed-item.
    if item.closed {
        with_retry(|| {
            handler
                .update(created.number)
                .state(octocrab::models::IssueState::Closed)
                .send()
        })
        .await?;
    }

    let external_ref = crate::github::external_ref_for(created.number);
    let local_updated = link_local(store, &push.clove_id, &external_ref)?;
    state.record(&external_ref, Some(created.updated_at), local_updated);
    Ok(())
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
    let handler = crab.issues(owner, repo);
    let want_closed = item.closed;
    // Bind owned values in this (longer-lived) scope so the per-attempt builder
    // can borrow them without the returned future outliving its referent.
    let assignees: Vec<String> = item.assignee.iter().cloned().collect();
    let updated: Issue = with_retry(|| {
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
        if !item.labels.is_empty() {
            builder = builder.labels(&item.labels);
        }
        if !assignees.is_empty() {
            builder = builder.assignees(&assignees);
        }
        builder.send()
    })
    .await?;

    let external_ref = crate::github::external_ref_for(push.number);
    state.record(&external_ref, Some(updated.updated_at), push.local_updated);
    Ok(())
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
            let created = with_retry(|| handler.create_comment(number, body.clone())).await?;
            entry.gh_comment_ids.insert(created.id.into_inner());
            entry.local_comment_hashes.insert(body_hash(&comment.body));
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
    let first = with_retry(|| handler.list_comments(number).per_page(100u8).send()).await?;
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

/// Run an octocrab call with bounded exponential backoff (up to 3 attempts).
///
/// A transient blip (network, 5xx, secondary rate-limit) shouldn't abort a sync
/// mid-stream and leave the two sides half-reconciled, so each call is retried a
/// few times before the error is surfaced. The happy path never sleeps.
async fn with_retry<T, F, Fut>(mut op: F) -> Result<T, ImportError>
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
                if attempt >= MAX_ATTEMPTS {
                    return Err(net_err(err));
                }
                tokio::time::sleep(delay).await;
                delay = delay.saturating_mul(2);
            }
        }
    }
}
