import { Hono } from "hono";
import { nip19 } from "nostr-tools";
import { z } from "zod";
import { requireAdmin } from "../auth.ts";
import { BUNKER_UNAVAILABLE } from "../bunker/host.ts";
import type { Bunker } from "../db/schema.ts";
import { type ApiDeps, type ApiEnv, audit, canAccessBunker } from "./context.ts";

const createSchema = z.object({
  name: z.string(),
  /** Defaults to a slug made from the name. */
  slug: z.string().optional(),
});

const updateSchema = z.object({
  name: z.string().optional(),
  enabled: z.boolean().optional(),
});

const pairSchema = z.object({
  /** A `bunker://` URI to consume, or absent to have npanel mint an invitation. */
  uri: z.string().optional(),
});

const grantsSchema = z.object({
  pubkeys: z.array(z.string()),
});

/**
 * Routes for the NIP-46 gateway.
 *
 * Every one of them talks to another thread, and can therefore fail in ways
 * the rest of the API cannot: the gateway may be restarting, or too slow, or
 * switched off entirely for want of a `SECRET_KEY`. All three arrive here as
 * an error with something to do about it in it.
 */
export function bunkerRoutes(deps: ApiDeps): Hono<ApiEnv> {
  const { db, bunkers, log } = deps;
  const app = new Hono<ApiEnv>();

  // Bunkers hold credentials to someone's real identity, so a deployment with
  // nowhere safe to put them does not get half a feature.
  app.use("*", async (c, next) => {
    if (!bunkers.available) {
      return c.json({ error: BUNKER_UNAVAILABLE }, 503);
    }
    await next();
  });

  app.get("/", async (c) => {
    const user = c.get("user");

    let query = db.selectFrom("bunkers").selectAll().orderBy("name");
    if (user.role !== "admin") {
      query = query.where("id", "in", (eb) =>
        eb.selectFrom("bunker_grants").select("bunker_id").where("user_pubkey", "=", user.pubkey),
      );
    }

    return c.json({ bunkers: (await query.execute()).map(serializeBunker) });
  });

  app.post("/", async (c) => {
    const user = c.get("user");
    const body = createSchema.safeParse(await c.req.json().catch(() => null));
    if (!body.success) {
      return c.json({ error: "Expected a JSON body with a name." }, 400);
    }

    const name = body.data.name.trim();
    if (!name) {
      return c.json({ error: "A bunker needs a name." }, 400);
    }

    const slug = slugify(body.data.slug ?? name);
    if (!slug) {
      return c.json({ error: "That name has no letters or digits in it to make a URL from." }, 400);
    }

    const clash = await db.selectFrom("bunkers").select("id").where("slug", "=", slug).executeTakeFirst();
    if (clash) {
      return c.json({ error: `There is already a bunker at /${slug}.` }, 409);
    }

    const now = Math.floor(Date.now() / 1000);
    const inserted = await db
      .insertInto("bunkers")
      .values({
        slug,
        name,
        user_pubkey: null,
        upstream_pubkey: null,
        upstream_secret: null,
        upstream_relays: "[]",
        key_fingerprint: null,
        status: "pending",
        error: null,
        last_ok_at: null,
        enabled: 1,
        created_by: user.pubkey,
        created_at: now,
        updated_at: now,
      })
      .returning("id")
      .executeTakeFirstOrThrow();

    // Whoever adds a bunker can pair it, so staff aren't locked out of their
    // own work — the same rule hosts follow.
    if (user.role !== "admin") {
      await db
        .insertInto("bunker_grants")
        .values({ user_pubkey: user.pubkey, bunker_id: inserted.id, created_at: now })
        .execute();
    }

    await audit(db, user.pubkey, "bunker.create", slug, undefined, { slug, name });
    log.info("bunker_created", { bunker: slug, actor: user.pubkey });

    const bunker = await db.selectFrom("bunkers").selectAll().where("id", "=", inserted.id).executeTakeFirstOrThrow();

    return c.json({ bunker: serializeBunker(bunker) }, 201);
  });

  app.get("/:slug", async (c) => {
    const bunker = await load(c.req.param("slug"));
    if (!bunker) return c.json({ error: "Bunker not found." }, 404);
    if (!(await canAccessBunker(db, c.get("user"), bunker.id))) {
      return c.json({ error: "You don't have access to this bunker." }, 403);
    }

    return c.json({ bunker: serializeBunker(bunker) });
  });

  app.patch("/:slug", async (c) => {
    const user = c.get("user");
    const bunker = await load(c.req.param("slug"));
    if (!bunker) return c.json({ error: "Bunker not found." }, 404);
    if (!(await canAccessBunker(db, user, bunker.id))) {
      return c.json({ error: "You don't have access to this bunker." }, 403);
    }

    const body = updateSchema.safeParse(await c.req.json().catch(() => null));
    if (!body.success) {
      return c.json({ error: "Expected a JSON body." }, 400);
    }

    const patch: { name?: string; enabled?: number } = {};

    if (body.data.name !== undefined) {
      const name = body.data.name.trim();
      if (!name) return c.json({ error: "A bunker needs a name." }, 400);
      patch.name = name;
    }
    if (body.data.enabled !== undefined) {
      patch.enabled = body.data.enabled ? 1 : 0;
    }

    if (!Object.keys(patch).length) {
      return c.json({ bunker: serializeBunker(bunker) });
    }

    // `updated_at` is deliberately untouched: the gateway keys its cached
    // signers on it, and renaming a bunker is not a reason to rebuild one.
    await db.updateTable("bunkers").set(patch).where("id", "=", bunker.id).execute();

    await audit(db, user.pubkey, "bunker.update", bunker.slug, { name: bunker.name, enabled: bunker.enabled }, patch);
    log.info("bunker_updated", { bunker: bunker.slug, actor: user.pubkey });

    void bunkers.reload().catch(() => {});

    const updated = await db.selectFrom("bunkers").selectAll().where("id", "=", bunker.id).executeTakeFirstOrThrow();

    return c.json({ bunker: serializeBunker(updated) });
  });

  app.delete("/:slug", async (c) => {
    const user = c.get("user");
    const bunker = await load(c.req.param("slug"));
    if (!bunker) return c.json({ error: "Bunker not found." }, 404);
    if (!(await canAccessBunker(db, user, bunker.id))) {
      return c.json({ error: "You don't have access to this bunker." }, 403);
    }

    await bunkers.cancelPairing(bunker.id).catch(() => {});
    await db.deleteFrom("bunkers").where("id", "=", bunker.id).execute();

    await audit(db, user.pubkey, "bunker.delete", bunker.slug, {
      slug: bunker.slug,
      name: bunker.name,
      user_pubkey: bunker.user_pubkey,
    });
    log.info("bunker_deleted", { bunker: bunker.slug, actor: user.pubkey });

    void bunkers.reload().catch(() => {});

    return c.json({ ok: true });
  });

  /**
   * Start connecting a bunker to the signer that holds its key.
   *
   * Returns as soon as there is something to show, because the interesting
   * half happens on someone's phone: the dashboard polls the companion GET
   * until the signer answers or the attempt times out.
   */
  app.post("/:slug/pair", async (c) => {
    const user = c.get("user");
    const bunker = await load(c.req.param("slug"));
    if (!bunker) return c.json({ error: "Bunker not found." }, 404);
    if (!(await canAccessBunker(db, user, bunker.id))) {
      return c.json({ error: "You don't have access to this bunker." }, 403);
    }

    const body = pairSchema.safeParse(await c.req.json().catch(() => ({})));
    if (!body.success) {
      return c.json({ error: "Expected a JSON body, optionally with a bunker:// uri." }, 400);
    }

    try {
      const status = await bunkers.startPairing({ bunkerId: bunker.id, uri: body.data.uri });

      await audit(db, user.pubkey, "bunker.pair", bunker.slug, undefined, {
        mode: body.data.uri ? "bunker_uri" : "nostrconnect",
      });
      log.info("bunker_pairing_started", { bunker: bunker.slug, actor: user.pubkey });

      return c.json({ pairing: status }, 202);
    } catch (error) {
      return c.json({ error: message(error) }, 502);
    }
  });

  app.get("/:slug/pair", async (c) => {
    const bunker = await load(c.req.param("slug"));
    if (!bunker) return c.json({ error: "Bunker not found." }, 404);
    if (!(await canAccessBunker(db, c.get("user"), bunker.id))) {
      return c.json({ error: "You don't have access to this bunker." }, 403);
    }

    try {
      return c.json({ pairing: await bunkers.pairingStatus(bunker.id) });
    } catch (error) {
      return c.json({ error: message(error) }, 502);
    }
  });

  app.delete("/:slug/pair", async (c) => {
    const bunker = await load(c.req.param("slug"));
    if (!bunker) return c.json({ error: "Bunker not found." }, 404);
    if (!(await canAccessBunker(db, c.get("user"), bunker.id))) {
      return c.json({ error: "You don't have access to this bunker." }, 403);
    }

    try {
      await bunkers.cancelPairing(bunker.id);
      return c.json({ ok: true });
    } catch (error) {
      return c.json({ error: message(error) }, 502);
    }
  });

  /** Ask the upstream signer who it is, and record whether it answered. */
  app.post("/:slug/test", async (c) => {
    const bunker = await load(c.req.param("slug"));
    if (!bunker) return c.json({ error: "Bunker not found." }, 404);
    if (!(await canAccessBunker(db, c.get("user"), bunker.id))) {
      return c.json({ error: "You don't have access to this bunker." }, 403);
    }

    try {
      return c.json({ result: await bunkers.testBunker(bunker.id) });
    } catch (error) {
      return c.json({ error: message(error) }, 502);
    }
  });

  app.get("/:slug/grants", requireAdmin(), async (c) => {
    const bunker = await load(c.req.param("slug"));
    if (!bunker) return c.json({ error: "Bunker not found." }, 404);

    const grants = await db
      .selectFrom("bunker_grants")
      .select(["user_pubkey", "created_at"])
      .where("bunker_id", "=", bunker.id)
      .execute();

    return c.json({ grants });
  });

  app.put("/:slug/grants", requireAdmin(), async (c) => {
    const user = c.get("user");
    const bunker = await load(c.req.param("slug"));
    if (!bunker) return c.json({ error: "Bunker not found." }, 404);

    const body = grantsSchema.safeParse(await c.req.json().catch(() => null));
    if (!body.success) {
      return c.json({ error: "Expected a JSON body with a pubkeys array." }, 400);
    }

    const pubkeys = [...new Set(body.data.pubkeys.map((pubkey) => pubkey.toLowerCase()))];
    if (pubkeys.some((pubkey) => !/^[0-9a-f]{64}$/.test(pubkey))) {
      return c.json({ error: "Grants must be hex pubkeys." }, 400);
    }

    const known = await db
      .selectFrom("users")
      .select("pubkey")
      .where("pubkey", "in", pubkeys.length ? pubkeys : [""])
      .execute();
    const missing = pubkeys.filter((pubkey) => !known.some((row) => row.pubkey === pubkey));
    if (missing.length) {
      return c.json({ error: `Not npanel users: ${missing.join(", ")}` }, 400);
    }

    const previous = await db
      .selectFrom("bunker_grants")
      .select("user_pubkey")
      .where("bunker_id", "=", bunker.id)
      .execute();
    const now = Math.floor(Date.now() / 1000);

    await db.transaction().execute(async (trx) => {
      await trx.deleteFrom("bunker_grants").where("bunker_id", "=", bunker.id).execute();
      if (pubkeys.length) {
        await trx
          .insertInto("bunker_grants")
          .values(pubkeys.map((pubkey) => ({ user_pubkey: pubkey, bunker_id: bunker.id, created_at: now })))
          .execute();
      }
    });

    await audit(
      db,
      user.pubkey,
      "bunker.grants",
      bunker.slug,
      previous.map((row) => row.user_pubkey),
      pubkeys,
    );

    return c.json({ grants: pubkeys });
  });

  // Addressed by slug for the same reason a host is addressed by hostname: it
  // is the part staff recognise, and it keeps row ids out of pasted URLs.
  async function load(slug: string): Promise<Bunker | undefined> {
    return await db.selectFrom("bunkers").selectAll().where("slug", "=", slug.toLowerCase()).executeTakeFirst();
  }

  return app;
}

