import assert from "node:assert/strict";
import { after, test } from "node:test";
import type { Kysely } from "kysely";
import { openDatabase } from "./db/index.ts";
import type { Database } from "./db/schema.ts";
import { SitePaths } from "./site-paths.ts";
import { insertBuild } from "./test/harness.ts";

const databases: Kysely<Database>[] = [];

after(async () => {
  for (const db of databases) await db.destroy();
});

const HASH_A = "a".repeat(64);
const HASH_C = "c".repeat(64);

/**
 * A database holding one build with the given path map, and its id.
 *
 * `sizes` decides whether the deploy recorded the blobs' sizes, which is what
 * makes a resolved path cacheable at all.
 */
async function version(
  paths: Record<string, string>,
  opts: { sizes?: boolean } = {},
): Promise<{ db: Kysely<Database>; build: number }> {
  const db = await openDatabase(":memory:");
  databases.push(db);

  const build = await insertBuild(db, paths);

  if (opts.sizes !== false) {
    for (const sha256 of new Set(Object.values(paths))) {
      await db.insertInto("blobs").values({ sha256, size: 10, br_size: -1, gzip_size: -1, seen_at: 1 }).execute();
    }
  }

  return { db, build };
}

/** Delete a build's file list, so anything answered afterwards came from the cache. */
async function forget(db: Kysely<Database>, build: number): Promise<void> {
  await db.deleteFrom("host_build_files").where("build_id", "=", build).execute();
}

test("a resolved path is answered again without the database", async () => {
  const { db, build } = await version({ "/index.html": HASH_A });
  const paths = new SitePaths(db);

  const first = await paths.resolve(build, "/", { spa: false });
  assert.deepEqual(first, {
    path: "/index.html",
    sha256: HASH_A,
    status: 200,
    variants: { identity: 10, br: undefined, gzip: undefined },
  });

  await forget(db, build);

  assert.deepEqual(await paths.resolve(build, "/", { spa: false }), first);
});

test("a path the build has no file for is cached too", async () => {
  const { db, build } = await version({ "/index.html": HASH_A });
  const paths = new SitePaths(db);

  assert.equal(await paths.resolve(build, "/nope.html", { spa: false }), undefined);

  // The row is back, but the answer that it isn't there was already settled.
  await db
    .insertInto("host_build_files")
    .values({ build_id: build, path: "/nope.html", sha256: HASH_C, event_id: null, source_sha256: null })
    .execute();

  assert.equal(await paths.resolve(build, "/nope.html", { spa: false }), undefined);
});

test("hosts that disagree about SPA get their own answers", async () => {
  const { db, build } = await version({ "/index.html": HASH_A, "/404.html": HASH_C });
  const paths = new SitePaths(db);

  assert.equal((await paths.resolve(build, "/about", { spa: true }))?.status, 200);
  assert.equal((await paths.resolve(build, "/about", { spa: false }))?.status, 404);

  await forget(db, build);

  assert.equal((await paths.resolve(build, "/about", { spa: true }))?.status, 200);
  assert.equal((await paths.resolve(build, "/about", { spa: false }))?.status, 404);
});

test("a blob whose sizes were never recorded is not cached", async () => {
  const { db, build } = await version({ "/index.html": HASH_A }, { sizes: false });
  const paths = new SitePaths(db);

  const first = await paths.resolve(build, "/", { spa: false });
  assert.deepEqual(first, { path: "/index.html", sha256: HASH_A, status: 200 });

  // Recorded late, as `compress-blobs` does. Nothing invalidates this cache,
  // so the only thing that can pick the sizes up is not having cached the row.
  await db.insertInto("blobs").values({ sha256: HASH_A, size: 10, br_size: 4, gzip_size: -1, seen_at: 1 }).execute();

  assert.deepEqual(await paths.resolve(build, "/", { spa: false }), {
    path: "/index.html",
    sha256: HASH_A,
    status: 200,
    variants: { identity: 10, br: 4, gzip: undefined },
  });
});

test("the cache is bounded, and drops the least recently used path", async () => {
  const { db, build } = await version({ "/index.html": HASH_A });
  const paths = new SitePaths(db);

  assert.ok(await paths.resolve(build, "/", { spa: false }));

  // Every one of these is a distinct key, so together they are more than the
  // cache holds and the entry above is the oldest of them.
  for (let i = 0; i < 5000; i++) {
    await paths.resolve(build, `/spray/${i}`, { spa: false });
  }

  await forget(db, build);

  assert.equal(await paths.resolve(build, "/", { spa: false }), undefined, "the oldest entry survived eviction");
});
