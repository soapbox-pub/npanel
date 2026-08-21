import { Hono } from "hono";
import { apiRoutes } from "./api/index.ts";
import type { ApiDeps } from "./api/index.ts";
import { createDashboardServer } from "./dashboard.ts";
import { requestHostname } from "./hostname.ts";
import { errFields } from "./log.ts";
import type { SiteServer } from "./serve-site.ts";

export interface AppDeps extends ApiDeps {
  /** Directory holding the built dashboard. */
  dashboardDir: string;
  /**
   * Serves nsites. Built outside so that the Tor listener, which reaches sites
   * by a different route, serves them through exactly the same code.
   */
  serveSite: SiteServer;
}

/**
 * The whole public HTTP surface, routed by `Host`: the dashboard hostname
 * serves the API and the dashboard SPA, every other hostname serves an nsite.
 *
 * Nothing here is exempt from that rule. Certificate authorization for the
 * proxy needs to answer requests that carry no hostname to route on, so it
 * listens on its own port instead — see {@link file://./tls-check.ts}.
 */
export function createApp(deps: AppDeps): Hono {
  const { log, serveSite } = deps;
  const serveDashboard = createDashboardServer({
    dir: deps.dashboardDir,
    dashboardHost: deps.dashboardHost,
    log,
  });

  const app = new Hono();

  app.use("*", async (c, next) => {
    const hostname = requestHostname(c.req.raw);

    if (hostname !== deps.dashboardHost) {
      return await serveSite(c.req.raw, { hostname, scheme: requestScheme(c.req.raw) });
    }

    await next();
  });

  // The dashboard and API are never meant to be framed, and the SPA's own CSP
  // is a <meta> tag, which cannot express frame-ancestors.
  app.use("*", async (c, next) => {
    await next();
    c.res.headers.set("content-security-policy", "frame-ancestors 'none'");
    c.res.headers.set("x-frame-options", "DENY");
    c.res.headers.set("x-content-type-options", "nosniff");
    c.res.headers.set("referrer-policy", "no-referrer");
  });

  app.route("/api", apiRoutes(deps));

  // Unmatched API paths must not fall through to the SPA: an API client asking
  // for a route that doesn't exist should get JSON, not an HTML page.
  app.all("/api/*", (c) => c.json({ error: "No such API route." }, 404));

  app.all("*", (c) => serveDashboard(c.req.raw));

  app.onError((error, c) => {
    log.error("request_failed", { path: c.req.path, ...errFields(error) });
    return c.json({ error: "Internal server error." }, 500);
  });

  return app;
}

/**
 * The scheme the client used to reach the proxy.
 *
 * npanel is always behind a proxy that terminates TLS, so the URL this process
 * sees says `http://` even when the request that reached the proxy was
 * `https://` — building a redirect from the local request's scheme would
 * downgrade every one of them. `X-Forwarded-Proto` is the proxy's word for it,
 * and `https` is the assumption when the proxy didn't say, since a plain-HTTP
 * deployment is the unusual one and the wrong guess there costs a redirect
 * rather than a downgrade.
 *
 * This is believed where `X-Forwarded-Host` is not, and the difference is what
 * a lie buys: a proxy that doesn't overwrite `X-Forwarded-Host` lets a visitor
 * to any site ask for the dashboard hostname instead, while the worst a forged
 * scheme achieves is a redirect to the one the visitor was already using.
 *
 * Even so, only this listener asks. A request that arrived over Tor came
 * through no proxy at all, so {@link file://./tor.ts} states its scheme rather
 * than reading a header its visitor wrote.
 */
export function requestScheme(request: Request): "http" | "https" {
  const forwarded = request.headers.get("x-forwarded-proto")?.split(",")[0]?.trim().toLowerCase();
  if (forwarded === "http" || forwarded === "https") return forwarded;

  return "https";
}
