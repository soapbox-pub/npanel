import type { Instance } from './api-types.ts';

/**
 * Which npanel server this dashboard talks to.
 *
 * The same build is served two ways: embedded in the npanel binary, where the
 * API is on the page's own origin, and as an nsite anywhere at all, where it
 * isn't and the reader has to say which server to manage. The API answers
 * cross-origin requests, and NIP-98 binds each request to the API's URL rather
 * than to the page's, so nothing else differs.
 */
export interface Server {
  /** The server's dashboard origin, e.g. `https://panel.example.com`. */
  origin: string;
  /** Chosen by the reader rather than found on the page's own origin. */
  standalone: boolean;
}

const STORAGE_KEY = 'npanel:server';

/** This dashboard's version, from Cargo.toml at build time. */
export const DASHBOARD_VERSION: string = __NPANEL_VERSION__;

/**
 * The origin a reader means by what they typed: `panel.example.com`,
 * `https://panel.example.com/hosts`, and so on. Bare names are taken to be
 * https, since npanel expects a TLS-terminating proxy in front of it.
 */
export function parseServerOrigin(input: string): string {
  const trimmed = input.trim();
  if (!trimmed) throw new Error('Enter the address of your npanel dashboard.');

  let url: URL;
  try {
    url = new URL(/^[a-z][a-z0-9+.-]*:\/\//i.test(trimmed) ? trimmed : `https://${trimmed}`);
  } catch {
    throw new Error(`"${trimmed}" isn't a web address. Enter the hostname of your npanel dashboard.`);
  }
  if (url.protocol !== 'https:' && url.protocol !== 'http:') {
    throw new Error('The address has to start with https://.');
  }
  return url.origin;
}

/**
 * The server's description of itself, or undefined when whatever answers at
 * that origin isn't npanel — an nsite gateway's SPA fallback answers this path
 * with HTML and a 200.
 */
export async function probeServer(origin: string, signal?: AbortSignal): Promise<Instance | undefined> {
  try {
    const response = await fetch(new URL('/api/instance', origin), { signal });
    if (!response.ok) return undefined;
    const body: unknown = await response.json();
    if (typeof body === 'object' && body !== null && typeof (body as Instance).dashboard_host === 'string') {
      return body as Instance;
    }
  } catch {
    // Unreachable, refused by CORS, or not JSON: not an npanel either way.
  }
  return undefined;
}

/**
 * The page's own origin when npanel serves it, otherwise the server last
 * chosen here, otherwise none yet.
 */
export async function resolveServer(signal?: AbortSignal): Promise<Server | undefined> {
  if (await probeServer(window.location.origin, signal)) {
    return { origin: window.location.origin, standalone: false };
  }
  const saved = savedServer();
  return saved ? { origin: saved, standalone: true } : undefined;
}

export function savedServer(): string | undefined {
  try {
    const saved = localStorage.getItem(STORAGE_KEY);
    return saved ? parseServerOrigin(saved) : undefined;
  } catch {
    return undefined;
  }
}

export function saveServer(origin: string | undefined): void {
  if (origin) localStorage.setItem(STORAGE_KEY, origin);
  else localStorage.removeItem(STORAGE_KEY);
}

/**
 * Whether a server is older than this dashboard, by major and minor version.
 * A server too old to report its version is older than any dashboard that
 * asks.
 */
export function serverIsBehind(serverVersion: string | undefined, dashboardVersion = DASHBOARD_VERSION): boolean {
  if (!serverVersion) return true;
  const [major, minor] = parts(serverVersion);
  const [ownMajor, ownMinor] = parts(dashboardVersion);
  return major < ownMajor || (major === ownMajor && minor < ownMinor);
}

function parts(version: string): [number, number] {
  const [major = 0, minor = 0] = version.split(/[.-]/).map((part) => Number.parseInt(part, 10) || 0);
  return [major, minor];
}
