<script lang="ts">
  import type { Item, Status, ItemType } from '$lib/types';
  import { store } from '$lib/store.svelte';
  import { page } from '$app/stores';
  import { goto } from '$app/navigation';
  import StatusGlyph from '$lib/components/StatusGlyph.svelte';
  import PriorityGlyph from '$lib/components/PriorityGlyph.svelte';
  import TypeIcon from '$lib/components/TypeIcon.svelte';
  import ShortId from '$lib/components/ShortId.svelte';
  import LabelChip from '$lib/components/LabelChip.svelte';
  import Avatar from '$lib/components/Avatar.svelte';
  import BlockedBadge from '$lib/components/BlockedBadge.svelte';
  import { relativeTime } from '$lib/glyphs';

  // ---- URL-encoded state ----
  const url = $derived($page.url);
  const tab = $derived((url.searchParams.get('tab') as 'all' | 'ready' | 'blocked') || 'all');
  const q = $derived(url.searchParams.get('q') || '');
  const fStatus = $derived(url.searchParams.get('status') as Status | null);
  const fAssignee = $derived(url.searchParams.get('assignee'));
  const fTypes = $derived(url.searchParams.getAll('type') as ItemType[]);
  const fPrios = $derived(url.searchParams.getAll('priority').map(Number));
  const fLabels = $derived(url.searchParams.getAll('label'));
  const sort = $derived(url.searchParams.get('sort') || 'updated');
  const dir = $derived((url.searchParams.get('dir') as 'asc' | 'desc') || 'desc');

  let searchInput = $state('');
  $effect(() => {
    searchInput = q;
  });

  function setParams(mut: (p: URLSearchParams) => void) {
    const p = new URLSearchParams(url.searchParams);
    mut(p);
    goto(`?${p.toString()}`, { replaceState: true, keepFocus: true, noScroll: true });
  }

  function setTab(t: string) {
    setParams((p) => (t === 'all' ? p.delete('tab') : p.set('tab', t)));
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
  function applySearch() {
    setParams((p) => (searchInput.trim() ? p.set('q', searchInput.trim()) : p.delete('q')));
  }
  function cycleSort(col: string) {
    setParams((p) => {
      if (p.get('sort') === col) {
        p.set('dir', p.get('dir') === 'asc' ? 'desc' : 'asc');
      } else {
        p.set('sort', col);
        p.set('dir', 'desc');
      }
    });
  }

  // ---- derived counts & filtered list ----
  const base = $derived(store.all);
  function tabFilter(items: Item[], t: string): Item[] {
    if (t === 'ready') return items.filter((i) => i.ready && i.status !== 'closed');
    if (t === 'blocked') return items.filter((i) => i.blocked_by.length > 0);
    return items;
  }

  const counts = $derived({
    all: base.length,
    ready: base.filter((i) => i.ready && i.status !== 'closed').length,
    blocked: base.filter((i) => i.blocked_by.length > 0).length
  });

  const filtered = $derived.by(() => {
    let items = tabFilter(base, tab);
    if (fStatus) items = items.filter((i) => i.status === fStatus);
    if (fAssignee) items = items.filter((i) => (i.assignee ?? '') === fAssignee);
    if (fTypes.length) items = items.filter((i) => fTypes.includes(i.type));
    if (fPrios.length) items = items.filter((i) => fPrios.includes(i.priority));
    if (fLabels.length) items = items.filter((i) => fLabels.every((l) => i.labels.includes(l)));
    if (q) {
      const n = q.toLowerCase();
      items = items.filter(
        (i) => i.title.toLowerCase().includes(n) || i.id.includes(n) || i.body.toLowerCase().includes(n)
      );
    }
    const mul = dir === 'asc' ? 1 : -1;
    items = [...items].sort((a, b) => {
      let c = 0;
      if (sort === 'priority') c = a.priority - b.priority;
      else if (sort === 'id') c = (Number(a.id) || 0) - (Number(b.id) || 0);
      else if (sort === 'created') c = a.created.localeCompare(b.created);
      else if (sort === 'updated') c = a.updated.localeCompare(b.updated);
      else c = a.priority - b.priority; // rank ≈ priority fallback
      return c * mul;
    });
    return items;
  });

  const meta = $derived(store.meta);

  // ---- keyboard nav ----
  let cursor = $state(0);
  $effect(() => {
    if (cursor >= filtered.length) cursor = Math.max(0, filtered.length - 1);
  });
  function onKey(e: KeyboardEvent) {
    const tag = (e.target as HTMLElement)?.tagName;
    if (tag === 'INPUT' || tag === 'TEXTAREA' || tag === 'SELECT') return;
    if (e.key === 'j') {
      e.preventDefault();
      cursor = Math.min(cursor + 1, filtered.length - 1);
    } else if (e.key === 'k') {
      e.preventDefault();
      cursor = Math.max(cursor - 1, 0);
    } else if (e.key === 'Enter') {
      const it = filtered[cursor];
      if (it) goto(`../items/${it.id}`);
    }
  }

  function sortArrow(col: string): string {
    if (sort !== col) return '';
    return dir === 'asc' ? '↑' : '↓';
  }
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
    {#each ['bug', 'feature', 'chore', 'docs', 'epic'] as t (t)}
      <button class="chip" class:on={fTypes.includes(t as ItemType)} onclick={() => toggleMulti('type', t)}>
        <TypeIcon type={t as ItemType} />
      </button>
    {/each}
  </div>
  <div class="multi" role="group" aria-label="Priority filter">
    {#each [0, 1, 2, 3, 4] as p (p)}
      <button class="chip" class:on={fPrios.includes(p)} onclick={() => toggleMulti('priority', String(p))}>
        <PriorityGlyph priority={p} />
      </button>
    {/each}
  </div>

  <form class="search" onsubmit={(e) => { e.preventDefault(); applySearch(); }}>
    <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" aria-hidden="true"
      ><circle cx="11" cy="11" r="7" /><path d="m21 21-4-4" /></svg
    >
    <input bind:value={searchInput} placeholder="filter items…" oninput={applySearch} aria-label="Filter" />
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

<div class="ltabs" role="tablist">
  {#each [['all', 'All'], ['ready', 'Ready'], ['blocked', 'Blocked']] as [key, label] (key)}
    <button class="ltab" class:active={tab === key} role="tab" aria-selected={tab === key} onclick={() => setTab(key)}>
      {label} <span class="n mono">{counts[key as keyof typeof counts]}</span>
    </button>
  {/each}
</div>

<div class="table-wrap panel">
  <table>
    <thead>
      <tr>
        <th style="width:34px"></th>
        <th class="sortable" style="width:60px" onclick={() => cycleSort('id')}>ID <span class="sort">{sortArrow('id')}</span></th>
        <th style="width:42px">Type</th>
        <th class="sortable" style="width:52px" onclick={() => cycleSort('priority')}>Pri <span class="sort">{sortArrow('priority')}</span></th>
        <th>Title</th>
        <th style="width:130px">Assignee</th>
        <th style="width:220px">Labels</th>
        <th class="sortable" style="width:110px" onclick={() => cycleSort('updated')}>Updated <span class="sort">{sortArrow('updated')}</span></th>
      </tr>
    </thead>
    <tbody>
      {#each filtered as item, i (item.id)}
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
      {#if filtered.length === 0}
        <tr><td colspan="8" class="empty dim">No items match these filters</td></tr>
      {/if}
    </tbody>
  </table>
</div>

<style>
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
