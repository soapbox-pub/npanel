import assert from "node:assert/strict";
import { after, test } from "node:test";
import type { Kysely } from "kysely";
import { openDatabase } from "./db/index.ts";
import type { Database } from "./db/schema.ts";
import { SitePaths } from "./site-paths.ts";

const databases: Kysely<Database>[] = [];

after(async () => {
  for (const db of databases) await db.destroy();
});

const PUBKEY = "3bf0c63fcb93463407af97a5e5ee64fa883d107ef9e558472c4eb9aaaefa459d";
const EVENT_ID = "a".repeat(64);
const HASH_A = "a".repeat(64);
const HASH_C = "c".repeat(64);

/** The one version these tests resolve against, as a host that doesn't merge sees it. */
const VERSION = { eventIds: [EVENT_ID], filesKey: EVENT_ID };

/**
 * A database holding one version with the given path map.
 *
 * `sizes` decides whether the deploy recorded the blobs' sizes, which is what
 * makes a resolved path cacheable at all.
 */
async function version(paths: Record<string, string>, opts: { sizes?: boolean } = {}): Promise<Kysely<Database>> {
  const db = await openDatabase(":memory:");
  databases.push(db);

  await db
    .insertInto("site_events")
    .values({
      event_id: EVENT_ID,
      pubkey: PUBKEY,
      kind: 15128,
      identifier: "",
      created_at: 1,
      event: "{}",
      status: "ready",
      files_total: Object.keys(paths).length,
      files_ready: Object.keys(paths).length,
      error: null,
      fetched_at: 1,
      ready_at: 1,
    })
    .execute();

  for (const [path, sha256] of Object.entries(paths)) {
    await db.insertInto("site_files").values({ event_id: EVENT_ID, path, sha256 }).execute();

    if (opts.sizes !== false) {
      await db
        .insertInto("blobs")
        .values({ sha256, size: 10, br_size: -1, gzip_size: -1, seen_at: 1 })
        .onConflict((oc) => oc.column("sha256").doNothing())
        .execute();
    }
  }

  return db;
}

/** Delete a version's file list, so anything answered afterwards came from the cache. */
async function forget(db: Kysely<Database>): Promise<void> {
  await db.deleteFrom("site_files").where("event_id", "=", EVENT_ID).execute();
}

test("a resolved path is answered again without the database", async () => {
  const db = await version({ "/index.html": HASH_A });
  const paths = new SitePaths(db);

  const first = await paths.resolve(VERSION, "/", { spa: false });
  assert.deepEqual(first, {
    path: "/index.html",
    sha256: HASH_A,
    status: 200,
    variants: { identity: 10, br: undefined, gzip: undefined },
  });

  await forget(db);

  assert.deepEqual(await paths.resolve(VERSION, "/", { spa: false }), first);
});

test("a path the version has no file for is cached too", async () => {
  const db = await version({ "/index.html": HASH_A });
  const paths = new SitePaths(db);

  assert.equal(await paths.resolve(VERSION, "/nope.html", { spa: false }), undefined);

  // The row is back, but the answer that it isn't there was already settled.
  await db.insertInto("site_files").values({ event_id: EVENT_ID, path: "/nope.html", sha256: HASH_C }).execute();

  assert.equal(await paths.resolve(VERSION, "/nope.html", { spa: false }), undefined);
});

test("hosts that disagree about SPA get their own answers", async () => {
  const db = await version({ "/index.html": HASH_A, "/404.html": HASH_C });
  const paths = new SitePaths(db);

  assert.equal((await paths.resolve(VERSION, "/about", { spa: true }))?.status, 200);
  assert.equal((await paths.resolve(VERSION, "/about", { spa: false }))?.status, 404);

  await forget(db);

  assert.equal((await paths.resolve(VERSION, "/about", { spa: true }))?.status, 200);
  assert.equal((await paths.resolve(VERSION, "/about", { spa: false }))?.status, 404);
});

test("a blob whose sizes were never recorded is not cached", async () => {
  const db = await version({ "/index.html": HASH_A }, { sizes: false });
  const paths = new SitePaths(db);

  const first = await paths.resolve(VERSION, "/", { spa: false });
  assert.deepEqual(first, { path: "/index.html", sha256: HASH_A, status: 200 });

  // Recorded late, as `compress-blobs` does. Nothing invalidates this cache,
  // so the only thing that can pick the sizes up is not having cached the row.
  await db.insertInto("blobs").values({ sha256: HASH_A, size: 10, br_size: 4, gzip_size: -1, seen_at: 1 }).execute();

  assert.deepEqual(await paths.resolve(VERSION, "/", { spa: false }), {
    path: "/index.html",
    sha256: HASH_A,
    status: 200,
    variants: { identity: 10, br: 4, gzip: undefined },
  });
});

test("the cache is bounded, and drops the least recently used path", async () => {
  const db = await version({ "/index.html": HASH_A });
  const paths = new SitePaths(db);

  assert.ok(await paths.resolve(VERSION, "/", { spa: false }));

  // Every one of these is a distinct key, so together they are more than the
  // cache holds and the entry above is the oldest of them.
  for (let i = 0; i < 5000; i++) {
    await paths.resolve(VERSION, `/spray/${i}`, { spa: false });
  }

  await forget(db);

  assert.equal(await paths.resolve(VERSION, "/", { spa: false }), undefined, "the oldest entry survived eviction");
});
