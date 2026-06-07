export interface Toast {
  id: number;
  msg: string;
  kind: 'info' | 'error';
}

const MAX_VISIBLE = 4;
const TTL = 4000;

class ToastStore {
  items = $state<Toast[]>([]);
  private nextId = 1;
  // Per-toast auto-dismiss timers, cleared on manual dismiss so they can't fire
  // against a recycled id.
  private timers = new Map<number, ReturnType<typeof setTimeout>>();

  push(msg: string, kind: 'info' | 'error' = 'info') {
    const id = this.nextId++;
    let next = [...this.items, { id, msg, kind }];
    // Cap visible toasts: drop the oldest (and clear its timer) beyond the cap.
    while (next.length > MAX_VISIBLE) {
      const dropped = next[0];
      next = next.slice(1);
      this.clearTimer(dropped.id);
    }
    this.items = next;
    this.timers.set(
      id,
      setTimeout(() => this.dismiss(id), TTL)
    );
  }
  error(msg: string) {
    this.push(msg, 'error');
  }
  dismiss(id: number) {
    this.clearTimer(id);
    this.items = this.items.filter((t) => t.id !== id);
  }
  private clearTimer(id: number) {
    const t = this.timers.get(id);
    if (t) {
      clearTimeout(t);
      this.timers.delete(id);
    }
  }
}

export const toasts = new ToastStore();
