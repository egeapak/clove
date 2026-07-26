# Read-path roadmap

Planned work on clove's read surfaces, written up so it can be picked up
independently of the session that planned it. Everything here is **not started**
unless a section says otherwise.

The organising goal is the one behind the shipped `view::Page` work: *the same
query, asked through any surface, answers the same way.* The CLI, the MCP tools,
the web API, and `cloved`'s RPCs are four front doors onto one store, and each
divergence between them is a bug a user eventually hits.

Status of what shipped, for context:

| Concern | State |
|---|---|
| `offset`/`limit` decoding | **Done** — `clove_core::view::Page`, all surfaces |
| Per-surface defaults | **Done** — `clove_core::view::defaults` |
| Result shaping (`fields`/`compact`) | **Done** — CLI, MCP, and the web API (§5) |
| Ordering (`sort`/`dir`) | **Done** — `clove_core::view::Order`, all surfaces (§1) |
| Filters | **Done** — `clove_core::view::Filters`, all surfaces (§2) |
| Read tiering (daemon → index → files) | **Done** — `clove-engine`, all surfaces (§4) |
| Search match set + ranking | **Done** — one substring matcher, no index tier (§6.1) |
| Canonical timestamp spelling | **Done** — `clove_types::canonical_rfc3339`, no index bump (§3) |
| Index rebuild on schema bump | **Done** — `Index::open_or_rebuild` |
| `/board` window | **Done** — per-column `limit`/`offset` |

---

## 1. `Order` — one sort contract, every surface — **DONE**

Shipped as specified. `clove_core::view::{SortField, Order}` is the single
comparator; `clove-web`'s `sort_items` is a thin wrapper over it and
`view::sort_by_rank` is gone (`Order::default()` is the same key). `--sort`/
`--desc` on `ls`/`ready`/`blocked`/`query`/`search`, `sort`/`desc` on the MCP
`FilterArgs`/`SearchArgs`, `?sort=`/`?dir=` unchanged on the web, and
`clove_ipc::QueryRequest.order` on the wire (no `PROTOCOL_VERSION` bump — a
`#[serde(default)]` field over length-delimited JSON is compatible both ways).
Both hardcoded `ORDER BY` clauses in `clove-index` are generated from one
`match` on the enum. `_meta.sort`/`_meta.dir` echo what was applied.

Three notes for whoever picks up §2/§4:

- Search ordering is a separate type, `view::SearchOrder` (`field: Option<
  SortField>`), because relevance is not a `SortField` and `--desc` alone had to
  stay meaningful. `_meta.sort` reads `relevance` in the default case.
- `clove blocked` is the one list with no index tier: its daemon RPC returns ids
  in rank order and carries no sort, so `cmd/blocked.rs` reorders locally
  (reversing for `rank --desc`, re-sorting for every other field). §4's engine
  extraction is the natural place to fold that away.
- `created`/`updated` on the index path compare as TEXT, and that is safe. The
  column is written from a parsed `DateTime<Utc>` via `to_rfc3339()`, so the
  index re-spells whatever the file said into one `+00:00` form; a hand-edited
  `Z` or `+02:00` cannot desynchronize the paths. Differing sub-second precision
  is safe too, and not by luck: `'+' (0x2B) < '.' (0x2E)`, so a truncated
  fraction sorts before a longer one at the same instant. An earlier draft of
  this section claimed the opposite and deferred it to §3 — it was wrong, and §3
  is smaller than it looked because of it.

**Left alone deliberately: two client-side sorters remain.** Neither is on a
read path this section covers, but both will drift when a sort field is added:

- `clove-tui`'s `app::mod::apply_sort` carries its own `SortField`
  (`Default|Priority|Created|Updated|Id`) over the in-memory view. Its four
  named fields match `view::Order` — same keys, same id tiebreak, same whole-key
  reverse — and it has no `status`/`type`. Its *default* order diverged until
  this phase: `apply_sort` returned before reading the direction, while
  `toggle_sort_dir` flipped it unconditionally and the header rendered the new
  arrow, so `S` on the default sort showed `↓` over a list that had not moved.
  Fixed here (`toggling_direction_reverses_the_default_order`). Folding the
  whole thing into `view::Order` is still worth doing; it was skipped because it
  churns the render snapshots.
- ~~The SPA's `web/src/lib/filter.ts::sortItems` sorts the fetched store
  client-side.~~ **Fixed in §5**, which is where it became a live bug rather
  than a latent one. `sortItems` is no longer on the live path at all — the list
  route renders the server's page in the server's order — and as the mock
  backend's sorter it now ends every key in an **id** tiebreak (not the fetched
  array's insertion order) and compares strings by byte order (not
  `localeCompare`, which is locale collation). The two were equivalent to the
  server's answer only while the fetch was unpaginated, exactly as this note
  predicted. It also gained `status`/`type`, so it covers every `SortField`.

The original write-up follows.

