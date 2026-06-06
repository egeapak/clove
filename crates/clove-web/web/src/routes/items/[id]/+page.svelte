<script lang="ts">
  import type { Item, Comment, DepTreeNode, Status } from '$lib/types';
  import { store } from '$lib/store.svelte';
  import { api } from '$lib/api';
  import { toasts } from '$lib/toast.svelte';
  import { page } from '$app/stores';
  import { goto } from '$app/navigation';
  import StatusGlyph from '$lib/components/StatusGlyph.svelte';
  import PriorityGlyph from '$lib/components/PriorityGlyph.svelte';
  import TypeIcon from '$lib/components/TypeIcon.svelte';
  import LabelChip from '$lib/components/LabelChip.svelte';
  import Avatar from '$lib/components/Avatar.svelte';
  import BlockedBadge from '$lib/components/BlockedBadge.svelte';
  import Markdown from '$lib/components/Markdown.svelte';
  import DepTree from '$lib/components/DepTree.svelte';
  import { shortId, shortDate, relativeTime, priorityLabel, statusLabel } from '$lib/glyphs';

  let { data } = $props();
  const id = $derived(data.id);

  // Base item comes from the normalized store (kept fresh by live events). The
  // list/board endpoints may omit `body`, so we fetch the full item separately
  // and merge it in — surviving store refetches triggered by live events.
  const stored = $derived<Item | undefined>(store.items.get(id));
  let full = $state<Item | null>(null);
  // The store is the source of truth for live/optimistic state; the separately
  // fetched `full` only backfills fields the list/board endpoints omit (`body`,
  // `comment_count`). Store fields win so inline edits stay reflected.
  const item = $derived<Item | undefined>(mergeItem(stored, full, id));

  function mergeItem(s: Item | undefined, f: Item | null, curId: string): Item | undefined {
    const fresh = f && f.id === curId ? f : null;
    if (!s) return fresh ?? undefined;
    if (!fresh) return s;
    // Store wins for live/optimistic fields; backfill body/comment_count when
    // the store item (from list/board) lacks them.
    return {
      ...s,
      body: s.body ?? fresh.body,
      comment_count: s.comment_count ?? fresh.comment_count
    };
  }

  let comments = $state<Comment[]>([]);
  let deptree = $state<DepTreeNode | null>(null);
  let newComment = $state('');
  let newDep = $state('');

  const view = $derived((($page.url.searchParams.get('view') as string) || 'overview'));

  function setView(v: string) {
    const p = new URLSearchParams($page.url.searchParams);
    if (v === 'overview') p.delete('view');
    else p.set('view', v);
    goto(`?${p.toString()}`, { replaceState: true, noScroll: true, keepFocus: true });
  }

  // load full item (body etc) + deptree when id changes
  $effect(() => {
    const curId = id;
    full = null;
    api
      .item(curId)
      .then((it) => {
        store.upsert(it);
        if (curId === id) full = it;
      })
      .catch(() => {});
    api.deptree(curId).then((t) => (deptree = t)).catch(() => (deptree = null));
  });

  // load comments only when the Comments tab is opened
  let commentsFor = $state('');
  $effect(() => {
    if (view !== 'comments') return;
    const curId = id;
    if (commentsFor === curId) return;
    commentsFor = curId;
    api
      .comments(curId)
      .then((c) => {
        if (curId === id) comments = c;
      })
      .catch(() => {
        if (curId === id) comments = [];
      });
  });

  // ---- inline edits (optimistic) ----
  async function patch(fields: Partial<Pick<Item, 'status' | 'priority' | 'assignee' | 'type'>>) {
    if (!item) return;
    const rollback = store.optimistic(id, fields);
    try {
      store.settle(id, await api.patch(id, fields));
    } catch (e) {
      rollback();
      toasts.error('Update failed');
    }
  }

  async function addComment() {
    const body = newComment.trim();
    if (!body) return;
    newComment = '';
    comments = [...comments, { author: 'you', timestamp: new Date().toISOString(), body }];
    try {
      const updated = await api.addComment(id, body);
      store.upsert(updated);
      if (updated.id === id) full = updated;
    } catch {
      toasts.error('Comment failed');
    }
  }

  async function addDep() {
    const dep = newDep.trim().replace(/^#/, '');
    if (!dep) return;
    newDep = '';
    try {
      store.upsert(await api.addDep(id, dep));
      deptree = await api.deptree(id);
      toasts.push(`Added dependency ${shortId(dep)}`);
    } catch (e) {
      toasts.error('Add dep failed: ' + (e instanceof Error ? e.message : 'error'));
    }
  }

  async function removeDep(dep: string) {
    try {
      store.upsert(await api.removeDep(id, dep));
      deptree = await api.deptree(id);
    } catch {
      toasts.error('Remove dep failed');
    }
  }

  async function removeLabel(l: string) {
    if (!item) return;
    const rollback = store.optimistic(id, { labels: item.labels.filter((x) => x !== l) });
    try {
      store.settle(id, await api.setLabels(id, [], [l]));
    } catch {
      rollback();
      toasts.error('Label update failed');
    }
  }

  let addingLabel = $state('');
  async function addLabel() {
    const l = addingLabel.trim();
    if (!l || !item) return;
    addingLabel = '';
    const rollback = store.optimistic(id, { labels: [...new Set([...item.labels, l])] });
    try {
      store.settle(id, await api.setLabels(id, [l], []));
    } catch {
      rollback();
      toasts.error('Label update failed');
    }
  }

  function onCommentKey(e: KeyboardEvent) {
    if ((e.metaKey || e.ctrlKey) && e.key === 'Enter') {
      e.preventDefault();
      addComment();
    }
  }
</script>

{#if !item}
  <div class="loading dim">Loading {shortId(id)}…</div>
{:else}
  <div class="screen panel detail">
    <div class="detail-main">
      <div class="dhead">
        <span class="id mono">{shortId(item.id)}</span>
        <TypeIcon type={item.type} />
        <span class="tag" style="color:{`var(--type-${item.type})`};border-color:{`var(--type-${item.type})`}">{item.type}</span>
        <span class="tag status"><StatusGlyph status={item.status} /> {statusLabel(item.status)}</span>
        <PriorityGlyph priority={item.priority} label />
      </div>
      <h1 class="dtitle">{item.title}</h1>

      <!-- sub-tabs -->
      <div class="subtabs" role="tablist">
        {#each [['overview', 'Overview'], ['deptree', 'Dep tree'], ['comments', `Comments · ${item.comment_count ?? comments.length}`]] as [k, l] (k)}
          <button class="subtab" class:active={view === k} role="tab" aria-selected={view === k} onclick={() => setView(k)}>{l}</button>
        {/each}
      </div>

      {#if view === 'overview'}
        <Markdown source={item.body} />
      {:else if view === 'deptree'}
        {#if deptree}
          <div class="deptree mono">
            <DepTree node={deptree} currentId={item.id} />
          </div>
        {:else}
          <p class="dim">No dependency tree.</p>
        {/if}
        <div class="dep-add">
          <input bind:value={newDep} placeholder="add dep #id…" onkeydown={(e) => e.key === 'Enter' && addDep()} />
          <button class="btn sm" onclick={addDep}>Add dep</button>
        </div>
        {#if item.deps.length}
          <div class="dep-list">
            {#each item.deps as d (d)}
              <span class="dep-chip mono">{shortId(d)} <button aria-label="remove dep {d}" onclick={() => removeDep(d)}>×</button></span>
            {/each}
          </div>
        {/if}
      {:else if view === 'comments'}
        <div class="comments">
          {#each comments as c (c.timestamp + c.author)}
            <div class="comment">
              <Avatar name={c.author} />
              <div class="body">
                <div class="meta"><b>{c.author}</b> · {relativeTime(c.timestamp)}</div>
                <div class="text">{c.body}</div>
              </div>
            </div>
          {/each}
          {#if comments.length === 0}<p class="dim">No comments yet.</p>{/if}
          <div class="addbox">
            <Avatar name={null} />
            <div class="field">
              <textarea bind:value={newComment} placeholder="Add a comment…" rows="2" onkeydown={onCommentKey}></textarea>
              <div class="addfoot">
                <span class="kbd">⌘↵ to send</span>
                <button class="btn primary sm" onclick={addComment} disabled={!newComment.trim()}>Comment</button>
              </div>
            </div>
          </div>
        </div>
      {/if}
    </div>

    <!-- sidebar -->
    <aside class="detail-side">
      <div class="side-block">
        <div class="side-label">Status</div>
        <select value={item.status} onchange={(e) => patch({ status: e.currentTarget.value as Status })}>
          <option value="open">Open</option>
          <option value="in_progress">In Progress</option>
          <option value="closed">Closed</option>
        </select>
      </div>
      <div class="side-block">
        <div class="side-label">Priority</div>
        <select value={item.priority} onchange={(e) => patch({ priority: Number(e.currentTarget.value) })}>
          {#each [0, 1, 2, 3, 4] as p (p)}<option value={p}>{priorityLabel(p)}</option>{/each}
        </select>
      </div>
      <div class="side-block">
        <div class="side-label">Assignee</div>
        <div class="side-row">
          <Avatar name={item.assignee} />
          <input
            class="inline-in"
            value={item.assignee ?? ''}
            placeholder="Unassigned"
            onchange={(e) => patch({ assignee: e.currentTarget.value.trim() || null })}
          />
        </div>
      </div>
      <div class="side-block">
        <div class="side-label">Labels</div>
        <div class="side-row labels">
          {#each item.labels as l (l)}<LabelChip label={l} removable onremove={() => removeLabel(l)} />{/each}
        </div>
        <input class="inline-in" bind:value={addingLabel} placeholder="add label…" onkeydown={(e) => e.key === 'Enter' && addLabel()} />
      </div>
      <div class="side-block">
        <div class="side-label">Relationships</div>
        {#if item.blocked_by.length}<div class="side-row"><BlockedBadge blockedBy={item.blocked_by} /></div>{/if}
        {#if item.parent}<div class="side-row"><TypeIcon type="epic" /> part of <a href="../items/{item.parent}">{shortId(item.parent)}</a></div>{/if}
        {#each item.relates as r (r)}<div class="side-row dim mono">relates {shortId(r)}</div>{/each}
        {#if !item.blocked_by.length && !item.parent && !item.relates.length}<div class="side-row dim">None</div>{/if}
      </div>
      <div class="side-block last">
        <div class="side-label">Dates</div>
        <div class="side-row mono">created <span class="dim">{shortDate(item.created)}</span></div>
        <div class="side-row mono">updated <span class="dim">{relativeTime(item.updated)}</span></div>
        {#if item.closed}<div class="side-row mono">closed <span class="dim">{shortDate(item.closed)}</span></div>{/if}
      </div>
    </aside>
  </div>
{/if}

<style>
  .loading {
    padding: 40px;
    text-align: center;
  }
  .screen {
    overflow: hidden;
  }
  .detail {
    display: grid;
    grid-template-columns: 1fr 300px;
  }
  .detail-main {
    padding: 18px 22px;
    border-right: 1px solid var(--border);
    min-width: 0;
  }
  .detail-side {
    padding: 18px;
    background: var(--surface-2);
  }
  .dhead {
    display: flex;
    align-items: center;
    gap: 10px;
    flex-wrap: wrap;
    margin-bottom: 6px;
  }
  .id {
    font-size: 14px;
    color: var(--accent);
    font-weight: 700;
  }
  .tag {
    font-size: 11px;
    font-family: var(--font-mono);
    border-radius: var(--radius-sm);
    padding: 2px 8px;
    border: 1px solid;
    background: color-mix(in srgb, currentColor 10%, transparent);
  }
  .tag.status {
    color: var(--text-muted);
    border-color: var(--border-strong);
    background: var(--surface-inset);
    display: inline-flex;
    align-items: center;
    gap: 5px;
  }
  .dtitle {
    font-size: 20px;
    font-weight: 600;
    margin: 4px 0 14px;
  }
  .subtabs {
    display: flex;
    gap: 4px;
    border-bottom: 1px solid var(--border);
    margin-bottom: 14px;
  }
  .subtab {
    background: none;
    border: none;
    color: var(--text-muted);
    padding: 8px 12px;
    font-size: 12px;
    border-bottom: 2px solid transparent;
    margin-bottom: -1px;
  }
  .subtab.active {
    color: var(--text);
    border-bottom-color: var(--accent);
  }
  .deptree {
    font-size: 12px;
    line-height: 1.7;
    background: var(--surface-inset);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    padding: 12px 14px;
  }
  .dep-add {
    display: flex;
    gap: 8px;
    margin-top: 12px;
  }
  .dep-add input {
    flex: 1;
    background: var(--surface-inset);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    padding: 6px 10px;
    color: var(--text);
    font-family: var(--font-mono);
    font-size: 12px;
  }
  .dep-list {
    display: flex;
    gap: 6px;
    flex-wrap: wrap;
    margin-top: 10px;
  }
  .dep-chip {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    font-size: 11px;
    background: var(--surface-inset);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    padding: 2px 7px;
  }
  .dep-chip button {
    border: none;
    background: none;
    color: var(--text-dim);
  }
  .comments .comment {
    display: flex;
    gap: 10px;
    margin: 12px 0;
  }
  .comment .body {
    flex: 1;
  }
  .comment .meta {
    font-size: 12px;
    color: var(--text-dim);
    margin-bottom: 3px;
  }
  .comment .meta b {
    color: var(--text);
  }
  .comment .text {
    color: var(--text-muted);
    font-size: 13px;
    white-space: pre-wrap;
  }
  .addbox {
    display: flex;
    gap: 10px;
    margin-top: 16px;
    align-items: flex-start;
  }
  .addbox .field {
    flex: 1;
  }
  .addbox textarea {
    width: 100%;
    background: var(--surface-inset);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    padding: 10px 12px;
    color: var(--text);
    font-size: 13px;
    font-family: var(--font-sans);
    resize: vertical;
  }
  .addfoot {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin-top: 6px;
  }
  .side-block {
    padding: 12px 0;
    border-bottom: 1px solid var(--border);
  }
  .side-block:first-child {
    padding-top: 0;
  }
  .side-block.last {
    border-bottom: none;
  }
  .side-label {
    font-size: 11px;
    text-transform: uppercase;
    letter-spacing: 0.5px;
    color: var(--text-dim);
    margin-bottom: 7px;
  }
  .side-row {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 12px;
    margin: 5px 0;
    color: var(--text-muted);
    flex-wrap: wrap;
  }
  .side-block select,
  .inline-in {
    width: 100%;
    background: var(--surface-inset);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    padding: 6px 8px;
    color: var(--text);
    font-size: 12px;
  }
  .inline-in {
    margin-top: 4px;
  }
  .side-row .inline-in {
    flex: 1;
    width: auto;
    margin-top: 0;
  }
  @media (max-width: 820px) {
    .detail {
      grid-template-columns: 1fr;
    }
    .detail-main {
      border-right: none;
      border-bottom: 1px solid var(--border);
    }
  }
</style>
