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
  // Present only when the series comes from recorded snapshots (real
  // point-in-time levels); absent for the file-synthesized fallback.
  captured_at?: string;
  in_progress?: number;
  total?: number;
  ready?: number;
  blocked?: number;
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
  /**
   * Window, sent to the server. `0` means **unlimited** (the API contract on
   * every surface), and so does an absent value — the web API's default is
   * unlimited, which is why a view that wants everything (board, timeline) must
   * still say `limit: 0` rather than relying on it.
   */
  limit?: number;
  offset?: number;
}

/**
 * One windowed list response: the rows the server returned plus the `_meta`
 * counts that describe the window. `total` is the match count *before* the
 * window, so "showing 1–50 of 128" reads both numbers from one answer and
 * cannot show a total that disagrees with the rows beneath it.
 */
export interface ItemPage {
  items: Item[];
  total: number;
  returned: number;
  offset: number;
  limit: number;
}
