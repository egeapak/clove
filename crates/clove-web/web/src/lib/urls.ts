// Server URLs, derived from the SvelteKit `base`. Standalone `clove serve` runs
// the app at the root (`base === ''`); the daemon hub mounts each project under
// `/p/<slug>` and injects that as the runtime base, so the API and the event
// socket must follow it or every project would talk to the same endpoints.

/** The hub-level project listing; lives at the root, outside any project. */
export const PROJECTS_URL = '/api/v1/projects';

/** The JSON API root for the app served at `base`. */
export function apiBase(base: string): string {
  return `${base}/api/v1`;
}

/** The live-update WebSocket URL for the app served at `base` on `loc`. */
export function eventsUrl(loc: { protocol: string; host: string }, base: string): string {
  const proto = loc.protocol === 'https:' ? 'wss:' : 'ws:';
  return `${proto}//${loc.host}${apiBase(base)}/events`;
}

/** The hub project slug in `base`, or `null` when served standalone. */
export function projectSlug(base: string): string | null {
  const match = /^\/p\/([^/]+)$/.exec(base);
  return match ? decodeURIComponent(match[1]) : null;
}
