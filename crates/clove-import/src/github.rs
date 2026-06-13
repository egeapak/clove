//! GitHub importer / exporter (T-M03): import from and export to GitHub Issues
//! (DESIGN.md §11.3).
//!
//! Unlike the file-based importers (tk, beads) the GitHub source/sink is a
//! network endpoint addressed by an `owner/repo` spec, not a [`camino::Utf8Path`],
//! so it does not implement the file-path [`crate::Importer`] trait. Instead it
//! exposes two free functions, [`import_github`] and [`export_github`], that keep
//! the same dry-run / idempotency / envelope semantics as tk and beads.
//!
//! ## Layering: pure mapping vs. network
//!
//! The field-mapping and `clove-meta` codec logic is **always compiled** (it is
//! not behind the `github` feature) so the offline unit tests cover it regardless
//! of how the crate is built:
//!
//! - [`encode_clove_meta`] / [`decode_clove_meta`] — the `<!-- clove-meta: {…} -->`
//!   HTML-comment round-trip codec.
//! - [`GitHubIssue`] — a plain, serde-deserializable intermediate view of a GitHub
//!   issue (mirrors the REST JSON shape). The network path deserializes octocrab's
//!   `Issue` into this via a `serde_json` round-trip, and the offline tests build
//!   it straight from committed JSON fixtures — so the mapping is exercised with
//!   no network.
//! - [`map_issue`] — `GitHubIssue → StagedIssue`, the pure mapping.
//! - [`plan_issues`] — the pure idempotency filter over a slice of mapped issues.
//!
//! Only the octocrab client construction, the tokio runtime, and the paginated
//! fetch / create / update calls are behind `#[cfg(feature = "github")]`.
//!
//! ## Field mapping (DESIGN §11.3)
//!
//! | GitHub | clove |
//! |---|---|
//! | `number` | idempotency key → `external_ref = "gh-<number>"`; clove mints a fresh `CloveId` |
//! | `title` | `title` |
//! | `state` (`open`/`closed`) | `status` (`closed` → [`ItemStatus::Closed`]) |
//! | `labels[].name` | `labels` (normalized via [`crate::map::map_labels`]) |
//! | `assignees[0].login` | `assignee` |
//! | `closed_at` | `closed` |
//! | `body` (minus the `clove-meta` comment) | body |
//! | `clove-meta.deps` / `.priority` | `deps` / `priority` |
//! | — | `source_system = "github"` |
//!
//! ## external_ref / idempotency rule
//!
//! The clove `external_ref` is always `"gh-<number>"`. This is the idempotency key
//! (DESIGN §11.3): on re-import an incoming issue whose `"gh-<number>"` already
//! exists in the store is skipped. On export, a local item that already carries
//! `external_ref = "gh-<number>"` is UPDATED (PATCH that issue number); an item
//! without one is CREATED and reports the new number.

use chrono::{DateTime, Utc};
use clove_types::{CloveId, ItemStatus, Priority};
use serde::{Deserialize, Serialize};

use crate::map::{coerce_priority, map_labels};
use crate::plan::{ImportPlan, PlanItem, SkipItem};
use crate::ImportCtx;

/// The HTML-comment marker prefix/suffix used to embed clove-only metadata in a
/// GitHub issue body so it survives the round-trip (GitHub has no `deps` /
/// `priority` / clove-`id` fields).
const META_OPEN: &str = "<!-- clove-meta:";
const META_CLOSE: &str = "-->";

/// The clove-only metadata embedded in (and parsed back out of) a GitHub issue
/// body as a `<!-- clove-meta: {json} -->` HTML comment.
///
/// Round-trippable: [`encode_clove_meta`] serializes this; [`decode_clove_meta`]
/// parses it back out of an arbitrary body, tolerating absence/malformedness by
/// returning `None`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CloveMeta {
    /// The clove `id` of the source item (informational; export only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// The item priority (0..=4).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<u8>,
    /// The item's dependency ids.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deps: Vec<String>,
}

/// Encode `meta` as a single-line `<!-- clove-meta: {json} -->` HTML comment.
///
/// The companion of [`decode_clove_meta`]; the two round-trip.
pub fn encode_clove_meta(meta: &CloveMeta) -> String {
    // serde_json on a plain struct is infallible here; fall back to `{}` defensively.
    let json = serde_json::to_string(meta).unwrap_or_else(|_| "{}".to_owned());
    format!("{META_OPEN} {json} {META_CLOSE}")
}

/// Append the `clove-meta` comment to an issue `body`, preserving the original
/// body. A trailing blank line separates the human body from the marker.
pub fn append_clove_meta(body: &str, meta: &CloveMeta) -> String {
    let comment = encode_clove_meta(meta);
    if body.trim().is_empty() {
        comment
    } else {
        format!("{}\n\n{comment}", body.trim_end())
    }
}

