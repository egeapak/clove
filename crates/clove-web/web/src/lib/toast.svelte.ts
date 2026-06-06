export interface Toast {
  id: number;
  msg: string;
  kind: 'info' | 'error';
}

class ToastStore {
  items = $state<Toast[]>([]);
  private nextId = 1;

  push(msg: string, kind: 'info' | 'error' = 'info') {
    const id = this.nextId++;
    this.items = [...this.items, { id, msg, kind }];
    setTimeout(() => this.dismiss(id), 4000);
  }
  error(msg: string) {
    this.push(msg, 'error');
  }
  dismiss(id: number) {
    this.items = this.items.filter((t) => t.id !== id);
  }
}

export const toasts = new ToastStore();
