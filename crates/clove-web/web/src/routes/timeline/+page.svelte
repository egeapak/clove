<script lang="ts">
  import type { Item, StatsHistoryPoint } from '$lib/types';
  import { store } from '$lib/store.svelte';
  import { api } from '$lib/api';
  import { goto } from '$app/navigation';
  import TypeIcon from '$lib/components/TypeIcon.svelte';
  import ShortId from '$lib/components/ShortId.svelte';
  import { shortId } from '$lib/glyphs';

  let history = $state<StatsHistoryPoint[]>([]);
  let range = $state<'30' | '90' | 'all'>('90');

  $effect(() => {
    api.history().then((h) => (history = h)).catch(() => (history = []));
  });

  // ---- Gantt domain: created → (closed | now) ----
  const items = $derived(store.all);
  const domain = $derived.by(() => {
    const times = items.flatMap((i) => [new Date(i.created).getTime(), new Date(i.closed ?? i.updated).getTime()]);
    const now = Date.now();
    let min = Math.min(...times, now);
    let max = Math.max(...times, now);
    if (!isFinite(min) || !isFinite(max) || min === max) {
      min = now - 30 * 86400000;
      max = now;
    }
    // pad
    const pad = (max - min) * 0.04;
    return { min: min - pad, max: max + pad, now };
  });

  function pct(t: number): number {
    const { min, max } = domain;
    return ((t - min) / (max - min)) * 100;
  }

  // sort: epics first then by created
  const rows = $derived(
    [...items].sort((a, b) => {
      if ((a.type === 'epic') !== (b.type === 'epic')) return a.type === 'epic' ? -1 : 1;
      return new Date(a.created).getTime() - new Date(b.created).getTime();
    })
  );

  function barStyle(i: Item): string {
    const start = pct(new Date(i.created).getTime());
    const end = pct(new Date(i.closed ?? domain.now).getTime());
    const w = Math.max(end - start, 1.5);
    return `left:${start}%;width:${w}%`;
  }
  function barClass(i: Item): string {
    return i.status === 'open' ? 'open' : i.type;
  }

  // axis ticks (~8)
  const ticks = $derived.by(() => {
    const out: { left: number; label: string }[] = [];
    const { min, max } = domain;
    const n = 8;
    for (let k = 0; k <= n; k++) {
      const t = min + ((max - min) * k) / n;
      out.push({
        left: (k / n) * 100,
        label: new Intl.DateTimeFormat(undefined, { month: 'short', day: 'numeric' }).format(new Date(t))
      });
    }
    return out;
  });

  // map id -> row index (for dep arrows)
  const rowIndex = $derived(new Map(rows.map((r, i) => [r.id, i])));
  const ROW_H = 38;

  interface Arrow { x1: number; y1: number; x2: number; y2: number; }
  const arrows = $derived.by(() => {
    const out: Arrow[] = [];
    rows.forEach((it, ri) => {
      for (const dep of it.deps) {
        const di = rowIndex.get(dep);
        if (di === undefined) continue;
        const depItem = items.find((x) => x.id === dep)!;
        const x1 = pct(new Date(depItem.closed ?? domain.now).getTime());
        const x2 = pct(new Date(it.created).getTime());
        out.push({ x1, y1: di * ROW_H + ROW_H / 2, x2, y2: ri * ROW_H + ROW_H / 2 });
      }
    });
    return out;
  });

  const svgH = $derived(rows.length * ROW_H);

  // ---- throughput ----
  const tput = $derived.by(() => {
    const days = range === 'all' ? history.length : Number(range);
    const pts = history.slice(-days);
    const maxV = Math.max(1, ...pts.map((p) => Math.max(p.created, p.closed)));
    return { pts, maxV };
  });
  const W = 800;
  const H = 90;
  function bx(i: number, n: number): number {
    return n <= 1 ? 0 : (i / (n - 1)) * (W - 40) + 20;
  }
  const closedLine = $derived(
    tput.pts.map((p, i) => `${bx(i, tput.pts.length)},${80 - (p.closed / tput.maxV) * 60}`).join(' ')
  );
