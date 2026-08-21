import { Hono } from "hono";
import { z } from "zod";
import { requireAdmin } from "../auth.ts";
import type { Host } from "../db/schema.ts";
import { parseHostname } from "../hostname.ts";
import { parseNsiteAddress } from "../nsite-address.ts";
import { activeEvent, newestEvent, siteKey } from "../sites.ts";
import {
  type ApiDeps,
  type ApiEnv,
  audit,
  canAccessHost,
  listedSites,
  serializeHost,
  serializeVersion,
} from "./context.ts";

const createSchema = z.object({
  hostname: z.string(),
  /** Any supported address format; stored decomposed. */
  address: z.string(),
  /** Keep the site off the public list from the moment it exists. */
  hidden: z.boolean().optional(),
  /** Fold `www` into the domain. On unless asked otherwise. */
  redirect_www: z.boolean().optional(),
  /** Serve the site's shell for paths it has no file for. Off by default. */
  spa: z.boolean().optional(),
  /** Serve this site under a Content-Security-Policy. On unless asked otherwise. */
  csp: z.boolean().optional(),
});

const updateSchema = z.object({
  hostname: z.string().optional(),
  address: z.string().optional(),
  enabled: z.boolean().optional(),
  hidden: z.boolean().optional(),
  redirect_www: z.boolean().optional(),
  spa: z.boolean().optional(),
  csp: z.boolean().optional(),
  /** Event id to pin, or null to follow the newest ready version. */
  pinned_event_id: z.string().nullable().optional(),
});

const grantsSchema = z.object({
  pubkeys: z.array(z.string()),
});

