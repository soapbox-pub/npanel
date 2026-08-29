import { createHash } from "node:crypto";
import { extname, join } from "node:path/posix";
import type { NostrEvent } from "@nostrify/nostrify";
import { type Kysely, sql } from "kysely";
import type { BlobVariants } from "./blobs.ts";
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
    // A literal, not a bound parameter: `site_events_ready_idx` is partial on
    // `status = 'ready'`, and SQLite only uses it when the query's SQL text
    // provably implies that predicate — a `?` it can't see through.
    .where("status", "=", sql.lit("ready"))
    .orderBy("created_at", "desc")
    .limit(1)
    .executeTakeFirst();
}

/**
 * How many versions a merged host resolves across.
 *
 * A bound rather than a policy: every uncached path lookup asks for its
 * candidates in each version, so the query grows with the chain, and the
 * versions past this point are the ones a `prune-blobs --keep` has most likely
 * taken away already. Deep enough that a site deploying daily keeps two months
 * of superseded asset URLs answering.
 */
export const MAX_MERGED_VERSIONS = 60;

/** The versions a host resolves paths across, and one name for that list. */
export interface VersionFiles {
  /** The versions a path is looked for in, newest first. */
  eventIds: string[];
  /**
   * One name for that list, for the caches keyed on which files are in play.
   *
   * An event id where there is only one version, so an unmerged host's cache
   * keys read exactly as they did before merging existed. Beyond that it is a
   * hash, which keeps a key the size of one event id however deep the chain is
   * — and is a name for an immutable thing, since every file list in the chain
   * is one.
   */
  filesKey: string;
}

/**
 * The versions a host resolves paths across, newest first.
 *
 * One entry — the version being served — unless the host merges deployments,
 * which stacks every earlier ready version underneath it. The order is the
 * precedence: a path is served from the newest version that has it, so a file
 * a later deploy dropped goes on answering from the deploy that published it.
 * That is what a build with content-hashed, dynamically imported chunks needs,
 * since a page already open in a browser goes on asking for the chunk names of
 * the deploy it came from.
 *
 * Nothing newer than the served version is ever included, so a pinned host
 * merges up to its pin rather than past it, and the served version always wins
 * — it heads the list outright rather than by a comparison two versions
 * sharing a `created_at` could get wrong.
 */
