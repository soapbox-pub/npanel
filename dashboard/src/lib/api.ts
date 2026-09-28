import type { NostrSigner } from '@nostrify/nostrify';

import type {
  AuditEntry,
  DeployLogEntry,
  DnsTarget,
  Host,
  HostPatch,
  HostVersion,
  Instance,
  Me,
  Nip05Name,
  NpanelUser,
  PublicSite,
  SiteCard,
  ServerStatus,
  Settings,
  SiteFile,
  Sparkline,
} from './api-types.ts';
import type { StatsQueryBody, StatsResponse } from './analytics/query.ts';

/** NIP-98 HTTP auth event kind. */
const NIP98_KIND = 27235;

export class ApiError extends Error {
  constructor(
    message: string,
    readonly status: number,
  ) {
    super(message);
    this.name = 'ApiError';
  }
}

/**
 * Client for the npanel API.
 *
 * Every request carries a freshly signed NIP-98 event whose `u`, `method` and
 * `payload` tags bind it to exactly this request, so a captured token cannot
 * be replayed against another endpoint.
 */
export class ApiClient {
  constructor(private readonly signer: NostrSigner) {}

  async request<T>(method: string, path: string, body?: unknown): Promise<T> {
    const url = new URL(path, window.location.origin);
    const payload = body === undefined ? undefined : JSON.stringify(body);

    const tags: string[][] = [
      ['method', method.toUpperCase()],
      ['u', url.toString()],
    ];
    if (payload !== undefined) {
      tags.push(['payload', await sha256Hex(payload)]);
    }

    const event = await this.signer.signEvent({
      kind: NIP98_KIND,
      created_at: Math.floor(Date.now() / 1000),
      tags,
      content: '',
    });

    const headers: Record<string, string> = {
      authorization: `Nostr ${base64(JSON.stringify(event))}`,
    };
    if (payload !== undefined) {
      headers['content-type'] = 'application/json';
    }

    const response = await fetch(url, { method, headers, body: payload });

    if (!response.ok) {
      throw new ApiError(await errorMessage(response), response.status);
    }

    return (await response.json()) as T;
  }

  me(): Promise<Me> {
    return this.request('GET', '/api/me');
  }

  status(): Promise<ServerStatus> {
    return this.request('GET', '/api/status');
  }

  dns(): Promise<DnsTarget> {
    return this.request('GET', '/api/dns');
  }

  /** Sites this user manages, plus the ones they may only be told the name of. */
  hosts(): Promise<{ hosts: Host[]; other_sites: PublicSite[] }> {
    return this.request('GET', '/api/hosts');
  }

  host(hostname: string): Promise<{ host: Host }> {
    return this.request('GET', `/api/hosts/${path(hostname)}`);
  }

  createHost(input: {
    hostname: string;
    address: string;
    hidden?: boolean;
    redirect_www?: boolean;
  }): Promise<{ host: Host }> {
    return this.request('POST', '/api/hosts', input);
  }

  updateHost(hostname: string, patch: HostPatch): Promise<{ host: Host }> {
    return this.request('PATCH', `/api/hosts/${path(hostname)}`, patch);
  }

  deleteHost(hostname: string): Promise<{ ok: true }> {
    return this.request('DELETE', `/api/hosts/${path(hostname)}`);
  }

  syncHost(hostname: string): Promise<{ ok: true }> {
    return this.request('POST', `/api/hosts/${path(hostname)}/sync`);
  }

  versions(hostname: string): Promise<{
    active_event_id: string | null;
    pinned_event_id: string | null;
    versions: HostVersion[];
  }> {
    return this.request('GET', `/api/hosts/${path(hostname)}/versions`);
  }

  deployLog(
    hostname: string,
    eventId: string,
  ): Promise<{ version: HostVersion; entries: DeployLogEntry[] }> {
    return this.request('GET', `/api/hosts/${path(hostname)}/versions/${eventId}/log`);
  }

  files(hostname: string, eventId?: string): Promise<{ event_id: string; files: SiteFile[] }> {
    const query = eventId ? `?event_id=${encodeURIComponent(eventId)}` : '';
    return this.request('GET', `/api/hosts/${path(hostname)}/files${query}`);
  }

  grants(hostname: string): Promise<{ grants: { user_pubkey: string; created_at: number }[] }> {
    return this.request('GET', `/api/hosts/${path(hostname)}/grants`);
  }

  setGrants(hostname: string, pubkeys: string[]): Promise<{ grants: string[] }> {
    return this.request('PUT', `/api/hosts/${path(hostname)}/grants`, { pubkeys });
  }

