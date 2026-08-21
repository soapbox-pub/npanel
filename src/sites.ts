import { extname, join } from "node:path/posix";
import type { NostrEvent } from "@nostrify/nostrify";
import type { Kysely } from "kysely";
import type { Database } from "./db/schema.ts";
import type { DeployStatus, Host, SiteEvent } from "./db/schema.ts";
import { isOnionAddress } from "./hostname.ts";
import { getManifestPaths, manifestAddress } from "./manifest.ts";
import type { NsiteAddress } from "./nsite-address.ts";

/** Stable key for a site, used to group hosts pointing at the same nsite. */
export function siteKey(address: NsiteAddress): string {
  return `${address.kind}:${address.pubkey}:${address.identifier}`;
}

/** Every distinct site referenced by an enabled host. */
export async function listConfiguredSites(db: Kysely<Database>): Promise<NsiteAddress[]> {
  const rows = await db
    .selectFrom("hosts")
    .select(["pubkey", "kind", "identifier"])
    .where("enabled", "=", 1)
    .distinct()
    .execute();

  return rows;
}

/** A host, with the onion address it also answers at if Tor has given it one. */
export type ServedHost = Host & { onion: string | null };

/**
 * Look up a host by any name it answers to: its hostname, or its onion address.
 *
 * Both in one query, because the onion address is also what the
 * `Onion-Location` header needs, and asking for it separately would put a
 * second round trip on every clearnet request. Which column to match is decided
 * by the shape of the name, which is unambiguous: `parseHostname` refuses to
 * store a `.onion` as a hostname, so the two namespaces cannot overlap.
 */
export async function findHost(db: Kysely<Database>, hostname: string): Promise<ServedHost | undefined> {
  const name = hostname.toLowerCase();

  const query = db
    .selectFrom("hosts")
    .leftJoin("onion_services", "onion_services.host_id", "hosts.id")
    .selectAll("hosts")
    .select("onion_services.address as onion");

  return await (isOnionAddress(name)
    ? query.where("onion_services.address", "=", name)
    : query.where("hosts.hostname", "=", name)
  ).executeTakeFirst();
}

/**
 * The version a host currently serves: its pinned event if it has one,
 * otherwise the newest version whose blobs are all downloaded.
 *
 * A deploy that is still fetching blobs, or that failed, is never returned —
 * the previous working version keeps serving until the new one is complete.
 */
export async function activeEvent(db: Kysely<Database>, host: Host): Promise<SiteEvent | undefined> {
  if (host.pinned_event_id) {
    return await db
      .selectFrom("site_events")
      .selectAll()
      .where("event_id", "=", host.pinned_event_id)
      .where("status", "=", "ready")
      .executeTakeFirst();
  }

  return await newestReadyEvent(db, host);
}

/** The newest fully-downloaded version of a site. */
export async function newestReadyEvent(db: Kysely<Database>, address: NsiteAddress): Promise<SiteEvent | undefined> {
  return await db
    .selectFrom("site_events")
    .selectAll()
    .where("pubkey", "=", address.pubkey)
    .where("kind", "=", address.kind)
    .where("identifier", "=", address.identifier)
    .where("status", "=", "ready")
    .orderBy("created_at", "desc")
    .limit(1)
    .executeTakeFirst();
}

/** The newest version of a site in any state. */
export async function newestEvent(db: Kysely<Database>, address: NsiteAddress): Promise<SiteEvent | undefined> {
  return await db
    .selectFrom("site_events")
    .selectAll()
    .where("pubkey", "=", address.pubkey)
    .where("kind", "=", address.kind)
    .where("identifier", "=", address.identifier)
    .orderBy("created_at", "desc")
    .limit(1)
    .executeTakeFirst();
}

export interface ResolvedFile {
  path: string;
  sha256: string;
  /** What to serve it as: a file the site really has, or its not-found page. */
  status: 200 | 404;
}

/**
 * Extensions a fallback page may stand in for.
 *
 * Everything else is an asset the document named, and an asset is worth
 * failing loudly: a stylesheet or a script that answers `200` with a page of
 * HTML doesn't render the site, it produces a parse error somewhere else
 * entirely.
 */
const DOCUMENT_EXTENSIONS = new Set(["", ".html", ".htm"]);