/// Locate the **last** `<!-- clove-meta: … -->` marker in `body`, returning the
/// byte span `(start, end)` covering the whole comment (from `<!-- clove-meta:`
/// through the closing `-->`).
///
/// [`append_clove_meta`] always writes clove's real metadata at the *end* of the
/// body, so a crafted/human earlier `clove-meta`-looking comment must never win.
/// Both [`decode_clove_meta`] and [`strip_clove_meta`] route through this single
/// helper so they can never diverge on which marker they pick. Returns `None`
/// when the marker is absent or unterminated.
fn last_clove_meta_span(body: &str) -> Option<(usize, usize)> {
    let start = body.rfind(META_OPEN)?;
    let after_open = start + META_OPEN.len();
    let rel_end = body[after_open..].find(META_CLOSE)?;
    let end = after_open + rel_end + META_CLOSE.len();
    Some((start, end))
}

/// Parse the **last** `<!-- clove-meta: {json} -->` comment out of `body`.
///
/// The last occurrence is the one clove itself appended (see
/// [`last_clove_meta_span`]); an earlier human/crafted marker is ignored.
///
/// Tolerant: returns `None` when the comment is absent OR its JSON payload is
/// malformed (never errors, never panics) so a foreign body can never break an
/// import.
pub fn decode_clove_meta(body: &str) -> Option<CloveMeta> {
    let (start, end) = last_clove_meta_span(body)?;
    let json = body[start + META_OPEN.len()..end - META_CLOSE.len()].trim();
    serde_json::from_str(json).ok()
}

/// Strip the **last** `<!-- clove-meta: … -->` comment (and surrounding blank
/// lines) from `body`, returning the human-facing body. The last occurrence is
/// the one clove appended (see [`last_clove_meta_span`]); any earlier
/// human/crafted marker is left intact as ordinary body text. If no comment is
/// present the body is returned trimmed of a trailing newline only.
pub fn strip_clove_meta(body: &str) -> String {
    let Some((start, end)) = last_clove_meta_span(body) else {
        return body.to_owned();
    };
    let mut out = String::with_capacity(body.len());
    out.push_str(&body[..start]);
    out.push_str(&body[end..]);
    out.trim().to_owned()
}

/// A GitHub label (`{ "name": "bug", … }`). Only `name` is consumed.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct GitHubLabel {
    #[serde(default)]
    pub name: String,
}

/// A GitHub user reference (`{ "login": "octocat", … }`). Only `login` is consumed.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct GitHubUser {
    #[serde(default)]
    pub login: String,
}

/// A plain, serde-deserializable intermediate view of a GitHub issue.
///
/// Mirrors the REST JSON shape closely enough that octocrab's `Issue` (which is
/// `Serialize`) round-trips into it via `serde_json`, and offline tests can build
/// it directly from committed JSON fixtures — so [`map_issue`] is fully testable
/// without the network. Unknown fields are ignored.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct GitHubIssue {
    /// The issue number (the `gh-<number>` idempotency key).
    #[serde(default)]
    pub number: u64,
    #[serde(default)]
    pub title: String,
    /// `"open"` or `"closed"`.
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub labels: Vec<GitHubLabel>,
    #[serde(default)]
    pub assignees: Vec<GitHubUser>,
    #[serde(default)]
    pub closed_at: Option<DateTime<Utc>>,
    /// Last-modified time reported by GitHub. The two-way sync compares this
    /// against the timestamp recorded at the previous sync to decide whether the
    /// remote side changed (see [`crate::sync`]). Absent in older fixtures →
    /// treated as "unknown / always re-examine".
    #[serde(default)]
    pub updated_at: Option<DateTime<Utc>>,
    /// Present on real issues; used to skip PRs (which the issues API also returns).
    #[serde(default)]
    pub pull_request: Option<serde_json::Value>,
}

/// A fully mapped GitHub issue, ready to be turned into an [`clove_types::Item`].
///
/// Mirrors the staging structs in `tk.rs` / `beads.rs`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedIssue {
    /// The idempotency key / clove `external_ref` (`"gh-<number>"`).
    pub external_ref: String,
    /// The source id surfaced in the plan (`"gh-<number>"`).
    pub source_id: String,
    pub title: String,
    pub status: ItemStatus,
    pub priority: Priority,
    pub assignee: Option<String>,
    pub deps: Vec<CloveId>,
    pub labels: Vec<String>,
    pub closed: Option<DateTime<Utc>>,
    pub body: String,
    /// The issue's GitHub `updated_at` (carried through so a sync can record it
    /// as the last-synced remote fingerprint). `None` when the source omitted it.
    pub updated_at: Option<DateTime<Utc>>,
}

/// The `external_ref` (== idempotency key) for a GitHub issue number.
pub fn external_ref_for(number: u64) -> String {
    format!("gh-{number}")
}

