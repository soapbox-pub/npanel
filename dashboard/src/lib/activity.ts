import { formatNsiteAddress, parseNsiteAddress } from '@shared/nsite-address';

import type { AuditEntry } from '@/lib/api-types';
import { formatBytes } from '@/lib/format';

/**
 * Audit log entries as sentences a person reads, rather than rows of JSON.
 *
 * Each entry becomes a {@link Activity}: what kind of thing happened, a
 * sentence naming who did it to what, and the particulars worth showing
 * underneath — which switches moved, what a setting went from and to, who
 * gained or lost access. The server records whole before-and-after values; the
 * work here is finding the part of them that changed.
 */

/** What an entry is about, which decides its glyph and colour. */
export type ActivityKind =
  | 'site-added'
  | 'site-changed'
  | 'site-removed'
  | 'access'
  | 'person-added'
  | 'person-changed'
  | 'person-removed'
  | 'name-set'
  | 'name-removed'
  | 'env-set'
  | 'env-removed'
  | 'settings'
  | 'other';

/** A piece of the headline: plain words, or something drawn as what it names. */
export type Part =
  | string
  | { site: string }
  | { person: string }
  | { strong: string };

export type Detail =
  /** A switch that was flipped. */
  | { type: 'toggle'; label: string; on: boolean }
  /** A value that became another. */
  | { type: 'change'; label: string; from: string; to: string; mono?: boolean }
  /** A value that was set where there was none, or is simply worth stating. */
  | { type: 'value'; label: string; value: string; mono?: boolean }
  /** Entries in a list that came and went. */
  | { type: 'list'; label: string; added: string[]; removed: string[] }
  /** People who came and went. */
  | { type: 'people'; label: string; added: string[]; removed: string[] }
  /** One person, by pubkey. */
  | { type: 'person'; label: string; pubkey: string };

export interface Activity {
  kind: ActivityKind;
  /** The headline, starting after the actor's name. */
  sentence: Part[];
  details: Detail[];
}

/** Every switch on a site, as the site page labels it. `listed` is `hidden` read the other way up. */
const SITE_FLAGS: { key: string; label: string; inverted?: boolean }[] = [
  { key: 'enabled', label: 'Serve this site' },
  { key: 'spa', label: 'Single-page app' },
  { key: 'merge_deployments', label: 'Keep files from earlier deploys' },
  { key: 'redirect_www', label: 'Redirect www' },
  { key: 'csp', label: 'Content Security Policy' },
  { key: 'analytics', label: 'Analytics' },
  { key: 'hidden', label: 'Public site list', inverted: true },
  { key: 'seo', label: 'Search engine defaults' },
];

/** Settings, as the Settings page labels them. */
const SETTINGS: Record<string, { label: string; format?: (value: unknown) => string; secret?: boolean }> = {
  relays: { label: 'Relays' },
  blossom_fallback_servers: { label: 'Fallback Blossom servers' },
  max_file_size: { label: 'Largest file', format: (value) => formatBytes(Number(value)) },
  max_site_size: { label: 'Largest site', format: (value) => formatBytes(Number(value)) },
  max_site_files: { label: 'Most files in a site' },
  deploy_attempts: { label: 'Attempts before giving up' },
  resync_interval: { label: 'Check relays every', format: (value) => `${value} seconds` },
  dns_targets: { label: 'DNS target' },
  allowed_host_suffixes: { label: 'Allowed domains' },
  tls_check_limit_per_host: { label: 'Certificates per domain', format: (value) => `${value} an hour` },
  tls_check_limit_total: { label: 'Certificates in total', format: (value) => `${value} an hour` },
  contact_npub: { label: 'Homepage contact' },
  public_site_list: { label: 'Public site list' },
  tor_enabled: { label: 'Tor' },
  tor_control_url: { label: 'Tor control address' },
  tor_control_password: { label: 'Tor control password', secret: true },
  analytics_timezone: { label: 'Analytics time zone', format: (value) => String(value).replaceAll('_', ' ') },
};

const ROLES: Record<string, string> = { admin: 'Admin', staff: 'Staff' };

