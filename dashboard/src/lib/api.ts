import type { NostrSigner } from '@nostrify/nostrify';

import type {
  AuditEntry,
  DeployLogEntry,
  DnsTarget,
  Host,
  HostVersion,
  Me,
  NpanelUser,
  ServerStatus,
  Settings,
  SiteFile,
} from './api-types.ts';

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

  hosts(): Promise<{ hosts: Host[] }> {
    return this.request('GET', '/api/hosts');
  }

  host(id: number): Promise<{ host: Host }> {
    return this.request('GET', `/api/hosts/${id}`);
  }

  createHost(input: { hostname: string; address: string }): Promise<{ host: Host }> {
    return this.request('POST', '/api/hosts', input);
  }

  updateHost(
    id: number,
    patch: {
      hostname?: string;
      address?: string;
      enabled?: boolean;
      pinned_event_id?: string | null;
    },
  ): Promise<{ host: Host }> {
    return this.request('PATCH', `/api/hosts/${id}`, patch);
  }

  deleteHost(id: number): Promise<{ ok: true }> {
    return this.request('DELETE', `/api/hosts/${id}`);
  }

  syncHost(id: number): Promise<{ ok: true }> {
    return this.request('POST', `/api/hosts/${id}/sync`);
  }

  versions(id: number): Promise<{
    active_event_id: string | null;
    pinned_event_id: string | null;
    versions: HostVersion[];
  }> {
    return this.request('GET', `/api/hosts/${id}/versions`);
  }

  deployLog(
    id: number,
    eventId: string,
  ): Promise<{ version: HostVersion; entries: DeployLogEntry[] }> {
    return this.request('GET', `/api/hosts/${id}/versions/${eventId}/log`);
  }

  files(id: number, eventId?: string): Promise<{ event_id: string; files: SiteFile[] }> {
    const query = eventId ? `?event_id=${encodeURIComponent(eventId)}` : '';
    return this.request('GET', `/api/hosts/${id}/files${query}`);
  }

  grants(id: number): Promise<{ grants: { user_pubkey: string; created_at: number }[] }> {
    return this.request('GET', `/api/hosts/${id}/grants`);
  }

  setGrants(id: number, pubkeys: string[]): Promise<{ grants: string[] }> {
    return this.request('PUT', `/api/hosts/${id}/grants`, { pubkeys });
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