/// Map a [`GitHubIssue`] to a [`StagedIssue`] (pure; no network, no writes).
///
/// Returns an error message string on a malformed `deps` id reference inside the
/// `clove-meta` comment (mirrors the file importers' record-level errors).
pub fn map_issue(issue: &GitHubIssue) -> Result<StagedIssue, String> {
    let external_ref = external_ref_for(issue.number);

    let status = match issue.state.trim().to_lowercase().as_str() {
        "closed" => ItemStatus::Closed,
        _ => ItemStatus::Open,
    };

    let raw_body = issue.body.clone().unwrap_or_default();
    let meta = decode_clove_meta(&raw_body).unwrap_or_default();
    let body = strip_clove_meta(&raw_body);

    let priority = meta
        .priority
        .map(|p| coerce_priority(i64::from(p)))
        .unwrap_or(Priority::DEFAULT);

    let deps = meta
        .deps
        .iter()
        .map(|d| {
            CloveId::new(d.trim())
                .map_err(|err| format!("invalid dep id `{d}` in clove-meta: {err}"))
        })
        .collect::<Result<Vec<_>, _>>()?;

    let label_names: Vec<&str> = issue.labels.iter().map(|l| l.name.as_str()).collect();
    let mut labels = map_labels(&label_names).map_err(|e| e.to_string())?;
    labels.sort();
    labels.dedup();

    let assignee = issue
        .assignees
        .first()
        .map(|u| u.login.clone())
        .filter(|s| !s.trim().is_empty());

    Ok(StagedIssue {
        external_ref,
        source_id: external_ref_for(issue.number),
        title: issue.title.clone(),
        status,
        priority,
        assignee,
        deps,
        labels,
        closed: issue.closed_at,
        body,
        updated_at: issue.updated_at,
    })
}

/// Compute the write-free [`ImportPlan`] over a slice of fetched issues: map each,
/// skipping pull requests, and partition into create/skip by the `external_ref`
/// idempotency index in `ctx`.
///
/// Returns the plan and the staged issues that would be created (in `would_create`
/// order) so the caller can apply them without re-mapping.
pub fn plan_issues(
    issues: &[GitHubIssue],
    ctx: &ImportCtx,
) -> Result<(ImportPlan, Vec<StagedIssue>), String> {
    let mut plan = ImportPlan::new();
    let mut staged = Vec::new();
    for issue in issues {
        // The issues REST endpoint also returns PRs; skip them.
        if issue.pull_request.is_some() {
            continue;
        }
        let mapped = map_issue(issue)?;
        if ctx.is_imported(&mapped.external_ref) {
            plan.would_skip.push(SkipItem {
                id: mapped.source_id.clone(),
                reason: "already_imported".to_owned(),
            });
        } else {
            plan.would_create.push(PlanItem {
                id: mapped.source_id.clone(),
                title: mapped.title.clone(),
            });
            staged.push(mapped);
        }
    }
    Ok((plan, staged))
}

/// A local clove item prepared for export to GitHub: the issue payload plus the
/// existing `gh-<number>` (if any) that decides create vs. update.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportItem {
    /// The clove id (for the plan + the `clove-meta` `id`).
    pub clove_id: String,
    /// The existing GitHub issue number parsed from `external_ref = "gh-<n>"`,
    /// if the item was previously synced. `Some` → update, `None` → create.
    pub gh_number: Option<u64>,
    pub title: String,
    /// The body WITH the `clove-meta` comment appended (what gets pushed).
    pub body: String,
    pub labels: Vec<String>,
    pub assignee: Option<String>,
    pub closed: bool,
}

/// Parse a `"gh-<number>"` external_ref into its issue number, tolerating any
/// other shape (returns `None`).
pub fn parse_gh_number(external_ref: &str) -> Option<u64> {
    external_ref.trim().strip_prefix("gh-")?.parse().ok()
}

