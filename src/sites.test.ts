import assert from "node:assert/strict";
import { after, test } from "node:test";
import type { Kysely } from "kysely";
import { openDatabase } from "./db/index.ts";
import type { Database } from "./db/schema.ts";
import { resolveSitePath } from "./sites.ts";

const databases: Kysely<Database>[] = [];

after(async () => {
  for (const db of databases) await db.destroy();
});

const PUBKEY = "3bf0c63fcb93463407af97a5e5ee64fa883d107ef9e558472c4eb9aaaefa459d";
const EVENT_ID = "a".repeat(64);
const HASH_A = "a".repeat(64);
const HASH_B = "b".repeat(64);
const HASH_C = "c".repeat(64);

/** A database holding one version with the given path map. */
async function version(paths: Record<string, string>): Promise<Kysely<Database>> {
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
  }

  return db;
}

test("resolves exact paths, index fallbacks, and 404s", async () => {
  const db = await version({
    "/index.html": HASH_A,
    "/blog/index.html": HASH_B,
    "/404.html": HASH_C,
  });

  assert.deepEqual(await resolveSitePath(db, EVENT_ID, "/index.html"), {
    path: "/index.html",
    sha256: HASH_A,
    status: 200,
  });
  assert.deepEqual(await resolveSitePath(db, EVENT_ID, "/"), {
    path: "/index.html",
    sha256: HASH_A,
    status: 200,
  });
  assert.deepEqual(await resolveSitePath(db, EVENT_ID, "/blog"), {
    path: "/blog/index.html",
    sha256: HASH_B,
    status: 200,
  });
  assert.deepEqual(await resolveSitePath(db, EVENT_ID, "/nope.html"), {
    path: "/404.html",
    sha256: HASH_C,
    status: 404,
  });
});

test("resolves an extensionless path to its .html sibling", async () => {
  const db = await version({
    "/index.html": HASH_A,
    "/self-hosting.html": HASH_B,
    "/404.html": HASH_C,
  });

  assert.deepEqual(await resolveSitePath(db, EVENT_ID, "/self-hosting"), {
    path: "/self-hosting.html",
    sha256: HASH_B,
    status: 200,
  });
  // The root is not "/.html".
  assert.deepEqual(await resolveSitePath(db, EVENT_ID, "/"), {
    path: "/index.html",
    sha256: HASH_A,
    status: 200,
  });
});

test("prefers a .html sibling over a directory index", async () => {
  const db = await version({ "/blog.html": HASH_A, "/blog/index.html": HASH_B });

  assert.deepEqual(await resolveSitePath(db, EVENT_ID, "/blog"), {
    path: "/blog.html",
    sha256: HASH_A,
    status: 200,
  });
});

test("resolution normalizes traversal and percent-encoding", async () => {
  const db = await version({ "/index.html": HASH_A, "/my file.txt": HASH_B });

  assert.equal((await resolveSitePath(db, EVENT_ID, "/../../index.html"))?.sha256, HASH_A);
  assert.equal((await resolveSitePath(db, EVENT_ID, "/blog/../index.html"))?.sha256, HASH_A);
  assert.equal((await resolveSitePath(db, EVENT_ID, "/my%20file.txt"))?.sha256, HASH_B);
  assert.equal(await resolveSitePath(db, EVENT_ID, "/bad%zz"), undefined);
});

test("returns undefined when there is no match and no 404 page", async () => {
  const db = await version({ "/index.html": HASH_A });

  assert.equal(await resolveSitePath(db, EVENT_ID, "/nope.html"), undefined);
});

test("an SPA serves its shell under a 200 for document paths", async () => {
  const db = await version({ "/index.html": HASH_A, "/assets/app.js": HASH_B });

  for (const path of ["/about", "/users/alex", "/about.html", "/DEEP.HTM"]) {
    assert.deepEqual(
      await resolveSitePath(db, EVENT_ID, path, { spa: true }),
      { path: "/index.html", sha256: HASH_A, status: 200 },
      path,
    );
  }

  // A file the site really has is served as itself, not as the shell.
  assert.deepEqual(await resolveSitePath(db, EVENT_ID, "/assets/app.js", { spa: true }), {
    path: "/assets/app.js",
    sha256: HASH_B,
    status: 200,
  });
});

test("an SPA reaches for 404.html first, where nsyte publishes the shell", async () => {
  const db = await version({ "/index.html": HASH_A, "/404.html": HASH_C });

  assert.deepEqual(await resolveSitePath(db, EVENT_ID, "/about", { spa: true }), {
    path: "/404.html",
    sha256: HASH_C,
    status: 200,
  });
});

test("an SPA leaves missing assets alone", async () => {
  const db = await version({ "/index.html": HASH_A, "/404.html": HASH_C });

  for (const path of ["/assets/app.js", "/assets/app.css", "/logo.png", "/feed.xml"]) {
    assert.deepEqual(
      await resolveSitePath(db, EVENT_ID, path, { spa: true }),
      { path: "/404.html", sha256: HASH_C, status: 404 },
      path,
    );
  }
});

test("an SPA with no shell at all resolves to nothing", async () => {
  const db = await version({ "/assets/app.js": HASH_B });

  assert.equal(await resolveSitePath(db, EVENT_ID, "/about", { spa: true }), undefined);
});
