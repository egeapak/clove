// @vitest-environment jsdom
import { describe, it, expect, afterEach } from 'vitest';
import { render, screen, cleanup } from '@testing-library/svelte';
import { store } from '$lib/store.svelte';
import type { Meta } from '$lib/types';
import { META } from '../../test/fake-api';
import SourceLink from './SourceLink.svelte';

afterEach(() => {
  cleanup();
  store.meta = null;
});

describe('source link (AGPL-3.0 section 13)', () => {
  it('links to the source of the running build', () => {
    store.meta = {
      ...META,
      source_code: {
        url: 'https://github.com/egeapak/clove/tree/v0.2.0',
        version: '0.2.0',
        license: 'AGPL-3.0-only'
      }
    } as unknown as Meta;
    render(SourceLink);
    const link = screen.getByRole('link', { name: 'Source code' });
    expect(link).toHaveAttribute('href', 'https://github.com/egeapak/clove/tree/v0.2.0');
    expect(link).toHaveAttribute('target', '_blank');
    expect(link).toHaveAttribute('rel', 'noopener noreferrer');
  });

  it('stays hidden when the server sends no source offer', () => {
    store.meta = META as unknown as Meta;
    const { container } = render(SourceLink);
    expect(container.querySelector('a')).toBeNull();
  });
});
