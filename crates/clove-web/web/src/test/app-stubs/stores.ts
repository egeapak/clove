// Stub of `$app/stores` for vitest. Route tests `page.set(...)` the URL they
// render under.
import { readable, writable } from 'svelte/store';

export const page = writable({
  url: new URL('http://localhost/'),
  params: {} as Record<string, string>,
  route: { id: null as string | null },
  status: 200,
  error: null,
  data: {},
  state: {},
  form: null
});

export const navigating = readable(null);

export const updated = { subscribe: readable(false).subscribe, check: async () => false };
