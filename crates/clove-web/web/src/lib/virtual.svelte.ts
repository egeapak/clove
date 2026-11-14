// Svelte-5 runes wrapper around @tanstack/virtual-core.
//
// virtual-core is framework-agnostic: it computes which rows are visible given
// a scroll element + offset, but knows nothing about reactivity. We bridge it
// to runes by mirroring its output (virtual items + total size) into $state and
// re-pulling on every `onChange`. Mount the returned virtualizer's lifecycle
// with `attach()` inside an $effect.
import {
  Virtualizer,
  elementScroll,
  observeElementOffset,
  observeElementRect,
  type VirtualItem,
  type VirtualizerOptions
} from '@tanstack/virtual-core';

export type { VirtualItem };

type Opts<S extends Element, I extends Element> = Pick<
  VirtualizerOptions<S, I>,
  | 'count'
  | 'getScrollElement'
  | 'estimateSize'
  | 'overscan'
  | 'getItemKey'
  | 'measureElement'
  | 'paddingStart'
  | 'paddingEnd'
  | 'gap'
  | 'scrollPaddingStart'
  | 'scrollPaddingEnd'
>;

export class Virtual<S extends Element = HTMLElement, I extends Element = HTMLElement> {
  #virtualizer: Virtualizer<S, I>;
  items = $state<VirtualItem[]>([]);
  total = $state(0);

  constructor(opts: Opts<S, I>) {
    this.#virtualizer = new Virtualizer<S, I>({
      observeElementRect,
      observeElementOffset,
      scrollToFn: elementScroll,
      ...opts,
      onChange: (v) => {
        // Pull computed state out of the (mutable, non-reactive) instance.
        this.items = v.getVirtualItems();
        this.total = v.getTotalSize();
      }
    });
  }

  // Push fresh options (count/estimate/etc.) when reactive inputs change, then
  // recompute. Call from an $effect that reads those inputs.
  update(opts: Partial<Opts<S, I>>) {
    this.#virtualizer.setOptions({ ...this.#virtualizer.options, ...opts });
    this.items = this.#virtualizer.getVirtualItems();
    this.total = this.#virtualizer.getTotalSize();
  }

  // Mount: wires up resize/scroll observers. Returns the teardown for $effect.
  attach(): () => void {
    const cleanup = this.#virtualizer._didMount();
    this.#virtualizer._willUpdate();
    this.items = this.#virtualizer.getVirtualItems();
    this.total = this.#virtualizer.getTotalSize();
    return cleanup;
  }

  // Bring row `index` into view (no-op when it already is). Used by keyboard
  // cursors, whose target row may not be rendered at all.
  scrollToIndex(index: number) {
    this.#virtualizer.scrollToIndex(index, { align: 'auto' });
  }

  // For dynamic (measured) heights: pass each row element to the observer.
  measure = (node: I | null) => this.#virtualizer.measureElement(node);
}

/**
 * Pair each virtual item with the row it stands for, dropping any whose index
 * is past the end of `rows`.
 *
 * The virtualizer's count is pushed from an `$effect`, which runs *after*
 * render. When the rows shrink, the window therefore still names the old
 * indexes for one render pass; a row that dereferenced `rows[v.index]` there
 * read `undefined` and threw, which froze the whole view. Render from this
 * instead of indexing `rows` in the template.
 */
export function visibleRows<T>(items: readonly VirtualItem[], rows: readonly T[]): Array<{ v: VirtualItem; item: T }> {
  const out: Array<{ v: VirtualItem; item: T }> = [];
  for (const v of items) {
    if (v.index < rows.length) out.push({ v, item: rows[v.index] });
  }
  return out;
}
