import { NPool, NRelay1 } from "@nostrify/nostrify";
import type { NostrEvent, NostrFilter } from "@nostrify/nostrify";
import { type Logger, errFields } from "./log.ts";
import { getManifestServers, parseRelayList, parseServerList } from "./manifest.ts";
import { NAMED_SITE_KIND, type NsiteAddress } from "./nsite-address.ts";

/** NIP-65 relay list. */
const RELAY_LIST_KIND = 10002;
/** BUD-03 Blossom server list. */
const SERVER_LIST_KIND = 10063;

/** How long a cached relay or server list is trusted before refetching. */
const LIST_STALE_TIME = 60 * 60 * 1000;

const DEFAULT_TIMEOUT = 10_000;

interface CachedList {
  value: string[];
  fetchedAt: number;
}

/**
 * Relay access for the sync worker.
 *
 * All relay traffic goes through one pool whose relay set is read from
 * settings on every request, so an admin editing the relay list in the
 * dashboard takes effect without a restart. Long-lived subscriptions are the
 * exception: {@link SyncWorker} restarts those itself when settings change.
 */
export class NostrService {
  private pool: NPool<NRelay1>;
  private relayLists = new Map<string, CachedList>();
  private serverLists = new Map<string, CachedList>();

  constructor(
    private readonly settings: { get(key: "relays"): string[] },
    private readonly log: Logger,
  ) {
    this.pool = new NPool({
      open: (url) => new NRelay1(url),
      reqRouter: (filters) => new Map(this.relays.map((url) => [url, filters])),
      eventRouter: () => this.relays,
    });
  }

  /** Relays configured in the dashboard. */
  get relays(): string[] {
    return this.settings.get("relays");
  }

  /** Stream events from relays until the signal aborts. */
  req(
    filters: NostrFilter[],
    opts: { signal: AbortSignal; relays?: string[] },
  ): AsyncIterable<["EVENT", string, NostrEvent] | ["EOSE", string] | ["CLOSED", string, string]> {
    return this.pool.req(filters, opts);
  }

  /** Collect events matching `filters`, giving up after `timeout` ms. */
  async query(
    filters: NostrFilter[],
    opts: { relays?: string[]; signal?: AbortSignal; timeout?: number } = {},
  ): Promise<NostrEvent[]> {
    const timeout = AbortSignal.timeout(opts.timeout ?? DEFAULT_TIMEOUT);
    const signal = opts.signal ? AbortSignal.any([opts.signal, timeout]) : timeout;

    const relays = dedupe([...(opts.relays ?? []), ...this.relays]);
    if (!relays.length) return [];

    try {
      return await this.pool.query(filters, { signal, relays });
    } catch (error) {
      this.log.warn("relay_query_failed", {
        filters: JSON.stringify(filters).slice(0, 500),
        ...errFields(error),
      });
      return [];
    }
  }

  /**
   * Relays where an author publishes, from their kind 10002 list.
   *
   * A manifest is most likely to be found on its author's write relays, so
   * these are merged into every manifest lookup.
   */
  async getAuthorRelays(pubkey: string, opts: { signal?: AbortSignal } = {}): Promise<string[]> {
    const cached = this.relayLists.get(pubkey);
    if (cached && Date.now() - cached.fetchedAt < LIST_STALE_TIME) {
      return cached.value;
    }

    const events = await this.query([{ kinds: [RELAY_LIST_KIND], authors: [pubkey], limit: 1 }], opts);
    const newest = latest(events);
    const value = newest ? parseRelayList(newest).write : [];

    this.relayLists.set(pubkey, { value, fetchedAt: Date.now() });
    this.log.debug("relay_list_fetched", { pubkey, relays: value.length });

    return value;
  }

  /** Blossom servers an author publishes to, from their kind 10063 list. */
  async getAuthorServers(pubkey: string, opts: { signal?: AbortSignal } = {}): Promise<string[]> {
    const cached = this.serverLists.get(pubkey);
    if (cached && Date.now() - cached.fetchedAt < LIST_STALE_TIME) {
      return cached.value;
    }

    const events = await this.query([{ kinds: [SERVER_LIST_KIND], authors: [pubkey], limit: 1 }], opts);
    const newest = latest(events);
    const value = newest ? parseServerList(newest) : [];

    this.serverLists.set(pubkey, { value, fetchedAt: Date.now() });
    this.log.debug("server_list_fetched", { pubkey, servers: value.length });

    return value;
  }

  /** Fetch the newest manifest event for a site, or undefined if none exists. */
  async fetchManifest(
    address: NsiteAddress,
    opts: { signal?: AbortSignal; timeout?: number } = {},
  ): Promise<NostrEvent | undefined> {
    const authorRelays = await this.getAuthorRelays(address.pubkey, opts);
    const events = await this.query([manifestFilter(address)], {
      ...opts,
      relays: authorRelays,
    });

    return latest(events);
  }

  /**
   * Blossom servers to try for a site's blobs, best first.
   *
   * Manifests usually name their own servers; the author's kind 10063 list is
   * the next best hint, and the configured fallbacks are a last resort for
   * manifests that carry neither.
   */
  async blobSources(manifest: NostrEvent, fallbacks: string[], opts: { signal?: AbortSignal } = {}): Promise<string[]> {
    const authorServers = await this.getAuthorServers(manifest.pubkey, opts);
    const servers = [...getManifestServers(manifest), ...authorServers, ...fallbacks]
      .map(normalizeServer)
      .filter((url): url is string => url !== undefined);

    return dedupe(servers);
  }

  /** Drop connections to relays that are no longer configured. */
  pruneRelays(): void {
    const configured = new Set(this.relays);
    for (const [url, relay] of this.pool.relays) {
      if (!configured.has(url)) {
        relay.close();
        this.log.debug("relay_closed", { url });
      }
    }
  }

  /** Forget cached relay and server lists, forcing a refetch. */
  clearCaches(): void {
    this.relayLists.clear();
    this.serverLists.clear();
  }

  async close(): Promise<void> {
    await this.pool.close();
  }
}

/** Filter matching a site's manifest events. */
export function manifestFilter(address: NsiteAddress, since?: number): NostrFilter {
  const filter: NostrFilter = {
    kinds: [address.kind],
    authors: [address.pubkey],
  };

  if (address.kind === NAMED_SITE_KIND) {
    filter["#d"] = [address.identifier];
  }
  if (since !== undefined) {
    filter.since = since;
  }

  return filter;
}

/** The URL a blob would live at on a Blossom server. */
export function blobUrl(server: string, sha256: string): string {
  return new URL(sha256, server).toString();
}

function normalizeServer(server: string): string | undefined {
  try {
    const url = new URL(server);
    if (url.protocol !== "https:" && url.protocol !== "http:") return undefined;
    if (!url.pathname.endsWith("/")) url.pathname += "/";
    return url.toString();
  } catch {
    return undefined;
  }
}

function latest(events: NostrEvent[]): NostrEvent | undefined {
  return events.reduce<NostrEvent | undefined>(
    (newest, event) => (!newest || event.created_at > newest.created_at ? event : newest),
    undefined,
  );
}

function dedupe(values: string[]): string[] {
  return [...new Set(values)];
}
