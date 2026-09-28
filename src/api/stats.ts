import { Hono } from "hono";
import type { WorkerResponse } from "../analytics/protocol.ts";
import { type ApiDeps, type ApiEnv, canAccessHost, loadHost } from "./context.ts";

/**
 * A site's analytics, as Plausible's dashboard API serves them.
 *
 * The three endpoints Plausible's own dashboard reads — `query`,
 * `current-visitors` and `suggestions` — under the host they describe, with
 * the same bodies in and out. Plausible's separate site and user management
 * has no counterpart: whoever may manage a site here may read its numbers,
 * which is the grant npanel already checks.
 *
 * The main thread only checks who is asking. The question goes to the
 * analytics thread whole and its answer comes back as a finished body, so a
 * slow report never occupies the thread serving sites.
 */
export function statsRoutes(deps: ApiDeps): Hono<ApiEnv> {
  const { db, analytics } = deps;
  const app = new Hono<ApiEnv>();

  app.use("/:hostname/stats/*", async (c, next) => {
    const host = await loadHost(db, c.req.param("hostname") ?? "");
    if (!host) return c.json({ error: "Host not found." }, 404);
    if (!(await canAccessHost(db, c.get("user"), host.id))) {
      return c.json({ error: "You don't have access to this host." }, 403);
    }

    c.set("host", { id: host.id, hostname: host.hostname });
    await next();
  });

  app.post("/:hostname/stats/query", async (c) => {
    const body = await c.req.json().catch(() => undefined);
    if (body === undefined) return c.json({ error: "Expected a JSON body describing the query." }, 400);

    return relay(await analytics.query(c.get("host"), body));
  });

  app.get("/:hostname/stats/current-visitors", async (c) => relay(await analytics.currentVisitors(c.get("host"))));

  app.get("/:hostname/stats/suggestions/:filter", async (c) =>
    relay(await analytics.suggestions(c.get("host"), c.req.param("filter"), c.req.query())),
  );

  return app;
}

function relay(response: WorkerResponse): Response {
  return new Response(response.body, { status: response.status, headers: response.headers });
}
