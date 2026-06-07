<script lang="ts">
  import type { Item } from '$lib/types';
  import { store } from '$lib/store.svelte';
  import { api } from '$lib/api';
  import { toasts } from '$lib/toast.svelte';
  import { goto } from '$app/navigation';
  import ItemForm from '$lib/components/ItemForm.svelte';
  import { buildPatch, isEmptyPatch, type FormState } from '$lib/itemForm';
  import { shortId } from '$lib/glyphs';

  let { data } = $props();
  const id = $derived(data.id);

  const stored = $derived<Item | undefined>(store.items.get(id));
  let full = $state<Item | null>(null);
  // Prefer the freshly fetched full item (it carries the body), falling back to
  // the lean store item so the form can render immediately.
  const item = $derived<Item | undefined>((full && full.id === id ? full : undefined) ?? stored);

  let saving = $state(false);

  // Load the full item (body etc.) for the form.
  $effect(() => {
    const curId = id;
    full = null;
    api
      .item(curId)
      .then((it) => {
        store.upsert(it);
        if (curId === id) full = it;
      })
      .catch(() => toasts.error('Failed to load item'));
  });

  function detailHref() {
    return `../${encodeURIComponent(id)}`;
  }

  async function save(form: FormState) {
    if (!full) return;
    // Diff against the same item the form was seeded from (the fully-loaded one).
    const patch = buildPatch(form, full);
    if (isEmptyPatch(patch)) {
      toasts.push('No changes');
      goto(detailHref());
      return;
    }
    saving = true;
    try {
      const updated = await api.patch(id, patch);
      store.settle(id, updated);
      toasts.push(`Saved ${shortId(id)}`);
      goto(detailHref());
    } catch (err) {
      toasts.error('Save failed: ' + (err instanceof Error ? err.message : 'unknown'));
    } finally {
      saving = false;
    }
  }

  // ---- relationships (graph ops apply immediately, like the detail page) ----
  let newDep = $state('');
  async function addDep() {
    const dep = newDep.trim().replace(/^#/, '');
    if (!dep) return;
    newDep = '';
    try {
      const updated = await api.addDep(id, dep);
      store.upsert(updated);
      full = updated;
      toasts.push(`Added dependency ${shortId(dep)}`);
    } catch (e) {
      toasts.error('Add dep failed: ' + (e instanceof Error ? e.message : 'error'));
    }
  }
  async function removeDep(dep: string) {
    try {
      const updated = await api.removeDep(id, dep);
      store.upsert(updated);
      full = updated;
    } catch {
      toasts.error('Remove dep failed');
    }
  }

  let parentDraft = $state('');
  let editingParent = $state(false);
  function startEditParent() {
    parentDraft = item?.parent ?? '';
    editingParent = true;
  }
  async function saveParent() {
    const p = parentDraft.trim().replace(/^#/, '') || null;
    editingParent = false;
    try {
      const updated = await api.setParent(id, p);
      store.upsert(updated);
      full = updated;
      toasts.push(p ? `Parent set to ${shortId(p)}` : 'Parent cleared');
    } catch (e) {
      toasts.error('Set parent failed: ' + (e instanceof Error ? e.message : 'error'));
    }
  }
</script>

<div class="screen panel edit-screen">
  {#if !item}
    <div class="loading dim">Loading {shortId(id)}…</div>
  {:else}
    <div class="ehead">
      <a class="back" href={detailHref()}>← {shortId(id)}</a>
      <h1>Edit item</h1>
    </div>

    <div class="cols">
      <div class="form-col">
        {#if full}
          <ItemForm mode="edit" item={full} submitting={saving} onsubmit={save} oncancel={() => goto(detailHref())} />
        {:else}
          <p class="dim sm">Loading editor…</p>
        {/if}
      </div>

      <aside class="rel-col">
        <div class="rel-block">
          <div class="rel-label">Dependencies</div>
          {#if item.deps.length}
            <div class="chips">
              {#each item.deps as d (d)}
                <span class="chip mono">{shortId(d)}<button type="button" aria-label="remove dep {d}" onclick={() => removeDep(d)}>×</button></span>
              {/each}
            </div>
          {:else}
            <p class="dim sm">No dependencies.</p>
          {/if}
          <div class="rel-add">
            <input bind:value={newDep} placeholder="add dep #id…" onkeydown={(e) => e.key === 'Enter' && addDep()} />
            <button type="button" class="btn sm" onclick={addDep}>Add</button>
          </div>
        </div>

        <div class="rel-block">
          <div class="rel-label">Parent</div>
          {#if !editingParent}
            <div class="rel-row">
              {#if item.parent}<a class="mono" href={`../${item.parent}`}>{shortId(item.parent)}</a>{:else}<span class="dim sm">None</span>{/if}
              <button type="button" class="btn sm" onclick={startEditParent}>Edit</button>
            </div>
          {:else}
            <div class="rel-add">
              <input bind:value={parentDraft} placeholder="parent #id (empty to clear)" onkeydown={(e) => e.key === 'Enter' && saveParent()} />
              <button type="button" class="btn sm primary" onclick={saveParent}>Set</button>
            </div>
          {/if}
        </div>
      </aside>
    </div>
  {/if}
</div>

<style>
  .edit-screen {
    padding: 20px 24px;
    max-width: 1000px;
  }
  .loading {
    padding: 40px;
    text-align: center;
  }
  .ehead {
    display: flex;
    align-items: baseline;
    gap: 14px;
    margin-bottom: 18px;
  }
  .ehead h1 {
    font-size: 18px;
    font-weight: 600;
    margin: 0;
  }
  .back {
    font-size: 12px;
    color: var(--accent);
    font-family: var(--font-mono);
    text-decoration: none;
  }
  .cols {
    display: grid;
    grid-template-columns: 1fr 260px;
    gap: 24px;
  }
  .rel-block {
    padding: 12px 0;
    border-bottom: 1px solid var(--border);
  }
  .rel-block:first-child {
    padding-top: 0;
  }
  .rel-label {
    font-size: 11px;
    text-transform: uppercase;
    letter-spacing: 0.5px;
    color: var(--text-dim);
    margin-bottom: 8px;
  }
  .rel-row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
  }
  .rel-add {
    display: flex;
    gap: 8px;
    margin-top: 8px;
  }
  .rel-add input {
    flex: 1;
    min-width: 0;
    background: var(--surface-inset);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    padding: 6px 8px;
    color: var(--text);
    font-family: var(--font-mono);
    font-size: 12px;
  }
  .chips {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }
  .chip {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    font-size: 11px;
    background: var(--surface-inset);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    padding: 2px 4px 2px 8px;
  }
  .chip button {
    border: none;
    background: none;
    color: var(--text-dim);
    cursor: pointer;
  }
  .sm {
    font-size: 12px;
  }
  @media (max-width: 820px) {
    .cols {
      grid-template-columns: 1fr;
    }
  }
</style>
