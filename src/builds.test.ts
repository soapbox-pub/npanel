import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { after, test } from "node:test";
import { BlobStore } from "./blobs.ts";
import { HostBuilder, currentBuild } from "./builds.ts";
import { Logger } from "./log.ts";
import { type Harness, createHarness } from "./test/harness.ts";

const harnesses: Harness[] = [];

after(async () => {
  for (const harness of harnesses) await harness.cleanup();
});

const PUBKEY = "3bf0c63fcb93463407af97a5e5ee64fa883d107ef9e558472c4eb9aaaefa459d";
const OTHER = "4bf0c63fcb93463407af97a5e5ee64fa883d107ef9e558472c4eb9aaaefa459d";

function sha256(value: string): string {
  return createHash("sha256").update(value).digest("hex");
}

async function setup(): Promise<{ harness: Harness; hostId: number }> {
  const harness = await createHarness();
  harnesses.push(harness);

  const { id } = await harness.db
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

  return { harness, hostId: id };
}

/** Record a ready version of a site, with its blobs on disk. */
async function deploy(
  harness: Harness,
  eventId: string,
  createdAt: number,
  files: Record<string, string>,
  pubkey = PUBKEY,
): Promise<void> {
  await harness.db
    .insertInto("site_events")
    .values({
      event_id: eventId,
      pubkey,
      kind: 15128,
      identifier: "",
      created_at: createdAt,
      event: "{}",
      status: "ready",
      files_total: Object.keys(files).length,
      files_ready: Object.keys(files).length,
      error: null,
      fetched_at: createdAt,
      ready_at: createdAt,
    })
    .execute();

  for (const [path, content] of Object.entries(files)) {
    const hash = sha256(content);
    await harness.db.insertInto("site_files").values({ event_id: eventId, path, sha256: hash }).execute();
    await harness.blobs.put(hash, new TextEncoder().encode(content));
  }
}

async function page(harness: Harness, path = "/"): Promise<{ status: number; body: string }> {
  const response = await harness.site("docs.example.com", path);
  return { status: response.status, body: await response.text() };
}

test("a host serves nothing until it has been built", async () => {
  const { harness } = await setup();
  await deploy(harness, "a".repeat(64), 1000, { "/index.html": "v1" });

  // Ready, but no build of it: the same 503 as a site that never deployed.
  assert.equal((await page(harness)).status, 503);

  await harness.builder.hostsChanged();

  assert.deepEqual(await page(harness), { status: 200, body: "v1" });
});

test("a new version goes live only once its build is ready", async () => {
  const { harness } = await setup();
  await deploy(harness, "a".repeat(64), 1000, { "/index.html": "v1" });
  await harness.builder.hostsChanged();

  await deploy(harness, "b".repeat(64), 2000, { "/index.html": "v2" });
  harness.serveCache.clear();

  // The version is ready, but the host's build is still of the last one.
  assert.equal((await page(harness)).body, "v1");

  await harness.builder.hostsChanged();
  assert.equal((await page(harness)).body, "v2");
});

test("a host whose inputs are unchanged is not rebuilt", async () => {
  const { harness, hostId } = await setup();
  await deploy(harness, "a".repeat(64), 1000, { "/index.html": "v1" });

  await harness.builder.hostsChanged();
  const first = await currentBuild(harness.db, hostId, "clearnet");

  // Header-only settings are read per request and are no reason to rebuild.
  await harness.db.updateTable("hosts").set({ spa: 1, csp: 0 }).where("id", "=", hostId).execute();
  await harness.builder.hostsChanged();

  assert.equal((await currentBuild(harness.db, hostId, "clearnet"))?.id, first?.id);
});

