import type { Kysely } from "kysely";
import type { Database } from "./db/schema.ts";
import type { Deployments } from "./deploy/deployer.ts";
import { isOnionAddress, requestHostname } from "./hostname.ts";
import { type Logger, errFields } from "./log.ts";
import type { ServeCache } from "./serve-cache.ts";
import type { SiteServer } from "./serve-site.ts";
import type { SettingsService } from "./settings.ts";
import { type OnionKey, TorControl } from "./tor-control.ts";

const TEXT = { "content-type": "text/plain; charset=utf-8" };

/** First and longest wait between attempts to reach Tor, in milliseconds. */
const RETRY_MIN = 1_000;
const RETRY_MAX = 30_000;

/**
 * The slice of Bun's server API this file uses.
 *
 * Declared structurally for the same reason `src/tls-check.ts` declares it:
 * pulling in `@types/bun` would put every Bun global in scope, and `tsc`
 * refusing them is what keeps the rest of the server portable.
 */
declare const Bun: {
  serve(options: {
    port: number;
    hostname: string;
    fetch(request: Request): Promise<Response>;
    error(error: Error): Response;
  }): OnionServer;
};

interface OnionServer {
  readonly port: number;
  stop(closeActiveConnections?: boolean): Promise<void>;
}

export interface TorServiceDeps {
  db: Kysely<Database>;
  settings: SettingsService;
  log: Logger;
  /** The same site server the public listener uses. */
  serveSite: SiteServer;
  /** Cleared when an onion is minted, so serving learns the new address. */
  serveCache: ServeCache;
  /** Told when an onion is minted, since the onion is served from a build of its own. */
  deployments: Deployments;
  /** First wait before retrying an unreachable Tor. Tests set it to zero. */
  retryBackoff?: number;
}

/** What the dashboard shows on the Settings page. */
export interface TorStatus {
  enabled: boolean;
  connected: boolean;
  /** How many onion services are published right now. */
  services: number;
  /** Why Tor is unreachable, as a sentence, or empty when it isn't. */
  error: string;
}

/** A host that should have an onion, with the key it already has, if any. */
interface Wanted {
  id: number;
  hostname: string;
  address: string | null;
  secretKey: string | null;
}

/**
 * Publishes every enabled site as a Tor onion service.
 *
 * npanel drives a Tor daemon over its control port rather than writing
 * `torrc`: `ADD_ONION` registers a service immediately, with no restart and no
 * configuration file shared between the two processes. Tor generates the key,
 * npanel keeps it, and that is what makes an address survive a restart — Tor
 * stores nothing, so every service is re-registered from the database each time
 * npanel connects.
 *
 * Sites are reconciled rather than tracked. Every trigger — connecting, a host
 * being added or removed, a setting changing — asks the same question of the
 * database and makes Tor match the answer. Nothing has to be remembered between
 * two of them, so a reconcile interrupted halfway is repaired by the next one
 * rather than leaving a site with an address nobody serves.
 */
export class TorService {
  private readonly db: Kysely<Database>;
  private readonly settings: SettingsService;
  private readonly log: Logger;
  private readonly serveSite: SiteServer;
  private readonly serveCache: ServeCache;
  private readonly deployments: Deployments;
  private readonly retryMin: number;

  private server: OnionServer | undefined;
  private control: TorControl | undefined;
  private unsubscribe: (() => void) | undefined;
  private retry: ReturnType<typeof setTimeout> | undefined;
  private retryDelay: number;
  private stopped = false;
  private lastError = "";

  /** Onion services this control connection has published, by host id. */
  private readonly live = new Map<number, OnionKey>();

  /** Serializes reconciles, so two triggers cannot both mint a key for one host. */
  private queue: Promise<void> = Promise.resolve();

  constructor(deps: TorServiceDeps) {
    this.db = deps.db;
    this.settings = deps.settings;
    this.log = deps.log;
    this.serveSite = deps.serveSite;
    this.serveCache = deps.serveCache;
    this.deployments = deps.deployments;
    this.retryMin = deps.retryBackoff ?? RETRY_MIN;
    this.retryDelay = this.retryMin;
  }