</script>

<div class="filters">
  <h2>Lifecycle timeline</h2>
  <span class="dim mono">{items.length} items · created → closed/now</span>
</div>

<div class="tl-grid panel">
  <div class="tl-corner"></div>
  <div class="tl-axis">
    {#each ticks as t (t.left)}
      <div class="tick mono" style="left:{t.left}%">{t.label}</div>
    {/each}
  </div>

  <div class="tl-labels">
    {#each rows as it (it.id)}
      <div class="rowlabel" class:epic={it.type === 'epic'}>
        <TypeIcon type={it.type} /> <ShortId id={it.id} />
        <span class="rl-title">{it.title}</span>
      </div>
    {/each}
  </div>

  <div class="tl-tracks" style="height:{svgH}px">
    <!-- grid -->
    {#each ticks as t (t.left)}
      <div class="grid-line" style="left:{t.left}%"></div>
    {/each}
    <!-- today -->
    <div class="today" style="left:{pct(domain.now)}%"><span>today</span></div>
    <!-- dep arrows -->
    <svg class="arrows" viewBox="0 0 100 {svgH}" preserveAspectRatio="none" aria-hidden="true">
      <defs>
        <marker id="ah" markerWidth="5" markerHeight="5" refX="4" refY="2.5" orient="auto">
          <path d="M0 0L5 2.5L0 5z" fill="var(--red)" />
        </marker>
      </defs>
      {#each arrows as a}
        <path
          d="M{a.x1} {a.y1} C {(a.x1 + a.x2) / 2} {a.y1}, {(a.x1 + a.x2) / 2} {a.y2}, {a.x2} {a.y2}"
          stroke="var(--red)"
          stroke-width="0.3"
          vector-effect="non-scaling-stroke"
          fill="none"
          stroke-dasharray="3 2"
          marker-end="url(#ah)"
        />
      {/each}
    </svg>
    {#each rows as it, ri (it.id)}
      <div class="track" style="top:{ri * ROW_H}px">
        <button
          class="bar {barClass(it)}"
          style={barStyle(it)}
          onclick={() => goto(`../items/${it.id}`)}
          title="{it.title}"
          aria-label="{shortId(it.id)} {it.title}"
        >
          {shortId(it.id)}{#if it.status === 'open'} ·blocked{/if}
        </button>
      </div>
    {/each}
  </div>
</div>

<!-- throughput -->
<div class="tput panel">
  <div class="tput-head">
    <h4>Throughput — created vs closed</h4>
    <div class="legend">
      <span><i style="background:var(--green)"></i>created</span>
      <span><i style="background:var(--accent)"></i>closed</span>
      <div class="range">
        {#each [['30', '30d'], ['90', '90d'], ['all', 'All']] as [k, l] (k)}
          <button class="btn sm" class:primary={range === k} onclick={() => (range = k as typeof range)}>{l}</button>
        {/each}
      </div>
    </div>
  </div>
  <svg width="100%" height={H} viewBox="0 0 {W} {H}" preserveAspectRatio="none" role="img" aria-label="throughput chart">
    <line x1="0" y1="80" x2={W} y2="80" stroke="var(--border)" />
    <g fill="var(--green)" opacity="0.85">
      {#each tput.pts as p, i (p.date)}
        {@const w = Math.max(2, (W - 40) / Math.max(tput.pts.length, 1) - 4)}
        <rect x={bx(i, tput.pts.length) - w / 2} y={80 - (p.created / tput.maxV) * 60} width={w} height={(p.created / tput.maxV) * 60} />
      {/each}
    </g>
    <polyline fill="none" stroke="var(--accent)" stroke-width="2" points={closedLine} vector-effect="non-scaling-stroke" />
    <g fill="var(--accent)">
      {#each tput.pts as p, i (p.date)}
        <circle cx={bx(i, tput.pts.length)} cy={80 - (p.closed / tput.maxV) * 60} r="3" />
      {/each}
    </g>
  </svg>
  {#if tput.pts.length === 0}<p class="dim">No history data.</p>{/if}
</div>

<style>
  .filters {
    display: flex;
    align-items: baseline;
    gap: 14px;
    padding: 4px 2px 14px;
  }
  .filters h2 {
    font-size: 16px;
    margin: 0;
  }
  .tl-grid {
    display: grid;
    grid-template-columns: 220px 1fr;
    overflow: hidden;
  }
  .tl-corner {
    border-bottom: 1px solid var(--border);
    border-right: 1px solid var(--border);
  }
  .tl-axis {
    position: relative;
    height: 34px;
    border-bottom: 1px solid var(--border);
  }
  .tick {
    position: absolute;
    top: 8px;
    font-size: 10px;
    color: var(--text-dim);
    transform: translateX(-50%);
    white-space: nowrap;
  }
  .tl-labels {
    border-right: 1px solid var(--border);
  }
  .rowlabel {
    height: 38px;
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 0 12px;
    border-bottom: 1px solid var(--border);
    font-size: 12px;
    color: var(--text-muted);
  }
  .rowlabel.epic {
    background: color-mix(in srgb, var(--type-epic) 8%, transparent);
  }
  .rl-title {
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .tl-tracks {
    position: relative;
  }
  .grid-line {
    position: absolute;
    top: 0;
    bottom: 0;
    width: 0;
    border-left: 1px solid var(--border);
    opacity: 0.5;
  }
  .arrows {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
    z-index: 3;
    overflow: visible;
    pointer-events: none;
  }
  .track {
    position: absolute;
    left: 0;
    right: 0;
    height: 38px;
    border-bottom: 1px solid var(--border);
  }
  .bar {
    position: absolute;
    top: 9px;
    height: 20px;
    border-radius: var(--radius-sm);
    display: flex;
    align-items: center;
    padding: 0 8px;
    font-family: var(--font-mono);
    font-size: 11px;
    color: #06131f;
    font-weight: 600;
    white-space: nowrap;
    overflow: hidden;
    border: none;
    z-index: 2;
  }
  .bar.epic {
    background: var(--type-epic);
  }
  .bar.feature {
    background: var(--type-feature);
  }
  .bar.bug {
    background: var(--type-bug);
    color: #fff;
  }
  .bar.chore {
    background: var(--type-chore);
  }
  .bar.docs {
    background: var(--type-docs);
  }
  .bar.open {
    background: transparent;
    border: 1px dashed var(--text-muted);
    color: var(--text-muted);
    opacity: 0.8;
  }
  .today {
    position: absolute;
    top: 0;
    bottom: 0;
    width: 0;
    border-left: 2px dashed var(--accent);
    z-index: 4;
  }
  .today span {
    position: absolute;
    top: 2px;
    left: 4px;
    font-family: var(--font-mono);
    font-size: 10px;
    color: var(--accent);
    background: var(--surface-app);
    padding: 0 3px;
  }
  .tput {
    margin-top: 14px;
    padding: 14px 16px;
  }
  .tput-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    flex-wrap: wrap;
  }
  .tput h4 {
    margin: 0;
    font-size: 13px;
  }
  .legend {
    display: flex;
    gap: 16px;
    align-items: center;
    font-size: 11px;
    color: var(--text-muted);
  }
  .legend i {
    display: inline-block;
    width: 10px;
    height: 10px;
    border-radius: 2px;
    margin-right: 5px;
    vertical-align: -1px;
  }
  .range {
    display: flex;
    gap: 4px;
  }
  @media (max-width: 720px) {
    .tl-grid {
      grid-template-columns: 130px 1fr;
    }
  }
</style>
