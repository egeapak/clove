<script lang="ts">
  import { theme, THEMES } from '$lib/theme.svelte';
  let open = $state(false);
  const cur = $derived(THEMES.find((t) => t.key === theme.current) ?? THEMES[0]);

  function pick(key: string) {
    theme.set(key);
    open = false;
  }
  function onKeydown(e: KeyboardEvent) {
    if (e.key === 'Escape' && open) {
      e.preventDefault();
      open = false;
    }
  }
</script>

<svelte:window on:keydown={onKeydown} />

<div class="wrap">
  <button class="trig btn sm" aria-haspopup="menu" aria-expanded={open} onclick={() => (open = !open)}>
    <span class="dot3" style="background:{cur.swatch[0]}"></span>
    Theme
  </button>
  {#if open}
    <button class="scrim" aria-label="close theme menu" onclick={() => (open = false)}></button>
    <div class="menu panel" role="menu">
      {#each THEMES as t (t.key)}
        <button class="opt" role="menuitemradio" aria-checked={t.key === theme.current} onclick={() => pick(t.key)}>
          <span class="sw">
            <i style="background:{t.swatch[1]}"></i>
            <i style="background:{t.swatch[0]}"></i>
            <i style="background:{t.swatch[2]}"></i>
          </span>
          <span class="nm">{t.name}</span>
          {#if t.key === theme.current}<span class="chk">✓</span>{/if}
        </button>
      {/each}
    </div>
  {/if}
</div>

<style>
  .wrap {
    position: relative;
  }
  .dot3 {
    width: 8px;
    height: 8px;
    border-radius: 50%;
  }
  .scrim {
    position: fixed;
    inset: 0;
    background: transparent;
    border: none;
    z-index: 40;
  }
  .menu {
    position: absolute;
    right: 0;
    top: calc(100% + 6px);
    z-index: 50;
    min-width: 190px;
    padding: 4px;
    box-shadow: var(--shadow-pop);
  }
  .opt {
    display: flex;
    align-items: center;
    gap: 8px;
    width: 100%;
    border: none;
    background: none;
    color: var(--text);
    padding: 7px 8px;
    border-radius: var(--radius-sm);
    font-size: 12px;
    text-align: left;
  }
  .opt:hover {
    background: var(--surface-hover);
  }
  .sw {
    display: inline-flex;
    border-radius: 4px;
    overflow: hidden;
    border: 1px solid var(--border);
  }
  .sw i {
    width: 12px;
    height: 16px;
    display: block;
  }
  .nm {
    flex: 1;
  }
  .chk {
    color: var(--accent);
  }
</style>
