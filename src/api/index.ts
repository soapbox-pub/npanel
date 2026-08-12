import { Hono } from "hono";
import { bodyLimit } from "hono/body-limit";
import { cors } from "hono/cors";
import { nip19 } from "nostr-tools";
import { type AuthUser, nip98Auth, requireAdmin } from "../auth.ts";
import { DEFAULT_SETTINGS, SETTING_KEYS, type SettingKey } from "../settings.ts";
import { type ApiDeps, type ApiEnv, audit } from "./context.ts";
import { hostRoutes } from "./hosts.ts";
import { userRoutes } from "./users.ts";

export type { ApiDeps } from "./context.ts";

/** Largest request body the API accepts, in bytes. */
const MAX_BODY_SIZE = 1024 * 1024;

/**
 * The dashboard's JSON API.
 *
 * Every route except `/api/tls-check` requires NIP-98 auth from a pubkey that
 * exists in the `users` table.
 */
export function apiRoutes(deps: ApiDeps): Hono<ApiEnv> {
  const { db, settings, log } = deps;
  const app = new Hono<ApiEnv>();

  app.use("*", cors());

  // Bodies are read before the caller is known — NIP-98 hashes the payload to
  // check it against the signature — so anyone able to sign an event could
  // otherwise make npanel buffer an arbitrarily large request. Every endpoint
  // here takes small JSON.
  app.use(
    "*",
    bodyLimit({
      maxSize: MAX_BODY_SIZE,
      onError: (c) => c.json({ error: "Request body is too large." }, 413),
    }),
  );

  /**
   * Certificate authorization for Caddy's `on_demand_tls ask`.
   *
   * Unauthenticated by necessity — Caddy calls it while deciding whether to
   * request a certificate — and it only ever reveals whether a hostname is
   * configured, which DNS already tells you.
   */
  app.get("/tls-check", async (c) => {
    const domain = (c.req.query("domain") ?? "").toLowerCase();

    if (domain === deps.dashboardHost) {
      return c.text("ok");
    }

    const host = await db
      .selectFrom("hosts")
      .select("id")
      .where("hostname", "=", domain)
      .where("enabled", "=", 1)
      .executeTakeFirst();

    if (!host) {
      log.debug("tls_check_denied", { domain });
      return c.text("unknown host", 404);
    }

    return c.text("ok");
  });

  app.use(
    "*",
    nip98Auth({
      dashboardHost: deps.dashboardHost,
      findUser: async (pubkey): Promise<AuthUser | undefined> => {
        const user = await db
          .selectFrom("users")
          .select(["pubkey", "role"])
          .where("pubkey", "=", pubkey)
          .executeTakeFirst();

        return user ?? undefined;
      },
    }),
  );

  app.get("/me", (c) => {
    const user = c.get("user");
    return c.json({
      pubkey: user.pubkey,
      npub: nip19.npubEncode(user.pubkey),
      role: user.role,
    });
  });

  app.route("/hosts", hostRoutes(deps));
  app.route("/users", userRoutes(deps));

  app.get("/settings", requireAdmin(), (c) => {
    return c.json({ settings: settings.all(), defaults: DEFAULT_SETTINGS });
  });

  app.put("/settings", requireAdmin(), async (c) => {
    const user = c.get("user");
    const body = (await c.req.json().catch(() => null)) as Record<string, unknown> | null;
    if (!body || typeof body !== "object") {
      return c.json({ error: "Expected a JSON body." }, 400);
    }

    const unknown = Object.keys(body).filter((key) => !SETTING_KEYS.includes(key as SettingKey));
    if (unknown.length) {
      return c.json({ error: `Unknown settings: ${unknown.join(", ")}` }, 400);
    }

    try {
      const changed = await settings.update(body, user.pubkey);
      return c.json({ settings: settings.all(), changed });
    } catch (error) {
      return c.json({ error: error instanceof Error ? error.message : String(error) }, 400);
    }
  });

  app.get("/audit-log", requireAdmin(), async (c) => {
    const limit = clamp(Number(c.req.query("limit") ?? 100), 1, 500);
    const offset = clamp(Number(c.req.query("offset") ?? 0), 0, 1_000_000);

    const entries = await db
      .selectFrom("audit_log")
      .selectAll()
      .orderBy("id", "desc")
      .limit(limit)
      .offset(offset)
      .execute();

    return c.json({
      entries: entries.map((entry) => ({
        id: entry.id,
        actor_pubkey: entry.actor_pubkey,
        action: entry.action,
        target: entry.target,
        old_value: entry.old_value ? JSON.parse(entry.old_value) : null,
        new_value: entry.new_value ? JSON.parse(entry.new_value) : null,
        created_at: entry.created_at,
      })),
    });
  });

  /** Server-side status, handy for a dashboard footer and for debugging. */
  app.get("/status", async (c) => {
    const [hosts, sites, blobs] = await Promise.all([
      db.selectFrom("hosts").select("id").execute(),
      db.selectFrom("site_events").select("event_id").where("status", "=", "ready").execute(),
      db.selectFrom("blobs").select(["sha256", "size"]).execute(),
    ]);

    return c.json({
      hosts: hosts.length,
      ready_versions: sites.length,
      blobs: blobs.length,
      blob_bytes: blobs.reduce((total, blob) => total + blob.size, 0),
      relays: settings.get("relays"),
    });
  });

  return app;
}

export { audit };

function clamp(value: number, min: number, max: number): number {
  if (!Number.isFinite(value)) return min;
  return Math.min(Math.max(Math.trunc(value), min), max);
}
