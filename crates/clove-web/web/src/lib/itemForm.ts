import type { Item, Status } from './types';

/** The editable state of the create/edit form, surface-agnostic of the UI. */
export interface FormState {
  title: string;
  type: string;
  priority: number;
  status: Status;
  assignee: string;
  labels: string[];
  body: string;
}

/** The POST /items create payload (shaped for `api.create`). */
export interface CreatePayload {
  title: string;
  type: string;
  priority: number;
  labels: string[];
  assignee?: string;
  body?: string;
}

/** A partial PATCH /items/:id payload — only the fields that changed. */
export interface PatchPayload {
  title?: string;
  status?: Status;
  priority?: number;
  type?: string;
  /** `string` to set, `null` to clear, absent to leave unchanged. */
  assignee?: string | null;
  body?: string;
  /** The full replacement label set (form semantics). */
  labels?: string[];
}

/** A blank form for the create flow, with sensible defaults. */
export function emptyForm(overrides: Partial<FormState> = {}): FormState {
  return {
    title: '',
    type: 'feature',
    priority: 2,
    status: 'open',
    assignee: '',
    labels: [],
    body: '',
    ...overrides
  };
}

/** Prefill a form from an existing item (the edit flow). */
export function formFromItem(item: Item): FormState {
  return {
    title: item.title,
    type: item.type,
    priority: item.priority,
    status: item.status,
    assignee: item.assignee ?? '',
    labels: [...item.labels],
    body: item.body ?? ''
  };
}

/** Normalize a label like the server: lowercase, trim, collapse whitespace. */
export function normalizeLabel(raw: string): string {
  return raw.toLowerCase().trim().split(/\s+/).join(' ');
}

/** Parse a comma/newline-separated label string into a clean, deduped list. */
export function parseLabels(raw: string): string[] {
  const seen = new Set<string>();
  for (const part of raw.split(/[,\n]/)) {
    const l = normalizeLabel(part);
    if (l) seen.add(l);
  }
  return [...seen];
}

/** Canonicalize a label list (normalize + dedupe + sort) for stable comparison. */
function canonicalLabels(labels: string[]): string[] {
  const seen = new Set<string>();
  for (const l of labels) {
    const n = normalizeLabel(l);
    if (n) seen.add(n);
  }
  return [...seen].sort();
}

/** Whether the form has the minimum needed to submit (a non-empty title). */
export function isSubmittable(form: FormState): boolean {
  return form.title.trim().length > 0;
}

/** Build the create payload from a form (drops blank optional fields). */
export function buildCreate(form: FormState): CreatePayload {
  const payload: CreatePayload = {
    title: form.title.trim(),
    type: form.type,
    priority: form.priority,
    labels: canonicalLabels(form.labels)
  };
  const assignee = form.assignee.trim();
  if (assignee) payload.assignee = assignee;
  const body = form.body.trim();
  if (body) payload.body = body;
  return payload;
}

/**
 * Build a minimal PATCH payload: only fields whose value differs from the
 * original item. Title is sent only when non-empty (the server rejects an empty
 * title); an empty assignee becomes `null` (clear).
 */
export function buildPatch(form: FormState, original: Item): PatchPayload {
  const patch: PatchPayload = {};

  const title = form.title.trim();
  if (title && title !== original.title) patch.title = title;
  if (form.status !== original.status) patch.status = form.status;
  if (form.priority !== original.priority) patch.priority = form.priority;
  if (form.type !== original.type) patch.type = form.type;

  const assignee = form.assignee.trim() || null;
  if (assignee !== (original.assignee ?? null)) patch.assignee = assignee;

  if (form.body !== original.body) patch.body = form.body;

  const labels = canonicalLabels(form.labels);
  const originalLabels = canonicalLabels(original.labels);
  if (labels.length !== originalLabels.length || labels.some((l, i) => l !== originalLabels[i])) {
    patch.labels = labels;
  }
  return patch;
}

/** Whether a PATCH payload would change anything. */
export function isEmptyPatch(patch: PatchPayload): boolean {
  return Object.keys(patch).length === 0;
}
