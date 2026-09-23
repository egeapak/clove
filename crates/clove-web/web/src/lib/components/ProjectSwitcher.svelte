<script lang="ts">
  import { onMount } from 'svelte';
  import type { Project } from '$lib/types';

  // `current` is the hub slug this app is served under, or null when served
  // standalone (then there is nothing to switch between and nothing is fetched).
  let { current, load }: { current: string | null; load: () => Promise<Project[]> } = $props();

  let projects = $state<Project[]>([]);

  onMount(() => {
    if (current === null) return;
    void load().then((list) => (projects = list));
  });

  function change(e: Event) {
    const slug = (e.currentTarget as HTMLSelectElement).value;
    const target = projects.find((p) => p.slug === slug);
    // A full navigation, not `goto`: each project is its own app base.
    if (target && slug !== current) location.assign(target.url);
  }
</script>

{#if projects.length > 1}
  <label class="switch">
    <span class="sr">Project</span>
    <select aria-label="Project" value={current} onchange={change}>
      {#each projects as p (p.slug)}
        <option value={p.slug} title={p.root}>{p.name}</option>
      {/each}
    </select>
  </label>
{/if}

<style>
  .switch {
    display: flex;
    align-items: center;
  }
  .sr {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip: rect(0 0 0 0);
  }
  select {
    background: var(--surface-2);
    color: var(--text);
    border: 1px solid var(--border-strong);
    border-radius: var(--radius-md);
    padding: 6px 8px;
    font-size: 12px;
    font-weight: 600;
    max-width: 180px;
  }
</style>
