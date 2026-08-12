import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { mkdtemp, rm } from "node:fs/promises";
import { type Server, type ServerResponse, createServer } from "node:http";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { text } from "node:stream/consumers";
import { after, test } from "node:test";
import type { NostrEvent } from "@nostrify/nostrify";
import type { Kysely } from "kysely";
import { finalizeEvent, generateSecretKey, getPublicKey } from "nostr-tools";
import { BlobStore } from "./blobs.ts";
import { openDatabase } from "./db/index.ts";
import type { Database } from "./db/schema.ts";
import { Logger } from "./log.ts";
import type { NostrService } from "./nostr.ts";
import { NAMED_SITE_KIND } from "./nsite-address.ts";
import { SettingsService } from "./settings.ts";
import { activeEvent, findHost, resolveSitePath } from "./sites.ts";
import { SyncWorker } from "./sync.ts";

const cleanups: (() => Promise<void>)[] = [];

after(async () => {
  for (const cleanup of cleanups) await cleanup();
});

const secretKey = generateSecretKey();
const PUBKEY = getPublicKey(secretKey);

function sha256(value: string): string {
  return createHash("sha256").update(value).digest("hex");
}

/** A Blossom-ish server serving `files` keyed by hash. */
async function blossom(files: Record<string, string>) {
  const requests: string[] = [];
  const server: Server = createServer((req, res: ServerResponse) => {
    const hash = (req.url ?? "/").slice(1);
    requests.push(hash);
    const body = files[hash];
    if (body === undefined) {
      res.statusCode = 404;
      res.end("not found");
      return;
    }
    res.end(body);
  });

  await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
  const address = server.address();
  if (typeof address === "string" || !address) throw new Error("no address");

  cleanups.push(() => new Promise<void>((resolve) => server.close(() => resolve())));

  return { url: `http://127.0.0.1:${address.port}/`, requests };
}

function manifest(files: Record<string, string>, opts: { servers?: string[]; createdAt?: number } = {}): NostrEvent {
  const tags: string[][] = [["d", "docs"]];
  for (const [path, content] of Object.entries(files)) {
    tags.push(["path", path, sha256(content)]);
  }
  for (const server of opts.servers ?? []) {
    tags.push(["server", server]);
  }

  return finalizeEvent(
    {
      kind: NAMED_SITE_KIND,
      created_at: opts.createdAt ?? Math.floor(Date.now() / 1000),
      tags,
      content: "",
    },
    secretKey,
  );
}

/** Minimal NostrService stand-in: no sockets, no relays. */
function fakeNostr(): NostrService {
  return {
    relays: [],
    req: async function* () {},
    query: async () => [],
    getAuthorRelays: async () => [],
    getAuthorServers: async () => [],
    fetchManifest: async () => undefined,
    blobSources: async (event: NostrEvent, fallbacks: string[]) => [
      ...event.tags.filter((tag) => tag[0] === "server").map((tag) => tag[1]),
      ...fallbacks,
    ],
    pruneRelays: () => {},
    clearCaches: () => {},
    close: async () => {},
  } as unknown as NostrService;
}

async function harness() {
  const dir = await mkdtemp(join(tmpdir(), "npanel-sync-"));
  const db: Kysely<Database> = await openDatabase(":memory:");
  const blobs = new BlobStore(join(dir, "blobs"));
  const settings = await SettingsService.load(db);
  // The test Blossom servers listen on loopback, which is exactly what the
  // SSRF guard exists to refuse.
  await settings.update({ allow_private_blob_servers: true }, PUBKEY);
  const log = new Logger("error");
  const worker = new SyncWorker({ db, blobs, nostr: fakeNostr(), settings, log });

  cleanups.push(async () => {
    await worker.stop();
    await db.destroy();
    await rm(dir, { recursive: true, force: true });
  });

  await db
    .insertInto("hosts")
    .values({
      hostname: "docs.example.com",
      pubkey: PUBKEY,
      kind: NAMED_SITE_KIND,
      identifier: "docs",
      pinned_event_id: null,
      enabled: 1,
      created_by: PUBKEY,
      created_at: 1,
      updated_at: 1,
    })
    .execute();

  await worker.start();

  return { db, blobs, settings, worker };
}

test("downloads every blob before a version becomes servable", async () => {
  const files = { "/index.html": "<h1>hi</h1>", "/style.css": "body{}" };
  const server = await blossom({
    [sha256(files["/index.html"])]: files["/index.html"],
    [sha256(files["/style.css"])]: files["/style.css"],
  });

  const { db, blobs, worker } = await harness();
  const event = manifest(files, { servers: [server.url] });

  await worker.ingest(event);
  await worker.whenIdle();

  const row = await db.selectFrom("site_events").selectAll().executeTakeFirstOrThrow();
  assert.equal(row.status, "ready");
  assert.equal(row.error, null);
  assert.equal(row.files_total, 2);
  assert.equal(row.files_ready, 2);
  assert.ok(row.ready_at);

  assert.equal(await text(blobs.read(sha256(files["/index.html"]))), files["/index.html"]);

  const host = await findHost(db, "docs.example.com");
  assert.ok(host);
  const active = await activeEvent(db, host);
  assert.equal(active?.event_id, event.id);

  const resolved = await resolveSitePath(db, event.id, "/");
  assert.equal(resolved?.path, "/index.html");

  const recorded = await db.selectFrom("blobs").selectAll().execute();
  assert.equal(recorded.length, 2);
});

