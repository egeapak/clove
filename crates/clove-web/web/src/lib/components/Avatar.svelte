<script lang="ts">
  import { initials } from '$lib/glyphs';
  let { name }: { name: string | null } = $props();

  // Deterministic hue from name so avatars are stable & distinct.
  function hue(n: string): number {
    let h = 0;
    for (let i = 0; i < n.length; i++) h = (h * 31 + n.charCodeAt(i)) % 360;
    return h;
  }
  const bg = $derived(name ? `hsl(${hue(name)} 55% 60%)` : 'var(--surface-3)');
</script>

<span
  class="av mono"
  class:none={!name}
  style="background:{bg}"
  role="img"
  aria-label={name ? `assignee ${name}` : 'unassigned'}
  title={name ?? 'unassigned'}
>
  {initials(name)}
</span>

<style>
  .av {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 22px;
    height: 22px;
    border-radius: 50%;
    font-size: 10px;
    font-weight: 700;
    color: #0b0b0b;
    flex: 0 0 auto;
  }
  .av.none {
    color: var(--text-dim);
    font-weight: 400;
    border: 1px dashed var(--border-strong);
  }
</style>
