<script lang="ts">
  import { api } from '$lib/api';
  import { store } from '$lib/store.svelte';
  import { toasts } from '$lib/toast.svelte';
  import { goto } from '$app/navigation';
  import { resolveRoute } from '$app/paths';

  let { onclose }: { onclose: () => void } = $props();

  let title = $state('');
  let type = $state('feature');
  let priority = $state(2);
  let labels = $state('');
  let assignee = $state('');
  let body = $state('');
  let saving = $state(false);

  async function submit(e: Event) {
    e.preventDefault();
    if (!title.trim()) return;
    saving = true;
    try {
      const item = await api.create({
        title: title.trim(),
        type,
        priority,
        labels: labels
          .split(',')
          .map((l) => l.trim())
          .filter(Boolean),
        assignee: assignee.trim() || undefined,
        body: body.trim() || undefined
      });
      store.upsert(item);
      toasts.push(`Created ${item.id}`);
      onclose();
      goto(`./items/${item.id}`);
    } catch (err) {
      toasts.error('Create failed: ' + (err instanceof Error ? err.message : 'unknown'));
    } finally {
      saving = false;
    }
  }
</script>

<button class="scrim" aria-label="close" onclick={onclose}></button>
<div class="modal panel" role="dialog" aria-modal="true" aria-label="Create item">
  <h2>New item</h2>
  <form onsubmit={submit}>
    <label>
      <span>Title</span>
      <!-- svelte-ignore a11y_autofocus -->
      <input bind:value={title} autofocus placeholder="Short summary…" required />
    </label>
    <div class="row">
      <label>
        <span>Type</span>
        <select bind:value={type}>
          <option value="feature">feature</option>
          <option value="bug">bug</option>
          <option value="chore">chore</option>
          <option value="docs">docs</option>
          <option value="epic">epic</option>
        </select>
      </label>
      <label>
        <span>Priority</span>
        <select bind:value={priority}>
          <option value={0}>p0</option>
          <option value={1}>p1</option>
          <option value={2}>p2</option>
          <option value={3}>p3</option>
          <option value={4}>p4</option>
        </select>
      </label>
    </div>
    <div class="row">
      <label>
        <span>Assignee</span>
        <input bind:value={assignee} placeholder="(optional)" />
      </label>
      <label>
        <span>Labels (comma sep)</span>
        <input bind:value={labels} placeholder="area:web, …" />
      </label>
    </div>
    <label>
      <span>Body</span>
      <textarea bind:value={body} rows="4" placeholder="Markdown…"></textarea>
    </label>
    <div class="actions">
      <button type="button" class="btn" onclick={onclose}>Cancel</button>
      <button type="submit" class="btn primary" disabled={saving || !title.trim()}>
        {saving ? 'Creating…' : 'Create'}
      </button>
    </div>
  </form>
</div>

<style>
  .scrim {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.5);
    border: none;
    z-index: 60;
  }
  .modal {
    position: fixed;
    z-index: 61;
    top: 8%;
    left: 50%;
    transform: translateX(-50%);
    width: min(520px, 92vw);
    padding: 20px 22px;
    box-shadow: var(--shadow-pop);
  }
  h2 {
    margin: 0 0 14px;
    font-size: 16px;
  }
  form {
    display: flex;
    flex-direction: column;
    gap: 12px;
  }
  label {
    display: flex;
    flex-direction: column;
    gap: 4px;
    flex: 1;
  }
  label span {
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
  }
  .actions {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
    margin-top: 4px;
  }
</style>
