<script lang="ts">
  import type { ItemType, ListQuery } from '$lib/types';
  import { store, retryLoad } from '$lib/store.svelte';
  import { page } from '$app/stores';
  import { goto } from '$app/navigation';
  import StatusGlyph from '$lib/components/StatusGlyph.svelte';
  import PriorityGlyph from '$lib/components/PriorityGlyph.svelte';
  import TypeIcon from '$lib/components/TypeIcon.svelte';
  import ShortId from '$lib/components/ShortId.svelte';
  import LabelChip from '$lib/components/LabelChip.svelte';
  import Avatar from '$lib/components/Avatar.svelte';
  import BlockedBadge from '$lib/components/BlockedBadge.svelte';
  import { relativeTime, priorityLabel } from '$lib/glyphs';
  import { parseQuery } from '$lib/query';
  import { defaultDir } from '$lib/filter';
  import { Virtual } from '$lib/virtual.svelte';

  // Fallbacks when /meta isn't available yet.
  const TYPES_FALLBACK: ItemType[] = ['bug', 'feature', 'chore', 'docs', 'epic'];
  const PRIOS_FALLBACK = [0, 1, 2, 3, 4];

  /** Rows per request. The API default is unlimited, so this is sent explicitly. */
  const PAGE_SIZE = 100;

  // ---- URL-encoded state ----
  const url = $derived($page.url);
  const query = $derived<ListQuery>(parseQuery(url.searchParams));
  const tab = $derived(query.mode ?? 'list');
  const q = $derived(query.q ?? '');
  const fStatus = $derived(query.status ?? null);
  const fAssignee = $derived(query.assignee ?? null);
  const fTypes = $derived(query.type ?? []);
  const fPrios = $derived(query.priority ?? []);
  const fLabels = $derived(query.label ?? []);
  const sort = $derived(query.sort || 'rank');
  // No explicit dir → the column's natural direction. A blanket 'desc' default
  // rendered the default (rank) view in REVERSE canonical order.
  const dir = $derived(query.dir || defaultDir(sort));

  let searchInput = $state('');
  $effect(() => {
    searchInput = q;
  });

  // 1-based page number, in the URL so the back button and a shared link land
  // on the same rows. The window itself (`limit`/`offset`) is derived from it
  // and sent to the server — the browser URL never carries those.
  const pageNum = $derived(Math.max(1, Math.trunc(Number(url.searchParams.get('page')) || 1)));
  const offset = $derived((pageNum - 1) * PAGE_SIZE);

  function setParams(mut: (p: URLSearchParams) => void) {
    const p = new URLSearchParams(url.searchParams);
    // Any change to the query invalidates the page number: page 4 of the old
    // filter is not page 4 of the new one, and is usually past the end. The
    // pager itself sets `page` inside `mut`, after this delete.
    p.delete('page');
    mut(p);
    goto(`?${p.toString()}`, { replaceState: true, keepFocus: true, noScroll: true });
  }

  function setTab(t: string) {
    // The list "tab" param and ListQuery "mode" are the same concept; we write
    // `mode` (query.ts canonical name) and treat 'all' as the no-op default.
    setParams((p) => {
      p.delete('tab');
      t === 'all' || t === 'list' ? p.delete('mode') : p.set('mode', t);
    });
  }
  function toggleMulti(key: string, val: string) {
    setParams((p) => {
      const cur = p.getAll(key);
      p.delete(key);
      const next = cur.includes(val) ? cur.filter((v) => v !== val) : [...cur, val];
      next.forEach((v) => p.append(key, v));
    });
  }
  function setSingle(key: string, val: string | null) {
    setParams((p) => (val ? p.set(key, val) : p.delete(key)));
  }

  // Debounce the search→URL write so each keystroke doesn't navigate + re-derive.
  let searchTimer: ReturnType<typeof setTimeout> | undefined;
  function applySearch(immediate = false) {
    clearTimeout(searchTimer);
    const run = () => setParams((p) => (searchInput.trim() ? p.set('q', searchInput.trim()) : p.delete('q')));
    if (immediate) run();
    else searchTimer = setTimeout(run, 220);
  }
  function cycleSort(col: string) {
    setParams((p) => {
      if ((p.get('sort') || 'rank') === col) {
        const cur = p.get('dir') || defaultDir(col);
        p.set('dir', cur === 'asc' ? 'desc' : 'asc');
      } else {
        p.set('sort', col);
        p.set('dir', defaultDir(col));
      }
    });
  }

  // ---- the server answers the query; we render the page it returns ----
  //
  // Filtering, ordering and windowing all happen server-side (read-path roadmap
  // §5). They used to happen here, over a client copy of the whole store, which
  // meant the shared `Filters`/`Order` the API implements were never exercised
  // by the browser — and the two disagreed about the same URL more than once.
  // Under paging a client-side filter is not merely duplicated, it is wrong: it
  // can only see the rows already fetched.
  $effect(() => {
    store.setQuery({ ...query, sort, dir, limit: PAGE_SIZE, offset });
  });

  const rows = $derived(store.all);
  /** Matches for this query before the window — the server's `_meta.total`. */
  const total = $derived(store.total);
  const pageCount = $derived(Math.max(1, Math.ceil(total / PAGE_SIZE)));
  const firstShown = $derived(rows.length ? offset + 1 : 0);
  const lastShown = $derived(offset + rows.length);

  function gotoPage(n: number) {
    const clamped = Math.min(Math.max(1, n), pageCount);
    setParams((p) => {
      if (clamped > 1) p.set('page', String(clamped));
    });
  }

  // Deleting enough items can strand the URL past the end of the result set;
  // the server answers honestly with zero rows, so step back to the last page
  // that has any. Guarded on `total > 0` so an empty result stays on page 1
  // rather than oscillating.
  $effect(() => {
    if (store.loaded && total > 0 && offset >= total) gotoPage(pageCount);
  });

  const meta = $derived(store.meta);
  const typeOptions = $derived((meta?.types as ItemType[] | undefined)?.length ? (meta!.types as ItemType[]) : TYPES_FALLBACK);
  const prioOptions = $derived(meta?.priorities?.length ? meta!.priorities : PRIOS_FALLBACK);

  // ---- keyboard nav ----
  let cursor = $state(0);
  $effect(() => {
    if (cursor >= rows.length) cursor = Math.max(0, rows.length - 1);
  });
  function onKey(e: KeyboardEvent) {
    if (e.ctrlKey || e.metaKey || e.altKey) return; // never hijack shortcuts
    const tag = (e.target as HTMLElement)?.tagName;
    if (tag === 'INPUT' || tag === 'TEXTAREA' || tag === 'SELECT') return;
    if (e.key === 'j') {
      e.preventDefault();
      cursor = Math.min(cursor + 1, rows.length - 1);
    } else if (e.key === 'k') {
      e.preventDefault();
      cursor = Math.max(cursor - 1, 0);
    } else if (e.key === 'Enter') {
      const it = rows[cursor];
      if (it) goto(`../items/${it.id}`);
    }
  }

  function sortArrow(col: string): string {
    if (sort !== col) return '';
    return dir === 'asc' ? '↑' : '↓';
  }
  function ariaSort(col: string): 'ascending' | 'descending' | 'none' {
    if (sort !== col) return 'none';
    return dir === 'asc' ? 'ascending' : 'descending';
  }
  function onThKey(e: KeyboardEvent, col: string) {
    if (e.key === 'Enter' || e.key === ' ') {
      e.preventDefault();
      cycleSort(col);
    }
  }

  // ---- virtualized rows (@tanstack/virtual-core) ----
  // Rows are uniform, so a fixed estimate is exact: no per-row measurement. We
  // render only the virtual rows and bracket them with two spacer <tr> (top =
  // first row's start offset, bottom = total - last row's end) so the <table>
  // stays valid and the sticky <thead> keeps working.
  const ROW_H = 38; // fixed row height (px); matches td padding + line height
  let scrollEl = $state<HTMLDivElement | undefined>();

  // Stable key fn — reads the live `rows` lazily (not captured at init).
  const itemKey = (i: number) => rows[i]?.id ?? i;

  const virtual = new Virtual({
    count: 0,
    getScrollElement: () => scrollEl ?? null,
    estimateSize: () => ROW_H,
    overscan: 12,
    getItemKey: itemKey
  });

  // Mount once the scroll container exists; teardown on unmount.
  $effect(() => {
    if (scrollEl) return virtual.attach();
  });
  // Re-sync the virtualizer whenever the page changes (count/keys).
  $effect(() => {
    virtual.update({ count: rows.length, getItemKey: itemKey });
  });

  const vItems = $derived(virtual.items);
  const padTop = $derived(vItems.length ? vItems[0].start : 0);
  const padBottom = $derived(vItems.length ? virtual.total - vItems[vItems.length - 1].end : 0);