export interface BunkerView {
  slug: string;
  name: string;
  /** Who the bunker signs as, once pairing has established it. */
  user_pubkey: string | null;
  npub: string | null;
  /** The upstream signer's pubkey. Not secret: it is half of a bunker URI. */
  upstream_pubkey: string | null;
  upstream_relays: string[];
  status: string;
  error: string | null;
  last_ok_at: number | null;
  enabled: boolean;
  created_by: string;
  created_at: number;
  updated_at: number;
}

/**
 * Shape a bunker row for the API.
 *
 * `upstream_secret` and `key_fingerprint` are absent by construction rather
 * than by deletion: the credential npanel holds is the one thing here that
 * must never leave the process.
 */
function serializeBunker(bunker: Bunker): BunkerView {
  return {
    slug: bunker.slug,
    name: bunker.name,
    user_pubkey: bunker.user_pubkey,
    npub: bunker.user_pubkey ? nip19.npubEncode(bunker.user_pubkey) : null,
    upstream_pubkey: bunker.upstream_pubkey,
    upstream_relays: parseRelays(bunker.upstream_relays),
    status: bunker.status,
    error: bunker.error,
    last_ok_at: bunker.last_ok_at,
    enabled: bunker.enabled === 1,
    created_by: bunker.created_by,
    created_at: bunker.created_at,
    updated_at: bunker.updated_at,
  };
}

/** A URL-safe name derived from whatever someone typed. */
function slugify(value: string): string {
  return value
    .trim()
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "")
    .slice(0, 48);
}

function parseRelays(json: string): string[] {
  try {
    const value: unknown = JSON.parse(json);
    return Array.isArray(value) ? value.filter((url): url is string => typeof url === "string") : [];
  } catch {
    return [];
  }
}

function message(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}
