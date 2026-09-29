import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { after, test } from "node:test";
import { BlobStore } from "../blobs.ts";
import { openDatabase } from "../db/index.ts";
import type { DeployStatus } from "../db/schema.ts";
import { Logger } from "../log.ts";
import { ServeCache } from "../serve-cache.ts";
import { SettingsService } from "../settings.ts";
import { serve } from "../test/serve.ts";
import { DeployService } from "./service.ts";

const cleanups: (() => Promise<void>)[] = [];

after(async () => {
  for (const cleanup of cleanups) await cleanup();
});

const PUBKEY = "3bf0c63fcb93463407af97a5e5ee64fa883d107ef9e558472c4eb9aaaefa459d";
const EVENT_ID = "a".repeat(64);
const PAGE = "<h1>built on another thread</h1>";

function sha256(value: string): string {
  return createHash("sha256").update(value).digest("hex");
}

/** Poll until `check` passes, failing the test if it never does. */
async function eventually(check: () => Promise<boolean>, message: string): Promise<void> {
  const deadline = Date.now() + 4_000;

  while (Date.now() < deadline) {
    if (await check()) return;
    await new Promise((resolve) => setTimeout(resolve, 20));
  }

  assert.fail(message);
}

/**
 * A gateway with one host and one version of its site, on a real database
 * file: the thread opens its own connection, which an in-memory database
 * would not survive.
 */
async function setup(status: DeployStatus) {
  const dir = await mkdtemp(join(tmpdir(), "npanel-deploy-"));
  const databasePath = join(dir, "npanel.sqlite3");
  const blobsDir = join(dir, "blobs");

  const db = await openDatabase(databasePath);
  const settings = await SettingsService.load(db);
  // The thread opens its own relay pool, so it can't be handed a fake one.
  // The defaults are real relays and Blossom servers, which no test may touch.
  // Relays must be wss://, and the thread can't be made to trust a test
  // certificate, so the one it gets is a local server that fails the TLS
  // handshake: nothing leaves the machine, and nothing here needs a relay.
  const relay = serve(() => new Response("Not a relay."));
  const relayUrl = relay.url.replace(/^http:/, "wss:");
  await settings.update({ relays: [relayUrl], blossom_fallback_servers: [], deploy_attempts: 1 }, PUBKEY);

  const { id: hostId } = await db
    .insertInto("hosts")
    .values({
      hostname: "docs.example.com",
      pubkey: PUBKEY,
      kind: 15128,
      identifier: "",
      pinned_event_id: null,
      enabled: 1,
      created_by: PUBKEY,
      created_at: 1,
      updated_at: 1,
    })
    .returning("id")
    .executeTakeFirstOrThrow();

  await db
    .insertInto("site_events")
    .values({
      event_id: EVENT_ID,
      pubkey: PUBKEY,
      kind: 15128,
      identifier: "",
      created_at: 1000,
      event: JSON.stringify({ id: EVENT_ID, pubkey: PUBKEY, kind: 15128, created_at: 1000, tags: [], content: "" }),
      status,
      files_total: 1,
      files_ready: status === "ready" ? 1 : 0,
      error: null,
      fetched_at: 1000,
      ready_at: status === "ready" ? 1000 : null,
    })
    .execute();
  await db
    .insertInto("site_files")
    .values({ event_id: EVENT_ID, path: "/index.html", sha256: sha256(PAGE) })
    .execute();

  const blobs = new BlobStore(blobsDir);
  const serveCache = new ServeCache(db);
  const service = new DeployService({
    settings,
    serveCache,
    log: new Logger("error"),
    config: { databasePath, blobsDir, logLevel: "error" },
  });

  cleanups.push(async () => {
    await service.stop();
    await relay.close();
    await db.destroy();
    await rm(dir, { recursive: true, force: true });
  });

  /** The version the host is served from, as the main thread's cache has it. */
  const served = async () => (await serveCache.lookup("docs.example.com"))?.version;

  const storePage = () => blobs.put(sha256(PAGE), new TextEncoder().encode(PAGE));

  return { db, settings, service, hostId, served, storePage };
}

test("a build the deploy thread makes goes live on the main thread", async () => {
  const { service, served, storePage } = await setup("ready");
  await storePage();

  // Primed while there is nothing to serve, so only the thread telling the
  // main thread can make it answer anything else before the TTL runs out.
  assert.equal(await served(), undefined);

  await service.start();

  await eventually(async () => (await served())?.eventId === EVENT_ID, "The build never went live.");
});

test("a host is rebuilt on the thread when the main thread asks", async () => {
  const { db, service, hostId, served, storePage } = await setup("ready");
  await storePage();
  await service.start();
  await eventually(async () => (await served()) !== undefined, "The first build never went live.");

  const first = (await served())?.buildId;

  // What a build is made of changed, which only the thread can act on.
  await db.updateTable("hosts").set({ seo: 0 }).where("id", "=", hostId).execute();
  service.rebuild([hostId]);

  await eventually(async () => {
    const build = (await served())?.buildId;
    return build !== undefined && build !== first;
  }, "The host was never rebuilt.");
});

test("a setting changed on the main thread reaches the deploy thread", async () => {
  // A deploy that already failed, which nothing but a settings change will
  // retry: the startup resync is stuck asking a relay that never answers.
  const { settings, service, served, storePage } = await setup("failed");
  await service.start();

  await storePage();
  // New fallbacks retry every version that isn't ready. The blob is on disk
  // by now, so the server named is never asked for anything.
  await settings.update({ blossom_fallback_servers: ["https://blossom.invalid/"] }, PUBKEY);

  await eventually(async () => (await served())?.eventId === EVENT_ID, "The retried deploy never went live.");
});