/// Build the GitHub export payload for a single clove item from its exported JSON
/// object (the `item_json::export_object` shape). Pure: no network.
///
/// - `title` ← `title`
/// - body ← `body` with `<!-- clove-meta: {id,priority,deps} -->` appended
/// - `labels` ← `labels`
/// - `assignee` ← `assignee`
/// - `gh_number` ← parsed from `external_ref` (`Some` → update, `None` → create)
pub fn build_export_item(obj: &serde_json::Map<String, serde_json::Value>) -> ExportItem {
    let s = |k: &str| obj.get(k).and_then(|v| v.as_str()).map(str::to_owned);
    let clove_id = s("id").unwrap_or_default();
    let title = s("title").unwrap_or_default();
    let body = obj
        .get("body")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_owned();

    let priority = obj
        .get("priority")
        .and_then(serde_json::Value::as_u64)
        .map(|p| p as u8);
    let deps = obj
        .get("deps")
        .and_then(serde_json::Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    let labels = obj
        .get("labels")
        .and_then(serde_json::Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    let assignee = s("assignee").filter(|s| !s.trim().is_empty());
    let closed = obj
        .get("status")
        .and_then(|v| v.as_str())
        .map(|s| s.eq_ignore_ascii_case("closed"))
        .unwrap_or(false);
    let gh_number = s("external_ref").and_then(|r| parse_gh_number(&r));

    let meta = CloveMeta {
        id: if clove_id.is_empty() {
            None
        } else {
            Some(clove_id.clone())
        },
        priority,
        deps,
    };
    let body = append_clove_meta(&body, &meta);

    ExportItem {
        clove_id,
        gh_number,
        title,
        body,
        labels,
        assignee,
        closed,
    }
}

/// The write-free export plan: which local items would be created vs. updated on
/// GitHub. Fully offline (computed from local items only) so `--dry-run` never
/// touches the network.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ExportPlan {
    /// Items with no `gh-<number>` yet — would be CREATED.
    pub would_create: Vec<ExportPlanItem>,
    /// Items already synced (`external_ref = "gh-<n>"`) — would be UPDATED.
    pub would_update: Vec<ExportPlanItem>,
}

/// One entry in an [`ExportPlan`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExportPlanItem {
    /// The clove id.
    pub id: String,
    pub title: String,
    /// The target GitHub issue number for updates (`None` for creates).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub number: Option<u64>,
}

/// Partition exported item objects into a create/update [`ExportPlan`] (pure).
pub fn plan_export(objs: &[serde_json::Map<String, serde_json::Value>]) -> ExportPlan {
    let mut plan = ExportPlan::default();
    for obj in objs {
        let item = build_export_item(obj);
        let entry = ExportPlanItem {
            id: item.clove_id.clone(),
            title: item.title.clone(),
            number: item.gh_number,
        };
        match item.gh_number {
            Some(_) => plan.would_update.push(entry),
            None => plan.would_create.push(entry),
        }
    }
    plan
}

/// A summary of what an export `apply` run actually pushed to GitHub.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ExportReport {
    /// Issues created (with their new numbers).
    pub created: Vec<u64>,
    /// Issues updated.
    pub updated: Vec<u64>,
}

// ---------------------------------------------------------------------------
// Network layer (octocrab + tokio) — only compiled with the `github` feature.
// ---------------------------------------------------------------------------

#[cfg(feature = "github")]
pub(crate) mod net {
    use super::*;
    use crate::error::ImportError;
    use crate::plan::ImportReport;
    use chrono::Utc;
    use clove_core::write::write_item_file;
    use clove_core::ItemStore;
    use clove_types::id::new_id;
    use clove_types::model::CURRENT_SCHEMA_VERSION;
    use clove_types::{Item, ItemFrontmatter, ItemType};
    use octocrab::Octocrab;

    /// Split an `owner/repo` spec into its parts, erroring cleanly on a bad shape.
    pub(crate) fn parse_repo_spec(spec: &str) -> Result<(String, String), ImportError> {
        let mut parts = spec.trim().splitn(2, '/');
        match (parts.next(), parts.next()) {
            (Some(owner), Some(repo)) if !owner.is_empty() && !repo.is_empty() => {
                Ok((owner.to_owned(), repo.trim_end_matches('/').to_owned()))
            }
            _ => Err(ImportError::Source {
                path: camino::Utf8PathBuf::from(spec),
                message: "expected an `owner/repo` spec".to_owned(),
            }),
        }
    }

    /// Resolve a GitHub token, in precedence order:
    /// 1. the `GITHUB_TOKEN` environment variable;
    /// 2. the `gh` CLI (`gh auth token`), if installed and authenticated.
    ///
    /// A missing `gh` binary or any non-zero/empty result falls through to
    /// `None` (never an error) so the caller can emit the auth-missing message.
    fn resolve_github_token() -> Option<String> {
        if let Ok(token) = std::env::var("GITHUB_TOKEN") {
            let token = token.trim();
            if !token.is_empty() {
                // Return the trimmed value: a trailing newline must not reach octocrab.
                return Some(token.to_owned());
            }
        }
        // Fall back to the gh CLI. A missing binary or failed call is "no token".
        let output = std::process::Command::new("gh")
            .args(["auth", "token"])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let token = String::from_utf8(output.stdout).ok()?.trim().to_owned();
        (!token.is_empty()).then_some(token)
    }

    /// Install the rustls **ring** crypto provider as the process default, once.
    ///
    /// octocrab pulls `hyper-rustls`, and via Cargo feature unification *both* the
    /// `ring` and `aws-lc-rs` rustls providers get compiled in. rustls 0.23 then
    /// refuses to auto-select one and panics ("Could not automatically determine
    /// the process-level CryptoProvider") on the first TLS handshake. Installing a
    /// provider explicitly before octocrab builds its client is the documented
    /// remedy. We prefer `ring`. The call is idempotent: a second install (or a
    /// provider already installed elsewhere) returns `Err`, which we ignore.
    fn install_crypto_provider() {
        use std::sync::Once;
        static INIT: Once = Once::new();
        INIT.call_once(|| {
            let _ = rustls::crypto::ring::default_provider().install_default();
        });
    }