test("a version whose blobs are unavailable fails and does not replace the live one", async () => {
  const working = { "/index.html": "v1" };
  const server = await blossom({ [sha256(working["/index.html"])]: working["/index.html"] });

  const { db, worker } = await harness();

  const first = manifest(working, { servers: [server.url], createdAt: 1000 });
  await worker.ingest(first);
  await worker.whenIdle();

  // Second version references a blob no server has.
  const second = manifest({ "/index.html": "v2-missing" }, { servers: [server.url], createdAt: 2000 });
  await worker.ingest(second);
  await worker.whenIdle();

  const failed = await db
    .selectFrom("site_events")
    .selectAll()
    .where("event_id", "=", second.id)
    .executeTakeFirstOrThrow();

  assert.equal(failed.status, "failed");
  assert.match(failed.error ?? "", /could not be downloaded/);

  const host = await findHost(db, "docs.example.com");
  assert.ok(host);
  const active = await activeEvent(db, host);
  assert.equal(active?.event_id, first.id, "the working version keeps serving");

  const logs = await db.selectFrom("deploy_log").selectAll().where("event_id", "=", second.id).execute();

  assert.ok(logs.some((entry) => entry.msg === "blob_download_failed"));
  assert.ok(logs.some((entry) => entry.msg === "blob_unavailable"));
  assert.ok(logs.some((entry) => entry.msg === "deploy_failed"));
});

test("explains itself when there is nowhere to download from", async () => {
  const { db, worker } = await harness();

  const event = manifest({ "/index.html": "orphan" });
  await worker.ingest(event);
  await worker.whenIdle();

  const row = await db.selectFrom("site_events").selectAll().executeTakeFirstOrThrow();
  assert.equal(row.status, "failed");
  assert.match(row.error ?? "", /No Blossom servers/);

  const logs = await db.selectFrom("deploy_log").selectAll().execute();
  assert.ok(logs.some((entry) => entry.msg === "no_blob_servers"));
});

test("falls back to the configured Blossom servers when a manifest names none", async () => {
  const files = { "/index.html": "fallback" };
  const server = await blossom({ [sha256(files["/index.html"])]: files["/index.html"] });

  const { db, settings, worker } = await harness();
  await settings.update({ blossom_fallback_servers: [server.url] }, PUBKEY);

  await worker.ingest(manifest(files));
  await worker.whenIdle();

  const row = await db.selectFrom("site_events").selectAll().executeTakeFirstOrThrow();
  assert.equal(row.status, "ready");
});

test("ignores manifests for sites no host points at", async () => {
  const { db, worker } = await harness();

  const otherKey = generateSecretKey();
  const foreign = finalizeEvent(
    {
      kind: NAMED_SITE_KIND,
      created_at: Math.floor(Date.now() / 1000),
      tags: [
        ["d", "docs"],
        ["path", "/index.html", sha256("x")],
      ],
      content: "",
    },
    otherKey,
  );

  await worker.ingest(foreign);
  await worker.whenIdle();

  assert.deepEqual(await db.selectFrom("site_events").selectAll().execute(), []);
});

test("skips blobs that are already stored", async () => {
  const files = { "/index.html": "cached" };
  const hash = sha256(files["/index.html"]);
  const server = await blossom({});

  const { db, blobs, worker } = await harness();
  await blobs.put(hash, new TextEncoder().encode(files["/index.html"]));

  await worker.ingest(manifest(files, { servers: [server.url] }));
  await worker.whenIdle();

  const row = await db.selectFrom("site_events").selectAll().executeTakeFirstOrThrow();
  assert.equal(row.status, "ready");
  assert.deepEqual(server.requests, [], "no download was attempted");
});

test("a deploy interrupted by shutdown is never marked ready", async () => {
  const files: Record<string, string> = {};
  // More files than BLOB_CONCURRENCY, so some are still queued when we stop.
  for (let i = 0; i < 8; i++) {
    files[`/file-${i}.txt`] = `content ${i}`;
  }

  // A server that accepts requests and never answers them.
  const held: ServerResponse[] = [];
  const server: Server = createServer((_req, res) => {
    held.push(res);
  });
  await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
  const address = server.address();
  if (typeof address === "string" || !address) throw new Error("no address");
  cleanups.push(async () => {
    for (const res of held) res.destroy();
    await new Promise<void>((resolve) => server.close(() => resolve()));
  });

  const { db, blobs, worker } = await harness();
  const event = manifest(files, { servers: [`http://127.0.0.1:${address.port}/`] });

  await worker.ingest(event);
  await new Promise((resolve) => setTimeout(resolve, 50));
  await worker.stop();
  await worker.whenIdle();

  const row = await db.selectFrom("site_events").selectAll().executeTakeFirstOrThrow();
  assert.notEqual(row.status, "ready", "a half-downloaded version must never serve");
  assert.equal(row.status, "pending", "and must be left in a state the next sync retries");

  for (const content of Object.values(files)) {
    assert.equal(await blobs.has(sha256(content)), false);
  }

  const logs = await db.selectFrom("deploy_log").selectAll().where("event_id", "=", event.id).execute();
  assert.ok(logs.some((entry) => entry.msg === "deploy_interrupted"));
});