  /**
   * Connect, publish, and keep doing so.
   *
   * Resolves once Tor has been reached and every site reconciled, which is what
   * tests wait on. Callers in production deliberately do not wait: a Tor that
   * accepts connections but never answers would otherwise hold up every other
   * listener, and there is nothing to serve over Tor until this finishes
   * anyway.
   */
  async start(): Promise<void> {
    this.unsubscribe = this.settings.onChange((keys) => {
      if (!keys.some((key) => key.startsWith("tor_"))) return;

      // Reconnecting on any of the three is deliberate. Only `tor_enabled`
      // strictly needs it, but a changed address or password is a different
      // Tor, and comparing them to decide would be more code than reconnecting
      // to the one that is configured now.
      void this.apply();
    });

    await this.apply();
  }

  async stop(): Promise<void> {
    this.stopped = true;
    this.unsubscribe?.();
    this.unsubscribe = undefined;

    await this.shutDown();
  }

  /**
   * Re-check which sites should have an onion. Safe to call at any time.
   *
   * Returns when Tor has been made to match, which callers handling a request
   * do not wait for — publishing an onion takes longer than answering the
   * `POST` that created the site should.
   */
  hostsChanged(): Promise<void> {
    return this.enqueue();
  }

  status(): TorStatus {
    return {
      enabled: this.settings.get("tor_enabled"),
      connected: this.control !== undefined,
      services: this.live.size,
      error: this.lastError,
    };
  }

  /** The loopback port Tor forwards onion traffic to. Bound only while enabled. */
  get port(): number | undefined {
    return this.server?.port;
  }

  private async apply(): Promise<void> {
    if (this.stopped) return;

    await this.shutDown();

    if (!this.settings.get("tor_enabled")) {
      this.log.info("tor_disabled", {});
      return;
    }

    this.retryDelay = this.retryMin;
    await this.connect();
  }

  private async connect(): Promise<void> {
    if (this.stopped || !this.settings.get("tor_enabled")) return;

    const url = this.settings.get("tor_control_url");

    let control: TorControl;
    try {
      control = await TorControl.connect({ url, password: this.settings.get("tor_control_password") });
    } catch (error) {
      this.fail(error);
      return;
    }

    // Bound only once Tor is known to be reachable, so an instance with Tor
    // switched off holds no port at all.
    this.listen();

    this.control = control;
    this.lastError = "";
    this.retryDelay = this.retryMin;

    control.once("close", (error: Error) => {
      if (this.control !== control) return;
      this.control = undefined;
      // Every service went with the connection, so none of them are published
      // any more. The next reconcile re-adds them from their stored keys.
      this.live.clear();
      this.fail(error);
    });

    this.log.info("tor_connected", { control_url: url, port: this.server?.port });
    await this.enqueue();
  }

  private fail(error: unknown): void {
    this.lastError = error instanceof Error ? error.message : String(error);

    if (this.stopped || !this.settings.get("tor_enabled")) return;

    this.log.warn("tor_unavailable", { retry_in: Math.round(this.retryDelay / 1000), ...errFields(error) });

    clearTimeout(this.retry);
    this.retry = setTimeout(() => void this.connect(), this.retryDelay);
    this.retryDelay = Math.min(this.retryDelay * 2, RETRY_MAX);
  }

  private enqueue(): Promise<void> {
    this.queue = this.queue.then(() => this.reconcile());
    return this.queue;
  }

  /**
   * Make Tor's published services match the enabled hosts.
   *
   * Never throws: it runs both from a caller that waits and from one that only
   * fires it off, and a rejection on the queue would leave the second kind as
   * an unhandled rejection while breaking the chain for everyone after it.
   */
  private async reconcile(): Promise<void> {
    try {
      await this.publishAll();
    } catch (error) {
      this.log.error("tor_reconcile_failed", errFields(error));
    }
  }

