import { extname, join } from "node:path/posix";
import type { NostrEvent } from "@nostrify/nostrify";
import { NAMED_SITE_KIND, type NsiteAddress, ROOT_SITE_KIND, SITE_KINDS } from "./nsite-address.ts";

const SHA256 = /^[0-9a-f]{64}$/;

/** Whether an event is an nsite manifest. */
export function isSiteManifest(event: NostrEvent): boolean {
  return SITE_KINDS.includes(event.kind);
}

/** The address a manifest event belongs to. */
export function manifestAddress(event: NostrEvent): NsiteAddress {
  return {
    pubkey: event.pubkey,
    kind: event.kind,
    identifier: event.kind === NAMED_SITE_KIND ? (getTagValue(event, "d") ?? "") : "",
  };
}

/**
 * The file map of a manifest: absolute site path to blob hash.
 *
 * Tags whose hash isn't a sha256 are dropped rather than failing the whole
 * deploy — a single malformed tag shouldn't take a site offline.
 */
export function getManifestPaths(event: NostrEvent): Map<string, string> {
  const paths = new Map<string, string>();

  for (const tag of event.tags) {
    if (tag[0] !== "path" || !tag[1] || !tag[2]) continue;
    const sha256 = tag[2].toLowerCase();
    if (!SHA256.test(sha256)) continue;
    paths.set(join("/", tag[1]), sha256);
  }

  return paths;
}

export interface ManifestPathMatch {
  path: string;
  sha256: string;
  /** True when the request fell through to the site's `/404.html`. */
  is404: boolean;
}

/**
 * Resolve a request path against a manifest's file map: exact match first,
 * then `index.html` for extensionless paths, then the site's `/404.html`.
 */
export function resolveManifestPath(
  paths: ReadonlyMap<string, string>,
  requestPath: string,
): ManifestPathMatch | undefined {
  const normalized = join("/", decodePath(requestPath));

  const exact = paths.get(normalized);
  if (exact) return { path: normalized, sha256: exact, is404: false };

  if (extname(normalized) === "") {
    const indexPath = join(normalized, "index.html");
    const index = paths.get(indexPath);
    if (index) return { path: indexPath, sha256: index, is404: false };
  }

  const notFound = paths.get("/404.html");
  if (notFound) return { path: "/404.html", sha256: notFound, is404: true };

  return undefined;
}

/** Relay hints embedded in a manifest. */
export function getManifestRelays(event: NostrEvent): string[] {
  return event.tags.filter((tag) => (tag[0] === "r" || tag[0] === "relay") && tag[1]).map((tag) => tag[1]);
}

/** Blossom servers embedded in a manifest, tried before the author's list. */
export function getManifestServers(event: NostrEvent): string[] {
  return event.tags.filter((tag) => tag[0] === "server" && tag[1]).map((tag) => tag[1]);
}

export function getManifestTitle(event: NostrEvent): string | undefined {
  return getTagValue(event, "title");
}

export function getManifestDescription(event: NostrEvent): string | undefined {
  return getTagValue(event, "description");
}

/**
 * Relays from a kind 10002 list, split by read/write markers.
 *
 * npanel only ever reads, so unmarked and `read` relays are what matter, but
 * write relays are where a user's own events land, which is exactly where
 * their manifests will be. Both sets are returned.
 */
export function parseRelayList(event: NostrEvent): { read: string[]; write: string[] } {
  const read: string[] = [];
  const write: string[] = [];

  for (const tag of event.tags) {
    if (tag[0] !== "r" || !tag[1]) continue;
    const marker = tag[2];
    if (marker === "read") read.push(tag[1]);
    else if (marker === "write") write.push(tag[1]);
    else {
      read.push(tag[1]);
      write.push(tag[1]);
    }
  }

  return { read, write };
}

/** Blossom servers from a kind 10063 list (BUD-03). */
export function parseServerList(event: NostrEvent): string[] {
  return event.tags.filter((tag) => tag[0] === "server" && tag[1]).map((tag) => tag[1]);
}

function getTagValue(event: NostrEvent, name: string): string | undefined {
  return event.tags.find((tag) => tag[0] === name)?.[1];
}

/** Percent-decode a request path, tolerating malformed escapes. */
function decodePath(path: string): string {
  try {
    return decodeURIComponent(path);
  } catch {
    return path;
  }
}

export { NAMED_SITE_KIND, ROOT_SITE_KIND };
