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

  // For dynamic (measured) heights: pass each row element to the observer.
  measure = (node: I | null) => this.#virtualizer.measureElement(node);
}
