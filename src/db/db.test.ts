import assert from "node:assert/strict";
import { test } from "node:test";
import { sql } from "kysely";
import { openDatabase } from "./index.ts";
import { migrate } from "./migrations.ts";

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

test("the Tor control password is scrubbed from audit entries written before it was redacted", async () => {
  const db = await openDatabase(":memory:");

  const entry = (target: string, oldValue: string, newValue: string) => ({
    actor_pubkey: "c".repeat(64),
    action: "settings.update",
    target,
    old_value: oldValue,
    new_value: newValue,
    created_at: 2,
  });
  await db
    .insertInto("audit_log")
    .values([entry("tor_control_password", '""', '"hunter2"'), entry("tor_control_url", '"tcp://a:1"', '"tcp://b:2"')])
    .execute();

  // Run the migration again over rows written the old way.
  await sql`DELETE FROM schema_migrations WHERE name = '018_audit_log_redact_secrets'`.execute(db);
  await migrate(db);

  const rows = await db.selectFrom("audit_log").select(["target", "old_value", "new_value"]).orderBy("id").execute();
  assert.deepEqual(rows, [
    { target: "tor_control_password", old_value: "false", new_value: "true" },
    { target: "tor_control_url", old_value: '"tcp://a:1"', new_value: '"tcp://b:2"' },
  ]);

  await db.destroy();
});