export function describe(entry: AuditEntry): Activity {
  const before = record(entry.old_value);
  const after = record(entry.new_value);
  const target = entry.target ?? '';

  switch (entry.action) {
    case 'host.create':
      return {
        kind: 'site-added',
        sentence: ['added', { site: target }],
        details: nsiteDetail('Serves', after.address),
      };

    case 'host.delete':
      return { kind: 'site-removed', sentence: ['deleted', { site: target }], details: [] };

    case 'host.update':
      return siteUpdate(target, before, after);

    case 'host.grants': {
      const was = strings(entry.old_value);
      const now = strings(entry.new_value);
      const added = now.filter((pubkey) => !was.includes(pubkey));
      const removed = was.filter((pubkey) => !now.includes(pubkey));

      // One person in or out is the common case, and reads best as a sentence.
      if (added.length === 1 && !removed.length) {
        return { kind: 'access', sentence: ['gave', { person: added[0] }, 'access to', { site: target }], details: [] };
      }
      if (removed.length === 1 && !added.length) {
        return {
          kind: 'access',
          sentence: ['took', { site: target }, 'away from', { person: removed[0] }],
          details: [],
        };
      }
      return {
        kind: 'access',
        sentence: ['changed who can manage', { site: target }],
        details: added.length || removed.length ? [{ type: 'people', label: 'Staff', added, removed }] : [],
      };
    }

    case 'user.create': {
      const role = ROLES[String(after.role)] ?? String(after.role);
      return {
        kind: 'person-added',
        sentence:
          target === entry.actor_pubkey && after.via === 'make-admin'
            ? ['became the first', { strong: role }, 'from the command line']
            : ['added', { person: target }, 'as', { strong: role }],
        details: [],
      };
    }

    case 'user.update': {
      const role = ROLES[String(after.role)] ?? String(after.role);
      return {
        kind: 'person-changed',
        sentence: ['made', { person: target }, article(role), { strong: role }],
        details: [],
      };
    }

    case 'user.delete':
      return { kind: 'person-removed', sentence: ['removed', { person: target }], details: [] };

    case 'nip05.set': {
      const pubkey = typeof after.pubkey === 'string' ? after.pubkey : '';
      const relays = strings(after.relays);
      const replaced = typeof before.pubkey === 'string' && before.pubkey !== pubkey ? before.pubkey : undefined;

      return {
        kind: 'name-set',
        sentence: [before.pubkey ? 'updated' : 'added', { strong: nip05Name(target) }],
        details: [
          ...(replaced ? [{ type: 'person' as const, label: 'Was', pubkey: replaced }] : []),
          { type: 'person', label: replaced ? 'Now' : 'For', pubkey },
          ...listDetail('Relays', strings(before.relays), relays),
        ],
      };
    }

    case 'nip05.delete':
      return { kind: 'name-removed', sentence: ['removed', { strong: nip05Name(target) }], details: [] };

    case 'env.set': {
      const name = String(after.name ?? '');
      const value = String(after.value ?? '');

      return {
        kind: 'env-set',
        sentence: [before.name ? 'changed' : 'set', { strong: name }, 'on', { site: target }],
        details: [
          typeof before.value === 'string'
            ? { type: 'change', label: 'Value', from: before.value, to: value, mono: true }
            : { type: 'value', label: 'Value', value, mono: true },
        ],
      };
    }

    case 'env.delete':
      return {
        kind: 'env-removed',
        sentence: ['removed', { strong: String(before.name ?? '') }, 'from', { site: target }],
        details: [],
      };

    case 'settings.update':
      return settingUpdate(target, entry.old_value, entry.new_value);

    default:
      return {
        kind: 'other',
        sentence: [entry.action.replaceAll(/[._]/g, ' '), ...(target ? [{ strong: target }] : [])],
        details: [],
      };
  }
}

