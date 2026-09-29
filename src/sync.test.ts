import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { text } from "node:stream/consumers";
import { after, test } from "node:test";
import type { NostrEvent } from "@nostrify/nostrify";
import type { Kysely } from "kysely";
import { finalizeEvent, generateSecretKey, getPublicKey } from "nostr-tools";
import { BlobStore } from "./blobs.ts";
import { HostBuilder, currentBuild } from "./builds.ts";
import { openDatabase } from "./db/index.ts";
import type { Database } from "./db/schema.ts";
import { Logger } from "./log.ts";
import type { NostrService } from "./nostr.ts";
import { NAMED_SITE_KIND } from "./nsite-address.ts";
import { ServeCache } from "./serve-cache.ts";
import { SettingsService } from "./settings.ts";
import { activeEvent, findHost, resolveSitePath } from "./sites.ts";
import { SyncWorker } from "./sync.ts";
import { serve } from "./test/serve.ts";

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
function blossom(files: Record<string, string>, opts: { failFirst?: number } = {}) {
  const requests: string[] = [];
  let failures = opts.failFirst ?? 0;

  const server = serve((request) => {
    const hash = new URL(request.url).pathname.slice(1);
    requests.push(hash);

    if (failures > 0) {
      failures--;
      return new Response("try again", { status: 500 });
    }

    const body = files[hash];
    if (body === undefined) return new Response("not found", { status: 404 });

    return new Response(body);
  });

  cleanups.push(() => server.close());

  return { url: `${server.url}/`, requests };
}

function manifest(
  files: Record<string, string>,
  opts: { servers?: string[]; createdAt?: number; identifier?: string } = {},
): NostrEvent {
  const tags: string[][] = [["d", opts.identifier ?? "docs"]];
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
    getAuthorsRelays: async () => new Map(),
    getAuthorServers: async () => [],
    fetchManifest: async () => undefined,
    blobSources: async (event: NostrEvent, fallbacks: string[]) => [
      ...event.tags.filter((tag) => tag[0] === "server").map((tag) => tag[1]),
      ...fallbacks,
    ],
    clearCaches: () => {},
    close: async () => {},
  } as unknown as NostrService;
}

async function harness(opts: { retryBackoff?: number } = {}) {
  const dir = await mkdtemp(join(tmpdir(), "npanel-sync-"));
  const db: Kysely<Database> = await openDatabase(":memory:");
  // The test Blossom servers listen on loopback, which is exactly what the
  // SSRF guard exists to refuse.
  const blobs = new BlobStore(join(dir, "blobs"), { isPublic: async () => true });
  const settings = await SettingsService.load(db);
  // The default fallbacks are real Blossom servers; no test may touch them.
  await settings.update({ blossom_fallback_servers: [] }, PUBKEY);
  const log = new Logger("error");
  const serveCache = new ServeCache(db);
  const builder = new HostBuilder({
    db,
    blobs,
    settings,
    servingChanged: () => serveCache.clear(),
    log,
    retryBackoff: 0,
  });
  // Retries are instant unless a test is about the backoff itself.
  const worker = new SyncWorker({
    db,
    blobs,
    nostr: fakeNostr(),
    settings,
    builder,
    log,
    retryBackoff: opts.retryBackoff ?? 0,
  });

  cleanups.push(async () => {
    await worker.stop();
    await builder.stop();
    await db.destroy();
    await rm(dir, { recursive: true, force: true });
  });

  await addHost(db, "docs");

  await worker.start();

  return { db, blobs, settings, worker, builder, serveCache };
}

/** Point a hostname at one of PUBKEY's sites. */
async function addHost(db: Kysely<Database>, identifier: string): Promise<void> {
  await db
    .insertInto("hosts")
    .values({
      hostname: `${identifier}.example.com`,
      pubkey: PUBKEY,
      kind: NAMED_SITE_KIND,
      identifier,
      pinned_event_id: null,
      enabled: 1,
      created_by: PUBKEY,
      created_at: 1,
      updated_at: 1,
    })
    .execute();
}

/** Poll until `check` passes, failing the test if it never does. */
async function eventually(check: () => Promise<boolean>, message: string): Promise<void> {
  const deadline = Date.now() + 2000;

  while (Date.now() < deadline) {
    if (await check()) return;
    await new Promise((resolve) => setTimeout(resolve, 10));
  }

  assert.fail(message);
}

