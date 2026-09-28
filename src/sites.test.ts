import assert from "node:assert/strict";
import { after, test } from "node:test";
import type { Kysely } from "kysely";
import { openDatabase } from "./db/index.ts";
import type { Database } from "./db/schema.ts";
import { newestByPath, newestReadyEvent, resolveSitePath, versionChain } from "./sites.ts";
import { insertBuild } from "./test/harness.ts";

const databases: Kysely<Database>[] = [];

after(async () => {
  for (const db of databases) await db.destroy();
});

const PUBKEY = "3bf0c63fcb93463407af97a5e5ee64fa883d107ef9e558472c4eb9aaaefa459d";
const EVENT_ID = "a".repeat(64);
const HASH_A = "a".repeat(64);
const HASH_B = "b".repeat(64);
const HASH_C = "c".repeat(64);

/** Record one ready version of the site, with the given path map. */
async function deploy(
  db: Kysely<Database>,
  eventId: string,
  createdAt: number,
  paths: Record<string, string>,
): Promise<void> {
  await db
    .insertInto("site_events")
    .values({
      event_id: eventId,
      pubkey: PUBKEY,
      kind: 15128,
      identifier: "",
      created_at: createdAt,
      event: "{}",
      status: "ready",
      files_total: Object.keys(paths).length,
      files_ready: Object.keys(paths).length,
      error: null,
      fetched_at: createdAt,
      ready_at: createdAt,
    })
    .execute();

  for (const [path, sha256] of Object.entries(paths)) {
    await db.insertInto("site_files").values({ event_id: eventId, path, sha256 }).execute();
  }
}

/** A database holding one version with the given path map. */
async function version(paths: Record<string, string>): Promise<Kysely<Database>> {
  const db = await openDatabase(":memory:");
  databases.push(db);

  await deploy(db, EVENT_ID, 1, paths);

  return db;
}

/** A database holding one build serving the given path map, and its id. */
async function built(paths: Record<string, string>): Promise<{ db: Kysely<Database>; id: number }> {
  const db = await openDatabase(":memory:");
  databases.push(db);

  return { db, id: await insertBuild(db, paths) };
}

test("resolves exact paths, index fallbacks, and 404s", async () => {
  const { db, id } = await built({
    "/index.html": HASH_A,
    "/blog/index.html": HASH_B,
    "/404.html": HASH_C,
  });

  assert.deepEqual(await resolveSitePath(db, id, "/index.html"), {
    path: "/index.html",
    sha256: HASH_A,
    status: 200,
  });
  assert.deepEqual(await resolveSitePath(db, id, "/"), {
    path: "/index.html",
    sha256: HASH_A,
    status: 200,
  });
  assert.deepEqual(await resolveSitePath(db, id, "/blog"), {
    path: "/blog/index.html",
    sha256: HASH_B,
    status: 200,
  });
  assert.deepEqual(await resolveSitePath(db, id, "/nope.html"), {
    path: "/404.html",
    sha256: HASH_C,
    status: 404,
  });
});

test("resolves an extensionless path to its .html sibling", async () => {
  const { db, id } = await built({
    "/index.html": HASH_A,
    "/self-hosting.html": HASH_B,
    "/404.html": HASH_C,
  });

  assert.deepEqual(await resolveSitePath(db, id, "/self-hosting"), {
    path: "/self-hosting.html",
    sha256: HASH_B,
    status: 200,
  });
  // The root is not "/.html".
  assert.deepEqual(await resolveSitePath(db, id, "/"), {
    path: "/index.html",
    sha256: HASH_A,
    status: 200,
  });
});

test("prefers a .html sibling over a directory index", async () => {
  const { db, id } = await built({ "/blog.html": HASH_A, "/blog/index.html": HASH_B });

  assert.deepEqual(await resolveSitePath(db, id, "/blog"), {
    path: "/blog.html",
    sha256: HASH_A,
    status: 200,
  });
});

test("resolution normalizes traversal and percent-encoding", async () => {
  const { db, id } = await built({ "/index.html": HASH_A, "/my file.txt": HASH_B });

  assert.equal((await resolveSitePath(db, id, "/../../index.html"))?.sha256, HASH_A);
  assert.equal((await resolveSitePath(db, id, "/blog/../index.html"))?.sha256, HASH_A);
  assert.equal((await resolveSitePath(db, id, "/my%20file.txt"))?.sha256, HASH_B);
  assert.equal(await resolveSitePath(db, id, "/bad%zz"), undefined);
});

