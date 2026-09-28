import type { Kysely } from "kysely";
import { currentBuild } from "./builds.ts";
import type { Database } from "./db/schema.ts";
import { isOnionAddress } from "./hostname.ts";
import { encodeNaddr } from "./nsite-address.ts";
import { type ServedHost, findHost } from "./sites.ts";

/**
 * How long an entry may be believed, in milliseconds.
 *
 * A backstop rather than the invalidation mechanism: every writer inside the
 * process — the hosts API, the builder putting a build live, Tor minting an
 * onion — calls {@link ServeCache.clear} the moment serving's answer changes.
 * The TTL is for the writers the process can't see, like `prune-blobs` running
 * against the same database, and bounds how long their work goes unnoticed.
 */
const TTL_MS = 60_000;

/** The build a hostname serves, with the response fields derived from it. */
export interface CachedVersion {
  /** The build every path is resolved in. Immutable, so safe to key caches on. */
  buildId: number;
  /** The version the build serves. */
  eventId: string;
  /** The build's `Last-Modified`, formatted once instead of per response. */
  lastModified: string;
}

/** What serving needs to know about a hostname, resolved in one place. */
export interface CachedSite {
  host: ServedHost;
  /**
   * What the host serves under this name: undefined while the host is
   * disabled or nothing has finished building for it.
   */
  version: CachedVersion | undefined;
  /**
   * The host's `X-Nsite-Addr`, which is a bech32 encode of three fields that
   * only change when staff repoint the host.
   */
  naddr: string;
}

/**
 * The hostname → build lookups behind every request, cached.
 *
 * Two queries open every site request — which host a hostname names, and
 * which build that host serves — and their answers change only when staff
 * edit a host or a build goes live. Holding them keeps the serving path off
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

  /** The host this hostname names and the build it serves, if either exists. */
  async lookup(hostname: string): Promise<CachedSite | undefined> {
    const cached = this.entries.get(hostname);
    if (cached && cached.expires > Date.now()) return cached.site;

    const host = await findHost(this.db, hostname);
    if (!host) {
      // An expired entry whose host is gone would otherwise sit here forever.
      this.entries.delete(hostname);
      return undefined;
    }

    // An onion has a build of its own, because the files npanel generates for
    // it name the onion rather than the clearnet hostname.
    const kind = isOnionAddress(hostname) ? "onion" : "clearnet";
    const build = host.enabled === 1 ? await currentBuild(this.db, host.id, kind) : undefined;

    const version: CachedVersion | undefined = build
      ? {
          buildId: build.id,
          eventId: build.event_id,
          lastModified: new Date(build.last_modified * 1000).toUTCString(),
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
