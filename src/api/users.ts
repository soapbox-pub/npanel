import { Hono } from "hono";
import { nip19 } from "nostr-tools";
import { z } from "zod";
import { requireAdmin } from "../auth.ts";
import { type ApiDeps, type ApiEnv, audit } from "./context.ts";

const createSchema = z.object({
  /** npub, nprofile, or hex pubkey. */
  pubkey: z.string(),
  role: z.enum(["admin", "staff"]).default("staff"),
});

const updateSchema = z.object({
  role: z.enum(["admin", "staff"]),
});

/** Admin-only routes for managing who can use npanel. */
export function userRoutes(deps: ApiDeps): Hono<ApiEnv> {
  const { db, log } = deps;
  const app = new Hono<ApiEnv>();

  app.use("*", requireAdmin());

  app.get("/", async (c) => {
    const users = await db.selectFrom("users").selectAll().orderBy("created_at").execute();
    const grants = await db.selectFrom("host_grants").select(["user_pubkey", "host_id"]).execute();

    return c.json({
      users: users.map((user) => ({
        pubkey: user.pubkey,
        npub: nip19.npubEncode(user.pubkey),
        role: user.role,
        added_by: user.added_by,
        created_at: user.created_at,
        host_ids: grants.filter((grant) => grant.user_pubkey === user.pubkey).map((grant) => grant.host_id),
      })),
    });
  });

  app.post("/", async (c) => {
    const actor = c.get("user");
    const body = createSchema.safeParse(await c.req.json().catch(() => null));
    if (!body.success) {
      return c.json({ error: "Expected a JSON body with a pubkey." }, 400);
    }

    let pubkey: string;
    try {
      pubkey = parsePubkey(body.data.pubkey);
    } catch (error) {
      return c.json({ error: message(error) }, 400);
    }

    const existing = await db.selectFrom("users").select("pubkey").where("pubkey", "=", pubkey).executeTakeFirst();
    if (existing) {
      return c.json({ error: "That user already has access." }, 409);
    }

    await db
      .insertInto("users")
      .values({
        pubkey,
        role: body.data.role,
        added_by: actor.pubkey,
        created_at: Math.floor(Date.now() / 1000),
      })
      .execute();

    await audit(db, actor.pubkey, "user.create", pubkey, undefined, { role: body.data.role });
    log.info("user_created", { pubkey, role: body.data.role, actor: actor.pubkey });

    return c.json({ user: { pubkey, npub: nip19.npubEncode(pubkey), role: body.data.role } }, 201);
  });

  app.patch("/:pubkey", async (c) => {
    const actor = c.get("user");

    let pubkey: string;
    try {
      pubkey = parsePubkey(c.req.param("pubkey"));
    } catch (error) {
      return c.json({ error: message(error) }, 400);
    }

    const body = updateSchema.safeParse(await c.req.json().catch(() => null));
    if (!body.success) {
      return c.json({ error: "Expected a JSON body with a role of admin or staff." }, 400);
    }

    const user = await db.selectFrom("users").selectAll().where("pubkey", "=", pubkey).executeTakeFirst();
    if (!user) return c.json({ error: "User not found." }, 404);

    if (user.role === "admin" && body.data.role !== "admin" && (await adminCount(db)) === 1) {
      return c.json({ error: "That's the last admin. Promote someone else first." }, 409);
    }

    await db.updateTable("users").set({ role: body.data.role }).where("pubkey", "=", pubkey).execute();
    await audit(db, actor.pubkey, "user.update", pubkey, { role: user.role }, { role: body.data.role });
    log.info("user_updated", { pubkey, role: body.data.role, actor: actor.pubkey });

    return c.json({ user: { pubkey, npub: nip19.npubEncode(pubkey), role: body.data.role } });
  });

  app.delete("/:pubkey", async (c) => {
    const actor = c.get("user");

    let pubkey: string;
    try {
      pubkey = parsePubkey(c.req.param("pubkey"));
    } catch (error) {
      return c.json({ error: message(error) }, 400);
    }

    const user = await db.selectFrom("users").selectAll().where("pubkey", "=", pubkey).executeTakeFirst();
    if (!user) return c.json({ error: "User not found." }, 404);

    if (user.role === "admin" && (await adminCount(db)) === 1) {
      return c.json({ error: "That's the last admin. Promote someone else first." }, 409);
    }

    await db.transaction().execute(async (trx) => {
      await trx.deleteFrom("host_grants").where("user_pubkey", "=", pubkey).execute();
      await trx.deleteFrom("users").where("pubkey", "=", pubkey).execute();
    });

    await audit(db, actor.pubkey, "user.delete", pubkey, { role: user.role });
    log.info("user_deleted", { pubkey, actor: actor.pubkey });

    return c.json({ ok: true });
  });

  return app;
}

/** Accept an npub, nprofile, or hex pubkey; always store hex. */
export function parsePubkey(input: string): string {
  const value = input.trim().replace(/^nostr:/i, "");

  if (/^[0-9a-fA-F]{64}$/.test(value)) return value.toLowerCase();

  try {
    const decoded = nip19.decode(value);
    if (decoded.type === "npub") return decoded.data;
    if (decoded.type === "nprofile") return decoded.data.pubkey;
  } catch {
    throw new Error(`Not a valid pubkey: ${input.trim()}`);
  }

  throw new Error(`Not a valid pubkey: ${input.trim()}`);
}

async function adminCount(db: ApiDeps["db"]): Promise<number> {
  const rows = await db.selectFrom("users").select("pubkey").where("role", "=", "admin").execute();
  return rows.length;
}

function message(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}