/** A site edit: whichever switches moved, a new name, a new nsite, or a pin. */
function siteUpdate(hostname: string, before: Record<string, unknown>, after: Record<string, unknown>): Activity {
  const details: Detail[] = [];

  if (typeof after.hostname === 'string' && after.hostname !== before.hostname) {
    details.push({ type: 'change', label: 'Domain', from: hostname, to: after.hostname });
  }

  if (typeof after.address === 'string') {
    const from = typeof before.pubkey === 'string' ? formatNsiteAddress({
      pubkey: before.pubkey,
      kind: Number(before.kind),
      identifier: String(before.identifier ?? ''),
    }) : '';
    details.push({ type: 'change', label: 'nsite', from, to: normalizeAddress(after.address), mono: true });
  }

  for (const flag of SITE_FLAGS) {
    if (!(flag.key in after) || truthy(after[flag.key]) === truthy(before[flag.key])) continue;
    details.push({ type: 'toggle', label: flag.label, on: truthy(after[flag.key]) !== !!flag.inverted });
  }

  const pinned = 'pinned_event_id' in after && after.pinned_event_id !== before.pinned_event_id;

  // A single change reads as what it was, not as "changed" plus a list of one.
  if (pinned && !details.length) {
    return typeof after.pinned_event_id === 'string'
      ? {
          kind: 'site-changed',
          sentence: ['rolled back', { site: hostname }],
          details: [{ type: 'value', label: 'To version', value: after.pinned_event_id.slice(0, 12), mono: true }],
        }
      : { kind: 'site-changed', sentence: ['unpinned', { site: hostname }, 'to follow new deploys'], details: [] };
  }

  const [only] = details;
  if (details.length === 1 && only.type === 'toggle') {
    if (only.label === 'Serve this site') {
      return { kind: 'site-changed', sentence: [only.on ? 'turned on' : 'turned off', { site: hostname }], details: [] };
    }
    return {
      kind: 'site-changed',
      sentence: [only.on ? 'turned on' : 'turned off', { strong: only.label }, 'for', { site: hostname }],
      details: [],
    };
  }
  if (details.length === 1 && only.type === 'change' && only.label === 'Domain') {
    return { kind: 'site-changed', sentence: ['renamed', { site: hostname }, 'to', { site: only.to }], details: [] };
  }

  if (pinned) {
    details.push(
      typeof after.pinned_event_id === 'string'
        ? { type: 'value', label: 'Pinned to version', value: after.pinned_event_id.slice(0, 12), mono: true }
        : { type: 'value', label: 'Pin', value: 'Following new deploys' },
    );
  }

  return { kind: 'site-changed', sentence: ['changed', { site: hostname }], details };
}

function settingUpdate(key: string, before: unknown, after: unknown): Activity {
  const setting = SETTINGS[key] ?? { label: key.replaceAll('_', ' ') };
  const sentence: Part[] = ['changed', { strong: setting.label }];

  if (setting.secret) {
    return { kind: 'settings', sentence: [after ? 'changed' : 'cleared', { strong: setting.label }], details: [] };
  }

  if (typeof after === 'boolean') {
    return { kind: 'settings', sentence: [after ? 'turned on' : 'turned off', { strong: setting.label }], details: [] };
  }

  if (Array.isArray(after) || Array.isArray(before)) {
    const details = listDetail('', strings(before), strings(after));
    return { kind: 'settings', sentence, details };
  }

  const format = setting.format ?? ((value: unknown) => String(value));
  const from = before === null || before === undefined || before === '' ? '' : format(before);
  const to = after === null || after === undefined || after === '' ? '' : format(after);

  if (key === 'contact_npub') {
    return {
      kind: 'settings',
      sentence: to ? ['set the', { strong: setting.label }] : ['cleared the', { strong: setting.label }],
      details: to ? [{ type: 'value', label: 'Now', value: to, mono: true }] : [],
    };
  }

  return {
    kind: 'settings',
    sentence,
    details: [
      from
        ? { type: 'change', label: '', from, to: to || 'Nothing' }
        : { type: 'value', label: 'Now', value: to || 'Nothing' },
    ],
  };
}

function nsiteDetail(label: string, address: unknown): Detail[] {
  if (typeof address !== 'string') return [];
  const pubkey = address.split(':')[1];
  return pubkey && /^[0-9a-f]{64}$/.test(pubkey) ? [{ type: 'person', label, pubkey }] : [];
}

function listDetail(label: string, before: string[], after: string[]): Detail[] {
  const added = after.filter((value) => !before.includes(value));
  const removed = before.filter((value) => !after.includes(value));
  return added.length || removed.length ? [{ type: 'list', label, added, removed }] : [];
}

/** An address as it was typed, in the one spelling the site page shows. */
function normalizeAddress(input: string): string {
  try {
    return formatNsiteAddress(parseNsiteAddress(input));
  } catch {
    return input;
  }
}

/** `_@example.com` is the name that reads as just the domain. */
function nip05Name(target: string): string {
  return target.startsWith('_@') ? target.slice(2) : target;
}

function article(word: string): string {
  return /^[aeiou]/i.test(word) ? 'an' : 'a';
}

/** Flags are stored as 1 and 0 before an edit and sent as booleans with it. */
function truthy(value: unknown): boolean {
  return value === true || value === 1;
}

function record(value: unknown): Record<string, unknown> {
  return value && typeof value === 'object' && !Array.isArray(value) ? (value as Record<string, unknown>) : {};
}

function strings(value: unknown): string[] {
  return Array.isArray(value) ? value.filter((item): item is string => typeof item === 'string') : [];
}