    /// Build an authenticated octocrab client from a resolved token
    /// (`GITHUB_TOKEN` or the `gh` CLI), erroring cleanly when neither is
    /// available (a network call needs it).
    ///
    /// The API base URI is overridable via `CLOVE_GITHUB_API_URL` (falling back
    /// to `GITHUB_API_URL`). This points clove at GitHub Enterprise, and — the
    /// reason it exists — lets the integration test-suite aim every REST call at
    /// a local deterministic mock server instead of github.com.
    pub(crate) fn build_client() -> Result<Octocrab, ImportError> {
        install_crypto_provider();
        let token = resolve_github_token().ok_or_else(|| ImportError::Source {
            path: camino::Utf8PathBuf::from("<github>"),
            message: "no GitHub token: set GITHUB_TOKEN or authenticate the gh CLI \
                      (`gh auth login`); a token is required to reach the GitHub API"
                .to_owned(),
        })?;
        let mut builder = Octocrab::builder().personal_token(token);
        if let Some(base) = api_base_override() {
            builder = builder
                .base_uri(base.clone())
                .map_err(|err| ImportError::Source {
                    path: camino::Utf8PathBuf::from("<github>"),
                    message: format!("invalid GitHub API base `{base}`: {err}"),
                })?;
        }
        builder.build().map_err(|err| ImportError::Source {
            path: camino::Utf8PathBuf::from("<github>"),
            message: format!("failed to build GitHub client: {err}"),
        })
    }

    /// The API base-URI override, if any (`CLOVE_GITHUB_API_URL`, then
    /// `GITHUB_API_URL`). Empty values are ignored.
    fn api_base_override() -> Option<String> {
        for key in ["CLOVE_GITHUB_API_URL", "GITHUB_API_URL"] {
            if let Ok(val) = std::env::var(key) {
                let val = val.trim();
                if !val.is_empty() {
                    return Some(val.to_owned());
                }
            }
        }
        None
    }

    /// Convert an octocrab `Issue` into our pure [`GitHubIssue`] via a serde_json
    /// round-trip (the octocrab model is `#[non_exhaustive]`, so we cannot build
    /// it directly — but it is `Serialize`).
    pub(crate) fn to_intermediate(
        issue: &octocrab::models::issues::Issue,
    ) -> Result<GitHubIssue, ImportError> {
        let value = serde_json::to_value(issue).map_err(|err| ImportError::Record {
            message: format!("github issue serialize failed: {err}"),
        })?;
        serde_json::from_value(value).map_err(|err| ImportError::Record {
            message: format!("github issue map failed: {err}"),
        })
    }

    /// Fetch every issue (all states, paginated) from `owner/repo`.
    pub(crate) async fn fetch_all(
        crab: &Octocrab,
        owner: &str,
        repo: &str,
    ) -> Result<Vec<GitHubIssue>, ImportError> {
        let first = crab
            .issues(owner, repo)
            .list()
            .state(octocrab::params::State::All)
            .per_page(100)
            .send()
            .await
            .map_err(net_err)?;
        let all = crab.all_pages(first).await.map_err(net_err)?;
        all.iter().map(to_intermediate).collect()
    }

    pub(crate) fn net_err(err: octocrab::Error) -> ImportError {
        ImportError::Source {
            path: camino::Utf8PathBuf::from("<github>"),
            message: format!("github api error: {err}"),
        }
    }

