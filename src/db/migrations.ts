import { type Kysely, sql } from "kysely";
import type { Logger } from "../log.ts";
import type { Database } from "./schema.ts";

interface Migration {
  name: string;
  statements: string[];
}

/**
 * Schema migrations, applied in order and recorded in `schema_migrations`.
 *
 * Migrations are append-only: never edit one that has shipped, add another.
 */
const MIGRATIONS: Migration[] = [
  {
    name: "001_initial",
    statements: [
      `CREATE TABLE users (
        pubkey TEXT PRIMARY KEY,
        role TEXT NOT NULL CHECK (role IN ('admin', 'staff')),
        added_by TEXT,
        created_at INTEGER NOT NULL
      )`,
      `CREATE TABLE hosts (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        hostname TEXT NOT NULL UNIQUE,
        pubkey TEXT NOT NULL,
        kind INTEGER NOT NULL,
        identifier TEXT NOT NULL DEFAULT '',
        pinned_event_id TEXT,
        enabled INTEGER NOT NULL DEFAULT 1,
        created_by TEXT NOT NULL,
        created_at INTEGER NOT NULL,
        updated_at INTEGER NOT NULL
      )`,
      "CREATE INDEX hosts_site_idx ON hosts (pubkey, kind, identifier)",
      `CREATE TABLE host_grants (
        user_pubkey TEXT NOT NULL,
        host_id INTEGER NOT NULL REFERENCES hosts (id) ON DELETE CASCADE,
        created_at INTEGER NOT NULL,
        PRIMARY KEY (user_pubkey, host_id)
      )`,
      `CREATE TABLE site_events (
        event_id TEXT PRIMARY KEY,
        pubkey TEXT NOT NULL,
        kind INTEGER NOT NULL,
        identifier TEXT NOT NULL DEFAULT '',
        created_at INTEGER NOT NULL,
        event TEXT NOT NULL,
        status TEXT NOT NULL CHECK (status IN ('pending', 'fetching_blobs', 'ready', 'failed')),
        files_total INTEGER NOT NULL DEFAULT 0,
        files_ready INTEGER NOT NULL DEFAULT 0,
        error TEXT,
        fetched_at INTEGER NOT NULL,
        ready_at INTEGER
      )`,
      "CREATE INDEX site_events_site_idx ON site_events (pubkey, kind, identifier, created_at DESC)",
      "CREATE INDEX site_events_status_idx ON site_events (status)",
      `CREATE TABLE site_files (
        event_id TEXT NOT NULL REFERENCES site_events (event_id) ON DELETE CASCADE,
        path TEXT NOT NULL,
        sha256 TEXT NOT NULL,
        PRIMARY KEY (event_id, path)
      )`,
      "CREATE INDEX site_files_sha_idx ON site_files (sha256)",
      `CREATE TABLE blobs (
        sha256 TEXT PRIMARY KEY,
        size INTEGER NOT NULL,
        seen_at INTEGER NOT NULL
      )`,
      `CREATE TABLE settings (
        key TEXT PRIMARY KEY,
        value TEXT NOT NULL,
        updated_at INTEGER NOT NULL
      )`,
      `CREATE TABLE audit_log (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        actor_pubkey TEXT NOT NULL,
        action TEXT NOT NULL,
        target TEXT,
        old_value TEXT,
        new_value TEXT,
        created_at INTEGER NOT NULL
      )`,
      "CREATE INDEX audit_log_created_idx ON audit_log (created_at DESC)",
      `CREATE TABLE deploy_log (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        event_id TEXT NOT NULL,
        level TEXT NOT NULL,
        msg TEXT NOT NULL,
        fields TEXT,
        created_at INTEGER NOT NULL
      )`,
      "CREATE INDEX deploy_log_event_idx ON deploy_log (event_id, id)",
    ],
  },
  {
    // Downloads are now always restricted to public addresses, so this row
    // would only sit in the table meaning nothing.
    name: "002_drop_allow_private_blob_servers",
    statements: ["DELETE FROM settings WHERE key = 'allow_private_blob_servers'"],
  },
  {
    // Existing hosts are listed like any other: the list itself is off until an
    // admin turns it on, so nothing becomes public by upgrading.
    name: "003_hosts_hidden",
    statements: ["ALTER TABLE hosts ADD COLUMN hidden INTEGER NOT NULL DEFAULT 0"],
  },
];

/** Create the schema and apply any migrations the database is missing. */
export async function migrate(db: Kysely<Database>, log?: Logger): Promise<void> {
  await sql`CREATE TABLE IF NOT EXISTS schema_migrations (
    name TEXT PRIMARY KEY,
    applied_at INTEGER NOT NULL
  )`.execute(db);

  const applied = await sql<{ name: string }>`SELECT name FROM schema_migrations`.execute(db);
  const done = new Set(applied.rows.map((row) => row.name));

  for (const migration of MIGRATIONS) {
    if (done.has(migration.name)) continue;

    await db.transaction().execute(async (trx) => {
      for (const statement of migration.statements) {
        await sql.raw(statement).execute(trx);
      }
      await sql`INSERT INTO schema_migrations (name, applied_at)
        VALUES (${migration.name}, ${Math.floor(Date.now() / 1000)})`.execute(trx);
    });

    log?.info("migration_applied", { name: migration.name });
  }
}
