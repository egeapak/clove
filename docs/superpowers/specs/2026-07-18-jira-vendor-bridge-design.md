# Jira vendor bridge — design (gh-25)

**Status:** design only (implementation deferred). Adds `clove sync jira
<PROJECT>` as a third two-way bridge **on top of the `VendorSync` trait defined
in the GitLab spec** ([gh-19](2026-07-18-gitlab-vendor-bridge-design.md)). Jira
is the stress test for that boundary: string keys, a configurable workflow, and
custom fields — so validating it against Jira confirms the abstraction is right.

## Prerequisite

This design assumes `VendorSync` + the generic `sync_net::sync_vendor<V>` from
gh-19 already exist. Jira adds a `JiraVendor` impl + a `jira.rs` mapping module;
**the reconciliation core and apply loop are untouched.** If gh-25 is built
before gh-19, the `VendorSync` extraction is its first step.

## What makes Jira different (and how the trait absorbs it)

| Concern | GitHub/GitLab | Jira | Handling |
|---|---|---|---|
| Issue id | numeric (`gh-42`, `gl-42`) | **string key** `PROJ-123` | `RemoteId` is already a string newtype → `external_ref` = `jira-PROJ-123` |
| Fetch | `updated_after` | **JQL**: `project = PROJ AND updated >= "<since>"` | `fetch_since` builds the JQL; pure and testable |
| Status | open/closed | **workflow** (To Do / In Progress / Done / custom) via `transitions` API | see "Status mapping" below — Jira is the one vendor where clove's `in_progress` maps natively |
| Body | Markdown | **ADF** (Atlassian Document Format) or wiki markup | convert Markdown ↔ ADF at the edge in `jira.rs`; `<!-- clove-meta -->` rides in a dedicated field, not the body (ADF makes HTML-comment smuggling brittle) |
| Comments | issue comments / notes | issue comments (ADF) | `list_comments`/`add_comment` as usual; ADF body conversion |
| Auth | token | **email + API token** (Basic) or OAuth | `JiraVendor::new` takes base URL + email + token |

The only trait-surface consequence: `clove-meta` should be carried in a
**named Jira field** rather than appended to the body (ADF round-tripping an
embedded HTML comment is lossy). This argues for a small `VendorSync` addition
already worth having — an associated way to stash/read the clove-meta blob that
defaults to "append to body" (GitHub/GitLab) and overrides to "a field" (Jira).
Concretely, add to the trait:

```rust
/// Read/attach the clove round-trip metadata. Default impls append/parse the
/// `<!-- clove-meta -->` HTML comment in the body (GitHub, GitLab); Jira stores
/// it in a dedicated field instead.
fn read_meta(&self, issue: &StagedIssue) -> Option<CloveMeta> { /* body codec */ }
fn attach_meta(&self, item: &mut ExportItem, meta: &CloveMeta) { /* body codec */ }
```

This is the one refinement Jira forces on the gh-19 boundary; GitLab should land
first so it is added deliberately, not retrofitted.

## Status mapping (the interesting part)

Jira has a per-project **workflow**, not a fixed open/closed. Map via Jira's
**status category** (the stable 3-value classifier every workflow has):

| Jira status category | clove status |
|---|---|
| `To Do` (`new`) | `open` |
| `In Progress` (`indeterminate`) | `in_progress` |
| `Done` (`done`) | `closed` |

- **Pull:** read `fields.status.statusCategory.key` → clove status. This finally
  lets `in_progress` round-trip natively (no clove-meta guard needed, unlike
  GitHub/GitLab which lack the state).
- **Push:** clove status → a **transition**, not a field write. Jira requires
  `POST /issue/:key/transitions` with a transition id valid from the current
  status. `jira.rs` fetches the available transitions and picks the one whose
  target category matches; if none exists (a locked workflow), report a
  non-fatal conflict/warning rather than failing the whole sync.

## Field mapping (`jira.rs`)

| clove | Jira field |
|---|---|
| `title` | `summary` |
| `body` | `description` (ADF) + clove-meta field |
| `type` bug/feature/chore/… | `issuetype` (configurable map; default Bug→Bug, everything else→Task) |
| `priority` 0–4 | `priority` (map to the instance's priority scheme; default a 5-level map) |
| `labels` | `labels` |
| `assignee` | `assignee.accountId` (resolve via `/user/search`; cache) |
| `external_ref` | `jira-<KEY>` |

Type/priority/assignee all depend on **instance configuration**, so each mapping
is a small configurable table with a sane default, surfaced in `[sync.jira]`
config rather than hard-coded.

## CLI & config

- `clove sync jira <PROJECT> [--prefer …] [--no-comments] [--dry-run]`.
- `[sync.jira]` config block: `base_url`, `email`, optional `issuetype_map`,
  `priority_map`. Token via `JIRA_API_TOKEN`.
- Daemon timer: `[daemon] jira_sync_interval_min` + `jira_sync_project`.

## Testing

Same pattern as GitHub/GitLab: an in-process **mock Jira REST server**
(`CLOVE_JIRA_API_URL`) driving the real binary — including the **transition**
flow (fetch transitions → pick → POST) which is Jira-unique and the most
error-prone. Offline unit tests for JQL construction, the status-category map,
ADF↔Markdown conversion, and the clove-meta-in-a-field codec.

## Risks / open questions

- **ADF conversion fidelity** is the biggest unknown; v1 can degrade to sending
  plain-text/`description` in wiki markup and only round-trip the clove-managed
  fields, treating rich Jira formatting as read-mostly. Decide during build.
- **Workflow diversity:** the status-category mapping is robust, but push
  transitions can be blocked by workflow conditions (approvals, required
  fields). Surfacing these as conflicts (not hard errors) keeps sync resilient.

## Non-goals

Epics/sub-tasks hierarchy sync, sprints, and custom-field sync beyond the mapped
set are out of scope for v1 (standard issues only).