test("a rename keeps the old build serving until the new one names the new host", async () => {
  const { harness, hostId } = await setup();
  await deploy(harness, "a".repeat(64), 1000, { "/index.html": "v1" });
  await harness.builder.hostsChanged();

  await harness.db.updateTable("hosts").set({ hostname: "new.example.com" }).where("id", "=", hostId).execute();
  harness.serveCache.clear();

  const stale = await harness.site("new.example.com", "/sitemap.xml");
  assert.equal(stale.status, 200, "the renamed host is not left with nothing to serve");
  assert.match(await stale.text(), /docs\.example\.com/);

  await harness.builder.hostsChanged([hostId]);

  const fresh = await harness.site("new.example.com", "/sitemap.xml");
  assert.match(await fresh.text(), /new\.example\.com/);
});

test("a host pointed at a site with nothing deployed stops serving the old one", async () => {
  const { harness, hostId } = await setup();
  await deploy(harness, "a".repeat(64), 1000, { "/index.html": "v1" });
  await harness.builder.hostsChanged();

  await harness.db.updateTable("hosts").set({ pubkey: OTHER }).where("id", "=", hostId).execute();
  await harness.builder.hostsChanged([hostId]);

  assert.equal((await page(harness)).status, 503);
  assert.equal(await currentBuild(harness.db, hostId, "clearnet"), undefined);
});

test("a replaced build is kept one generation, then discarded", async () => {
  const { harness, hostId } = await setup();

  for (const [index, id] of ["a", "b", "c"].entries()) {
    await deploy(harness, id.repeat(64), 1000 + index, { "/index.html": `v${index}` });
    await harness.builder.hostsChanged();
  }

  const builds = await harness.db
    .selectFrom("host_builds")
    .select(["event_id", "status"])
    .where("host_id", "=", hostId)
    .orderBy("id")
    .execute();

  assert.deepEqual(builds, [
    { event_id: "b".repeat(64), status: "ready" },
    { event_id: "c".repeat(64), status: "ready" },
  ]);
});

/** A logger for a test that fails on purpose, and has no use for hearing it. */
class Quiet extends Logger {
  override error(): void {}
}

/** A store whose writes fail, standing in for a full disk. */
class FullDisk extends BlobStore {
  override async put(): Promise<number> {
    throw new Error("No space left on device");
  }
}

test("a build that fails leaves the previous one serving", async () => {
  const { harness, hostId } = await setup();
  await deploy(harness, "a".repeat(64), 1000, { "/index.html": "v1" });
  await harness.builder.hostsChanged();

  await deploy(harness, "b".repeat(64), 2000, { "/index.html": "v2" });

  const broken = new HostBuilder({
    db: harness.db,
    blobs: new FullDisk("/nonexistent"),
    settings: harness.settings,
    serveCache: harness.serveCache,
    log: new Quiet("error"),
    retryBackoff: 0,
  });
  await harness.settings.update({ deploy_attempts: 1 }, PUBKEY);
  await broken.hostsChanged();
  await broken.stop();

  assert.equal((await page(harness)).body, "v1");

  const failed = await harness.db
    .selectFrom("host_builds")
    .selectAll()
    .where("host_id", "=", hostId)
    .where("status", "=", "failed")
    .executeTakeFirst();
  assert.match(failed?.error ?? "", /No space left/);

  const log = await harness.db.selectFrom("deploy_log").select("msg").where("event_id", "=", "b".repeat(64)).execute();
  assert.ok(
    log.some((entry) => entry.msg === "build_failed"),
    "the version's log says why it isn't live",
  );
});

test("startup clears builds a crash left unfinished", async () => {
  const { harness, hostId } = await setup();
  await deploy(harness, "a".repeat(64), 1000, { "/index.html": "v1" });

  await harness.db
    .insertInto("host_builds")
    .values({
      host_id: hostId,
      kind: "clearnet",
      origin: "https://docs.example.com",
      event_id: "a".repeat(64),
      fingerprint: "",
      status: "building",
      error: null,
      last_modified: 1,
      created_at: 1,
      built_at: null,
    })
    .execute();

  await harness.builder.start();
  await harness.builder.whenIdle();

  const statuses = await harness.db.selectFrom("host_builds").select("status").execute();
  assert.deepEqual(statuses, [{ status: "ready" }]);
  assert.equal((await page(harness)).body, "v1");
});