**Problem.** `sort` and `dir` exist only on the web API
(`crates/clove-web/src/read.rs::sort_items`, fields `rank|priority|created|
updated|id`). The CLI and MCP have no sort argument at all and always return
`(priority, topological rank, id)`. `docs/DESIGN.md` documented `clove ls
--sort`/`--asc`/`--desc` for a long time; clap rejects them (that stale line is
now corrected).

This matters more than it looks. An agent asking "what changed most recently"
has to pull the whole store and sort client-side — precisely the payload the
shaping work was about cutting.

**Plan.**

1. Add to `clove_core::view`, alongside `Page`:

   ```rust
   pub enum SortField { Rank, Priority, Created, Updated, Id, Status, Type }
   pub struct Order { pub field: SortField, pub descending: bool }
   impl Order {
       pub fn parse(field: Option<&str>, dir: Option<&str>) -> Result<Order, CloveError>;
       pub fn apply(&self, items: &mut [ItemFrontmatter], ranks: &HashMap<CloveId, usize>);
   }
   ```

   `Rank` is the default and keeps today's `(priority, topo, id)`. **Every**
   variant must end in `.then_with(|| a.id.cmp(&b.id))` — the same total-order
   requirement `view::sort_by_match` documents. Pagination over a non-total
   order silently repeats and skips rows; that bug has already been shipped once
   here.

2. Move `view::sort_by_rank` to be `Order::apply` for `SortField::Rank`, so
   there is one sorter rather than two.

