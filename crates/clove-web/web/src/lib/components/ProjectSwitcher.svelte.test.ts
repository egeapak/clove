// @vitest-environment jsdom
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, screen, cleanup } from '@testing-library/svelte';
import type { Project } from '$lib/types';
import ProjectSwitcher from './ProjectSwitcher.svelte';

afterEach(() => cleanup());

function project(slug: string): Project {
  return { slug, name: slug, root: `/src/${slug}`, url: `/p/${slug}/` };
}

describe('ProjectSwitcher', () => {
  it('offers every hub project and selects the current one', async () => {
    const load = vi.fn(async () => [project('alpha'), project('beta')]);
    render(ProjectSwitcher, { props: { current: 'beta', load } });
    const select = (await screen.findByLabelText('Project')) as HTMLSelectElement;
    expect(select.value).toBe('beta');
    expect([...select.options].map((o) => o.value)).toEqual(['alpha', 'beta']);
  });

  it('stays hidden with a single project', async () => {
    const load = vi.fn(async () => [project('alpha')]);
    render(ProjectSwitcher, { props: { current: 'alpha', load } });
    await vi.waitFor(() => expect(load).toHaveBeenCalled());
    expect(screen.queryByLabelText('Project')).toBeNull();
  });

  it('does not ask for projects when served standalone', () => {
    const load = vi.fn(async () => [project('alpha'), project('beta')]);
    render(ProjectSwitcher, { props: { current: null, load } });
    expect(load).not.toHaveBeenCalled();
    expect(screen.queryByLabelText('Project')).toBeNull();
  });
});
