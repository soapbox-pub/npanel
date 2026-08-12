import { extname, join } from "node:path/posix";
import type { NostrEvent } from "@nostrify/nostrify";
import type { Kysely } from "kysely";
import type { Database } from "./db/schema.ts";
import type { DeployStatus, Host, SiteEvent } from "./db/schema.ts";
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

/** Look up a host by its hostname. */
export async function findHost(db: Kysely<Database>, hostname: string): Promise<Host | undefined> {
  return await db.selectFrom("hosts").selectAll().where("hostname", "=", hostname.toLowerCase()).executeTakeFirst();
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
  is404: boolean;
}

/**
 * Resolve a request path within a stored version.
 *
 * Mirrors {@link resolveManifestPath}, but reads the path map from the
 * database so serving a file never parses a manifest event.
 */
export async function resolveSitePath(
  db: Kysely<Database>,
  eventId: string,
  requestPath: string,
): Promise<ResolvedFile | undefined> {
  const normalized = join("/", decodePath(requestPath));
  const indexPath = extname(normalized) === "" ? join(normalized, "index.html") : undefined;
  const candidates = [normalized, indexPath, "/404.html"].filter((path): path is string => path !== undefined);

  const rows = await db
    .selectFrom("site_files")
    .select(["path", "sha256"])
    .where("event_id", "=", eventId)
    .where("path", "in", candidates)
    .execute();

  const found = new Map(rows.map((row) => [row.path, row.sha256]));

  const exact = found.get(normalized);
  if (exact) return { path: normalized, sha256: exact, is404: false };

  if (indexPath) {
    const index = found.get(indexPath);
    if (index) return { path: indexPath, sha256: index, is404: false };
  }

  const notFound = found.get("/404.html");
  if (notFound) return { path: "/404.html", sha256: notFound, is404: true };

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
