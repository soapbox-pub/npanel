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
 * The whole HTTP surface, routed by `Host`: the dashboard hostname serves the
 * API and the dashboard SPA, every other hostname serves an nsite.
 */
export function createApp(deps: AppDeps): Hono {
  const { log } = deps;
  const serveDashboard = createDashboardServer({ dir: deps.dashboardDir, log });
  const serveSite = createSiteServer({ db: deps.db, blobs: deps.blobs, log });

  const app = new Hono();

  app.use("*", async (c, next) => {
    const hostname = requestHostname(c.req.raw);

    if (hostname !== deps.dashboardHost) {
      return await serveSite(c.req.raw, hostname);
    }

    await next();
  });

  app.route("/api", apiRoutes(deps));

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
 * otherwise make every site unreachable.
 */
export function requestHostname(request: Request): string {
  const forwarded = request.headers.get("x-forwarded-host")?.split(",")[0];
  const header = forwarded || request.headers.get("host") || "";

  return header.trim().toLowerCase().replace(/:\d+$/, "");
}