  private async publishAll(): Promise<void> {
    const control = this.control;
    const port = this.server?.port;
    if (!control || port === undefined) return;

    const wanted = await this.db
      .selectFrom("hosts")
      .leftJoin("onion_services", "onion_services.host_id", "hosts.id")
      .select([
        "hosts.id as id",
        "hosts.hostname as hostname",
        "onion_services.address as address",
        "onion_services.secret_key as secretKey",
      ])
      .where("hosts.enabled", "=", 1)
      .execute();

    const keep = new Set(wanted.map((host) => host.id));

    for (const [hostId, key] of [...this.live]) {
      if (keep.has(hostId)) continue;

      try {
        await control.delOnion(key.address);
      } catch (error) {
        // Whatever the reason, this connection is no longer publishing it as
        // far as npanel is concerned; a stale service is cleared by the
        // reconnect that follows any real control failure.
        this.log.warn("tor_onion_unpublish_failed", { onion: key.address, ...errFields(error) });
      }
      this.live.delete(hostId);
      this.log.info("tor_onion_unpublished", { onion: key.address });
    }

    for (const host of wanted) {
      if (this.live.has(host.id)) continue;

      try {
        await this.publish(control, port, host);
      } catch (error) {
        // One site's key being unusable is not a reason to leave the rest
        // unpublished, so the loop carries on and the next reconcile retries.
        this.log.error("tor_onion_failed", { hostname: host.hostname, ...errFields(error) });
      }
    }
  }

  private async publish(control: TorControl, port: number, host: Wanted): Promise<void> {
    const existing = host.address && host.secretKey ? { address: host.address, secretKey: host.secretKey } : undefined;
    const key = await control.addOnion(port, existing);

    if (!existing) {
      await this.db
        .insertInto("onion_services")
        .values({
          host_id: host.id,
          address: key.address,
          secret_key: key.secretKey,
          created_at: Math.floor(Date.now() / 1000),
        })
        .execute();

      // The lookup for this host now answers with the onion address too, and
      // the onion needs a build naming it before it serves anything.
      this.serveCache.clear();
      this.deployments.rebuild([host.id]);
    }

    this.live.set(host.id, key);
    this.log.info("tor_onion_published", { hostname: host.hostname, onion: key.address, new: !existing });
  }

  /**
   * Bind the listener Tor forwards to.
   *
   * On loopback and on a port the kernel picks, because npanel is the one
   * telling Tor where to send traffic — nothing else ever needs to know the
   * number, so there is nothing to configure and nothing to collide with.
   */
  private listen(): void {
    if (this.server) return;

    this.server = Bun.serve({
      port: 0,
      hostname: "127.0.0.1",
      fetch: (request) => this.handle(request),
      error: (error) => {
        this.log.error("tor_request_failed", errFields(error));
        return new Response("Something went wrong.", { status: 500, headers: TEXT });
      },
    });
  }

  /**
   * Serve one request that arrived over Tor.
   *
   * Tor hands a visitor's bytes on unaltered, so every header here is the
   * visitor's to choose and nothing about the request may be taken on trust.
   * That is why this listener exists rather than the onions pointing at the
   * public one: onion addresses are the only hostnames it will answer for, so
   * the dashboard and the JSON API cannot be reached through one however a
   * request dresses itself up.
   *
   * `http` is the truthful scheme on this path — there is no proxy and no TLS
   * anywhere on it. Tor authenticates and encrypts the connection itself,
   * which is what the scheme would otherwise be claiming.
   */
  private async handle(request: Request): Promise<Response> {
    const hostname = requestHostname(request);

    if (!isOnionAddress(hostname)) {
      this.log.debug("tor_unknown_host", { hostname });
      return new Response("Not found.", { status: 404, headers: TEXT });
    }

    return await this.serveSite(request, { hostname, scheme: "http" });
  }

  private async shutDown(): Promise<void> {
    clearTimeout(this.retry);
    this.retry = undefined;

    // Closing the control connection unregisters every service with it, since
    // none of them were detached.
    this.control?.removeAllListeners("close");
    this.control?.close();
    this.control = undefined;
    this.live.clear();
    this.lastError = "";

    const server = this.server;
    this.server = undefined;
    await server?.stop(true);
  }
}