test("a finished deploy is built and served without waiting out the cache", async () => {
  const files = { "/index.html": "<h1>fresh</h1>" };
  const server = blossom({ [sha256(files["/index.html"])]: files["/index.html"] });
  const { worker, builder, serveCache } = await harness();

  // Prime the cache while the host has nothing to serve.
  const before = await serveCache.lookup("docs.example.com");
  assert.ok(before);
  assert.equal(before.version, undefined);

  const event = manifest(files, { servers: [server.url] });
  await worker.ingest(event);
  await worker.whenIdle();
  await builder.whenIdle();

  // No TTL to wait out: the build going live cleared the cache.
  const after = await serveCache.lookup("docs.example.com");
  assert.equal(after?.version?.eventId, event.id);
});

test("downloads every blob before a version becomes servable", async () => {
  const files = { "/index.html": "<h1>hi</h1>", "/style.css": "body{}" };
  const server = blossom({
    [sha256(files["/index.html"])]: files["/index.html"],
    [sha256(files["/style.css"])]: files["/style.css"],
  });

  const { db, blobs, worker, builder } = await harness();
  const event = manifest(files, { servers: [server.url] });

  await worker.ingest(event);
  await worker.whenIdle();
  await builder.whenIdle();

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

  const build = await currentBuild(db, host.id, "clearnet");
  assert.ok(build);
  assert.equal(build.event_id, event.id);
  const resolved = await resolveSitePath(db, build.id, "/");
  assert.equal(resolved?.path, "/index.html");
  // Too small to compress, so the deploy recorded the absence: sizes come
  // with the lookup, and serving needs no stat.
  assert.deepEqual(resolved?.variants, {
    identity: files["/index.html"].length,
    br: undefined,
    gzip: undefined,
  });

  const recorded = await db
    .selectFrom("blobs")
    .selectAll()
    .where("sha256", "in", Object.values(files).map(sha256))
    .execute();
  assert.equal(recorded.length, 2);
});

