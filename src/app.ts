import { Hono } from "hono";
import { apiRoutes } from "./api/index.ts";
import type { ApiDeps } from "./api/index.ts";
import { createDashboardServer } from "./dashboard.ts";
import { errFields } from "./log.ts";
import { createSiteServer } from "./serve-site.ts";

export interface AppDeps extends ApiDeps {
  /** Directory holding the built dashboard. */
  dashboardDir: string;
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
  const { log } = deps;
  const serveDashboard = createDashboardServer({
    dir: deps.dashboardDir,
    dashboardHost: deps.dashboardHost,
    log,
  });
  const serveSite = createSiteServer({ db: deps.db, blobs: deps.blobs, settings: deps.settings, log });

  const app = new Hono();

  app.use("*", async (c, next) => {
    const hostname = requestHostname(c.req.raw);

    if (hostname !== deps.dashboardHost) {
      return await serveSite(c.req.raw, hostname);
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
 * The hostname the client asked for.
 *
 * `X-Forwarded-Host` wins because a reverse proxy that rewrites `Host` would
 * otherwise make every site unreachable. This trusts whatever proxy sits in
 * front of npanel to overwrite a client-supplied value — Caddy and nginx both
 * do — so npanel must not be exposed to the internet directly.
 *
 * The trailing dot of a fully-qualified name is dropped, since `example.com.`
 * and `example.com` are the same host.
 */
export function requestHostname(request: Request): string {
  const forwarded = request.headers.get("x-forwarded-host")?.split(",")[0];
  const header = forwarded || request.headers.get("host") || "";

  return header.trim().toLowerCase().replace(/:\d+$/, "").replace(/\.$/, "");
}