export async function versionFiles(db: Kysely<Database>, host: Host, active: SiteEvent): Promise<VersionFiles> {
  if (host.merge_deployments !== 1) {
    return { eventIds: [active.event_id], filesKey: active.event_id };
  }

  const older = await db
    .selectFrom("site_events")
    .select("event_id")
    .where("pubkey", "=", active.pubkey)
    .where("kind", "=", active.kind)
    .where("identifier", "=", active.identifier)
    // A literal for the same reason `newestReadyEvent` uses one, and for the
    // same index. Only ready versions: an unfinished deploy's blobs may not be
    // on disk, and merging must not undo the invariant that serving never
    // reaches one.
    .where("status", "=", sql.lit("ready"))
    .where("created_at", "<=", active.created_at)
    .where("event_id", "!=", active.event_id)
    .orderBy("created_at", "desc")
    .orderBy("event_id", "desc")
    .limit(MAX_MERGED_VERSIONS - 1)
    .execute();

  const eventIds = [active.event_id, ...older.map((row) => row.event_id)];

  return { eventIds, filesKey: createHash("sha256").update(eventIds.join("")).digest("hex") };
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
  /**
   * The blob's sizes, when a deploy has recorded them. Undefined for rows that
   * predate the sizes being recorded, in which case the caller stats the disk
   * the way it always did.
   */
  variants?: BlobVariants;
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
 * `eventIds` is the chain {@link versionFiles} built: one version, or a merged
 * host's stack of them newest first. Every step above searches the whole chain
 * and takes the newest version that has the path, so the steps themselves are
 * unchanged — an exact match in a superseded version still beats an
 * `index.html` in the served one, exactly as it would within a single version.
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
  eventIds: string[],
  requestPath: string,
  options: { spa?: boolean } = {},
): Promise<ResolvedFile | undefined> {
  const normalized = normalizeSitePath(requestPath);
  const extensionless = extname(normalized) === "";
  const htmlPath = extensionless && normalized !== "/" ? `${normalized}.html` : undefined;
  const indexPath = extensionless ? join(normalized, "index.html") : undefined;
  const shell = options.spa && DOCUMENT_EXTENSIONS.has(extname(normalized).toLowerCase());

  const candidates = [normalized, htmlPath, indexPath, "/404.html", shell ? "/index.html" : undefined].filter(
    (path): path is string => path !== undefined,
  );

  // The blob's sizes ride along on the lookup the request makes anyway — a
  // primary-key seek per candidate — so serving a recorded blob touches the
  // filesystem only to send it.
  const rows = await db
    .selectFrom("site_files")
    .leftJoin("blobs", "blobs.sha256", "site_files.sha256")
    .select([
      "site_files.path",
      "site_files.sha256",
      "site_files.event_id",
      "blobs.size",
      "blobs.br_size",
      "blobs.gzip_size",
    ])
    .where("site_files.event_id", "in", eventIds)
    .where("site_files.path", "in", candidates)
    .execute();

  const found = newestByPath(rows, eventIds);

  const exact = found.get(normalized);
  if (exact) return resolved(exact, normalized, 200);

  if (htmlPath) {
    const page = found.get(htmlPath);
    if (page) return resolved(page, htmlPath, 200);
  }

  if (indexPath) {
    const index = found.get(indexPath);
    if (index) return resolved(index, indexPath, 200);
  }

  const fallback = shell ? 200 : 404;

  const notFound = found.get("/404.html");
  if (notFound) return resolved(notFound, "/404.html", fallback);

  if (shell) {
    const index = found.get("/index.html");
    if (index) return resolved(index, "/index.html", 200);
  }

  return undefined;
}

interface ResolvedRow {
  sha256: string;
  size: number | null;
  br_size: number | null;
  gzip_size: number | null;
}

/**
 * One row per path: the one from the newest version of the chain that has it.
 *
 * A single-version chain reduces to what this always did, since no path can
 * appear twice under one `event_id`.
 */
export function newestByPath<T extends { path: string; event_id: string }>(
  rows: T[],
  eventIds: string[],
): Map<string, T> {
  const rank = new Map(eventIds.map((eventId, index) => [eventId, index]));
  // A row from outside the chain can't happen — the query asked for these
  // versions — but ranking one last is the harmless way to be sure.
  const rankOf = (row: T) => rank.get(row.event_id) ?? Number.POSITIVE_INFINITY;

  const best = new Map<string, T>();
  for (const row of rows) {
    const previous = best.get(row.path);
    if (previous && rankOf(previous) <= rankOf(row)) continue;
    best.set(row.path, row);
  }

  return best;
}

function resolved(row: ResolvedRow, path: string, status: 200 | 404): ResolvedFile {
  const variants = recordedVariants(row);
  return variants ? { path, sha256: row.sha256, status, variants } : { path, sha256: row.sha256, status };
}

/**
 * The sizes a deploy recorded for a blob, or undefined when it hasn't.
 *
 * NULL anywhere means the row predates the sizes being recorded — the blob
 * row itself may even be missing under the left join — and the caller falls
 * back to statting. -1 is a recorded answer: no such copy is kept.
 */
function recordedVariants(row: ResolvedRow): BlobVariants | undefined {
  if (row.size === null || row.br_size === null || row.gzip_size === null) return undefined;

  return {
    identity: row.size,
    br: row.br_size < 0 ? undefined : row.br_size,
    gzip: row.gzip_size < 0 ? undefined : row.gzip_size,
  };
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

/**
 * A request path as resolution sees it: percent-decoded and rooted, so it can
 * be compared against the paths `site_files` stores. Shared with the canonical
 * header, which has to ask whether a request spelled the file it was served.
 */
export function normalizeSitePath(requestPath: string): string {
  return join("/", decodePath(requestPath));
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
