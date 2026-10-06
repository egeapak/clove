// An item's link to its external record (a synced GitHub issue), mirroring
// `clove-web`'s `sync::is_synced`: `gh-<number>` is the ref `clove sync github`
// writes, and the issue URL comes from the project's sync target in `/meta`.
import type { Item, Meta } from './types';

export interface ExternalRef {
  /** Full spelling, e.g. `GitHub #67`. */
  label: string;
  /** Compact spelling for tight rows, e.g. `GH#67`. */
  short: string;
  /** The issue URL, when the project's sync target is known. */
  href: string | null;
  /** A GitHub issue, shown with the GitHub mark. */
  github: boolean;
}

/** The GitHub issue number in a `gh-<n>` ref, or null. */
export function githubIssueNumber(ref: string | null | undefined): number | null {
  const m = /^gh-(\d+)$/.exec((ref ?? '').trim());
  return m ? Number(m[1]) : null;
}

/** Whether the item is linked to a GitHub issue (`?synced=true` on the API). */
export function isSynced(item: Pick<Item, 'external_ref'>): boolean {
  return githubIssueNumber(item.external_ref) !== null;
}

/** Whether the project syncs with anything; markers stay hidden otherwise. */
export function hasSyncTarget(meta: Meta | null | undefined): boolean {
  return (meta?.sync?.length ?? 0) > 0;
}

/** The sync-target names for a tooltip, e.g. `GitHub egeapak/clove`. */
export function syncTargetLabel(meta: Meta | null | undefined): string {
  return (meta?.sync ?? []).map((t) => `${providerName(t.provider)} ${t.repo}`).join(', ');
}

function providerName(provider: string): string {
  return provider === 'github' ? 'GitHub' : provider;
}

/** How to show the item's external ref, or null when it has none. */
export function externalRef(item: Pick<Item, 'external_ref' | 'source_system'>, meta: Meta | null | undefined): ExternalRef | null {
  const ref = item.external_ref?.trim();
  if (!ref) return null;
  const number = githubIssueNumber(ref);
  if (number !== null) {
    // An item synced with one repo is that repo's issue; with several targets
    // the number alone cannot say which, so no link is better than a wrong one.
    const targets = (meta?.sync ?? []).filter((t) => t.provider === 'github');
    return {
      label: `GitHub #${number}`,
      short: `GH#${number}`,
      href: targets.length === 1 ? `${targets[0].issue_url}${number}` : null,
      github: true
    };
  }
  const system = item.source_system ? `${item.source_system} ` : '';
  return { label: `${system}${ref}`, short: ref, href: null, github: false };
}