test("a version whose blobs are unavailable fails and does not replace the live one", async () => {
  const working = { "/index.html": "v1" };
  const server = blossom({ [sha256(working["/index.html"])]: working["/index.html"] });

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

test("a version published after a newer one is never downloaded", async () => {
  const newer = { "/index.html": "v2" };
  const older = { "/index.html": "v1" };
  const server = blossom({
    [sha256(newer["/index.html"])]: newer["/index.html"],
    [sha256(older["/index.html"])]: older["/index.html"],
  });

  const { db, worker } = await harness();

  const second = manifest(newer, { servers: [server.url], createdAt: 2000 });
  const first = manifest(older, { servers: [server.url], createdAt: 1000 });
  await worker.ingest(second);
  await worker.ingest(first);
  await worker.whenIdle();

  const rows = await db.selectFrom("site_events").select(["event_id", "status", "error"]).execute();
  const status = Object.fromEntries(rows.map((row) => [row.event_id, row]));

  assert.equal(status[second.id].status, "ready");
  assert.equal(status[first.id].status, "failed");
  assert.match(status[first.id].error ?? "", /newer version was published/);
  assert.ok(!server.requests.includes(sha256(older["/index.html"])), "the superseded blob was never fetched");
});

test("a version listing more files than allowed is refused before downloading anything", async () => {
  const files = { "/index.html": "<h1>hi</h1>", "/style.css": "body{}" };
  const server = blossom({
    [sha256(files["/index.html"])]: files["/index.html"],
    [sha256(files["/style.css"])]: files["/style.css"],
  });

  const { db, settings, worker } = await harness();
  await settings.update({ max_site_files: 1 }, PUBKEY);

  await worker.ingest(manifest(files, { servers: [server.url] }));
  await worker.whenIdle();

  const row = await db.selectFrom("site_events").selectAll().executeTakeFirstOrThrow();
  assert.equal(row.status, "failed");
  assert.match(row.error ?? "", /2 files, more than the 1/);
  assert.deepEqual(server.requests, []);

  // No retry was queued: the answer would be the same.
  const logs = await db.selectFrom("deploy_log").select("msg").execute();
  assert.ok(logs.some((entry) => entry.msg === "too_many_files"));
  assert.ok(!logs.some((entry) => entry.msg === "deploy_retrying"));

  // Raising the limit is what a refused deploy was waiting for.
  await settings.update({ max_site_files: 10 }, PUBKEY);
  await eventually(
    async () => (await db.selectFrom("site_events").select("status").executeTakeFirstOrThrow()).status === "ready",
    "the version deploys once the limit allows it",
  );
});

test("a version larger than allowed stops downloading and fails", async () => {
  const files = {
    "/a.bin": "a".repeat(700 * 1024),
    "/b.bin": "b".repeat(700 * 1024),
  };
  const server = blossom({
    [sha256(files["/a.bin"])]: files["/a.bin"],
    [sha256(files["/b.bin"])]: files["/b.bin"],
  });

  const { db, settings, worker } = await harness();
  await settings.update({ max_site_size: 1024 * 1024 }, PUBKEY);

  await worker.ingest(manifest(files, { servers: [server.url] }));
  await worker.whenIdle();

  const row = await db.selectFrom("site_events").selectAll().executeTakeFirstOrThrow();
  assert.equal(row.status, "failed");
  assert.match(row.error ?? "", /larger than the 1 MB/);

  const logs = await db.selectFrom("deploy_log").select("msg").execute();
  assert.ok(logs.some((entry) => entry.msg === "site_too_large"));
  assert.ok(!logs.some((entry) => entry.msg === "deploy_retrying"));
});

test("retries a failed deploy, keeping what the last attempt downloaded", async () => {
  const files = { "/index.html": "<h1>hi</h1>", "/style.css": "body{}" };
  const server = blossom(
    {
      [sha256(files["/index.html"])]: files["/index.html"],
      [sha256(files["/style.css"])]: files["/style.css"],
    },
    { failFirst: 1 },
  );

  const { db, worker } = await harness();

  await worker.ingest(manifest(files, { servers: [server.url] }));
  await worker.whenIdle();

  const row = await db.selectFrom("site_events").selectAll().executeTakeFirstOrThrow();
  assert.equal(row.status, "ready");
  assert.equal(row.error, null);

  assert.equal(server.requests.length, 3, "the second attempt only re-fetched the blob that failed");

  const logs = await db.selectFrom("deploy_log").selectAll().execute();
  assert.ok(logs.some((entry) => entry.msg === "deploy_retrying"));
});

test("gives up after the configured number of attempts", async () => {
  const files = { "/index.html": "nowhere to be found" };
  const server = blossom({});

  const { db, settings, worker } = await harness();
  await settings.update({ deploy_attempts: 2 }, PUBKEY);

  await worker.ingest(manifest(files, { servers: [server.url] }));
  await worker.whenIdle();

  const hash = sha256(files["/index.html"]);
  assert.deepEqual(server.requests, [hash, hash], "two attempts, no more");

  const row = await db.selectFrom("site_events").selectAll().executeTakeFirstOrThrow();
  assert.equal(row.status, "failed");
  assert.match(row.error ?? "", /Attempt 2 of 2\. The next sync will try again\./);

  const logs = await db.selectFrom("deploy_log").selectAll().execute();
  assert.equal(logs.filter((entry) => entry.msg === "deploy_retrying").length, 1);
});

test("waits before trying again", async () => {
  const files = { "/index.html": "nowhere to be found" };
  const server = blossom({});

  const { db, settings, worker } = await harness({ retryBackoff: 100 });
  await settings.update({ deploy_attempts: 2 }, PUBKEY);

  const started = Date.now();
  await worker.ingest(manifest(files, { servers: [server.url] }));
  await worker.whenIdle();

  assert.equal(server.requests.length, 2);
  assert.ok(Date.now() - started >= 100, "the second attempt waited out the backoff");

  const row = await db.selectFrom("site_events").selectAll().executeTakeFirstOrThrow();
  assert.equal(row.status, "failed");
});

test("a retry waiting out its backoff holds up no other site", async () => {
  const files = { "/index.html": "a site that works" };
  const server = blossom({ [sha256(files["/index.html"])]: files["/index.html"] });

  // Long enough that the test can only pass if the queue steps over the retry.
  const { db, settings, worker } = await harness({ retryBackoff: 60_000 });
  await settings.update({ deploy_attempts: 2 }, PUBKEY);
  await addHost(db, "blog");

  await worker.ingest(manifest({ "/index.html": "nowhere to be found" }, { servers: [server.url] }));
  const blog = manifest(files, { servers: [server.url], identifier: "blog" });
  await worker.ingest(blog);

  await eventually(async () => {
    const row = await db.selectFrom("site_events").selectAll().where("event_id", "=", blog.id).executeTakeFirst();
    return row?.status === "ready";
  }, "the second site never deployed");

  const waiting = await db
    .selectFrom("site_events")
    .selectAll()
    .where("event_id", "!=", blog.id)
    .executeTakeFirstOrThrow();
  assert.equal(waiting.status, "failed", "while the first site is still waiting to be retried");
});

test("shutdown doesn't wait out a pending retry", async () => {
  const server = blossom({});

  const { worker } = await harness({ retryBackoff: 60_000 });

  await worker.ingest(manifest({ "/index.html": "nowhere to be found" }, { servers: [server.url] }));
  await eventually(async () => server.requests.length === 1, "the first attempt never ran");

  const started = Date.now();
  await worker.stop();
  await worker.whenIdle();

  assert.ok(Date.now() - started < 1000, "stopping abandons the backoff instead of sitting through it");
});

test("does not retry when attempts are set to one", async () => {
  const files = { "/index.html": "nowhere to be found" };
  const server = blossom({});

  const { db, settings, worker } = await harness();
  await settings.update({ deploy_attempts: 1 }, PUBKEY);

  await worker.ingest(manifest(files, { servers: [server.url] }));
  await worker.whenIdle();

  assert.deepEqual(server.requests, [sha256(files["/index.html"])]);

  const row = await db.selectFrom("site_events").selectAll().executeTakeFirstOrThrow();
  assert.equal(row.status, "failed");
  assert.doesNotMatch(row.error ?? "", /Attempt/, "one attempt needs no counting");
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
  const server = blossom({ [sha256(files["/index.html"])]: files["/index.html"] });

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
  const server = blossom({});

  const { db, blobs, worker } = await harness();
  await blobs.put(hash, new TextEncoder().encode(files["/index.html"]));

  await worker.ingest(manifest(files, { servers: [server.url] }));
  await worker.whenIdle();

  const row = await db.selectFrom("site_events").selectAll().executeTakeFirstOrThrow();
  assert.equal(row.status, "ready");
  assert.deepEqual(server.requests, [], "no download was attempted");
});

/** Long enough to clear the size floor, repetitive enough to compress. */
const HTML = "<p>compress me</p>".repeat(100);
const JPEG = "<p>not really a jpeg</p>".repeat(100);

test("a deploy compresses the files worth compressing", async () => {
  const files = { "/index.html": HTML, "/photo.jpg": JPEG };
  const server = blossom({ [sha256(HTML)]: HTML, [sha256(JPEG)]: JPEG });

  const { db, blobs, worker } = await harness();
  await worker.ingest(manifest(files, { servers: [server.url] }));
  await worker.whenIdle();

  const html = await blobs.variants(sha256(HTML));
  assert.ok(html?.br && html.br < html.identity);
  assert.ok(html?.gzip && html.gzip < html.identity);

  // Compressible bytes, but nobody will ever be served them as text: the
  // gate is what the file is served as, and a second pass over a real JPEG
  // would be deploy-time CPU spent for nothing.
  const jpeg = await blobs.variants(sha256(JPEG));
  assert.equal(jpeg?.br, undefined);
  assert.equal(jpeg?.gzip, undefined);

  // What the deploy settled is recorded, so serving reads sizes off the row
  // rather than the disk — including the settled absence, which is -1 and
  // not NULL: NULL would send every request for the JPEG back to statting.
  const rows = await db.selectFrom("blobs").selectAll().execute();
  const htmlRow = rows.find((row) => row.sha256 === sha256(HTML));
  assert.equal(htmlRow?.br_size, html?.br);
  assert.equal(htmlRow?.gzip_size, html?.gzip);

  const jpegRow = rows.find((row) => row.sha256 === sha256(JPEG));
  assert.equal(jpegRow?.br_size, -1);
  assert.equal(jpegRow?.gzip_size, -1);
});

test("a blob already on disk still gets compressed", async () => {
  const files = { "/index.html": HTML };
  const server = blossom({});

  const { blobs, worker } = await harness();
  await blobs.put(sha256(HTML), new TextEncoder().encode(HTML));

  await worker.ingest(manifest(files, { servers: [server.url] }));
  await worker.whenIdle();

  // Nothing was downloaded, so this is the path that carries sites deployed
  // before npanel compressed anything.
  assert.deepEqual(server.requests, []);
  assert.ok((await blobs.variants(sha256(HTML)))?.br);
});

test("a deploy interrupted by shutdown is never marked ready", async () => {
  const files: Record<string, string> = {};
  // More files than BLOB_CONCURRENCY, so some are still queued when we stop.
  for (let i = 0; i < 8; i++) {
    files[`/file-${i}.txt`] = `content ${i}`;
  }

  // A server that accepts requests and never answers them: the promise the
  // handler returns is never resolved.
  const server = serve(() => new Promise<Response>(() => {}));
  cleanups.push(() => server.close());

  const { db, blobs, worker } = await harness();
  const event = manifest(files, { servers: [`${server.url}/`] });

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