</script>

<svelte:window on:keydown={onKey} />

<div class="lbar">
  <div class="dd-group">
    <select aria-label="Status filter" value={fStatus ?? ''} onchange={(e) => setSingle('status', e.currentTarget.value || null)}>
      <option value="">Status: any</option>
      <option value="open">Open</option>
      <option value="in_progress">In Progress</option>
      <option value="closed">Closed</option>
    </select>
    <select aria-label="Assignee filter" value={fAssignee ?? ''} onchange={(e) => setSingle('assignee', e.currentTarget.value || null)}>
      <option value="">Assignee: any</option>
      {#each meta?.assignees ?? [] as a (a)}
        <option value={a}>{a}</option>
      {/each}
    </select>
  </div>

  <div class="multi" role="group" aria-label="Type filter">
    {#each typeOptions as t (t)}
      <button
        class="chip"
        class:on={fTypes.includes(t)}
        aria-label="Filter by type {t}"
        aria-pressed={fTypes.includes(t)}
        title="type: {t}"
        onclick={() => toggleMulti('type', t)}
      >
        <TypeIcon type={t} />
      </button>
    {/each}
  </div>
  <div class="multi" role="group" aria-label="Priority filter">
    {#each prioOptions as p (p)}
      <button
        class="chip"
        class:on={fPrios.includes(p)}
        aria-label="Filter by {priorityLabel(p)}"
        aria-pressed={fPrios.includes(p)}
        title={priorityLabel(p)}
        onclick={() => toggleMulti('priority', String(p))}
      >
        <PriorityGlyph priority={p} />
      </button>
    {/each}
  </div>

  <form class="search" onsubmit={(e) => { e.preventDefault(); applySearch(true); }}>
    <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" aria-hidden="true"
      ><circle cx="11" cy="11" r="7" /><path d="m21 21-4-4" /></svg
    >
    <input bind:value={searchInput} placeholder="filter items…" oninput={() => applySearch()} aria-label="Filter" />
  </form>
</div>

{#if fLabels.length}
  <div class="active-labels">
    <span class="dim">Labels (AND):</span>
    {#each fLabels as l (l)}
      <LabelChip label={l} removable onremove={() => toggleMulti('label', l)} />
    {/each}
  </div>
{/if}

<!--
  Only the *active* tab carries a count, and it is `_meta.total` for the query
  that produced the rows below. The three counts used to be computed from a
  browser copy of the whole store; with only one page in hand they cannot be,
  and a number derived from anything but this response would be the same defect
  the Comments tab has to avoid — a full count standing over a partial list.
-->
<div class="ltabs" role="tablist">
  {#each [['list', 'All'], ['ready', 'Ready'], ['blocked', 'Blocked']] as [mode, label] (mode)}
    {@const active = (tab === 'list' ? 'list' : tab) === mode}
    <button class="ltab" class:active role="tab" aria-selected={active} onclick={() => setTab(mode)}>
      {label}
      {#if active && store.loaded}<span class="n mono">{total}</span>{/if}
    </button>
  {/each}
</div>

{#if store.loadError && !store.loaded}
  <div class="panel loaderr" role="alert">
    <div class="loaderr-title">Couldn’t reach the backend</div>
    <p class="dim">{store.loadError}</p>
    <button class="btn primary" onclick={() => retryLoad()}>Retry</button>
  </div>
{:else}
  <div class="table-wrap panel" bind:this={scrollEl}>
    <table>
      <thead>
        <tr>
          <th style="width:34px"></th>
          <th class="sortable" style="width:60px" aria-sort={ariaSort('id')}>
            <button type="button" class="th-btn" onclick={() => cycleSort('id')} onkeydown={(e) => onThKey(e, 'id')}>ID <span class="sort">{sortArrow('id')}</span></button>
          </th>
          <th style="width:42px">Type</th>
          <th class="sortable" style="width:52px" aria-sort={ariaSort('priority')}>
            <button type="button" class="th-btn" onclick={() => cycleSort('priority')} onkeydown={(e) => onThKey(e, 'priority')}>Pri <span class="sort">{sortArrow('priority')}</span></button>
          </th>
          <th>Title</th>
          <th style="width:130px">Assignee</th>
          <th style="width:220px">Labels</th>
          <th class="sortable" style="width:110px" aria-sort={ariaSort('updated')}>
            <button type="button" class="th-btn" onclick={() => cycleSort('updated')} onkeydown={(e) => onThKey(e, 'updated')}>Updated <span class="sort">{sortArrow('updated')}</span></button>
          </th>
        </tr>
      </thead>
      <tbody>
        {#if padTop > 0}<tr class="spacer" style="height:{padTop}px" aria-hidden="true"><td colspan="8"></td></tr>{/if}
        {#each vItems as row (row.key)}
          {@const i = row.index}
          {@const item = rows[i]}
          <tr
            class:cursor={i === cursor}
            onclick={() => goto(`../items/${item.id}`)}
            onmouseenter={() => (cursor = i)}
          >
            <td><StatusGlyph status={item.status} /></td>
            <td><ShortId id={item.id} /></td>
            <td><TypeIcon type={item.type} /></td>
            <td><PriorityGlyph priority={item.priority} /></td>
            <td class="title">
              {item.title}
              {#if item.blocked_by.length}<BlockedBadge blockedBy={item.blocked_by} />{/if}
            </td>
            <td>
              <span class="assignee"><Avatar name={item.assignee} /> <span class="muted">{item.assignee ?? '—'}</span></span>
            </td>
            <td>
              <span class="lblrow">
                {#each item.labels.slice(0, 3) as l (l)}<LabelChip label={l} />{/each}
              </span>
            </td>
            <td class="upd mono">{relativeTime(item.updated)}</td>
          </tr>
        {/each}
        {#if padBottom > 0}<tr class="spacer" style="height:{padBottom}px" aria-hidden="true"><td colspan="8"></td></tr>{/if}
        {#if rows.length === 0}
          <tr><td colspan="8" class="empty dim">{store.loaded ? 'No items match these filters' : 'Loading…'}</td></tr>
        {/if}
      </tbody>
    </table>
  </div>

  <!--
    The range and the total come from the same response as the rows, so
    "101–200 of 412" can never stand over a list that holds something else.
  -->
  <nav class="pager" aria-label="Pagination">
    <span class="range dim mono">
      {#if total === 0}
        {store.loaded ? 'No items' : 'Loading…'}
      {:else}
        {firstShown}–{lastShown} of {total}
      {/if}
    </span>
    {#if pageCount > 1}
      <div class="pbtns">
        <button class="btn" disabled={pageNum <= 1} onclick={() => gotoPage(1)} aria-label="First page">«</button>
        <button class="btn" disabled={pageNum <= 1} onclick={() => gotoPage(pageNum - 1)} aria-label="Previous page">‹</button>
        <span class="dim mono">Page {pageNum} / {pageCount}</span>
        <button class="btn" disabled={pageNum >= pageCount} onclick={() => gotoPage(pageNum + 1)} aria-label="Next page">›</button>
        <button class="btn" disabled={pageNum >= pageCount} onclick={() => gotoPage(pageCount)} aria-label="Last page">»</button>
      </div>
    {/if}
  </nav>
{/if}

<style>
  .pager {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    padding: 10px 2px 0;
    font-size: 12px;
    flex-wrap: wrap;
  }
  .pbtns {
    display: flex;
    align-items: center;
    gap: 6px;
  }
  .pbtns .btn {
    padding: 3px 9px;
  }
  .pbtns .btn:disabled {
    opacity: 0.4;
    cursor: default;
  }
  .lbar {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 4px 2px 12px;
    flex-wrap: wrap;
  }
  .dd-group {
    display: flex;
    gap: 8px;
  }
  select {
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    padding: 6px 8px;
    color: var(--text);
    font-size: 12px;
  }
  .multi {
    display: flex;
    gap: 3px;
  }
  .chip {
    display: inline-flex;
    align-items: center;
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    padding: 3px 6px;
    opacity: 0.55;
  }
  .chip.on {
    opacity: 1;
    border-color: var(--border-strong);
    background: var(--surface-hover);
  }
  .search {
    flex: 1;
    min-width: 180px;
    max-width: 280px;
    margin-left: auto;
    display: flex;
    align-items: center;
    gap: 8px;
    background: var(--surface-inset);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    padding: 6px 10px;
    color: var(--text-dim);
  }
  .search input {
    all: unset;
    flex: 1;
    min-width: 0;
    color: var(--text);
    font-size: 12px;
  }
  .active-labels {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 0 2px 10px;
    font-size: 11px;
  }
  .ltabs {
    display: flex;
    gap: 4px;
    padding: 8px 2px;
  }
  .ltab {
    font-size: 12px;
    color: var(--text-muted);
    padding: 6px 12px;
    border-radius: var(--radius-sm);
    display: flex;
    align-items: center;
    gap: 7px;
    background: none;
    border: none;
  }
  .ltab.active {
    background: var(--surface-hover);
    color: var(--text);
    box-shadow: inset 0 0 0 1px var(--border-strong);
  }
  .ltab .n {
    font-size: 11px;
    color: var(--text-dim);
  }
  .ltab.active .n {
    color: var(--accent);
  }
  .table-wrap {
    overflow: auto;
    max-height: calc(100vh - 220px);
  }
  .loaderr {
    text-align: center;
    padding: 40px 24px;
  }
  .loaderr-title {
    font-size: 15px;
    font-weight: 600;
    margin-bottom: 6px;
  }
  .loaderr p {
    margin: 0 0 14px;
  }
  .th-btn {
    all: unset;
    cursor: pointer;
    display: inline-flex;
    align-items: center;
    gap: 4px;
    font: inherit;
    color: inherit;
  }
  .th-btn:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 1px;
    border-radius: 2px;
  }
  tr.spacer {
    cursor: default;
  }
  tr.spacer:hover {
    background: none;
  }
  tr.spacer td {
    padding: 0;
    border: none;
  }
  table {
    width: 100%;
    border-collapse: collapse;
    font-size: 13px;
  }
  thead th {
    text-align: left;
    font-weight: 500;
    font-size: 11px;
    color: var(--text-dim);
    text-transform: uppercase;
    letter-spacing: 0.5px;
    padding: 9px 12px;
    background: var(--surface);
    border-bottom: 1px solid var(--border);
    white-space: nowrap;
    position: sticky;
    top: 0;
  }
  th.sortable {
    cursor: pointer;
    color: var(--text-muted);
  }
  th .sort {
    color: var(--accent);
  }
  tbody td {
    padding: 9px 12px;
    border-bottom: 1px solid var(--border);
    vertical-align: middle;
    white-space: nowrap;
  }
  tbody tr {
    cursor: pointer;
  }
  tbody tr:hover,
  tbody tr.cursor {
    background: var(--surface-hover);
  }
  tbody tr.cursor td:first-child {
    box-shadow: inset 2px 0 0 var(--accent);
  }
  td.title {
    white-space: normal;
    color: var(--text);
    font-weight: 500;
    max-width: 360px;
    display: flex;
    align-items: center;
    gap: 8px;
  }
  td.upd {
    font-size: 11px;
    color: var(--text-dim);
  }
  .assignee {
    display: inline-flex;
    align-items: center;
    gap: 6px;
  }
  .lblrow {
    display: inline-flex;
    gap: 4px;
  }
  .empty {
    text-align: center;
    padding: 28px;
  }
</style>
