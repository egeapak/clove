<script lang="ts">
  import { api } from '$lib/api';
  import { store } from '$lib/store.svelte';
  import { toasts } from '$lib/toast.svelte';
  import { goto } from '$app/navigation';
  import { base } from '$app/paths';
  import { onMount, tick } from 'svelte';
  import ItemForm from '$lib/components/ItemForm.svelte';
  import { buildCreate, type FormState } from '$lib/itemForm';

  let { onclose }: { onclose: () => void } = $props();

  let saving = $state(false);
  // Escape and a click on the backdrop are easy to hit by accident. With
  // something typed they ask first instead of throwing the draft away.
  let dirty = $state(false);
  let confirmingDiscard = $state(false);

  function requestClose() {
    if (dirty && !confirmingDiscard) {
      confirmingDiscard = true;
      return;
    }
    onclose();
  }

  let dialogEl = $state<HTMLDivElement | undefined>();
  let firstField = $state<HTMLInputElement | undefined>();
  const opener = typeof document !== 'undefined' ? (document.activeElement as HTMLElement | null) : null;

  onMount(() => {
    void tick().then(() => firstField?.focus());
    return () => {
      // Restore focus to whatever opened the modal.
      opener?.focus?.();
    };
  });

  function focusables(): HTMLElement[] {
    if (!dialogEl) return [];
    return [
      ...dialogEl.querySelectorAll<HTMLElement>(
        'input, select, textarea, button, [href], [tabindex]:not([tabindex="-1"])'
      )
    ].filter((el) => !el.hasAttribute('disabled') && el.offsetParent !== null);
  }

  function onKeydown(e: KeyboardEvent) {
    if (e.key === 'Escape') {
      e.preventDefault();
      // A second Escape answers the question with "keep editing".
      if (confirmingDiscard) confirmingDiscard = false;
      else requestClose();
      return;
    }
    if (e.key !== 'Tab') return;
    const els = focusables();
    if (els.length === 0) return;
    const first = els[0];
    const last = els[els.length - 1];
    const active = document.activeElement;
    if (e.shiftKey && active === first) {
      e.preventDefault();
      last.focus();
    } else if (!e.shiftKey && active === last) {
      e.preventDefault();
      first.focus();
    }
  }

  async function create(form: FormState) {
    saving = true;
    try {
      const item = await api.create(buildCreate(form));
      store.upsert(item);
      toasts.push(`Created ${item.id}`);
      onclose();
      // Base-relative: `./items/…` resolved against the CURRENT path, so
      // creating from a detail page navigated to /items/items/<id> (a 404).
      goto(`${base}/items/${item.id}`);
    } catch (err) {
      toasts.error('Create failed: ' + (err instanceof Error ? err.message : 'unknown'));
    } finally {
      saving = false;
    }
  }
</script>

<button class="scrim" aria-label="close" onclick={requestClose}></button>
<div
  class="modal panel"
  role="dialog"
  aria-modal="true"
  aria-label="Create item"
  tabindex="-1"
  bind:this={dialogEl}
  onkeydown={onKeydown}
>
  <h2>New item</h2>
  {#if confirmingDiscard}
    <div class="discard" role="alert">
      <span>Discard this draft?</span>
      <button type="button" class="btn sm" onclick={() => (confirmingDiscard = false)}>Keep editing</button>
      <button type="button" class="btn sm" onclick={onclose}>Discard</button>
    </div>
  {/if}
  <ItemForm mode="create" submitting={saving} onsubmit={create} oncancel={onclose} bind:firstField bind:dirty />
</div>

<style>
  .discard {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 8px 10px;
    margin-bottom: 12px;
    border: 1px solid var(--border);
    border-radius: 6px;
    background: var(--surface-hover);
    font-size: 13px;
  }
  .discard span {
    margin-right: auto;
  }
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
    width: min(560px, 92vw);
    padding: 20px 22px;
    box-shadow: var(--shadow-pop);
    max-height: 84vh;
    overflow-y: auto;
  }
  h2 {
    margin: 0 0 14px;
    font-size: 16px;
  }
</style>
