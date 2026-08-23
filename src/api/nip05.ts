import { Hono } from "hono";
import { z } from "zod";
import type { Host } from "../db/schema.ts";
import { type Nip05Entry, parseNip05Name, parseRelayList } from "../nip05.ts";
import { parsePubkey } from "../pubkey.ts";
import { parseRelayUrl } from "../relay-url.ts";
import { type ApiDeps, type ApiEnv, audit, canAccessHost, loadHost, message } from "./context.ts";

/**
 * How many relays one name may carry.
 *
 * Every lookup for this host serves the list, and the NIP asks for the relays a
 * user *may be found* on, not for their whole subscription. Ten is past any
 * honest answer and short of a row worth paying to send.
 */
const MAX_RELAYS = 10;

const setSchema = z.object({
  /** npub, nprofile, or hex; stored and served as hex, which the NIP requires. */
  pubkey: z.string(),
  relays: z.array(z.string()).optional(),
});

interface Nip05View extends Nip05Entry {
  created_at: number;
  updated_at: number;
}

/**
 * The NIP-05 names a host answers for.
 *
 * Mounted alongside {@link file://./hosts.ts} rather than inside it, and
 * guarded the same way: a name is a site-specific setting, so anyone with a
 * grant on the site manages it, with no admin in the loop.
 */
export function nip05Routes(deps: ApiDeps): Hono<ApiEnv> {
  const { db, log } = deps;
  const app = new Hono<ApiEnv>();

  app.get("/:hostname/nip05", async (c) => {
    const host = await load(c.req.param("hostname"));
    if (!host) return c.json({ error: "Host not found." }, 404);
    if (!(await canAccessHost(db, c.get("user"), host.id))) {
      return c.json({ error: "You don't have access to this host." }, 403);
    }

    return c.json({ names: await listView(host) });
  });

  app.put("/:hostname/nip05/:name", async (c) => {
    const user = c.get("user");
    const host = await load(c.req.param("hostname"));
    if (!host) return c.json({ error: "Host not found." }, 404);
    if (!(await canAccessHost(db, user, host.id))) {
      return c.json({ error: "You don't have access to this host." }, 403);
    }

    const body = setSchema.safeParse(await c.req.json().catch(() => null));
    if (!body.success) {
      return c.json({ error: "Expected a JSON body with a pubkey." }, 400);
    }

    if ((body.data.relays?.length ?? 0) > MAX_RELAYS) {
      return c.json({ error: `A name can list at most ${MAX_RELAYS} relays.` }, 400);
    }

    let name: string;
    let pubkey: string;
    let relays: string[];
    try {
      name = parseNip05Name(c.req.param("name"));
      pubkey = parsePubkey(body.data.pubkey);
      // Deduplicated after normalizing, so the same relay written two ways is
      // stored once rather than served twice.
      relays = [...new Set((body.data.relays ?? []).map(parseRelayUrl))];
    } catch (error) {
      return c.json({ error: message(error) }, 400);
    }

    const previous = await db
      .selectFrom("nip05_names")
      .selectAll()
      .where("host_id", "=", host.id)
      .where("name", "=", name)
      .executeTakeFirst();

    const now = Math.floor(Date.now() / 1000);
    const values = { pubkey, relays: JSON.stringify(relays), updated_at: now };

    if (previous) {
      await db.updateTable("nip05_names").set(values).where("host_id", "=", host.id).where("name", "=", name).execute();
    } else {
      await db
        .insertInto("nip05_names")
        .values({ host_id: host.id, name, ...values, created_at: now })
        .execute();
    }

    await audit(
      db,
      user.pubkey,
      "nip05.set",
      `${name}@${host.hostname}`,
      previous ? { pubkey: previous.pubkey, relays: parseRelayList(previous.relays) } : undefined,
      { pubkey, relays },
    );

    log.info("nip05_name_set", { hostname: host.hostname, name, pubkey, actor: user.pubkey });

    return c.json({ names: await listView(host) }, previous ? 200 : 201);
  });

  app.delete("/:hostname/nip05/:name", async (c) => {
    const user = c.get("user");
    const host = await load(c.req.param("hostname"));
    if (!host) return c.json({ error: "Host not found." }, 404);
    if (!(await canAccessHost(db, user, host.id))) {
      return c.json({ error: "You don't have access to this host." }, 403);
    }

    let name: string;
    try {
      name = parseNip05Name(c.req.param("name"));
    } catch (error) {
      return c.json({ error: message(error) }, 400);
    }

    const previous = await db
      .selectFrom("nip05_names")
      .selectAll()
      .where("host_id", "=", host.id)
      .where("name", "=", name)
      .executeTakeFirst();

    if (!previous) {
      return c.json({ error: `${host.hostname} has no name ${name}.` }, 404);
    }

    await db.deleteFrom("nip05_names").where("host_id", "=", host.id).where("name", "=", name).execute();

    await audit(db, user.pubkey, "nip05.delete", `${name}@${host.hostname}`, {
      pubkey: previous.pubkey,
      relays: parseRelayList(previous.relays),
    });

    log.info("nip05_name_deleted", { hostname: host.hostname, name, actor: user.pubkey });

    return c.json({ names: await listView(host) });
  });

  /** The whole list, which every write returns so the dashboard never guesses. */
  async function listView(host: Host): Promise<Nip05View[]> {
    const rows = await db
      .selectFrom("nip05_names")
      .selectAll()
      .where("host_id", "=", host.id)
      .orderBy("name")
      .execute();

    return rows.map((row) => ({
      name: row.name,
      pubkey: row.pubkey,
      relays: parseRelayList(row.relays),
      created_at: row.created_at,
      updated_at: row.updated_at,
    }));
  }

  const load = (hostname: string) => loadHost(db, hostname);

  return app;
}
