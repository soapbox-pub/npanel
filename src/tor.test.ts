import assert from "node:assert/strict";
import { mkdtemp } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { after, describe, it } from "node:test";
import type { Kysely } from "kysely";
import { BlobStore } from "./blobs.ts";
import { HostBuilder } from "./builds.ts";
import { openDatabase } from "./db/index.ts";
import type { Database } from "./db/schema.ts";
import { Logger } from "./log.ts";
import { ServeCache } from "./serve-cache.ts";
import { createSiteServer } from "./serve-site.ts";
import { SettingsService } from "./settings.ts";
import { SiteCsp } from "./site-csp.ts";
import { type FakeTorDaemon, startFakeTorDaemon } from "./test/fake-tor.ts";
import { TorService } from "./tor.ts";

const ACTOR = "f".repeat(64);

/** Wait for something a reconnect will bring about, rather than for a delay. */
async function waitFor(condition: () => boolean): Promise<void> {
  for (let attempt = 0; attempt < 200; attempt += 1) {
    if (condition()) return;
    await new Promise((resolve) => setTimeout(resolve, 10));
  }
  assert.fail("Timed out waiting for Tor to catch up.");
}

interface Setup {
  db: Kysely<Database>;
  tor: TorService;
  daemon: FakeTorDaemon;
  addHost(hostname: string, enabled?: number): Promise<number>;
  cleanup(): Promise<void>;
}

async function setup(): Promise<Setup> {
  const dir = await mkdtemp(join(tmpdir(), "npanel-tor-"));
  const db = await openDatabase(":memory:");
  const settings = await SettingsService.load(db);
  const log = new Logger("error");
  const blobs = new BlobStore(join(dir, "blobs"));
  const daemon = await startFakeTorDaemon();

  await settings.update({ tor_enabled: true, tor_control_url: `tcp://127.0.0.1:${daemon.port}` }, ACTOR);

  const serveCache = new ServeCache(db);
  const builder = new HostBuilder({ db, blobs, settings, serveCache, log, retryBackoff: 0 });
  const tor = new TorService({
    db,
    settings,
    log,
    serveSite: createSiteServer({ db, blobs, siteCsp: new SiteCsp(db, blobs, log), serveCache, log }),
    serveCache,
    builder,
    // No test should sit through a reconnect's backoff.
    retryBackoff: 0,
  });

  return {
    db,
    tor,
    daemon,
    async addHost(hostname, enabled = 1) {
      const row = await db
        .insertInto("hosts")
        .values({
          hostname,
          pubkey: "a".repeat(64),
          kind: 15128,
          identifier: "",
          pinned_event_id: null,
          enabled,
          created_by: ACTOR,
          created_at: 0,
          updated_at: 0,
        })
        .returning("id")
        .executeTakeFirstOrThrow();
      return row.id;
    },
    async cleanup() {
      await tor.stop();
      await daemon.close();
      await db.destroy();
    },
  };
}

describe("TorService", () => {
  const open: Setup[] = [];

  after(async () => {
    await Promise.all(open.map((one) => one.cleanup()));
  });

  async function start(): Promise<Setup> {
    const one = await setup();
    open.push(one);
    return one;
  }

  it("publishes an onion for every enabled host, and keeps the key", async () => {
    const one = await start();
    await one.addHost("a.example.com");
    await one.addHost("b.example.com");

    await one.tor.start();

    assert.equal(one.daemon.onions.size, 2);

    const stored = await one.db.selectFrom("onion_services").selectAll().execute();
    assert.equal(stored.length, 2);
    for (const row of stored) {
      assert.match(row.address, /^[a-z2-7]{56}\.onion$/);
      assert.ok(one.daemon.onions.has(row.address.replace(".onion", "")));
    }
  });

  it("leaves a disabled host unpublished", async () => {
    const one = await start();
    await one.addHost("on.example.com");
    await one.addHost("off.example.com", 0);

    await one.tor.start();

    assert.equal(one.daemon.onions.size, 1);
  });

  it("unpublishes a host that is disabled later, but keeps its address", async () => {
    const one = await start();
    const id = await one.addHost("a.example.com");
    await one.tor.start();

    const before = await one.db.selectFrom("onion_services").selectAll().executeTakeFirstOrThrow();

    await one.db.updateTable("hosts").set({ enabled: 0 }).where("id", "=", id).execute();
    await one.tor.hostsChanged();

    assert.equal(one.daemon.onions.size, 0);

    // Re-enabling has to bring back the same address: it is what anyone who
    // bookmarked the site has.
    await one.db.updateTable("hosts").set({ enabled: 1 }).where("id", "=", id).execute();
    await one.tor.hostsChanged();

    const after = await one.db.selectFrom("onion_services").selectAll().executeTakeFirstOrThrow();
    assert.equal(after.address, before.address);
    assert.ok(one.daemon.onions.has(before.address.replace(".onion", "")));
  });

  it("forgets the key when the host is deleted", async () => {
    const one = await start();
    const id = await one.addHost("a.example.com");
    await one.tor.start();

    await one.db.deleteFrom("hosts").where("id", "=", id).execute();
    await one.tor.hostsChanged();

    assert.equal(one.daemon.onions.size, 0);
    assert.deepEqual(await one.db.selectFrom("onion_services").selectAll().execute(), []);
  });

  it("republishes everything from stored keys after a reconnect", async () => {
    const one = await start();
    await one.addHost("a.example.com");
    await one.tor.start();

    const before = await one.db.selectFrom("onion_services").selectAll().executeTakeFirstOrThrow();
    assert.equal(one.daemon.onions.size, 1);

    // A control connection dropping unregisters every service with it.
    one.daemon.onions.clear();
    one.daemon.drop();

    await waitFor(() => one.daemon.onions.size === 1);

    assert.ok(one.daemon.onions.has(before.address.replace(".onion", "")));
    assert.equal((await one.db.selectFrom("onion_services").selectAll().execute()).length, 1);
  });

  it("reports its status for the dashboard", async () => {
    const one = await start();
    await one.addHost("a.example.com");
    await one.tor.start();

    assert.deepEqual(one.tor.status(), { enabled: true, connected: true, services: 1, error: "" });
  });

  describe("the listener Tor forwards to", () => {
    it("serves onion addresses and nothing else, whatever headers a visitor sends", async () => {
      const one = await start();
      await one.addHost("a.example.com");
      await one.tor.start();

      const onion = await one.db.selectFrom("onion_services").selectAll().executeTakeFirstOrThrow();

      // The site has no ready version, so 503 is what its own hostname would
      // answer — which is the point: the onion resolved to that host and not to
      // whatever the header asked for.
      const site = await fetch(`http://127.0.0.1:${one.tor.port}/`, {
        headers: { host: onion.address, "x-forwarded-host": "panel.example.com" },
      });
      assert.equal(site.status, 503);

      // And the reverse: nothing but an onion address is served here, however
      // the request dresses itself up.
      const dashboard = await fetch(`http://127.0.0.1:${one.tor.port}/`, {
        headers: { host: "panel.example.com", "x-forwarded-host": onion.address },
      });
      assert.equal(dashboard.status, 404);
    });

    it("is bound only while Tor is enabled", async () => {
      const one = await start();
      await one.tor.start();
      assert.ok(one.tor.port);

      await one.tor.stop();
      assert.equal(one.tor.port, undefined);
    });
  });
});