    /// Import every issue from `owner/repo` into `store` (or, for a dry run,
    /// compute the plan only — but the fetch still needs a token). Returns the
    /// plan plus, when not a dry run, the apply report.
    pub fn import_github(
        spec: &str,
        ctx: &ImportCtx,
        store: &ItemStore,
        prefix: &str,
    ) -> Result<(ImportPlan, Option<ImportReport>), ImportError> {
        let (owner, repo) = parse_repo_spec(spec)?;
        let rt = tokio::runtime::Runtime::new().map_err(|err| ImportError::Source {
            path: camino::Utf8PathBuf::from("<github>"),
            message: format!("failed to start async runtime: {err}"),
        })?;
        // The octocrab client must be built *inside* the runtime context: its
        // tower `Buffer` layer spawns a worker task at build time, which panics
        // ("there is no reactor running") if no runtime is entered. Building it
        // within `block_on` keeps the context active. (Using `rt.enter()` here
        // would instead panic with "Cannot start a runtime from within a
        // runtime" once `block_on` re-enters.)
        let issues = rt.block_on(async {
            let crab = build_client()?;
            fetch_all(&crab, &owner, &repo).await
        })?;

        let (plan, staged) =
            plan_issues(&issues, ctx).map_err(|message| ImportError::Record { message })?;

        if ctx.dry_run {
            return Ok((plan, None));
        }

        let now = Utc::now();
        let mut created = 0usize;
        for item in &staged {
            let id = new_id(prefix, store.issues_dir())?;
            let frontmatter = ItemFrontmatter {
                schema: CURRENT_SCHEMA_VERSION,
                id: id.clone(),
                title: item.title.clone(),
                status: item.status,
                item_type: ItemType::default(),
                priority: item.priority,
                created: now,
                updated: now,
                closed: item.closed.or(if item.status == ItemStatus::Closed {
                    Some(now)
                } else {
                    None
                }),
                assignee: item.assignee.clone(),
                parent: None,
                labels: item.labels.clone(),
                deps: item.deps.clone(),
                relates: Vec::new(),
                duplicates: Vec::new(),
                supersedes: Vec::new(),
                source_system: Some("github".to_owned()),
                external_ref: Some(item.external_ref.clone()),
            };
            let new_item = Item {
                frontmatter,
                body: item.body.clone(),
            };
            write_item_file(&new_item, &store.path_for(&id))?;
            created += 1;
        }

        let report = ImportReport {
            created,
            skipped: plan.would_skip.len(),
            conflicts: plan.conflicts.len(),
        };
        Ok((plan, Some(report)))
    }

    /// Export the given local item objects to `owner/repo`. For a dry run the plan
    /// is computed entirely offline (no token, no network). Otherwise each item is
    /// created or updated via octocrab.
    pub fn export_github(
        spec: &str,
        objs: &[serde_json::Map<String, serde_json::Value>],
        store: &ItemStore,
        dry_run: bool,
    ) -> Result<(ExportPlan, Option<ExportReport>), ImportError> {
        // Validate the spec shape even for dry runs (cheap, offline).
        let (owner, repo) = parse_repo_spec(spec)?;
        let plan = plan_export(objs);
        if dry_run {
            return Ok((plan, None));
        }

        let rt = tokio::runtime::Runtime::new().map_err(|err| ImportError::Source {
            path: camino::Utf8PathBuf::from("<github>"),
            message: format!("failed to start async runtime: {err}"),
        })?;
        // Build the client inside the runtime context (see `import_github`): the
        // octocrab/tower Buffer worker is spawned at build time and needs a reactor.
        let report = rt.block_on(async {
            let crab = build_client()?;
            push_all(&crab, &owner, &repo, objs, store).await
        })?;
        Ok((plan, Some(report)))
    }

    /// Create/update every item on GitHub, returning the affected issue numbers.
    ///
    /// A freshly *created* issue's number is written back onto the local item as
    /// `external_ref = "gh-<number>"`, so a subsequent export (or sync) UPDATES
    /// that issue instead of creating a duplicate — the idempotency the one-way
    /// export previously lacked.
    async fn push_all(
        crab: &Octocrab,
        owner: &str,
        repo: &str,
        objs: &[serde_json::Map<String, serde_json::Value>],
        store: &ItemStore,
    ) -> Result<ExportReport, ImportError> {
        let handler = crab.issues(owner, repo);
        let mut report = ExportReport::default();
        for obj in objs {
            let item = build_export_item(obj);
            let state = if item.closed {
                octocrab::models::IssueState::Closed
            } else {
                octocrab::models::IssueState::Open
            };
            match item.gh_number {
                Some(number) => {
                    let mut update = handler.update(number);
                    update = update.title(&item.title).body(&item.body).state(state);
                    if !item.labels.is_empty() {
                        update = update.labels(&item.labels);
                    }
                    let assignees = item
                        .assignee
                        .as_ref()
                        .map(|a| vec![a.clone()])
                        .unwrap_or_default();
                    if !assignees.is_empty() {
                        update = update.assignees(&assignees);
                    }
                    update.send().await.map_err(net_err)?;
                    report.updated.push(number);
                }
                None => {
                    let mut create = handler.create(&item.title).body(item.body.clone());
                    if !item.labels.is_empty() {
                        create = create.labels(item.labels.clone());
                    }
                    if let Some(assignee) = &item.assignee {
                        create = create.assignees(vec![assignee.clone()]);
                    }
                    let created = create.send().await.map_err(net_err)?;
                    // A created issue is OPEN; if the local item is closed, close it.
                    if item.closed {
                        handler
                            .update(created.number)
                            .state(octocrab::models::IssueState::Closed)
                            .send()
                            .await
                            .map_err(net_err)?;
                    }
                    write_back_ref(store, &item.clove_id, created.number)?;
                    report.created.push(created.number);
                }
            }
        }
        Ok(report)
    }

