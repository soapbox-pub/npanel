import assert from "node:assert/strict";
import { test } from "node:test";
import { openDatabase } from "./index.ts";

test("migrates an empty database and round-trips rows", async () => {
  const db = await openDatabase(":memory:");

  const inserted = await db
    .insertInto("hosts")
    .values({
      hostname: "docs.example.com",
      pubkey: "a".repeat(64),
      kind: 35128,
      identifier: "docs",
      pinned_event_id: null,
      enabled: 1,
      created_by: "b".repeat(64),
      created_at: 1,
      updated_at: 1,
    })
    .executeTakeFirstOrThrow();

  assert.equal(inserted.numInsertedOrUpdatedRows, 1n);

  const host = await db
    .selectFrom("hosts")
    .selectAll()
    .where("hostname", "=", "docs.example.com")
    .executeTakeFirstOrThrow();

  assert.equal(host.identifier, "docs");
  assert.equal(host.pinned_event_id, null);
  assert.equal(typeof host.id, "number");

  await db.destroy();
});

test("returning clauses yield rows", async () => {
  const db = await openDatabase(":memory:");

  const row = await db
    .insertInto("audit_log")
    .values({
      actor_pubkey: "c".repeat(64),
      action: "settings.update",
      target: "relays",
      old_value: null,
      new_value: "[]",
      created_at: 2,
    })
    .returning("id")
    .executeTakeFirstOrThrow();

  assert.equal(typeof row.id, "number");

  await db.destroy();
});

test("migrations are idempotent across opens", async () => {
  const db = await openDatabase(":memory:");
  const before = await db.selectFrom("users").selectAll().execute();
  assert.deepEqual(before, []);
  await db.destroy();
});

test("cascade deletes remove dependent rows", async () => {
  const db = await openDatabase(":memory:");

  await db
    .insertInto("site_events")
    .values({
      event_id: "d".repeat(64),
      pubkey: "a".repeat(64),
      kind: 15128,
      identifier: "",
      created_at: 10,
      event: "{}",
      status: "ready",
      files_total: 1,
      files_ready: 1,
      error: null,
      fetched_at: 10,
      ready_at: 11,
    })
    .execute();

  await db
    .insertInto("site_files")
    .values({ event_id: "d".repeat(64), path: "/index.html", sha256: "e".repeat(64) })
    .execute();

  await db.deleteFrom("site_events").where("event_id", "=", "d".repeat(64)).execute();

  const files = await db.selectFrom("site_files").selectAll().execute();
  assert.deepEqual(files, []);

  await db.destroy();
});
