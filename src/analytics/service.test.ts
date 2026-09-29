import assert from "node:assert/strict";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { after, test } from "node:test";
import { openDatabase } from "../db/index.ts";
import { openSqlite } from "../db/sqlite.ts";
import { Logger } from "../log.ts";
import { SettingsService } from "../settings.ts";
import { MAX_EVENT_BODY } from "./protocol.ts";
import { AnalyticsService } from "./service.ts";

const cleanups: (() => Promise<void>)[] = [];

after(async () => {
  for (const cleanup of cleanups) await cleanup();
});

const FIREFOX = "Mozilla/5.0 (X11; Linux x86_64; rv:130.0) Gecko/20100101 Firefox/130.0";

async function setup() {
  const dir = await mkdtemp(join(tmpdir(), "npanel-analytics-"));
  const db = await openDatabase(":memory:");

  const { id } = await db
    .insertInto("hosts")
    .values({
      hostname: "docs.example.com",
      pubkey: "a".repeat(64),
      kind: 15128,
      identifier: "",
      pinned_event_id: null,
      enabled: 1,
      analytics: 1,
      created_by: "a".repeat(64),
      created_at: 1,
      updated_at: 1,
    })
    .returning("id")
    .executeTakeFirstOrThrow();

  const service = new AnalyticsService({
    db,
    settings: await SettingsService.load(db),
    log: new Logger("error"),
    config: { port: 0, address: "127.0.0.1", dataDir: dir, logLevel: "error", downloadGeo: false },
  });

  cleanups.push(async () => {
    await service.stop();
    await db.destroy();
    await rm(dir, { recursive: true, force: true });
  });

  /** Every event the thread wrote, read after it has stopped and flushed. */
  const recorded = async () => {
    await service.stop();
    const analytics = await openSqlite(join(dir, "analytics.sqlite3"));
    const rows = analytics.prepare("SELECT host_id, pathname FROM events ORDER BY pathname").all();
    analytics.close();
    return rows;
  };

  return { db, hostId: id, service, recorded };
}

const event = (path: string) =>
  JSON.stringify({ n: "pageview", u: `https://docs.example.com${path}`, d: "docs.example.com" });

test("the analytics thread records events sent to its own port", async () => {
  const { service, recorded, hostId } = await setup();
  await service.start();

  const response = await fetch(`http://127.0.0.1:${service.port}/api/event`, {
    method: "POST",
    headers: { "content-type": "text/plain", "user-agent": FIREFOX, "x-forwarded-for": "203.0.113.7" },
    body: event("/direct"),
  });

  assert.equal(response.status, 202);
  assert.equal(await response.text(), "ok");
  assert.equal(response.headers.get("access-control-allow-origin"), "*");

  const preflight = await fetch(`http://127.0.0.1:${service.port}/.npanel/api/event`, { method: "OPTIONS" });
  assert.equal(preflight.status, 204);

  assert.deepEqual(await recorded(), [{ host_id: hostId, pathname: "/direct" }]);
});

test("an event that reached the main thread is handed to the analytics thread", async () => {
  const { service, recorded, hostId } = await setup();
  await service.start();

  const response = await service.event(
    new Request("http://abc.onion/.npanel/api/event", {
      method: "POST",
      headers: { "user-agent": FIREFOX },
      body: event("/over-tor"),
    }),
  );

  assert.equal(response.status, 202);
  assert.deepEqual(await recorded(), [{ host_id: hostId, pathname: "/over-tor" }]);
});

test("an oversized event reaching the main thread is refused without being read whole", async () => {
  const { service, recorded } = await setup();
  await service.start();

  const declared = await service.event(
    new Request("http://abc.onion/.npanel/api/event", {
      method: "POST",
      headers: { "content-length": String(MAX_EVENT_BODY + 1) },
      body: "x".repeat(MAX_EVENT_BODY + 1),
    }),
  );
  assert.equal(declared.status, 413);
  assert.equal(declared.headers.get("access-control-allow-origin"), "*");

  // No length declared: the stream is cut off once it passes the limit.
  let pulled = 0;
  const endless = new ReadableStream<Uint8Array>({
    pull(controller) {
      pulled++;
      controller.enqueue(new Uint8Array(16 * 1024));
    },
  });
  const streamed = await service.event(
    new Request("http://abc.onion/.npanel/api/event", { method: "POST", body: endless, duplex: "half" } as RequestInit),
  );
  assert.equal(streamed.status, 413);
  assert.ok(pulled <= MAX_EVENT_BODY / (16 * 1024) + 2, "reading stops at the limit");

  assert.deepEqual(await recorded(), []);
});

test("a site that stops counting is dropped from then on", async () => {
  const { db, service, recorded, hostId } = await setup();
  await service.start();

  await db.updateTable("hosts").set({ analytics: 0 }).where("id", "=", hostId).execute();
  await service.hostsChanged();

  const response = await fetch(`http://127.0.0.1:${service.port}/api/event`, {
    method: "POST",
    headers: { "user-agent": FIREFOX },
    body: event("/after"),
  });

  assert.equal(response.headers.get("x-plausible-dropped"), "1");
  assert.deepEqual(await recorded(), []);
});

test("an analytics thread that isn't running answers events with a 503", async () => {
  const { service } = await setup();

  const response = await service.event(new Request("http://x/.npanel/api/event", { method: "POST", body: "{}" }));
  assert.equal(response.status, 503);
});