test("returns undefined when there is no match and no 404 page", async () => {
  const { db, id } = await built({ "/index.html": HASH_A });

  assert.equal(await resolveSitePath(db, id, "/nope.html"), undefined);
});

test("an SPA serves its shell under a 200 for document paths", async () => {
  const { db, id } = await built({ "/index.html": HASH_A, "/assets/app.js": HASH_B });

  for (const path of ["/about", "/users/alex", "/about.html", "/DEEP.HTM"]) {
    assert.deepEqual(
      await resolveSitePath(db, id, path, { spa: true }),
      { path: "/index.html", sha256: HASH_A, status: 200 },
      path,
    );
  }

  // A file the site really has is served as itself, not as the shell.
  assert.deepEqual(await resolveSitePath(db, id, "/assets/app.js", { spa: true }), {
    path: "/assets/app.js",
    sha256: HASH_B,
    status: 200,
  });
});

test("an SPA reaches for 404.html first, where nsyte publishes the shell", async () => {
  const { db, id } = await built({ "/index.html": HASH_A, "/404.html": HASH_C });

  assert.deepEqual(await resolveSitePath(db, id, "/about", { spa: true }), {
    path: "/404.html",
    sha256: HASH_C,
    status: 200,
  });
});

test("an SPA leaves missing assets alone", async () => {
  const { db, id } = await built({ "/index.html": HASH_A, "/404.html": HASH_C });

  for (const path of ["/assets/app.js", "/assets/app.css", "/logo.png", "/feed.xml"]) {
    assert.deepEqual(
      await resolveSitePath(db, id, path, { spa: true }),
      { path: "/404.html", sha256: HASH_C, status: 404 },
      path,
    );
  }
});

test("an SPA with no shell at all resolves to nothing", async () => {
  const { db, id } = await built({ "/assets/app.js": HASH_B });

  assert.equal(await resolveSitePath(db, id, "/about", { spa: true }), undefined);
});

const NEWER = "b".repeat(64);

/** A chain's path map, flattened the way a build flattens it. */
async function merge(db: Kysely<Database>, eventIds: string[]): Promise<Map<string, string>> {
  const rows = await db.selectFrom("site_files").selectAll().where("event_id", "in", eventIds).execute();
  return new Map([...newestByPath(rows, eventIds)].map(([path, row]) => [path, row.sha256]));
}

/** A database holding two versions of one site, newest first in the chain. */
async function chain(older: Record<string, string>, newer: Record<string, string>): Promise<Kysely<Database>> {
  const db = await version(older);
  await deploy(db, NEWER, 2, newer);

  return db;
}

test("a chain takes each path from the newest version that has it", async () => {
  const db = await chain({ "/index.html": HASH_A, "/old.js": HASH_B }, { "/index.html": HASH_C });
  const merged = await merge(db, [NEWER, EVENT_ID]);

  // Published by both: the newest wins.
  assert.equal(merged.get("/index.html"), HASH_C);
  // Dropped by the newest: the version that last published it answers.
  assert.equal(merged.get("/old.js"), HASH_B);
});

test("every resolution step searches the whole chain", async () => {
  // The older version spells the page as a file; the newer has only a
  // directory index. An exact match is the first step wherever it is found,
  // so the older file is what `/blog` names — the same order a single version
  // would have resolved these two paths in.
  const db = await chain({ "/blog.html": HASH_B }, { "/blog/index.html": HASH_C });
  const id = await insertBuild(db, Object.fromEntries(await merge(db, [NEWER, EVENT_ID])));

  assert.deepEqual(await resolveSitePath(db, id, "/blog"), {
    path: "/blog.html",
    sha256: HASH_B,
    status: 200,
  });
});

test("a merged host stacks its earlier ready versions, newest first", async () => {
  const db = await chain({ "/index.html": HASH_A }, { "/index.html": HASH_C });
  const active = await newestReadyEvent(db, { pubkey: PUBKEY, kind: 15128, identifier: "" });
  assert.ok(active);

  const host = await db
    .insertInto("hosts")
    .values({
      hostname: "docs.example.com",
      pubkey: PUBKEY,
      kind: 15128,
      identifier: "",
      pinned_event_id: null,
      enabled: 1,
      merge_deployments: 1,
      created_by: PUBKEY,
      created_at: 1,
      updated_at: 1,
    })
    .returningAll()
    .executeTakeFirstOrThrow();

  assert.deepEqual(await versionChain(db, host, active), [NEWER, EVENT_ID]);

  // Without the flag the chain is the one version.
  assert.deepEqual(await versionChain(db, { ...host, merge_deployments: 0 }, active), [NEWER]);
});
