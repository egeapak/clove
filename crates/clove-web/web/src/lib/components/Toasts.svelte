<script lang="ts">
  import { toasts } from '$lib/toast.svelte';
</script>

<div class="toasts" aria-live="polite">
  {#each toasts.items as t (t.id)}
    <div class="toast" class:err={t.kind === 'error'}>
      {t.msg}
      <button class="x" aria-label="dismiss" onclick={() => toasts.dismiss(t.id)}>×</button>
    </div>
  {/each}
</div>

<style>
  .toasts {
    position: fixed;
    bottom: 16px;
    right: 16px;
    display: flex;
    flex-direction: column;
    gap: 8px;
    z-index: 100;
  }
  .toast {
    display: flex;
    align-items: center;
    gap: 10px;
    background: var(--surface-2);
    border: 1px solid var(--border-strong);
    border-radius: var(--radius-md);
    padding: 10px 12px;
    font-size: 12px;
    color: var(--text);
    box-shadow: var(--shadow-pop);
    max-width: 340px;
  }
  .toast.err {
    border-color: color-mix(in srgb, var(--red) 60%, transparent);
    color: var(--red);
  }
  .x {
    border: none;
    background: none;
    color: inherit;
    font-size: 14px;
    line-height: 1;
  }
</style>
