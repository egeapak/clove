// @vitest-environment jsdom
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, screen, fireEvent, cleanup } from '@testing-library/svelte';
import type { Item } from '$lib/types';
import ItemForm from './ItemForm.svelte';

// Keep the component isolated from the live store / WebSocket wiring.
vi.mock('$lib/store.svelte', () => ({ store: { meta: null } }));

afterEach(() => cleanup());

function item(overrides: Partial<Item> = {}): Item {
  return {
    id: 'proj-7AF3K2MN',
    title: 'Original title',
    status: 'open',
    type: 'feature',
    priority: 2,
    assignee: 'alice',
    parent: null,
    labels: ['area:web', 'urgent'],
    deps: [],
    relates: [],
    created: '2026-01-01T00:00:00Z',
    updated: '2026-01-01T00:00:00Z',
    closed: null,
    body: 'Original body',
    comment_count: 0,
    ready: true,
    blocked_by: [],
    dangling_deps: [],
    ...overrides
  };
}

describe('ItemForm', () => {
  it('prefills fields from the item in edit mode', () => {
    render(ItemForm, { props: { mode: 'edit', item: item(), onsubmit: vi.fn() } });
    expect((screen.getByLabelText('Title') as HTMLInputElement).value).toBe('Original title');
    expect((screen.getByLabelText('Assignee') as HTMLInputElement).value).toBe('alice');
    // Edit mode shows a Status field; the existing labels render as chips.
    expect(screen.getByText('Status')).toBeInTheDocument();
    expect(screen.getByText('area:web')).toBeInTheDocument();
    expect(screen.getByText('urgent')).toBeInTheDocument();
  });

  it('omits the Status field in create mode', () => {
    render(ItemForm, { props: { mode: 'create', onsubmit: vi.fn() } });
    expect(screen.queryByText('Status')).not.toBeInTheDocument();
    expect((screen.getByLabelText('Title') as HTMLInputElement).value).toBe('');
  });

  it('submits an edited form state through the onsubmit callback', async () => {
    const onsubmit = vi.fn();
    render(ItemForm, { props: { mode: 'edit', item: item(), onsubmit } });

    await fireEvent.input(screen.getByLabelText('Title'), { target: { value: 'Edited title' } });
    await fireEvent.input(screen.getByLabelText('Assignee'), { target: { value: '' } });
    await fireEvent.click(screen.getByRole('button', { name: /save changes/i }));

    expect(onsubmit).toHaveBeenCalledTimes(1);
    const form = onsubmit.mock.calls[0][0];
    expect(form.title).toBe('Edited title');
    expect(form.assignee).toBe('');
    expect(form.body).toBe('Original body');
  });

  it('adds and removes labels', async () => {
    const onsubmit = vi.fn();
    render(ItemForm, { props: { mode: 'edit', item: item(), onsubmit } });

    // Remove an existing label.
    await fireEvent.click(screen.getByRole('button', { name: 'remove label urgent' }));
    expect(screen.queryByText('urgent')).not.toBeInTheDocument();

    // Add a new one via Enter.
    const labelInput = screen.getByPlaceholderText('add label, Enter to add…');
    await fireEvent.input(labelInput, { target: { value: 'New Label' } });
    await fireEvent.keyDown(labelInput, { key: 'Enter' });
    expect(screen.getByText('new label')).toBeInTheDocument(); // normalized

    await fireEvent.click(screen.getByRole('button', { name: /save changes/i }));
    const form = onsubmit.mock.calls[0][0];
    expect(form.labels).toEqual(['area:web', 'new label']);
  });

  it('disables submit until a title is present (create mode)', async () => {
    render(ItemForm, { props: { mode: 'create', onsubmit: vi.fn() } });
    const submit = screen.getByRole('button', { name: /create/i }) as HTMLButtonElement;
    expect(submit.disabled).toBe(true);
    await fireEvent.input(screen.getByLabelText('Title'), { target: { value: 'Has a title' } });
    expect(submit.disabled).toBe(false);
  });

  it('toggles the body preview tab', async () => {
    render(ItemForm, { props: { mode: 'create', onsubmit: vi.fn() } });
    // Write tab shows a textarea by default.
    expect(screen.getByPlaceholderText('Markdown…')).toBeInTheDocument();
    await fireEvent.click(screen.getByRole('tab', { name: 'Preview' }));
    // Empty body → placeholder, so no markdown render is attempted.
    expect(screen.getByText('Nothing to preview.')).toBeInTheDocument();
  });
});
