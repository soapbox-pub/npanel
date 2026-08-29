import type { Kysely } from "kysely";
import type { Database, SiteEvent } from "./db/schema.ts";
import { encodeNaddr } from "./nsite-address.ts";
import { type ServedHost, type VersionFiles, activeEvent, findHost, versionFiles } from "./sites.ts";

/**
 * How long an entry may be believed, in milliseconds.
 *
 * A backstop rather than the invalidation mechanism: every writer inside the
 * process — the hosts API, the sync worker marking a version ready, Tor
 * minting an onion — calls {@link ServeCache.clear} the moment serving's
 * answer changes. The TTL is for the writers the process can't see, like
 * `prune-blobs` running against the same database, and bounds how long their
 * work goes unnoticed.
 */
const TTL_MS = 60_000;

/** The version a host serves, with the response fields derived from it. */
export interface CachedVersion extends VersionFiles {
  event: SiteEvent;
  /**
   * The version's `Last-Modified`, formatted once instead of per response.
   *
   * Kept beside the event rather than on {@link CachedSite}, so that a host
   * with nothing to serve has neither and the two can't be read apart.
   */
  lastModified: string;
}

/** What serving needs to know about a hostname, resolved in one place. */
export interface CachedSite {
  host: ServedHost;
  /**
   * The version the host serves: undefined while the host is disabled or
   * nothing has finished deploying, exactly as {@link activeEvent} answers.
   */
  version: CachedVersion | undefined;
  /**
   * The host's `X-Nsite-Addr`, which is a bech32 encode of three fields that
   * only change when staff repoint the host.
   */
  naddr: string;
}

/**
 * The hostname → site lookups behind every request, cached.
 *
 * Two queries open every site request — which host a hostname names, and
 * which version that host serves — and their answers change only when staff
 * edit a host or a deploy completes. Holding them keeps the serving path off
 * the database entirely for the requests in between.
 *
 * Only hostnames that resolve to a host are kept. A miss is asked again every
 * time, so a scanner spraying random `Host` headers can't grow the map: every
 * entry corresponds to a real host row, which bounds the cache at the number
 * of configured hostnames and onions.
 */
export class ServeCache {
  private entries = new Map<string, { site: CachedSite; expires: number }>();

  constructor(private readonly db: Kysely<Database>) {}

  /** The host this hostname names and the version it serves, if either exists. */
  async lookup(hostname: string): Promise<CachedSite | undefined> {
    const cached = this.entries.get(hostname);
    if (cached && cached.expires > Date.now()) return cached.site;

    const host = await findHost(this.db, hostname);
    if (!host) {
      // An expired entry whose host is gone would otherwise sit here forever.
      this.entries.delete(hostname);
      return undefined;
    }

    const event = host.enabled === 1 ? await activeEvent(this.db, host) : undefined;
    // Which versions a path is looked for in is settled here, with the rest of
    // what a hostname resolves to, rather than per request: a merged host's
    // chain costs a query, and it changes only when a deploy or an edit has
    // already emptied this cache.
    const version: CachedVersion | undefined = event
      ? {
          event,
          ...(await versionFiles(this.db, host, event)),
          lastModified: lastModified(event.created_at, event.fetched_at),
        }
      : undefined;

    const site: CachedSite = { host, version, naddr: encodeNaddr(host) };
    this.entries.set(hostname, { site, expires: Date.now() + TTL_MS });

    return site;
  }

  /**
   * Drop everything. Host writes are rare enough that no writer needs to work
   * out which hostnames its change touched.
   */
  clear(): void {
    this.entries.clear();
  }
}

/**
 * `Last-Modified` for a version.
 *
 * A manifest's `created_at` is whatever its author put there, including dates
 * in the future or beyond what `Date` can represent, so it is capped at the
 * moment npanel actually saw the event.
 */
function lastModified(createdAt: number, fetchedAt: number): string {
  const seconds = Number.isFinite(createdAt) ? Math.min(createdAt, fetchedAt) : fetchedAt;
  return new Date(Math.max(seconds, 0) * 1000).toUTCString();
}
