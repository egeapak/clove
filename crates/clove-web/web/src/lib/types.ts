export type Status = 'open' | 'in_progress' | 'closed';
export type ItemType = 'bug' | 'feature' | 'chore' | 'docs' | 'epic';

export interface Item {
  id: string;
  title: string;
  status: Status;
  type: ItemType;
  priority: number; // 0..4
  assignee: string | null;
  parent: string | null;
  labels: string[];
  deps: string[];
  relates: string[];
  created: string;
  updated: string;
  closed: string | null;
  body: string;
  comment_count: number;
  ready: boolean;
  blocked_by: string[];
  dangling_deps: string[];
}

export interface Comment {
  timestamp: string;
  author: string;
  body: string;
}

export interface DepTreeNode {
  id: string;
  title: string;
  status: Status;
  ready: boolean;
  cycle_ref: boolean;
  /** Subtree already expanded elsewhere in the tree (shown as a reference). */
  repeat_ref?: boolean;
  children: DepTreeNode[];
}

export interface BoardColumn {
  key: string;
  label: string;
  count: number;
  items: Item[];
}

export interface Board {
  columns: BoardColumn[];
}

export interface Meta {
  id_prefix: string;
  types: string[];
  statuses: string[];
  priorities: number[];
  labels: string[];
  assignees: string[];
  daemon: { running: boolean; web_addr: string | null };
  source: string;
}

export interface StatsHistoryPoint {
  date: string;
  created: number;
  closed: number;
  open: number;
}

export interface Envelope<T> {
  v: number;
  ok: boolean;
  data?: T;
  error?: { code: string; message: string; exit: number };
  _meta?: Record<string, unknown>;
}

export type ConnState = 'connecting' | 'live' | 'offline' | 'mock';

export interface ListQuery {
  status?: Status;
  type?: ItemType[];
  priority?: number[];
  assignee?: string;
  label?: string[];
  q?: string;
  sort?: string;
  dir?: 'asc' | 'desc';
  mode?: 'list' | 'ready' | 'blocked';
}