    /// Stamp `source_system = github` + `external_ref = gh-<number>` onto the
    /// local item and persist it through the store's atomic write path.
    fn write_back_ref(store: &ItemStore, clove_id: &str, number: u64) -> Result<(), ImportError> {
        let id = clove_types::CloveId::new(clove_id).map_err(|e| ImportError::Record {
            message: format!("invalid local id `{clove_id}`: {e}"),
        })?;
        let mut item = store.get(&id)?;
        item.frontmatter.source_system = Some("github".to_owned());
        item.frontmatter.external_ref = Some(external_ref_for(number));
        store.update(&item, Utc::now())?;
        Ok(())
    }
}

#[cfg(feature = "github")]
pub use net::{export_github, import_github};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clove_meta_round_trips() {
        let meta = CloveMeta {
            id: Some("proj-AAAA1111".to_owned()),
            priority: Some(2),
            deps: vec!["proj-BBBB2222".to_owned(), "proj-CCCC3333".to_owned()],
        };
        let encoded = encode_clove_meta(&meta);
        assert!(encoded.starts_with("<!-- clove-meta:"));
        assert!(encoded.ends_with("-->"));
        let decoded = decode_clove_meta(&encoded).unwrap();
        assert_eq!(decoded, meta);
    }

    #[test]
    fn clove_meta_absent_is_none() {
        assert!(decode_clove_meta("a plain body with no marker").is_none());
        assert!(decode_clove_meta("").is_none());
    }

    #[test]
    fn clove_meta_malformed_is_none() {
        // Marker present but the JSON payload is broken → None, never a panic.
        assert!(decode_clove_meta("<!-- clove-meta: {not json -->").is_none());
        // Open marker with no close → None.
        assert!(decode_clove_meta("<!-- clove-meta: {}").is_none());
    }

    #[test]
    fn append_and_strip_preserve_human_body() {
        let meta = CloveMeta {
            id: Some("proj-AAAA1111".to_owned()),
            priority: Some(1),
            deps: vec![],
        };
        let body = "Human readable description.\n";
        let with_meta = append_clove_meta(body, &meta);
        assert!(with_meta.contains("Human readable description."));
        assert!(with_meta.contains("<!-- clove-meta:"));
        // The human body is recoverable.
        assert_eq!(strip_clove_meta(&with_meta), "Human readable description.");
        // And the meta is recoverable.
        assert_eq!(decode_clove_meta(&with_meta).unwrap(), meta);
    }

    #[test]
    fn clove_meta_picks_trailing_real_marker_over_earlier_human_one() {
        // A crafted/human earlier marker must never win over clove's real meta,
        // which `append_clove_meta` always writes at the END of the body.
        let real = CloveMeta {
            id: Some("proj-AAAA1111".to_owned()),
            priority: Some(2),
            deps: vec!["proj-BBBB2222".to_owned()],
        };
        let human = "Human readable description.\n\n<!-- clove-meta: {\"priority\":4} -->\n";
        let with_meta = append_clove_meta(human, &real);

        // decode must return the TRAILING (real) meta, not the human priority:4.
        let decoded = decode_clove_meta(&with_meta).expect("trailing meta decodes");
        assert_eq!(decoded, real, "decode must pick the last (real) marker");
        assert_eq!(decoded.priority, Some(2));

        // strip must remove the TRAILING comment, leaving the earlier human one
        // intact as ordinary body text.
        let stripped = strip_clove_meta(&with_meta);
        assert!(
            stripped.contains("<!-- clove-meta: {\"priority\":4} -->"),
            "earlier human marker survives as body text:\n{stripped}"
        );
        assert!(
            !stripped.contains("proj-AAAA1111"),
            "trailing real marker is removed:\n{stripped}"
        );
        assert!(stripped.starts_with("Human readable description."));
    }

    /// Build a `GitHubIssue` from the committed REST-shaped JSON fixture (no network).
    fn fixture_issue() -> GitHubIssue {
        let json = include_str!("../tests/fixtures/github/issue.json");
        serde_json::from_str(json).expect("fixture parses")
    }

    #[test]
    fn maps_github_issue_to_staged() {
        let issue = fixture_issue();
        let s = map_issue(&issue).unwrap();
        // gh-number → external_ref idempotency key.
        assert_eq!(s.external_ref, "gh-42");
        assert_eq!(s.source_id, "gh-42");
        assert_eq!(s.title, "Crash on startup");
        // state closed → status Closed.
        assert_eq!(s.status, ItemStatus::Closed);
        // closed_at → closed.
        assert!(s.closed.is_some());
        // labels normalized + sorted.
        assert_eq!(s.labels, vec!["area:core", "bug"]);
        // assignees[0].login → assignee.
        assert_eq!(s.assignee.as_deref(), Some("octocat"));
        // clove-meta deps + priority parsed.
        assert_eq!(s.priority, Priority(0));
        assert_eq!(s.deps.len(), 1);
        // The human body survives, the marker is stripped.
        assert!(s.body.contains("It crashes."));
        assert!(!s.body.contains("clove-meta"));
    }

    #[test]
    fn open_issue_maps_to_open_no_meta() {
        let issue = GitHubIssue {
            number: 7,
            title: "Open one".to_owned(),
            state: "open".to_owned(),
            body: Some("no marker here".to_owned()),
            ..Default::default()
        };
        let s = map_issue(&issue).unwrap();
        assert_eq!(s.status, ItemStatus::Open);
        assert!(s.closed.is_none());
        assert_eq!(s.priority, Priority::DEFAULT);
        assert!(s.deps.is_empty());
        assert_eq!(s.body, "no marker here");
        assert!(s.assignee.is_none());
    }

    #[test]
    fn idempotency_skips_already_imported() {
        use std::collections::{HashMap, HashSet};
        let issues = vec![
            GitHubIssue {
                number: 1,
                title: "first".to_owned(),
                state: "open".to_owned(),
                ..Default::default()
            },
            GitHubIssue {
                number: 2,
                title: "second".to_owned(),
                state: "open".to_owned(),
                ..Default::default()
            },
        ];
        // Pretend gh-1 was already imported.
        let mut external_refs = HashMap::new();
        external_refs.insert(
            "gh-1".to_owned(),
            CloveId::new("proj-AAAA1111").unwrap().into(),
        );
        let ctx = ImportCtx {
            external_refs,
            store_ids: HashSet::new(),
            dry_run: true,
        };
        let (plan, staged) = plan_issues(&issues, &ctx).unwrap();
        assert_eq!(plan.would_skip.len(), 1);
        assert_eq!(plan.would_skip[0].id, "gh-1");
        assert_eq!(plan.would_create.len(), 1);
        assert_eq!(plan.would_create[0].id, "gh-2");
        assert_eq!(staged.len(), 1);
        assert_eq!(staged[0].source_id, "gh-2");
    }

    #[test]
    fn pull_requests_are_skipped() {
        use std::collections::{HashMap, HashSet};
        let issues = vec![GitHubIssue {
            number: 9,
            title: "a PR".to_owned(),
            state: "open".to_owned(),
            pull_request: Some(serde_json::json!({ "url": "x" })),
            ..Default::default()
        }];
        let ctx = ImportCtx {
            external_refs: HashMap::new(),
            store_ids: HashSet::new(),
            dry_run: true,
        };
        let (plan, staged) = plan_issues(&issues, &ctx).unwrap();
        assert!(plan.would_create.is_empty());
        assert!(staged.is_empty());
    }

    #[test]
    fn parse_gh_number_tolerant() {
        assert_eq!(parse_gh_number("gh-42"), Some(42));
        assert_eq!(parse_gh_number("beads:bd-1"), None);
        assert_eq!(parse_gh_number("gh-notanumber"), None);
    }

    #[test]
    fn export_body_encodes_meta_and_preserves_body() {
        let obj: serde_json::Map<String, serde_json::Value> =
            serde_json::from_value(serde_json::json!({
                "id": "proj-AAAA1111",
                "title": "Do the thing",
                "body": "Some description.",
                "priority": 1,
                "deps": ["proj-BBBB2222"],
                "labels": ["bug"],
                "assignee": "octocat",
                "status": "open"
            }))
            .unwrap();
        let item = build_export_item(&obj);
        assert_eq!(item.gh_number, None); // no external_ref → create
        assert!(item.body.contains("Some description."));
        assert!(item.body.contains("<!-- clove-meta:"));
        // The clove-meta carries deps + priority + id.
        let meta = decode_clove_meta(&item.body).unwrap();
        assert_eq!(meta.priority, Some(1));
        assert_eq!(meta.deps, vec!["proj-BBBB2222".to_owned()]);
        assert_eq!(meta.id.as_deref(), Some("proj-AAAA1111"));
        assert_eq!(item.labels, vec!["bug".to_owned()]);
        assert_eq!(item.assignee.as_deref(), Some("octocat"));
        assert!(!item.closed);
    }

    #[test]
    fn export_existing_ref_is_update() {
        let obj: serde_json::Map<String, serde_json::Value> =
            serde_json::from_value(serde_json::json!({
                "id": "proj-AAAA1111",
                "title": "Synced",
                "body": "x",
                "external_ref": "gh-101",
                "status": "closed"
            }))
            .unwrap();
        let item = build_export_item(&obj);
        assert_eq!(item.gh_number, Some(101));
        assert!(item.closed);

        let plan = plan_export(&[obj]);
        assert_eq!(plan.would_update.len(), 1);
        assert_eq!(plan.would_update[0].number, Some(101));
        assert!(plan.would_create.is_empty());
    }
}