/** Routes for the hostname → nsite mappings that are npanel's whole point. */
export function hostRoutes(deps: ApiDeps): Hono<ApiEnv> {
  const { db, settings, sync, tor, log } = deps;
  const app = new Hono<ApiEnv>();

  app.get("/", async (c) => {
    const user = c.get("user");

    let query = db.selectFrom("hosts").selectAll().orderBy("hostname");
    if (user.role !== "admin") {
      query = query.where("id", "in", (eb) =>
        eb.selectFrom("host_grants").select("host_id").where("user_pubkey", "=", user.pubkey),
      );
    }

    const hosts = await query.execute();

    // Staff are told what else this gateway serves on the same terms as the
    // public, which spares them asking an admin whether a name is already
    // taken. Admins have every host in `hosts` already.
    const mine = new Set(hosts.map((host) => host.hostname));
    const others =
      user.role === "admin" ? [] : (await listedSites(db, settings)).filter((site) => !mine.has(site.hostname));

    return c.json({
      hosts: await Promise.all(hosts.map((host) => serializeHost(db, host))),
      other_sites: others,
    });
  });

  app.post("/", async (c) => {
    const user = c.get("user");
    const body = createSchema.safeParse(await c.req.json().catch(() => null));
    if (!body.success) {
      return c.json({ error: "Expected a JSON body with hostname and address." }, 400);
    }

    let hostname: string;
    let address: ReturnType<typeof parseNsiteAddress>;
    try {
      hostname = parseHostname(body.data.hostname, settings.get("allowed_host_suffixes"));
      address = parseNsiteAddress(body.data.address);
    } catch (error) {
      return c.json({ error: message(error) }, 400);
    }

    if (hostname === deps.dashboardHost) {
      return c.json({ error: "That domain serves the dashboard itself." }, 409);
    }

    const existing = await db.selectFrom("hosts").select("id").where("hostname", "=", hostname).executeTakeFirst();
    if (existing) {
      return c.json({ error: `${hostname} is already configured.` }, 409);
    }

    const now = Math.floor(Date.now() / 1000);
    const inserted = await db
      .insertInto("hosts")
      .values({
        hostname,
        pubkey: address.pubkey,
        kind: address.kind,
        identifier: address.identifier,
        pinned_event_id: null,
        enabled: 1,
        hidden: body.data.hidden ? 1 : 0,
        redirect_www: body.data.redirect_www === false ? 0 : 1,
        spa: body.data.spa ? 1 : 0,
        csp: body.data.csp === false ? 0 : 1,
        created_by: user.pubkey,
        created_at: now,
        updated_at: now,
      })
      .returning("id")
      .executeTakeFirstOrThrow();

    // Whoever adds a host can manage it, so staff aren't locked out of their own work.
    if (user.role !== "admin") {
      await db
        .insertInto("host_grants")
        .values({ user_pubkey: user.pubkey, host_id: inserted.id, created_at: now })
        .execute();
    }

    await audit(db, user.pubkey, "host.create", hostname, undefined, {
      hostname,
      address: siteKey(address),
    });

    log.info("host_created", { hostname, pubkey: address.pubkey, actor: user.pubkey });

    void sync.hostsChanged();
    void tor.hostsChanged();

    const host = await db.selectFrom("hosts").selectAll().where("id", "=", inserted.id).executeTakeFirstOrThrow();

    return c.json({ host: await serializeHost(db, host) }, 201);
  });

  app.get("/:hostname", async (c) => {
    const host = await load(c.req.param("hostname"));
    if (!host) return c.json({ error: "Host not found." }, 404);
    if (!(await canAccessHost(db, c.get("user"), host.id))) {
      return c.json({ error: "You don't have access to this host." }, 403);
    }

    return c.json({ host: await serializeHost(db, host) });
  });

  app.patch("/:hostname", async (c) => {
    const user = c.get("user");
    const host = await load(c.req.param("hostname"));
    if (!host) return c.json({ error: "Host not found." }, 404);
    if (!(await canAccessHost(db, user, host.id))) {
      return c.json({ error: "You don't have access to this host." }, 403);
    }

    const body = updateSchema.safeParse(await c.req.json().catch(() => null));
    if (!body.success) {
      return c.json({ error: "Expected a JSON body." }, 400);
    }

    const patch: Partial<Host> = {};

    if (body.data.hostname !== undefined) {
      let hostname: string;
      try {
        hostname = parseHostname(body.data.hostname, settings.get("allowed_host_suffixes"));
      } catch (error) {
        return c.json({ error: message(error) }, 400);
      }
      if (hostname === deps.dashboardHost) {
        return c.json({ error: "That domain serves the dashboard itself." }, 409);
      }
      if (hostname !== host.hostname) {
        const clash = await db.selectFrom("hosts").select("id").where("hostname", "=", hostname).executeTakeFirst();
        if (clash) return c.json({ error: `${hostname} is already configured.` }, 409);
        patch.hostname = hostname;
      }
    }

    if (body.data.address !== undefined) {
      try {
        const address = parseNsiteAddress(body.data.address);
        patch.pubkey = address.pubkey;
        patch.kind = address.kind;
        patch.identifier = address.identifier;
        // A retarget invalidates a pin that belongs to the old site.
        patch.pinned_event_id = null;
      } catch (error) {
        return c.json({ error: message(error) }, 400);
      }
    }

    if (body.data.enabled !== undefined) {
      patch.enabled = body.data.enabled ? 1 : 0;
    }

    if (body.data.hidden !== undefined) {
      patch.hidden = body.data.hidden ? 1 : 0;
    }

    if (body.data.redirect_www !== undefined) {
      patch.redirect_www = body.data.redirect_www ? 1 : 0;
    }

    if (body.data.spa !== undefined) {
      patch.spa = body.data.spa ? 1 : 0;
    }

    if (body.data.csp !== undefined) {
      patch.csp = body.data.csp ? 1 : 0;
    }

    if (body.data.pinned_event_id !== undefined && body.data.address === undefined) {
      if (body.data.pinned_event_id === null) {
        patch.pinned_event_id = null;
      } else {
        const version = await db
          .selectFrom("site_events")
          .selectAll()
          .where("event_id", "=", body.data.pinned_event_id)
          .executeTakeFirst();

        if (
          !version ||
          version.pubkey !== host.pubkey ||
          version.kind !== host.kind ||
          version.identifier !== host.identifier
        ) {
          return c.json({ error: "That version belongs to a different site." }, 400);
        }
        if (version.status !== "ready") {
          return c.json({ error: "That version isn't fully downloaded yet." }, 409);
        }
        patch.pinned_event_id = version.event_id;
      }
    }

    if (!Object.keys(patch).length) {
      return c.json({ host: await serializeHost(db, host) });
    }

    await db
      .updateTable("hosts")
      .set({ ...patch, updated_at: Math.floor(Date.now() / 1000) })
      .where("id", "=", host.id)
      .execute();

    await audit(db, user.pubkey, "host.update", host.hostname, hostSummary(host), {
      ...hostSummary(host),
      ...patch,
    });

    log.info("host_updated", { hostname: host.hostname, actor: user.pubkey });

    if (patch.pubkey || patch.enabled !== undefined) {
      void sync.hostsChanged();
    }

    // A narrower condition than the sync worker's: Tor publishes a name, and
    // does not care which nsite is behind it.
    if (patch.enabled !== undefined) {
      void tor.hostsChanged();
    }

    const updated = await db.selectFrom("hosts").selectAll().where("id", "=", host.id).executeTakeFirstOrThrow();

    return c.json({ host: await serializeHost(db, updated) });
  });

  app.delete("/:hostname", async (c) => {
    const user = c.get("user");
    const host = await load(c.req.param("hostname"));
    if (!host) return c.json({ error: "Host not found." }, 404);
    if (!(await canAccessHost(db, user, host.id))) {
      return c.json({ error: "You don't have access to this host." }, 403);
    }

    await db.deleteFrom("hosts").where("id", "=", host.id).execute();
    await audit(db, user.pubkey, "host.delete", host.hostname, hostSummary(host));

    log.info("host_deleted", { hostname: host.hostname, actor: user.pubkey });

    void sync.hostsChanged();
    void tor.hostsChanged();

    return c.json({ ok: true });
  });

  app.get("/:hostname/versions", async (c) => {
    const host = await load(c.req.param("hostname"));
    if (!host) return c.json({ error: "Host not found." }, 404);
    if (!(await canAccessHost(db, c.get("user"), host.id))) {
      return c.json({ error: "You don't have access to this host." }, 403);
    }

    const limit = clamp(Number(c.req.query("limit") ?? 50), 1, 200);

    const versions = await db
      .selectFrom("site_events")
      .selectAll()
      .where("pubkey", "=", host.pubkey)
      .where("kind", "=", host.kind)
      .where("identifier", "=", host.identifier)
      .orderBy("created_at", "desc")
      .limit(limit)
      .execute();

    const active = await activeEvent(db, host);

    return c.json({
      active_event_id: active?.event_id ?? null,
      pinned_event_id: host.pinned_event_id,
      versions: versions.map(serializeVersion),
    });
  });

  app.get("/:hostname/versions/:eventId/log", async (c) => {
    const host = await load(c.req.param("hostname"));
    if (!host) return c.json({ error: "Host not found." }, 404);
    if (!(await canAccessHost(db, c.get("user"), host.id))) {
      return c.json({ error: "You don't have access to this host." }, 403);
    }

    const eventId = c.req.param("eventId");
    const version = await db.selectFrom("site_events").selectAll().where("event_id", "=", eventId).executeTakeFirst();

    if (
      !version ||
      version.pubkey !== host.pubkey ||
      version.kind !== host.kind ||
      version.identifier !== host.identifier
    ) {
      return c.json({ error: "That version belongs to a different site." }, 404);
    }

    const entries = await db
      .selectFrom("deploy_log")
      .selectAll()
      .where("event_id", "=", eventId)
      .orderBy("id", "asc")
      .execute();

    return c.json({
      version: serializeVersion(version),
      entries: entries.map((entry) => ({
        id: entry.id,
        level: entry.level,
        msg: entry.msg,
        fields: entry.fields ? JSON.parse(entry.fields) : null,
        created_at: entry.created_at,
      })),
    });
  });

  app.get("/:hostname/files", async (c) => {
    const host = await load(c.req.param("hostname"));
    if (!host) return c.json({ error: "Host not found." }, 404);
    if (!(await canAccessHost(db, c.get("user"), host.id))) {
      return c.json({ error: "You don't have access to this host." }, 403);
    }

    const requested = c.req.query("event_id");
    const version = requested
      ? await db.selectFrom("site_events").selectAll().where("event_id", "=", requested).executeTakeFirst()
      : ((await activeEvent(db, host)) ?? (await newestEvent(db, host)));

    if (
      !version ||
      version.pubkey !== host.pubkey ||
      version.kind !== host.kind ||
      version.identifier !== host.identifier
    ) {
      return c.json({ error: "No version to list." }, 404);
    }

    const files = await db
      .selectFrom("site_files")
      .leftJoin("blobs", "blobs.sha256", "site_files.sha256")
      .select(["site_files.path", "site_files.sha256", "blobs.size"])
      .where("site_files.event_id", "=", version.event_id)
      .orderBy("site_files.path")
      .execute();

    return c.json({
      event_id: version.event_id,
      files: files.map((file) => ({
        path: file.path,
        sha256: file.sha256,
        size: file.size,
        stored: file.size !== null,
      })),
    });
  });

  app.post("/:hostname/sync", async (c) => {
    const host = await load(c.req.param("hostname"));
    if (!host) return c.json({ error: "Host not found." }, 404);
    if (!(await canAccessHost(db, c.get("user"), host.id))) {
      return c.json({ error: "You don't have access to this host." }, 403);
    }

    // Relay round trips take seconds; the dashboard polls the host for status.
    void sync.syncSite({ pubkey: host.pubkey, kind: host.kind, identifier: host.identifier });

    return c.json({ ok: true }, 202);
  });

  app.get("/:hostname/grants", requireAdmin(), async (c) => {
    const host = await load(c.req.param("hostname"));
    if (!host) return c.json({ error: "Host not found." }, 404);

    const grants = await db
      .selectFrom("host_grants")
      .select(["user_pubkey", "created_at"])
      .where("host_id", "=", host.id)
      .execute();

    return c.json({ grants });
  });

  app.put("/:hostname/grants", requireAdmin(), async (c) => {
    const user = c.get("user");
    const host = await load(c.req.param("hostname"));
    if (!host) return c.json({ error: "Host not found." }, 404);

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

    const previous = await db.selectFrom("host_grants").select("user_pubkey").where("host_id", "=", host.id).execute();
    const now = Math.floor(Date.now() / 1000);

    await db.transaction().execute(async (trx) => {
      await trx.deleteFrom("host_grants").where("host_id", "=", host.id).execute();
      if (pubkeys.length) {
        await trx
          .insertInto("host_grants")
          .values(pubkeys.map((pubkey) => ({ user_pubkey: pubkey, host_id: host.id, created_at: now })))
          .execute();
      }
    });

    await audit(
      db,
      user.pubkey,
      "host.grants",
      host.hostname,
      previous.map((row) => row.user_pubkey),
      pubkeys,
    );

    return c.json({ grants: pubkeys });
  });

  // A host is addressed by its hostname, which is unique and is the thing staff
  // actually recognise. The row id stays inside the server, where a URL someone
  // pastes into chat can't turn into a guess at what host 12 might be.
  async function load(hostname: string): Promise<Host | undefined> {
    return await db.selectFrom("hosts").selectAll().where("hostname", "=", hostname.toLowerCase()).executeTakeFirst();
  }

  return app;
}

function hostSummary(host: Host) {
  return {
    hostname: host.hostname,
    pubkey: host.pubkey,
    kind: host.kind,
    identifier: host.identifier,
    enabled: host.enabled,
    hidden: host.hidden,
    redirect_www: host.redirect_www,
    spa: host.spa,
    csp: host.csp,
    pinned_event_id: host.pinned_event_id,
  };
}

function clamp(value: number, min: number, max: number): number {
  if (!Number.isFinite(value)) return min;
  return Math.min(Math.max(Math.trunc(value), min), max);
}

function message(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}
