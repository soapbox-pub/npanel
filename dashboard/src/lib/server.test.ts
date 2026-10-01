import type { NostrEvent, NostrSigner } from '@nostrify/nostrify';
import { afterEach, describe, expect, it, vi } from 'vitest';

import { ApiClient } from './api';
import { parseServerOrigin, resolveServer, saveServer, serverIsBehind } from './server';

afterEach(() => {
  vi.unstubAllGlobals();
  localStorage.clear();
});

/** Answers fetches by URL; anything unlisted fails the way an unreachable host does. */
function stubFetch(routes: Record<string, () => Response>) {
  const fetch = vi.fn(async (input: URL | string) => {
    const route = routes[String(input)];
    if (!route) throw new TypeError('Failed to fetch');
    return route();
  });
  vi.stubGlobal('fetch', fetch);
  return fetch;
}

const instance = () => Response.json({ contact_npub: null, dashboard_host: 'panel.example.com', version: '0.2.0' });

describe('parseServerOrigin', () => {
  it('takes what a reader would type to an origin', () => {
    expect(parseServerOrigin('panel.example.com')).toBe('https://panel.example.com');
    expect(parseServerOrigin(' https://Panel.Example.com/hosts?x=1 ')).toBe('https://panel.example.com');
    expect(parseServerOrigin('http://localhost:3000')).toBe('http://localhost:3000');
  });

  it('refuses what is not a web address', () => {
    for (const input of ['', '   ', 'ftp://panel.example.com', 'https://']) {
      expect(() => parseServerOrigin(input)).toThrow();
    }
  });
});

describe('resolveServer', () => {
  it('uses the page origin when npanel serves the page', async () => {
    saveServer('https://other.example.com');
    stubFetch({ [`${window.location.origin}/api/instance`]: instance });
    expect(await resolveServer()).toEqual({ origin: window.location.origin, standalone: false });
  });

  it('falls back to the saved server when the page is an nsite', async () => {
    // A gateway's SPA fallback answers every path with the page itself.
    stubFetch({ [`${window.location.origin}/api/instance`]: () => new Response('<!doctype html>', { status: 200 }) });
    expect(await resolveServer()).toBeUndefined();

    saveServer('https://panel.example.com');
    expect(await resolveServer()).toEqual({ origin: 'https://panel.example.com', standalone: true });
  });
});

describe('serverIsBehind', () => {
  it('compares major and minor versions', () => {
    expect(serverIsBehind('0.2.0', '0.2.5')).toBe(false);
    expect(serverIsBehind('0.3.0', '0.2.0')).toBe(false);
    expect(serverIsBehind('0.1.9', '0.2.0')).toBe(true);
    expect(serverIsBehind('1.0.0-rc.1', '1.1.0')).toBe(true);
    expect(serverIsBehind(undefined, '0.2.0')).toBe(true);
  });
});

describe('ApiClient', () => {
  it('signs for, and calls, the server rather than the page', async () => {
    const fetch = stubFetch({ 'https://panel.example.com/api/me': () => Response.json({ pubkey: 'x', role: 'admin' }) });
    const signed: NostrEvent[] = [];
    const signer: NostrSigner = {
      getPublicKey: async () => 'x',
      signEvent: async (template) => {
        const event = { ...template, id: 'id', pubkey: 'x', sig: 'sig' };
        signed.push(event);
        return event;
      },
    };

    await new ApiClient(signer, 'https://panel.example.com').me();

    expect(signed[0].tags).toContainEqual(['u', 'https://panel.example.com/api/me']);
    expect(String(fetch.mock.calls[0][0])).toBe('https://panel.example.com/api/me');
  });
});