const PAGE = "<!doctype html><html><head><title>Docs</title></head><body>hi</body></html>";
const SCRIPT_TAG = '<script type="module" src="/.npanel/script.js"></script>';

test("a host counting visits serves every page with the tracker added", async () => {
  const { harness, hostId } = await setup();
  await deploy(harness, "a".repeat(64), 1000, { "/index.html": PAGE, "/about.html": PAGE, "/app.js": "code" });
  await harness.db.updateTable("hosts").set({ analytics: 1 }).where("id", "=", hostId).execute();
  await harness.builder.hostsChanged();

  const index = await page(harness);
  assert.equal(index.body, PAGE.replace("</head>", `${SCRIPT_TAG}</head>`));
  assert.equal((await page(harness, "/about")).body, index.body);
  assert.equal((await page(harness, "/app.js")).body, "code");

  // Rewritten once, however many paths share the page, and recorded as a
  // rewrite of what the site published.
  const rows = await harness.db
    .selectFrom("host_build_files")
    .select(["path", "sha256", "source_sha256"])
    .where("path", "in", ["/index.html", "/about.html"])
    .execute();
  assert.equal(new Set(rows.map((row) => row.sha256)).size, 1);
  assert.ok(rows.every((row) => row.source_sha256 === sha256(PAGE)));
});

test("the tracker is served from the site, reporting under the site's name", async () => {
  const { harness, hostId } = await setup();
  await deploy(harness, "a".repeat(64), 1000, { "/index.html": PAGE });
  await harness.db.updateTable("hosts").set({ analytics: 1 }).where("id", "=", hostId).execute();
  await harness.builder.hostsChanged();

  const response = await harness.site("docs.example.com", "/.npanel/script.js", {
    headers: { "accept-encoding": "br" },
  });

  assert.equal(response.status, 200);
  assert.equal(response.headers.get("content-type"), "text/javascript; charset=utf-8");
  // Stored like any other file, so it goes out as its compressed copy.
  assert.equal(response.headers.get("content-encoding"), "br");

  const plain = await (await harness.site("docs.example.com", "/.npanel/script.js")).text();
  assert.match(plain, /init\(\{"domain":"docs\.example\.com","endpoint":"\/\.npanel\/api\/event"/);
});

test("a site's own policy is widened in its page and in the header it is served under", async () => {
  const { harness, hostId } = await setup();
  const declared = `<html><head><meta http-equiv="Content-Security-Policy" content="default-src 'none'"></head></html>`;
  await deploy(harness, "a".repeat(64), 1000, { "/index.html": declared });

  await harness.builder.hostsChanged();
  const before = (await harness.site("docs.example.com", "/")).headers.get("content-security-policy");
  assert.equal(before, "default-src 'none'");

  await harness.db.updateTable("hosts").set({ analytics: 1 }).where("id", "=", hostId).execute();
  await harness.builder.hostsChanged();
  harness.serveCache.clear();

  const response = await harness.site("docs.example.com", "/");
  const policy = "default-src 'none'; script-src 'self'; connect-src 'self'";
  assert.equal(response.headers.get("content-security-policy"), policy);
  assert.match(await response.text(), new RegExp(`content="${policy}"`));
});

test("turning analytics off rebuilds the site as published", async () => {
  const { harness, hostId } = await setup();
  await deploy(harness, "a".repeat(64), 1000, { "/index.html": PAGE });
  await harness.db.updateTable("hosts").set({ analytics: 1 }).where("id", "=", hostId).execute();
  await harness.builder.hostsChanged();

  await harness.db.updateTable("hosts").set({ analytics: 0 }).where("id", "=", hostId).execute();
  await harness.builder.hostsChanged();

  assert.equal((await page(harness)).body, PAGE);
  assert.equal((await page(harness, "/.npanel/script.js")).status, 404);
});
