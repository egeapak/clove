import type { Item, Comment, StatsHistoryPoint } from './types';

const now = Date.now();
const ago = (h: number) => new Date(now - h * 3600 * 1000).toISOString();
const daysAgo = (d: number) => new Date(now - d * 86400 * 1000).toISOString();

function mk(p: Partial<Item> & { id: string; title: string }): Item {
  return {
    status: 'open',
    type: 'feature',
    priority: 2,
    assignee: null,
    parent: null,
    labels: [],
    deps: [],
    relates: [],
    created: daysAgo(20),
    updated: ago(24),
    closed: null,
    body: '',
    comment_count: 0,
    ready: true,
    blocked_by: [],
    dangling_deps: [],
    ...p
  };
}

export const MOCK_ITEMS: Item[] = [
  mk({
    id: '4',
    title: 'Payments revamp',
    type: 'epic',
    status: 'in_progress',
    priority: 1,
    assignee: 'ada',
    labels: ['area:payments'],
    created: daysAgo(40),
    updated: ago(5),
    body: '# Payments revamp\n\nUmbrella epic tracking the payments rewrite.\n\n- [x] Scope the work\n- [ ] Land idempotency fix (#45)\n- [ ] Ship webhook handler (#42)'
  }),
  mk({
    id: '45',
    title: 'Fix idempotency key race',
    type: 'bug',
    status: 'in_progress',
    priority: 0,
    assignee: 'lin',
    parent: '4',
    labels: ['area:payments', 'regression'],
    created: daysAgo(12),
    updated: ago(2),
    comment_count: 1,
    body: 'A race in the idempotency key store lets two concurrent requests both pass the dedupe check.\n\n## Repro\n1. Fire two identical requests within ~5ms\n2. Both create a charge\n\n## Fix\nUse a transactional `INSERT ... ON CONFLICT` guard.'
  }),
  mk({
    id: '42',
    title: 'Add Stripe webhook handler',
    type: 'feature',
    status: 'open',
    priority: 1,
    parent: '4',
    labels: ['area:payments'],
    deps: ['45'],
    relates: ['53'],
    blocked_by: ['45'],
    ready: false,
    created: daysAgo(10),
    updated: ago(3),
    comment_count: 2,
    body: "Receive and verify Stripe webhook events so subscription state stays in sync. Must be idempotent and tolerate retries from Stripe's at-least-once delivery.\n\n## Acceptance criteria\n- Verify `Stripe-Signature` using the endpoint secret\n- Persist the raw event before processing (replay safety)\n- Return 2xx within 5s; defer heavy work to a queue\n\n## Checklist\n- [x] Define event schema\n- [x] Add signature verification\n- [ ] Wire up the queue worker\n- [ ] Backfill missed events (blocked by #45)"
  }),
  mk({
    id: '53',
    title: 'Backfill payment audit log',
    type: 'chore',
    status: 'open',
    priority: 2,
    parent: '4',
    assignee: 'lin',
    labels: ['area:payments'],
    deps: ['45'],
    blocked_by: ['45'],
    ready: false,
    created: daysAgo(8),
    updated: daysAgo(1)
  }),
  mk({
    id: '60',
    title: 'Kanban drag-and-drop',
    type: 'feature',
    status: 'open',
    priority: 2,
    labels: ['area:web'],
    deps: ['61'],
    blocked_by: ['61'],
    ready: false,
    created: daysAgo(7),
    updated: daysAgo(2)
  }),
  mk({
    id: '61',
    title: 'WebSocket live updates',
    type: 'feature',
    status: 'in_progress',
    priority: 2,
    assignee: 'ada',
    labels: ['area:web'],
    created: daysAgo(9),
    updated: daysAgo(1)
  }),
  mk({
    id: '70',
    title: 'Timeline view',
    type: 'feature',
    status: 'open',
    priority: 2,
    labels: ['area:web'],
    created: daysAgo(6),
    updated: daysAgo(2)
  }),
  mk({
    id: '51',
    title: 'Upgrade tokio to 1.40',
    type: 'chore',
    status: 'open',
    priority: 3,
    labels: ['area:build'],
    created: daysAgo(11),
    updated: daysAgo(3)
  }),
  mk({
    id: '88',
    title: 'FTS5 search misses unicode',
    type: 'bug',
    status: 'open',
    priority: 1,
    labels: ['area:index', 'regression'],
    created: daysAgo(5),
    updated: ago(8)
  }),
  mk({
    id: '91',
    title: 'Flaky windows signing test',
    type: 'chore',
    status: 'open',
    priority: 4,
    labels: ['area:ci'],
    created: daysAgo(14),
    updated: daysAgo(4)
  }),
  mk({
    id: '38',
    title: 'Document merge driver',
    type: 'docs',
    status: 'closed',
    priority: 2,
    assignee: 'sam',
    labels: ['area:docs'],
    created: daysAgo(18),
    updated: daysAgo(5),
    closed: daysAgo(5),
    body: 'Document the custom git merge driver for `.clove` files.'
  }),
  mk({
    id: '29',
    title: 'Daemon leaves stale socket',
    type: 'bug',
    status: 'closed',
    priority: 2,
    assignee: 'lin',
    labels: ['area:daemon'],
    created: daysAgo(22),
    updated: daysAgo(6),
    closed: daysAgo(6)
  }),
  mk({
    id: '14',
    title: 'Exact incremental graph',
    type: 'feature',
    status: 'closed',
    priority: 1,
    assignee: 'ada',
    labels: ['area:core'],
    created: daysAgo(30),
    updated: daysAgo(9),
    closed: daysAgo(9)
  }),
  mk({
    id: '77',
    title: 'Stats dashboard',
    type: 'feature',
    status: 'closed',
    priority: 3,
    assignee: 'sam',
    labels: ['area:web'],
    created: daysAgo(16),
    updated: daysAgo(7),
    closed: daysAgo(7)
  })
];

export const MOCK_COMMENTS: Record<string, Comment[]> = {
  '42': [
    {
      author: 'lin',
      timestamp: ago(4),
      body: 'Holding this until #45 lands — the race makes replay tests flaky, so the idempotency assertions here will be unreliable.'
    },
    {
      author: 'ada',
      timestamp: ago(2),
      body: "Agreed. I'll bump #45 to p0 and pick it up today. Once merged this unblocks #42 and #53 both."
    }
  ],
  '45': [
    {
      author: 'ada',
      timestamp: ago(3),
      body: 'Bumped to p0. Picking this up now.'
    }
  ]
};

export function mockHistory(): StatsHistoryPoint[] {
  const pts: StatsHistoryPoint[] = [];
  let open = 6;
  for (let i = 13; i >= 0; i--) {
    const created = Math.floor(Math.random() * 3) + (i % 3 === 0 ? 2 : 0);
    const closed = Math.floor(Math.random() * 2) + (i % 4 === 0 ? 1 : 0);
    open = Math.max(0, open + created - closed);
    pts.push({ date: daysAgo(i).slice(0, 10), created, closed, open });
  }
  return pts;
}