3. Thread it through: `ops::{list,ready,blocked,search}` take `Order` next to
   `Page`; CLI gains `--sort`/`--desc`; MCP gains `sort`/`desc` on `FilterArgs`
   and `SearchArgs`; `clove-web`'s `sort_items` is deleted in favour of the
   shared one (keep accepting today's `?sort=`/`?dir=` spellings).

4. **The index path needs matching `ORDER BY`.** `crates/clove-index/src/query.rs`
   has **two** hardcoded order clauses and they are not identical — the list
   query uses `ORDER BY priority ASC, topological_rank ASC, id ASC` and the
   search query adds a `topological_rank IS NULL ASC` term. Both need the new
   ordering; changing only the one you find first leaves search sorted the old
   way. Each `SortField` needs its column, always with `, id ASC` last. Build the
   clause from a match on the enum — never interpolate a user string into SQL.

5. `_meta` echoes `sort` and `dir`, same reasoning as `_meta.limit`.

**Risk.** The index and file paths must agree on ordering or `--no-index`
changes results. Test by fixture: build a store where every sort field
discriminates differently, then assert the file, index, and daemon paths return
identical id sequences for each `SortField`. That triple-comparison is the test
this work lives or dies on.

**Search is a special case.** `ops::search` orders by
`(match class, priority, id)` — relevance first. An explicit `sort` should
replace the whole key, not just the tail, and the default must stay
relevance-first.

---

## 2. `Filters` — one filter set, and an exhaustive push-down — **DONE**

Shipped as specified, with two deviations noted below. `clove_core::view::Filters`
is the single filter set on every surface: sets for `status`/`item_type`/
`priority` (any-of), `labels` (all-of), plus `assignee` and `q`, with an empty
set meaning unconstrained. `Filters::parse` keeps the single-value spelling;
`Filters::parse_multi` takes repeated/csv values. `--status/--type/--priority/
--label` repeat on the CLI, the MCP tools take `string | string[]` through an
untagged wrapper, and `clove query`'s JSON filter does too. `clove_index::
push_down` is the exhaustive split, `_meta.filters` echoes the parsed set, and
`clove_ipc::PROTOCOL_VERSION` is 5 (6 after §6.1 removed the `search` RPC).

Notes for whoever picks up §4:

- **The wire carries `Filters` whole**, as `QueryRequest.filters`, rather than
  the flat vectors this section described. Same information; the difference is
  that there is no per-field packing/unpacking left on either side of the RPC,
  so a newly-added filter cannot be forgotten in a translation that still
  compiles. `GraphRequest::Blocked` lost `include_warnings` and gained `order`
  in the same bump (§7's first bullet, folded in here as planned).
- **`q` is the only residue, and it is a design decision, not a gap.** SQLite's
  `LIKE`/`lower()` case-fold ASCII only while `str::to_lowercase` is full
  Unicode, so pushing `q` into SQL would reintroduce the ASCII-folding half of
  §6.1's file-vs-index divergence on a *filter* rather than a search. A residue changes the query
  mechanics too — the `LIMIT` may not be pushed down and `COUNT(*)` is not the
  total — and `clove_index::query_filtered` is the one place that knows it. The
  residue path also has to select full rows, since `q` reads labels and the lean
  projection does not carry them.
- **`clove blocked` no longer re-sorts locally.** The daemon orders the blocked
  set by running the index's own `ORDER BY` over the whole store and retaining
  the blocked ids from that sequence — reusing `order_by_sql` rather than adding
  a third comparator. The graph cache's `ItemMeta` has no timestamps, so sorting
  from the graph alone was never going to cover `created`/`updated`.
- **One behaviour change on the web**, beyond gaining nothing it did not have:
  an unparseable filter value is now a `VALIDATION_ERROR` instead of a filter
  that matches nothing, and `?q=` matches id/title/labels separately rather than
  concatenated into one haystack.

The original write-up follows.

**Problem.** The web API accepts strictly more than `clove_core::Filters`:
multi-valued (csv) `status`/`type`/`priority`, AND-ed multi-label, and a `q`
substring over id/title/labels. The CLI and MCP accept one value per field. So
"open or in progress, labelled area:core and area:ios" is expressible in the
browser and nowhere else.

**Plan.**

1. Widen `clove_core::view::Filters` to `status: Vec<ItemStatus>`,
   `item_type: Vec<ItemType>`, `priority: Vec<Priority>`, `labels: Vec<String>`
   (AND), plus `q: Option<String>`. Empty vec = unconstrained, so
   `Filters::default()` behaves exactly as today.

2. Keep `Filters::parse` accepting single values (the CLI/MCP spelling) and add
   `Filters::parse_multi` for csv. `matches()` becomes "any-of within a field,
   all-of across fields" — which is what the web already does, so the web keeps
   its behaviour and the other two gain it.

3. CLI: allow repeating `--status`/`--type`/`--priority`/`--label`. MCP: accept
   `string | string[]` on those args (`#[serde(untagged)]` wrapper) so existing
   single-value callers are unaffected.

4. **Exhaustive push-down.** `clove_index::Filter` must express every
   `Filters` field, and there must be a test that fails when a field is added
   to one and not the other. The cheap version: a function
   `fn push_down(f: &Filters) -> (index::Filter, Option<PostFilter>)` where
   `PostFilter` is the residue applied in memory, plus a match on `Filters` with
   no `..` rest pattern so a new field is a compile error. Without this a filter
   silently stops constraining on the index path — the class of bug that
   `--include-warnings` and `?limit=0` already demonstrated.

5. `_meta.filters` echoes the parsed filter set, so a client can confirm what
   was actually applied.

**Wire impact.** `clove_ipc::QueryRequest`'s scalar filter fields become
vectors. The codec is length-delimited **JSON**, so `#[serde(default)]` on the
new fields is compatible both directions and a mixed-version `clove`/`cloved`
pair keeps working — but the *semantics* change (a client sending
`status: "open"` against a daemon expecting a list). Bump
`clove_ipc::PROTOCOL_VERSION` from its current **4** to 5, and let the existing
handshake reject the mismatch — `client.rs` already fails a version mismatch;
`clove daemon` restarts are cheap and the daemon is a cache, not a source of
truth.

---

**Left alone deliberately: two client-side predicates remain.** Same shape as
§1's client-side sorters.

- `clove-tui`'s `app::listing::ViewFilter::matches` is a fifth copy of the
  predicate (single-valued `status`, no `q`, labels all-of, types/priorities
  any-of), with `q_matches` re-implemented inline in `app::mod`. No behaviour
  difference today where they overlap.
- ~~The SPA's `web/src/lib/filter.ts::applyFilters` matters more, because the
  shipped UI **never sends filters to the server**.~~ **Fixed in §5**, and it was
  indeed the same change as making it page. `store.svelte.ts` is query-driven
  now: the list route sends the filters, the ordering and the window, and
  renders what comes back. `applyFilters` survives only inside `api.ts`'s mock
  backend, where its job is to answer like the server rather than to decide what
  the browser shows.


## 3. Canonical timestamps — **DONE**

Shipped, with **no index-schema bump** and two deliberate exceptions. There is one
spelling clove writes — `clove_types::canonical_rfc3339` (UTC, whole seconds,
`Z`) — and every read accepts any parseable RFC 3339 and normalizes it.
`ItemFrontmatter` does the normalizing at the *type* boundary
(`clove_types::time::{serde_ts, serde_ts_opt}`), so YAML frontmatter, `import
json` get it — the surfaces that carry an `ItemFrontmatter` — and neither can be
forgotten. No flag day, no `clove migrate`: a store is re-spelled as it is
written.

**The index version stayed at 6, and checking was the right call.** The item
timestamp columns are an internal ordering key — the list projection
(`ItemListRow`) carries no timestamps at all, so nothing renders them — and
canonicalizing them would have rewritten the bytes of every row in every existing
index to change a string no surface shows. What *is* canonical is the value they
are written from, so the index and file paths cannot rank two items differently.
Pinned by `timestamp_columns_keep_their_stored_spelling` (clove-index), which
exists to fail if someone re-spells them without bumping. The section's own note
turned out to matter in the other direction too: the `snapshots` table — the one
place old and new spellings genuinely coexist — is *preserved verbatim* across a
schema rebuild, so a bump would not have migrated the very rows this section
cares about. What fixes those is the lazy rewrite in `record_snapshot` plus a
spelling-agnostic comparison (`substr(captured_at, 1, 19)`) for both the
`ORDER BY` and the `--since` bound.

**Two things the plan got wrong.**

- **"Comment timestamps … are not [truncated]" — half right.** The gap was real
  but it is in the *rendering*: `ops::comments` rendered `to_rfc3339()` of a
  nanosecond `Utc::now()`, so a comment stamped
  `2026-06-02T08:54:22.904816670+00:00` sat a line below an item's
  `2026-06-02T08:54:22Z`. Truncating what is *stored* — the file name — is a
  regression, not a fix: comment files are append-only and never rewritten, so
  the name's fraction is the only record of the order of two comments added in
  the same second. Truncating it re-orders such a thread arbitrarily, which two
  existing tests (`comments_limit_returns_most_recent_n`,
  `comments_page_from_the_newest_end`) caught immediately. The name format is
  therefore unchanged — which also means comments written by an older clove need
  no migration and cannot produce a duplicate-looking file — and only the
  rendering is canonical. The tie this exposed *was* worth fixing: `list_comments`
  now breaks a timestamp+author tie on the file name, so a thread pulled from
  GitHub (whose `created_at` has second resolution) cannot come back in `readdir`
  order.
- **"`clove-import`'s GitHub sync compares them" — true, but not as strings.**
  Every comparison in `sync`/`sync_net` is over parsed `DateTime<Utc>` values, so
  a pure `Z`-vs-`+00:00` difference was never the bug. The reachable failure is
  *precision*: `local_updated > entry.local_updated` is true for a re-spelling
  that adds a sub-second fraction to the same second, so a merge, a hand-edit, or
  a foreign tool produced a no-op PATCH on every subsequent sync. Pinned by
  `a_re_spelled_local_timestamp_is_not_a_change` in
  `crates/clove/tests/sync_github.rs`.

The original write-up follows.

Two separate version numbers, easy to confuse: the **item** `schema` field
(`clove_types::CURRENT_SCHEMA_VERSION`, **1**) and the **index** schema
(`clove_index::SCHEMA_VERSION`, in `PRAGMA user_version`, now **6** after §6.1
dropped the FTS tables). This section bumps the index one again. The item
schema stays at 1 — canonical RFC3339 is still RFC3339, so nothing about the
file format changes shape.

**Problem.** Timestamps are stored as written. `clove-import`'s GitHub sync
compares them, `clove stats --history` sorts by them as strings
(`ORDER BY captured_at DESC`), and the index stores them verbatim. Two
equivalent RFC3339 spellings (`Z` vs `+00:00`, differing sub-second precision)
compare unequal, so a round-trip through a foreign tracker can look like a
change when nothing changed.

**Plan.**

1. Canonicalize on write: parse to `DateTime<Utc>`, emit one spelling
   (`to_rfc3339_opts(SecondsFormat::Secs, true)`). Item frontmatter is already
   second-truncated, but **comment timestamps and stats snapshots are not** —
   both carry nanoseconds (`…T08:54:22.904816670+00:00`), so the canonicalization
   has to cover them too.
2. Bump `clove_index::SCHEMA_VERSION` (now **6**, after §6.1) **only if** the
   canonicalization changes what the index stores. Note it may not: the index
   already normalizes item timestamps through `to_rfc3339()` on write (see §1),
   so the item columns are canonical today. Check before bumping — a bump costs
   every user a rebuild. Existing indexes holding non-canonical strings would be
   replaced rather than compared
   against. The tripwire assertion in `db.rs` makes this a deliberate act.
3. ~~`Index::open_or_rebuild`~~ — **done**, shipped with the v5 bump for FTS
   labels and exercised again by the v6 FTS removal (§6.1). On a version
   mismatch it rebuilds from the files rather than
   leaving an empty index. It was a prerequisite for bumping the version at all:
   without it every bump ships a window where searches return nothing and the
   CLI silently falls back to file scans for every query.
4. Migration on read: accept any parseable RFC3339, rewrite on next mutation.
   No flag day, no `clove migrate` — the store is files and users have branches
   in flight.

**Test.** A fixture with every spelling variant that must compare equal after
canonicalization, and a `sync github` round-trip asserting zero diffs when
nothing changed.

This section is now smaller than it was: the rebuild half shipped with §6.1,
leaving only the canonicalization itself.

---

## 4. `clove-engine` — the read tier as one crate — **DONE**

Shipped as specified. `crates/clove-engine` owns the daemon → index → files
cascade once per method (`list`, `ready`, `blocked`, `search`, `show`,
`comments`, `dep_tree`, `stats`); the CLI's `cmd/{ls,ready,blocked,query,
search}`, `clove-mcp`'s tool engine, and `clove-web`'s `read.rs` are adapters
(parse → call → render). `cmd/index_read.rs` is gone. `_meta.source` — plain
`source` on the MCP page, which has no `_meta` — is always
`clove_engine::Source::as_str` on the five list commands — `stats` and `export`
still write theirs literally. DESIGN §6.8 is the spec.

**Six notes for whoever comes next.**

- **`Projection` is what makes a tier usable by a full-object surface.** `Lean`
  returns the index/daemon row as-is (no file reads at all) — `clove ls`'s fast
  path. `Full` lets a tier answer the *query* (filtering, ordering, and counting
  stay in SQL) and reads back only the returned page, frontmatter-only and in
  parallel above 500 rows. That is what let the MCP tools and the web gain the
  tiers without changing a single field of their output. `Files` is a caller
  *policy* — `clove ls --fields id,created` uses it, because the CLI's contract
  is that a field outside the lean row falls back to the files rather than
  quietly arriving from a different projection.
- **The engine windows before it returns.** `emit` (CLI) and
  `ops::page_payload` (MCP/core) therefore must not re-page; `total` is always
  the pre-window count. This is what made §5's residue question answerable.
- **The file tier hands back the graph it built** (`ListAnswer::graph`). Without
  it the web scanned and built a *second* whole-store graph on top of the one
  `ops::list_rows` had just discarded — a regression the extraction would
  otherwise have introduced. A tiered answer has no graph and derives the same
  `ready`/`blocked_by`/`dangling_deps` per item from
  `ops::graph_terms_detailed`, whose agreement with the whole-store partition is
  pinned by `detailed_terms_report_the_dangling_subset_like_the_graph`.
- **`--no-index` disables the daemon tier too**, as it always did — the flag
  promises a file scan, and a daemon answering from its hot index is no more of
  one. Nothing tested this: every parity test spends the flag on establishing
  ground truth *before* spawning a daemon. `no_index_bypasses_a_live_daemon`
  (daemon_routing) now pins it.
- **`search` still has exactly one tier**, and `Engine::search` is the one place
  that could change it. `search_reports_files_even_with_a_live_index` guards the
  engine; the CLI additionally builds its engine with the tiers off, so both
  halves are covered.
- **The web's `_meta.source` changed meaning** — the one deliberate behaviour
  change here. It used to report the *serving mode* (`"standalone"`/`"daemon"`),
  so a `cloved`-hosted server claimed `"daemon"` for an answer it had just
  scanned off disk. It now names the tier. The serving mode is still `source` in
  `GET /api/v1/meta`'s payload, which is where the SPA reads it.

**Not done, deliberately.** `clove show`, `clove comments`, `clove dep tree`, and
`clove stats` on the **CLI** still call `ops`/`cmd` code directly rather than the
engine; the engine methods exist and MCP uses all of them (the web still calls
`store.get` directly for item detail, so it has no daemon tier there), but the
CLI's renderers for those four differ enough from `ops::show`/`dep_tree` that
routing them would be a behaviour change, not a refactor. `clove blocked`'s
daemon tier still fetches every blocked id before filtering (`GraphRequest::
Blocked` carries no filter or window); giving it one is a protocol bump and was
out of scope. `clove-tui` reads the store directly and is untouched.

The original write-up follows.

**Problem, precisely stated.** Reads are not tiered consistently:

- **CLI** tiers daemon → index → files (`cmd/index_read.rs`), and each of
  `ls`/`ready`/`blocked`/`query`/`search` re-implements the same three-branch
  cascade with slightly different fallback conditions. `search` alone has its
  own `usable_index` gate that differs from the list commands' on purpose (for a
  search, "zero rows" is indistinguishable from "no matches", so an unverified
  index must not answer).
- **MCP** always reads files. `Engine`'s doc comment says reads are "always
  correct, no daemon needed" — true, but it means the MCP server pays a full
  store scan per call while a hot daemon sits idle beside it. The user's ask was
  explicit: *"CLI should use daemon as well if available and fallback manual if
  not. MCP too. Both should be same."* The write half of that is done (Topology
  B); the read half is not.
- **web** always reads files, and rebuilds the whole graph per request.

**Plan.**

1. New crate `clove-engine`, depending on `clove-core`, `clove-index`,
   `clove-ipc`. One type:

   ```rust
   pub struct Engine { /* store, optional index, cached daemon client, config */ }
   impl Engine {
       pub fn list(&self, f: &Filters, o: Order, p: Page) -> Result<Value, CloveError>;
       pub fn ready(&self, …) -> …;      pub fn blocked(&self, …) -> …;
       pub fn search(&self, …) -> …;     pub fn show(&self, id: &CloveId) -> …;
       pub fn comments(&self, id: &CloveId, p: Page) -> …;
       pub fn dep_tree(&self, …) -> …;   pub fn stats(&self, …) -> …;
   }
   ```

   Each method owns the tiering decision *once*, including the search-specific
   gate. `_meta.source` reports which tier answered — that field already exists
   and is already asserted in tests, which makes the refactor verifiable.

2. `clove-mcp`'s `Engine` and the CLI's `cmd/{ls,ready,blocked,query,search}`
   become thin adapters: parse arguments → call the engine → render. The web's
   `read.rs` follows.

3. **Sequencing matters.** Do §1 (`Order`) and §2 (`Filters`) *first*. They
   change every read signature; doing them after the extraction means touching
   every call site twice.

**Honest caveat.** This is the largest item here and the one with the least
user-visible payoff on its own — it is an enabler. The payoff is that MCP reads
get the daemon and index tiers for free, and the next cross-surface feature is
written once rather than four times. If effort is limited, §1/§2/§5 deliver more
per unit of risk.

---

## 5. Web pagination + SPA paging — **DONE**

Shipped as specified, plus the two client-side leftovers §1 and §2 recorded.
`?fields=`/`?compact=` are on the web API (`/items`, `/items/:id`, `/board`);
the SPA sends `limit`/`offset` and pages; and the list route's client-side
`applyFilters`/`sortItems` are off the live path entirely. `WEB_LIMIT` stays 0.

**The SPA is now query-driven, and that is the whole change.** `store.svelte.ts`
used to issue one unparameterized `GET /items` at startup and hold the store;
every view then filtered, sorted and sliced that copy. It now holds **one server
window**: a view declares what it needs with `store.setQuery(…)` and the server
decides the contents *and* the order. `/list` asks for `limit: 100` at the
current offset; `/board` and `/timeline`, which genuinely render every item, ask
for `limit: 0`. Startup loads `/meta` alone — routes mount before the layout's
`onMount`, so the view has already said which rows it wants by then, and a deep
link to `/list` no longer pulls the whole store first.

**Filtering and sorting moved server-side; `filter.ts` did not die, it changed
job.** The query string already carried the full shared `Filters` and `Order`,
so the list route sends them and renders the response as-is. What is left in
`filter.ts` is the **mock backend's** emulation of the API (`api.ts::filterMock`,
which now filters, sorts *and* windows, in that order) — the fixture the
frontend tests and `npm run dev` run against, which has to answer like the
server or it is worse than useless. Two things §1 flagged are fixed there rather
than tolerated: the tiebreak is the **id**, not the fetched array's insertion
order (only the same answer while the fetch is the entire store — the moment it
pages, ties either side of a page boundary repeat and skip), and string
comparison is byte order rather than `localeCompare`, which is locale collation
and therefore runtime-dependent. `sortItems` also gained `status`/`type`, so
every `SortField` the API accepts has a mock answer.

**Four notes.**

- **`WEB_LIMIT` stays 0, and the reason is that it is not per-endpoint.** The
  gate this section set — "only after the SPA pages" — is open, but the constant
  is shared by `GET /items`, `GET /board`'s per-column window, `GET
  /stats/history`, *and* `GET /items/:id/comments`. A non-zero value is
  therefore not a change to the item list; it is a change to four endpoints at
  once, three of which have no pager — and on comments it would reintroduce
  precisely the defect noted below, a `comment_count` heading over a truncated
  thread. The client it would protect no longer exists either: every SPA view
  now states its own window. Splitting the default per endpoint is the
  prerequisite, and that is a larger change than this section.
- **The tab counts had to give up something, and it is the inactive tabs.**
  All/Ready/Blocked used to count a browser copy of the whole store. With one
  page in hand they cannot be computed, and any *other* source (a `/stats` call,
  a cached number) is a total that does not describe the rows underneath it —
  the same shape as the `comment_count` warning. So only the **active** tab
  carries a number, and it is `_meta.total` from the response that produced the
  visible rows. The pager reads the same field: "101–200 of 412" and the tab
  count cannot disagree, because they are one value.
- **A stale response can no longer overwrite a newer one.** The query changes
  per keystroke in the filter box and `fetch` promises no ordering, so the store
  stamps each load with a token and drops a reply whose token has been
  superseded. Under the old unparameterized fetch there was only ever one
  request shape, so this could not arise.
- **`?fields=`/`?compact=` take the CLI's semantics, not the MCP server's** —
  both default *off*. `clove-mcp` compacts by default because it is spending a
  model's context window; a browser sending no parameters has to keep every key,
  since the SPA reads `assignee: null` and `labels: []` as answers. On the board
  the shaping runs *after* the grouping, which reads each row's `status`:
  projecting first would let `?fields=id` empty every column. An unparseable
  boolean (`?compact=yes`) is a `VALIDATION_ERROR`, like `?sort=` and
  `?status=` — a silent `false` is a response a client cannot distinguish from a
  server that does not implement the parameter.

The original write-up follows.

**Problem.** `WEB_LIMIT` is 0 (unlimited) on purpose — the bundled SPA fetches
the store once and virtualizes. That is fine at a few thousand items and not
fine beyond it: `GET /api/v1/items` scans every file, builds the whole graph,
and serializes every row on every request.

**Plan.**

1. Keep the API default unlimited (changing it breaks the SPA), but have the SPA
   *send* `limit`/`offset` explicitly and page.
2. `crates/clove-web/web/src/lib/api.ts` gains page parameters; the list route
   reads `_meta.total`/`returned`/`limit` — all three already present — and
   renders a pager or infinite scroll.
3. `?fields=`/`?compact=` on the web API, matching CLI and MCP (the one shaping
   gap left).
4. Only after the SPA pages: consider a non-zero `WEB_LIMIT`.

**Watch for.** The Comments tab renders its heading from
`item.comment_count` while listing the fetched array. If a limit is ever
introduced there, the two disagree — a full count above a truncated list. That
is exactly why `GET /items/:id/comments` kept the unlimited web default.

---

**Two things §2 left for this section — both done in §4.**

- ~~**A filter residue ships the whole match set over the RPC.**~~ **Fixed**, at
  the wire rather than in `query_filtered`. The local contract is right as it
  stands: with a residue the `LIMIT` may not be pushed into SQL (slicing before
  the residue removes rows returns too few, and with an offset the wrong ones),
  so `query_filtered` returns every match and the caller windows —
  `query_filtered_defers_the_limit_and_count_when_a_residue_applies` pins
  exactly that, and changing it would have been a behaviour change to fix a
  transfer problem. What was wrong was only what crossed the socket, so
  `cloved`'s `handle_query` truncates to `offset + limit` *after* the residue.
  The client windows what it receives, so the answer is byte-identical, and
  `total` is still the pre-window count. Pinned by
  `a_residue_does_not_ship_the_whole_match_set` (crates/cloved).
  The request side had the same shape and no observable at all — an engine that
  asks for the whole store still renders the right page, because it re-windows
  locally — so it is pinned by a unit test on the request builder
  (`the_query_request_carries_the_window_the_filters_and_the_order`).
- ~~**`clove blocked` has no index tier at all.**~~ **Fixed**:
  `clove_index::QueryMode::Blocked` is the `ready` clause with its last conjuncts
  negated as a disjunction (`has_dangling_deps OR EXISTS(unclosed dep)`, both
  inside `active AND NOT excluded`), so the two modes partition the store exactly
  as `GraphStore` does. The disjunction matters: a dangling-only item has no
  unclosed dep at all, so negating just the `EXISTS` would lose it — and "not
  ready" is not "blocked", since a closed item and a cycle member are neither.
  Pinned by `blocked_set_matches_the_graph_and_partitions_with_ready`
  (clove-index), which asserts the set equals `GraphStore::blocked_items`, that
  the two modes are disjoint, and that their union is exactly the active,
  non-excluded items.


## 6. Divergences addressed

6.1 and 6.2 are both closed. Kept here because the resolutions are decisions
worth recording, not just diffs.

**6.1 `search` disagreed with itself across surfaces — RESOLVED (both halves).**

`ops::search` (the `clove_search` MCP tool; the web has no search route) matched
title, **labels**, and body with three ranking classes; `clove search`'s file
path matched title and body with two, and its index path used an FTS table over
`title, body` only. A label-only hit was returned by the MCP tool and by neither
CLI path.

**Resolved:** matching and ranking now live in `view::rank_search_hits`, used by
`ops::search` and by both CLI paths, and `items_fts` indexes labels (index schema
4 → 5, which is what forced `open_or_rebuild` to be written first). Option (a) of
three: (b) dropping labels from `ops::search` loses capability, and (c) routing
only the CLI's file path through the shared matcher would have made `--no-index`
change results, which is worse than the original bug. Pinned by
`search_agrees_across_the_file_and_index_paths` in
`crates/clove/tests/cli_commands.rs`.

**Second half — substring vs whole token: RESOLVED by deleting the FTS.**
`view::match_class` used `contains()`; `clove_index::query::search` quoted the
needle as a single FTS5 *phrase*, which matches whole tokens. The FTS was
therefore a strictly narrower prefilter and the two disagreed for any needle that
was not a whole token:

| query | fixture | `--no-index` | index |
|---|---|---|---|
| `core` | label `area:core`, body `the corepart word` | 2 | 1 |
| `icode` | label `ünicode-tag` | 1 | 0 |
| `Ünicode` | label `ünicode-tag` | 1 | 0 |

The last row was a second axis: `tokenize='ascii'` case-folds ASCII only, so a
non-ASCII **needle** differing in case from the stored text was found by the file
path (which lowercases both sides) and missed by the index path. Labels are
canonicalized to lowercase on write, so the case difference had to come from the
query — `ünicode` against `ünicode-tag` matched on *both* paths, and a fixture
written the other way round proves nothing.

**The decision: option (a) — drop the FTS, always scan files.** The four options
on the table were widen the FTS to a prefix match (closes `core`, never `icode`),
switch to `unicode61` (closes `Ünicode`, never `icode`), narrow `match_class` to
whole tokens (closes all three by removing substring search), or keep both
behaviours behind an "is this one token" test (silently variable performance).
No FTS query is a superset of substring matching, so the real choice was between
losing mid-word matching — a capability users have today — and losing the
prefilter.

**Measured first, and the numbers decided it.** Two synthetic stores (1,000 and
10,000 items, ~200-word prose bodies, mixed labels/status/type), release build,
warm page cache, best of 7 whole-process runs of `clove search <needle> --limit
0`:

| store | needle | matches | file scan | FTS index path |
|---|---|---|---|---|
| 10k | `zzznotfound` | 0 | 62 ms | 8 ms |
| 10k | `quokka` | 487 (4.9%) | 70 ms | 22 ms |
| 10k | `gateway` | 6,738 (67%) | 173 ms | **233 ms** |
| 10k | `breaker` | 9,872 (99%) | 216 ms | **350 ms** |
| 1k | `zzznotfound` | 0 | 10 ms | 5 ms |
| 1k | `quokka` | 46 | 11 ms | 6 ms |
| 1k | `gateway` | 679 | 18 ms | **26 ms** |
| 1k | `breaker` | 987 | 22 ms | **36 ms** |

The file scan is 62–216 ms at 10k — inside the "few hundred ms" bar — and the
index path *lost* on two of the four needles. That is not a surprise once
written down: the FTS only ever narrowed the **candidate** set, and `clove
search` then had to read every matched item file anyway, because ranking needs
labels and body. The index bought a 40–60 ms saving on a highly selective needle
and cost 60–130 ms on a broad one. It also cost 16.4 MB of the 21.2 MB index file
and most of the reindex time.

So (a) costs no capability, deletes a divergence rather than redefining one, and
is *faster* in the common broad-match case. (b) — prefix-token semantics on both
paths — would have removed mid-word matching to buy a saving the measurements say
is not reliably there.

**What was removed, and the two version bumps.** Leaving the FTS in place as dead
schema would be worse than removing it, so both went:

- **Index schema 5 → 6**: `items_fts` and `fts_map` dropped, along with
  `query::search`, `Index::search`, and `write::fts_rowid`.
  `Index::open_or_rebuild` (shipped in the first half of this section) makes the
  bump free at the point of use — the first read on an old index rebuilds it from
  the files, with no user action. `integrity_check`'s FTS row-count cross-check
  became a `labels`→`items` orphan check, the equivalent for the side table that
  remains. Side effects on a 10k store: `index.db` 21.2 MB → 4.8 MB (−77%),
  `clove reindex` 1.46 s → 0.39 s (−73%).
- **IPC protocol 5 → 6**: the `search` RPC and `SearchRequest` removed. With no
  FTS the daemon had nothing to contribute — the client has to read every matched
  file itself — and a v5 daemon still answering searches from its hot FTS would
  have been exactly the divergence this closes. The bump is what makes the `ping`
  handshake reject it; a `cloved` restart is cheap and the daemon is a cache.

`clove search` now has **one** implementation on every surface:
`view::rank_search_hits` over `ItemStore::scan()`. `_meta.source` is always
`"files"`, and `--no-index` is a no-op on `search`. The `clove_search` MCP tool
already shared `rank_search_hits`, so it needed no change — it was the CLI's
index tier that was the odd one out.

Pinned by three tests, each of which fails if an index tier reappears:
`search_agrees_across_the_file_and_index_paths` in
`crates/clove/tests/cli_commands.rs` (now carrying the three needles above, plus
a literal-needle case for `"quoted" OR x*`, and asserting a *live* index is
present so the `source: "files"` claim is not vacuous);
`search_is_a_file_scan_even_with_a_live_daemon` in
`crates/clove/tests/daemon_routing.rs`; and `schema_has_no_full_text_tables` in
`clove-index`.

**The web's `?q=` stays a filter, and that is correct.** It is
`view::q_matches` over id/title/labels, never the body, applied alongside
`status`/`label`/… — the same predicate the CLI's `--q` and the index path's
`PostFilter` residue use, shared since §2. Folding it into `rank_search_hits`
would make `clove ls --q x` read every body and would cost it composability with
the other filters, so it stays distinct. DESIGN §7.8 now carries a table naming
which is which, and `clove agent-doc` says it too.

**6.2 `GET /api/v1/board` took no window.** It shares `matches()`/`sort_items()`
with the item list, so it accepted every filter and sort parameter and silently
dropped `limit`/`offset`.

Resolved as a **per-column** window rather than by rejecting the parameters: a
board caps how tall a column gets, which is the only reading of one limit over
grouped columns that means anything. `count` stays the column's full size so a
header reading "Closed · 412" over 50 visible cards is honest; `returned` is what
came back; `_meta.per_column` marks the difference from a flat list.

---

## 7. Smaller items

- ~~**`GraphRequest::Blocked { include_warnings }`**~~ — **done**, removed with
  the §2 protocol bump (v4 → v5) as planned. The variant now carries `order`
  instead.
- **Malformed query values on the web** (`?limit=abc`, `?limit=-1`) fall through
  to the default via `.ok()`. The CLI rejects the same input with a clap error.
  Either reject with a `VALIDATION_ERROR` or document the leniency.
- **`--no-index`/`--deep` are `global = true`**, so they appear in `--help` for
  `comments`, `version`, and other commands where they do nothing.
- **`clove stats --since`/`--limit`/`--offset` are silently ignored without
  `--history`.** The help text says "With `--history`:", but silence is still the
  advertised-and-ignored pattern worth avoiding.
- **`_meta` has no schema.** `item-list.json` types it as a bare
  `{"type": "object"}`, so `_meta.limit` validates — and is invisible to any
  client generating from the published schema. The MCP page payload
  (`{total, returned, offset, limit, items}`) has no published schema at all, and
  no `outputSchema` is advertised in `tools/list`.
- **MCP wire duplication.** Every tool result carries the payload twice —
  byte-identical `content[0].text` and `structuredContent` (measured: a 9035-byte
  frame for a 4157-byte payload). Deliberately left alone: the saving is IPC
  bytes, not model context, since the client feeds the model one copy. Worth
  revisiting only alongside publishing `outputSchema`, which is what would let a
  client drop the text copy.