  /** Plausible's dashboard query, answered for this site. */
  statsQuery(hostname: string, body: StatsQueryBody): Promise<StatsResponse> {
    return this.request('POST', `/api/hosts/${path(hostname)}/stats/query`, body);
  }

  /** The last 24 hours of every site this user manages, keyed by hostname. */
  sparklines(): Promise<{ sparklines: Record<string, Sparkline> }> {
    return this.request('GET', '/api/stats/sparklines');
  }

  currentVisitors(hostname: string): Promise<number> {
    return this.request('GET', `/api/hosts/${path(hostname)}/stats/current-visitors`);
  }

  suggestions(hostname: string, filter: string, params: URLSearchParams): Promise<{ value: string; label: string }[]> {
    return this.request('GET', `/api/hosts/${path(hostname)}/stats/suggestions/${filter}?${params}`);
  }

  nip05Names(hostname: string): Promise<{ names: Nip05Name[] }> {
    return this.request('GET', `/api/hosts/${path(hostname)}/nip05`);
  }

  /** Adds the name, or replaces the one already stored under it. */
  setNip05Name(
    hostname: string,
    name: string,
    input: { pubkey: string; relays: string[] },
  ): Promise<{ names: Nip05Name[] }> {
    return this.request('PUT', `/api/hosts/${path(hostname)}/nip05/${path(name)}`, input);
  }

  deleteNip05Name(hostname: string, name: string): Promise<{ names: Nip05Name[] }> {
    return this.request('DELETE', `/api/hosts/${path(hostname)}/nip05/${path(name)}`);
  }

  users(): Promise<{ users: NpanelUser[] }> {
    return this.request('GET', '/api/users');
  }

  createUser(input: { pubkey: string; role: 'admin' | 'staff' }): Promise<unknown> {
    return this.request('POST', '/api/users', input);
  }

  updateUser(pubkey: string, role: 'admin' | 'staff'): Promise<unknown> {
    return this.request('PATCH', `/api/users/${pubkey}`, { role });
  }

  deleteUser(pubkey: string): Promise<{ ok: true }> {
    return this.request('DELETE', `/api/users/${pubkey}`);
  }

  settings(): Promise<{ settings: Settings; defaults: Settings }> {
    return this.request('GET', '/api/settings');
  }

  updateSettings(patch: Partial<Settings>): Promise<{ settings: Settings; changed: string[] }> {
    return this.request('PUT', '/api/settings', patch);
  }

  auditLog(limit = 100): Promise<{ entries: AuditEntry[] }> {
    return this.request('GET', `/api/audit-log?limit=${limit}`);
  }
}

/**
 * The one endpoint that takes no signature, so the homepage can introduce this
 * instance to a visitor who has no Nostr key at all — hence a bare function
 * rather than an {@link ApiClient} method, which would need a signer.
 */
export async function fetchInstance(): Promise<Instance> {
  const response = await fetch(new URL('/api/instance', window.location.origin));

  if (!response.ok) {
    throw new ApiError(await errorMessage(response), response.status);
  }

  return (await response.json()) as Instance;
}

/**
 * The sites this gateway serves, as far as a stranger is allowed to know.
 *
 * Empty unless an admin turned the public list on, so the homepage can call it
 * unconditionally and simply render nothing.
 */
export async function fetchSites(): Promise<{ sites: SiteCard[] }> {
  const response = await fetch(new URL('/api/sites', window.location.origin));

  if (!response.ok) {
    throw new ApiError(await errorMessage(response), response.status);
  }

  return (await response.json()) as { sites: SiteCard[] };
}

/**
 * A hostname or name as one path segment.
 *
 * A no-op for anything the server would accept, but it keeps a stray slash in
 * a value the dashboard was handed from turning into a different endpoint.
 */
function path(value: string): string {
  return encodeURIComponent(value);
}

async function errorMessage(response: Response): Promise<string> {
  try {
    const body = (await response.json()) as { error?: string };
    if (body.error) return body.error;
  } catch {
    // Non-JSON error body.
  }
  return `Request failed with status ${response.status}`;
}

async function sha256Hex(value: string): Promise<string> {
  const digest = await crypto.subtle.digest('SHA-256', new TextEncoder().encode(value));
  return Array.from(new Uint8Array(digest))
    .map((byte) => byte.toString(16).padStart(2, '0'))
    .join('');
}

/** Base64 that survives non-ASCII characters, which `btoa` alone does not. */
function base64(value: string): string {
  const bytes = new TextEncoder().encode(value);
  let binary = '';
  for (const byte of bytes) {
    binary += String.fromCharCode(byte);
  }
  return btoa(binary);
}
