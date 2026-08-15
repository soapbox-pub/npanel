import type { Kysely } from "kysely";
import type { Database } from "./db/schema.ts";
import { wwwApex } from "./hostname.ts";
import { type Logger, errFields } from "./log.ts";
import { RateLimiter } from "./rate-limit.ts";
import type { SettingsService } from "./settings.ts";

/** The only path this listener answers. */
const TLS_CHECK_PATH = "/tls-check";

/** The window both approval limits are expressed over. */
const HOUR = 60 * 60 * 1000;

const TEXT = { "content-type": "text/plain; charset=utf-8" };

/**
 * The slice of Bun's server API npanel uses.
 *
 * Declared structurally, the way `src/db/sqlite.ts` declares its SQLite
 * binding, rather than by adding `@types/bun`: that would put every Bun global
 * in scope, and `tsc` is what currently stops `Bun.file` and friends from
 * creeping into code that has to keep running on Node and Deno. Serving HTTP is
 * the one place where the runtime's own API is the right answer, so it is the
 * one thing declared here.
 */
declare const Bun: {
  serve(options: {
    port: number;
    hostname: string;
    fetch(request: Request): Promise<Response>;
    error(error: Error): Response;
  }): TlsCheckServer;
};

/** A running listener: enough of Bun's `Server` to find it and shut it down. */
export interface TlsCheckServer {
  /** The bound port — what a requested port of `0` resolved to. */
  readonly port: number;
  stop(closeActiveConnections?: boolean): Promise<void>;
}

export interface TlsCheckDeps {
  db: Kysely<Database>;
  settings: SettingsService;
  log: Logger;
  /** Never refused a certificate, whatever the budget looks like. */
  dashboardHost: string;
}

export interface TlsCheckOptions {
  /** `0` binds any free port, which is how the tests avoid collisions. */
  port: number;
  /** Address to bind. The same one the public server uses. */
  address: string;
}

/**
 * Certificate authorization for Caddy's `on_demand_tls ask`, on a port of its
 * own.
 *
 * This cannot live on the public server. Caddy asks with a plain GET to the
 * address it proxies to, so the request carries no hostname npanel could route
 * on and the endpoint would have to answer on every hostname it serves —
 * including every nsite. That would publish it to the internet, and anyone
 * could then drive approvals for a configured host until the hourly budget
 * below was spent, denying certificates to the sites that deserve them. It
 * would also shadow that path on every site npanel hosts.
 *
 * A second listener is the whole fix: it binds beside the public one, nothing
 * routes to it, and only the proxy can reach it. Binding is synchronous and
 * throws on a port already in use, so a misconfiguration fails startup rather
 * than the first certificate.
 */
export function startTlsCheckServer(deps: TlsCheckDeps, opts: TlsCheckOptions): TlsCheckServer {
  const { db, settings, log } = deps;

  const perHost = new RateLimiter();
  const total = new RateLimiter({ maxKeys: 1 });

  /**
   * Whether npanel answers for `domain` at all.
   *
   * A `www` name npanel only redirects still needs a certificate of its own:
   * the redirect is an HTTPS response like any other, and without one the
   * browser refuses the connection before it can be told where to go.
   */
  async function served(domain: string): Promise<boolean> {
    const configured = await db
      .selectFrom("hosts")
      .select("id")
      .where("hostname", "=", domain)
      .where("enabled", "=", 1)
      .executeTakeFirst();

    if (configured) return true;

    const apex = wwwApex(domain);
    if (!apex) return false;

    const redirects = await db
      .selectFrom("hosts")
      .select("id")
      .where("hostname", "=", apex)
      .where("enabled", "=", 1)
      .where("redirect_www", "=", 1)
      .executeTakeFirst();

    return redirects !== undefined;
  }

  /**
   * Whether `domain` may have a certificate.
   *
   * Reveals only whether a hostname is configured here, which DNS already
   * tells you — but see the note above about who gets to ask.
   */
  async function authorize(domain: string): Promise<Response> {
    // Exempt: locking admins out of the dashboard because something else
    // exhausted the budget would be worse than whatever caused it.
    if (domain === deps.dashboardHost) {
      return new Response("ok", { headers: TEXT });
    }

    if (!(await served(domain))) {
      log.debug("tls_check_denied", { domain });
      return new Response("unknown host", { status: 404, headers: TEXT });
    }

    const withinHost = perHost.take(domain, {
      capacity: settings.get("tls_check_limit_per_host"),
      interval: HOUR,
    });
    // Short-circuit deliberately: a host that is already over its own limit
    // must not go on draining the budget the other hosts share.
    const withinTotal =
      withinHost &&
      total.take("", {
        capacity: settings.get("tls_check_limit_total"),
        interval: HOUR,
      });

    if (!withinTotal) {
      log.warn("tls_check_throttled", { domain, scope: withinHost ? "total" : "host" });
      return new Response("rate limited", { status: 429, headers: { ...TEXT, "retry-after": "60" } });
    }

    return new Response("ok", { headers: TEXT });
  }

  return Bun.serve({
    port: opts.port,
    hostname: opts.address,

    async fetch(request) {
      if (request.method !== "GET" && request.method !== "HEAD") {
        return new Response("method not allowed", { status: 405, headers: { ...TEXT, allow: "GET, HEAD" } });
      }

      const url = new URL(request.url);

      if (url.pathname !== TLS_CHECK_PATH) {
        return new Response("not found", { status: 404, headers: TEXT });
      }

      return await authorize((url.searchParams.get("domain") ?? "").trim().toLowerCase());
    },

    // Caddy treats anything but a 2xx as a refusal, so a failure here costs a
    // certificate rather than a page. It has to be visible in the log.
    error(error) {
      log.error("tls_check_failed", errFields(error));
      return new Response("internal error", { status: 500, headers: TEXT });
    },
  });
}
