<script lang="ts">
  let { label, removable = false, onremove }: { label: string; removable?: boolean; onremove?: () => void } =
    $props();
  const isReg = $derived(/regression|bug|hot|urgent/i.test(label));
</script>

<span class="lbl mono" class:reg={isReg}>
  {label}
  {#if removable}
    <button class="x" aria-label="remove label {label}" onclick={onremove}>×</button>
  {/if}
</span>

<style>
  .lbl {
    display: inline-flex;
    align-items: center;
    gap: 3px;
    font-size: 10px;
    color: var(--text-muted);
    background: var(--surface-inset);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    padding: 2px 6px;
  }
  .lbl.reg {
    color: var(--red);
    border-color: color-mix(in srgb, var(--red) 45%, transparent);
    background: color-mix(in srgb, var(--red) 12%, transparent);
  }
  .x {
    border: none;
    background: none;
    color: var(--text-dim);
    padding: 0;
    line-height: 1;
    font-size: 12px;
  }
  .x:hover {
    color: var(--red);
  }
</style>