/**
 * Resolve a request path within a stored version: exact match, then — for an
 * extensionless path — a `.html` sibling and then `index.html` beneath it, and
 * finally the site's own `/404.html`.
 *
 * The `.html` step is what a multi-page generator needs. VitePress with
 * `cleanUrls` emits `self-hosting.html` and links to `/self-hosting`, so
 * without it every page of such a site answers `404` while sitting right there
 * in the manifest. It is tried ahead of `index.html` because that is the order
 * `try_files {path} {path}.html {path}/index.html` established, and a site
 * carrying both spellings meant the first one.
 *
 * Reads the path map from the database, so serving a file never parses a
 * manifest event.
 *
 * `spa` turns the last step into a client-side router's entry point rather
 * than a dead end. A single-page app owns paths that exist only in its own
 * routing table, so for a document request npanel hands back the site's shell
 * under a `200`: `/404.html` first, since NIP-5A names it as *the* fallback
 * path and an nsyte build publishes its shell there for exactly that reason,
 * and `/index.html` for a build that expects the `try_files` other static
 * hosts do. Asset requests are untouched, and so is every host without the
 * flag — a `404` is still the honest answer for a file a site doesn't have.
 */
export async function resolveSitePath(
  db: Kysely<Database>,
  eventId: string,
  requestPath: string,
  options: { spa?: boolean } = {},
): Promise<ResolvedFile | undefined> {
  const normalized = join("/", decodePath(requestPath));
  const extensionless = extname(normalized) === "";
  const htmlPath = extensionless && normalized !== "/" ? `${normalized}.html` : undefined;
  const indexPath = extensionless ? join(normalized, "index.html") : undefined;
  const shell = options.spa && DOCUMENT_EXTENSIONS.has(extname(normalized).toLowerCase());

  const candidates = [normalized, htmlPath, indexPath, "/404.html", shell ? "/index.html" : undefined].filter(
    (path): path is string => path !== undefined,
  );

  const rows = await db
    .selectFrom("site_files")
    .select(["path", "sha256"])
    .where("event_id", "=", eventId)
    .where("path", "in", candidates)
    .execute();

  const found = new Map(rows.map((row) => [row.path, row.sha256]));

  const exact = found.get(normalized);
  if (exact) return { path: normalized, sha256: exact, status: 200 };

  if (htmlPath) {
    const page = found.get(htmlPath);
    if (page) return { path: htmlPath, sha256: page, status: 200 };
  }

  if (indexPath) {
    const index = found.get(indexPath);
    if (index) return { path: indexPath, sha256: index, status: 200 };
  }

  const fallback = shell ? 200 : 404;

  const notFound = found.get("/404.html");
  if (notFound) return { path: "/404.html", sha256: notFound, status: fallback };

  if (shell) {
    const index = found.get("/index.html");
    if (index) return { path: "/index.html", sha256: index, status: 200 };
  }

  return undefined;
}

/**
 * Record a manifest event and its file list.
 *
 * Returns false when the event was already stored, so callers can skip
 * redeploying something they've already seen.
 */
export async function storeManifest(
  db: Kysely<Database>,
  event: NostrEvent,
  status: DeployStatus = "pending",
): Promise<boolean> {
  const existing = await db
    .selectFrom("site_events")
    .select("event_id")
    .where("event_id", "=", event.id)
    .executeTakeFirst();

  if (existing) return false;

  const address = manifestAddress(event);
  const paths = getManifestPaths(event);
  const now = Math.floor(Date.now() / 1000);

  await db.transaction().execute(async (trx) => {
    await trx
      .insertInto("site_events")
      .values({
        event_id: event.id,
        pubkey: address.pubkey,
        kind: address.kind,
        identifier: address.identifier,
        created_at: event.created_at,
        event: JSON.stringify(event),
        status,
        files_total: paths.size,
        files_ready: 0,
        error: null,
        fetched_at: now,
        ready_at: null,
      })
      .execute();

    const files = [...paths].map(([path, sha256]) => ({ event_id: event.id, path, sha256 }));
    for (const chunk of chunks(files, 200)) {
      await trx.insertInto("site_files").values(chunk).execute();
    }
  });

  return true;
}

/** Parse a stored event row back into a Nostr event. */
export function parseStoredEvent(row: SiteEvent): NostrEvent {
  return JSON.parse(row.event) as NostrEvent;
}

/** Percent-decode a request path, tolerating malformed escapes. */
function decodePath(path: string): string {
  try {
    return decodeURIComponent(path);
  } catch {
    return path;
  }
}

function* chunks<T>(items: T[], size: number): Generator<T[]> {
  for (let i = 0; i < items.length; i += size) {
    yield items.slice(i, i + size);
  }
}
