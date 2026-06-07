<script lang="ts">
  import type { Item } from '$lib/types';
  import { untrack } from 'svelte';
  import { store } from '$lib/store.svelte';
  import { emptyForm, formFromItem, isSubmittable, parseLabels, type FormState } from '$lib/itemForm';
  import Markdown from '$lib/components/Markdown.svelte';

  let {
    mode,
    item,
    submitting = false,
    submitLabel,
    onsubmit,
    oncancel,
    firstField = $bindable()
  }: {
    mode: 'create' | 'edit';
    item?: Item;
    submitting?: boolean;
    submitLabel?: string;
    onsubmit: (form: FormState) => void;
    oncancel?: () => void;
    firstField?: HTMLInputElement | undefined;
  } = $props();

  // One form-state object, snapshotted from the item once at mount (the parent
  // remounts/gates the form when a different item should be edited, so this is
  // deliberately non-reactive — user edits must not be clobbered by refetches).
  let form = $state<FormState>(untrack(() => (item ? formFromItem(item) : emptyForm())));

  const TYPES = $derived(store.meta?.types?.length ? store.meta.types : ['feature', 'bug', 'chore', 'docs', 'epic']);
  const PRIOS = $derived(store.meta?.priorities?.length ? store.meta.priorities : [0, 1, 2, 3, 4]);
  const STATUSES = $derived(store.meta?.statuses?.length ? store.meta.statuses : ['open', 'in_progress', 'closed']);

  let labelDraft = $state('');
  let bodyTab = $state<'write' | 'preview'>('write');

  const canSubmit = $derived(isSubmittable(form) && !submitting);

  function addLabels() {
    const parsed = parseLabels(labelDraft);
    if (parsed.length) {
      const set = new Set([...form.labels, ...parsed]);
      form.labels = [...set];
    }
    labelDraft = '';
  }

  function removeLabel(l: string) {
    form.labels = form.labels.filter((x) => x !== l);
  }

  function onLabelKey(e: KeyboardEvent) {
    if (e.key === 'Enter' || e.key === ',') {
      e.preventDefault();
      addLabels();
    }
  }

  function submit(e: Event) {
    e.preventDefault();
    if (!canSubmit) return;
    onsubmit(form);
  }
</script>

<form class="item-form" onsubmit={submit}>
  <label class="field">
    <span>Title</span>
    <input bind:this={firstField} bind:value={form.title} placeholder="Short summary…" required />
  </label>

  <div class="row">
    {#if mode === 'edit'}
      <label class="field">
        <span>Status</span>
        <select bind:value={form.status}>
          {#each STATUSES as s (s)}<option value={s}>{s}</option>{/each}
        </select>
      </label>
    {/if}
    <label class="field">
      <span>Type</span>
      <select bind:value={form.type}>
        {#each TYPES as t (t)}<option value={t}>{t}</option>{/each}
      </select>
    </label>
    <label class="field">
      <span>Priority</span>
      <select bind:value={form.priority}>
        {#each PRIOS as p (p)}<option value={p}>p{p}</option>{/each}
      </select>
    </label>
  </div>

  <label class="field">
    <span>Assignee</span>
    <input bind:value={form.assignee} placeholder="(unassigned)" />
  </label>

  <div class="field">
    <span>Labels</span>
    {#if form.labels.length}
      <div class="chips">
        {#each form.labels as l (l)}
          <span class="chip">{l}<button type="button" aria-label="remove label {l}" onclick={() => removeLabel(l)}>×</button></span>
        {/each}
      </div>
    {/if}
    <input
      bind:value={labelDraft}
      placeholder="add label, Enter to add…"
      onkeydown={onLabelKey}
      onblur={addLabels}
    />
  </div>

  <div class="field">
    <div class="body-head">
      <span>Body</span>
      <div class="seg" role="tablist">
        <button type="button" role="tab" class:active={bodyTab === 'write'} aria-selected={bodyTab === 'write'} onclick={() => (bodyTab = 'write')}>Write</button>
        <button type="button" role="tab" class:active={bodyTab === 'preview'} aria-selected={bodyTab === 'preview'} onclick={() => (bodyTab = 'preview')}>Preview</button>
      </div>
    </div>
    {#if bodyTab === 'write'}
      <textarea bind:value={form.body} rows="10" placeholder="Markdown…"></textarea>
    {:else}
      <div class="preview">
        {#if form.body.trim()}
          <Markdown source={form.body} />
        {:else}
          <p class="dim">Nothing to preview.</p>
        {/if}
      </div>
    {/if}
  </div>

  <div class="actions">
    {#if oncancel}
      <button type="button" class="btn" onclick={oncancel}>Cancel</button>
    {/if}
    <button type="submit" class="btn primary" disabled={!canSubmit}>
      {submitting ? 'Saving…' : (submitLabel ?? (mode === 'create' ? 'Create' : 'Save changes'))}
    </button>
  </div>
</form>

<style>
  .item-form {
    display: flex;
    flex-direction: column;
    gap: 14px;
  }
  .field {
    display: flex;
    flex-direction: column;
    gap: 4px;
    flex: 1;
  }
  .field > span,
  .body-head > span {
    font-size: 11px;
    text-transform: uppercase;
    letter-spacing: 0.5px;
    color: var(--text-dim);
  }
  .row {
    display: flex;
    gap: 12px;
  }
  input,
  select,
  textarea {
    background: var(--surface-inset);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    padding: 8px 10px;
    color: var(--text);
    font-size: 13px;
  }
  textarea {
    resize: vertical;
    font-family: var(--font-mono);
    line-height: 1.5;
  }
  .chips {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    margin-bottom: 6px;
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
    font-size: 13px;
    line-height: 1;
  }
  .body-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
  }
  .seg {
    display: flex;
    gap: 2px;
  }
  .seg button {
    background: none;
    border: 1px solid transparent;
    color: var(--text-muted);
    font-size: 11px;
    padding: 3px 10px;
    border-radius: var(--radius-sm);
    cursor: pointer;
  }
  .seg button.active {
    color: var(--text);
    background: var(--surface-inset);
    border-color: var(--border);
  }
  .preview {
    min-height: 180px;
    background: var(--surface-inset);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    padding: 8px 12px;
  }
  .actions {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
    margin-top: 4px;
  }
</style>
