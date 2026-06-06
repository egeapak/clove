<script lang="ts">
  import '$lib/styles/app.css';
  import { onMount } from 'svelte';
  import { page } from '$app/stores';
  import { goto } from '$app/navigation';
  import { theme } from '$lib/theme.svelte';
  import { store, startLive } from '$lib/store.svelte';
  import ThemeSwitcher from '$lib/components/ThemeSwitcher.svelte';
  import Toasts from '$lib/components/Toasts.svelte';
  import NewItemModal from '$lib/components/NewItemModal.svelte';

  let { children } = $props();

  let search = $state('');
  let showNew = $state(false);
  let searchEl: HTMLInputElement | undefined = $state();

  onMount(() => {
    theme.init();
    void startLive();
  });

  const tabs = [
    { href: 'board', label: 'Board' },
    { href: 'list', label: 'List' },
    { href: 'timeline', label: 'Timeline' }
  ];

  const path = $derived($page.url.pathname);
  function isActive(href: string): boolean {
    return path.includes('/' + href) || (href === 'board' && (path === '/' || path.endsWith('/')));
  }

  const connLabel = $derived(
    store.conn === 'live'
      ? 'live'
      : store.conn === 'mock'
        ? 'demo'
        : store.conn === 'connecting'
          ? 'connecting'
          : 'offline'
  );

  function submitSearch(e: Event) {
    e.preventDefault();
    const q = search.trim();
    goto(`./list${q ? '?q=' + encodeURIComponent(q) : ''}`);
  }

  function onKey(e: KeyboardEvent) {
    const tag = (e.target as HTMLElement)?.tagName;
    if (tag === 'INPUT' || tag === 'TEXTAREA' || tag === 'SELECT') {
      if (e.key === 'Escape') (e.target as HTMLElement).blur();
      return;
    }
    if (e.key === '/') {
      e.preventDefault();
      searchEl?.focus();
    } else if (e.key === 'c') {
      e.preventDefault();
      showNew = true;
    } else if (e.key === 'g') {
      // simple chord: g then b/l/t
      const once = (ev: KeyboardEvent) => {
        if (ev.key === 'b') goto('./board');
        else if (ev.key === 'l') goto('./list');
        else if (ev.key === 't') goto('./timeline');
        window.removeEventListener('keydown', once, true);
      };
      window.addEventListener('keydown', once, true);
    }
  }
</script>

<svelte:window on:keydown={onKey} />

<div class="topbar">
  <a class="logo" href="./board">
    <svg class="leaf" width="18" height="18" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true"
      ><path
        d="M12 2C7 5 4 9 4 14a8 8 0 0 0 16 0c0-5-3-9-8-12zm0 4.5c2.8 2 4.5 4.6 4.5 7.5a4.5 4.5 0 0 1-9 0c0-2.9 1.7-5.5 4.5-7.5z"
      /></svg
    >
    clove
  </a>
  <nav class="tabs" aria-label="Views">
    {#each tabs as t (t.href)}
      <a class="tab" class:active={isActive(t.href)} href="./{t.href}">{t.label}</a>
    {/each}
  </nav>
  <form class="search" onsubmit={submitSearch}>
    <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" aria-hidden="true"
      ><circle cx="11" cy="11" r="7" /><path d="m21 21-4-4" /></svg
    >
    <input bind:this={searchEl} bind:value={search} placeholder="Search items…" aria-label="Search" />
    <span class="kbd">/</span>
  </form>
  <div class="spacer"></div>
  <div class="live" title="connection: {connLabel}">
    <span class="dot {store.conn}"></span>
    {connLabel}
  </div>
  <ThemeSwitcher />
  <button class="btn primary" onclick={() => (showNew = true)}>
    <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.4" aria-hidden="true"
      ><path d="M12 5v14M5 12h14" /></svg
    >
    New item
  </button>
</div>

<main class="page">
  {@render children()}
</main>

<Toasts />
{#if showNew}
  <NewItemModal onclose={() => (showNew = false)} />
{/if}

<style>
  .topbar {
    display: flex;
    align-items: center;
    gap: var(--s5);
    padding: 10px 20px;
    background: var(--surface);
    border-bottom: 1px solid var(--border);
    backdrop-filter: blur(var(--glass-blur));
    position: sticky;
    top: 0;
    z-index: 30;
  }
  .logo {
    display: flex;
    align-items: center;
    gap: 8px;
    font-weight: 700;
    font-size: 15px;
    color: var(--text);
    text-decoration: none;
  }
  .logo .leaf {
    color: var(--green);
  }
  .tabs {
    display: flex;
    gap: 2px;
    background: var(--surface-inset);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    padding: 3px;
  }
  .tab {
    font-size: 12px;
    color: var(--text-muted);
    padding: 5px 12px;
    border-radius: var(--radius-sm);
    text-decoration: none;
  }
  .tab:hover {
    color: var(--text);
    text-decoration: none;
  }
  .tab.active {
    background: var(--surface-hover);
    color: var(--text);
    box-shadow: inset 0 0 0 1px var(--border-strong);
  }
  .search {
    flex: 1;
    max-width: 360px;
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
    color: var(--text);
    font-family: var(--font-mono);
    font-size: 12px;
    min-width: 0;
  }
  .live {
    display: flex;
    align-items: center;
    gap: 6px;
    font-size: 12px;
    color: var(--text-muted);
  }
  .dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--text-dim);
  }
  .dot.live {
    background: var(--green);
    box-shadow: 0 0 0 3px color-mix(in srgb, var(--green) 22%, transparent);
  }
  .dot.mock {
    background: var(--prio-2);
  }
  .dot.connecting {
    background: var(--prio-1);
  }
  .dot.offline {
    background: var(--red);
  }
  .page {
    max-width: 1440px;
    margin: 0 auto;
    padding: 16px 20px 64px;
  }
  @media (max-width: 720px) {
    .topbar {
      flex-wrap: wrap;
      gap: 10px;
    }
    .search {
      order: 5;
      max-width: none;
      flex-basis: 100%;
    }
  }
</style>
