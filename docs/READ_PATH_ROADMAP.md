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
| Result shaping (`fields`/`compact`) | **Done** on CLI + MCP; **absent on the web API** (§5) |
| Ordering (`sort`/`dir`) | Web only — §1 |
| Filters | Web has a superset — §2 |
| Read tiering (daemon → index → files) | CLI only; MCP always reads files — §4 |
| Search match set + ranking | Labels: **done**; substring-vs-token still diverges — §6.1 |
| Index rebuild on schema bump | **Done** — `Index::open_or_rebuild` |
| `/board` window | **Done** — per-column `limit`/`offset` |

---

## 1. `Order` — one sort contract, every surface

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

## 2. `Filters` — one filter set, and an exhaustive push-down

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

## 3. Canonical timestamps

Two separate version numbers, easy to confuse: the **item** `schema` field
(`clove_types::CURRENT_SCHEMA_VERSION`, **1**) and the **index** schema
(`clove_index::SCHEMA_VERSION`, in `PRAGMA user_version`, now **5** after the
FTS-labels bump in §6.1). This section bumps the index one again. The item
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
2. Bump `clove_index::SCHEMA_VERSION` (now **5**, after §6.1) so existing
   indexes holding non-canonical strings are replaced rather than compared
   against. The tripwire assertion in `db.rs` makes this a deliberate act.
3. ~~`Index::open_or_rebuild`~~ — **done**, shipped with the v5 bump for FTS
   labels (§6.1). On a version mismatch it rebuilds from the files rather than
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

## 4. `clove-engine` — the read tier as one crate

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

## 5. Web pagination + SPA paging

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

## 6. Divergences addressed

6.2 is closed; 6.1 is half closed and its remaining half is specified below.
Kept here because the resolutions are decisions worth recording, not just diffs.

**6.1 `search` disagreed with itself across surfaces — labels half resolved,
matching half still open.**

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

**Still open — substring vs whole token.** `view::match_class` uses `contains()`;
`clove_index::query::search` quotes the needle as a single FTS *phrase*, which
matches whole tokens. The FTS is therefore a strictly narrower prefilter and the
two disagree for any needle that is not a whole token:

| query | fixture | `--no-index` | index |
|---|---|---|---|
| `core` | label `area:core`, body `the corepart word` | 2 | 1 |
| `icode` | label `ünicode-tag` | 1 | 0 |
| `Ünicode` | label `ünicode-tag` | 1 | 0 |

The last row is a second axis: `tokenize='ascii'` case-folds ASCII only, so a
non-ASCII **needle** differing in case from the stored text is found by the file
path (which lowercases both sides) and missed by the index path. Labels are
canonicalized to lowercase on write, so the case difference has to come from the
query — `ünicode` against `ünicode-tag` matches on *both* paths, and a fixture
written the other way round proves nothing. This is pre-existing — the index path has always been FTS
while the file path has always been `contains` — but it means "the same query
answers the same way on both paths" is **not** yet true for search, and the
pinning test uses whole-token needles only, so it cannot see it.

Options, none obviously right:
- **Widen the FTS query** to a prefix match (`"needle"*`) — closes the `core`
  row, not the `icode` row, since FTS cannot match inside a token.
- **Switch the tokenizer** to `unicode61` — closes the `ünicode` row at some
  index-size and speed cost, still not `icode`.
- **Narrow `match_class` to whole tokens** so both paths agree — closes all
  three by removing substring search, which is a capability users have today.
- **Treat the index as an accelerator only for whole-token queries** and fall
  back to the file scan otherwise — keeps both behaviours, needs a reliable
  "is this one token" test, and silently changes performance characteristics.

**Also unaddressed:** the web's `?q=` (`crates/clove-web/src/read.rs::matches`)
is a *fourth* predicate — id + title + labels, **no body**, no match-class
ranking. Folding it into `rank_search_hits` needs a decision about whether `?q=`
is a filter (its current role, applied alongside `status`/`label`/…) or a
search.

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

- **`GraphRequest::Blocked { include_warnings }`** (`clove-ipc/src/protocol.rs`)
  is dead: no surface can set it and the only caller hard-codes `true`. Remove
  it with the §2 protocol bump rather than on its own.
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
