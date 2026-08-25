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
  {
    // On for every existing host, including the ones this can't apply to: the
    // flag means "fold www in where that makes sense", and whether it makes
    // sense is decided against the Public Suffix List at the moment it matters,
    // not frozen into a column that a later rename would make a lie.
    name: "004_hosts_redirect_www",
    statements: ["ALTER TABLE hosts ADD COLUMN redirect_www INTEGER NOT NULL DEFAULT 1"],
  },
  {
    // Off for existing hosts, and for new ones. Unlike the www redirect, which
    // preserves what a response means, this changes a "you asked for something
    // that isn't here" into a "here you go" — not something to hand a site
    // whose owner never asked for it.
    name: "005_hosts_spa",
    statements: ["ALTER TABLE hosts ADD COLUMN spa INTEGER NOT NULL DEFAULT 0"],
  },
  {
    // The NIP-46 gateway, which shipped and was then withdrawn. It stays here
    // because it ran: a deployment that applied it has the name recorded, and
    // removing the entry would leave 007 dropping tables that this npanel had
    // never claimed to create. 007 is what undoes it.
    name: "006_bunkers",
    statements: [
      `CREATE TABLE bunkers (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        slug TEXT NOT NULL UNIQUE,
        name TEXT NOT NULL,
        user_pubkey TEXT,
        upstream_pubkey TEXT,
        upstream_secret TEXT,
        upstream_relays TEXT NOT NULL DEFAULT '[]',
        key_fingerprint TEXT,
        status TEXT NOT NULL CHECK (status IN ('pending', 'connected', 'error')),
        error TEXT,
        last_ok_at INTEGER,
        enabled INTEGER NOT NULL DEFAULT 1,
        created_by TEXT NOT NULL,
        created_at INTEGER NOT NULL,
        updated_at INTEGER NOT NULL
      )`,
      `CREATE TABLE bunker_grants (
        user_pubkey TEXT NOT NULL,
        bunker_id INTEGER NOT NULL REFERENCES bunkers (id) ON DELETE CASCADE,
        created_at INTEGER NOT NULL,
        PRIMARY KEY (user_pubkey, bunker_id)
      )`,
      `CREATE TABLE bunker_connections (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        bunker_id INTEGER NOT NULL REFERENCES bunkers (id) ON DELETE CASCADE,
        name TEXT NOT NULL,
        signer_secret TEXT NOT NULL,
        signer_pubkey TEXT NOT NULL UNIQUE,
        client_pubkey TEXT,
        secret TEXT,
        relays TEXT NOT NULL DEFAULT '[]',
        permissions TEXT NOT NULL DEFAULT '[]',
        created_by TEXT NOT NULL,
        created_at INTEGER NOT NULL,
        last_used_at INTEGER,
        last_error TEXT
      )`,
      "CREATE INDEX bunker_connections_bunker_idx ON bunker_connections (bunker_id)",
    ],
  },
  {
    // Bunkers are gone. Dropping the tables rather than leaving them empty
    // matters for the one row that isn't empty: a stored `signer_secret` is a
    // credential, and keeping unreadable ciphertext around after the code that
    // could read it was removed is how a "deleted" secret outlives its feature.
    //
    // The settings rows go too, since nothing validates them any more and a
    // future key of the same name would inherit a stale value. The audit log
    // keeps its `bunker.*` entries: it records what happened, and this did.
    name: "007_drop_bunkers",
    statements: [
      "DROP TABLE IF EXISTS bunker_connections",
      "DROP TABLE IF EXISTS bunker_grants",
      "DROP TABLE IF EXISTS bunkers",
      `DELETE FROM settings WHERE key IN
        ('bunker_relays', 'bunker_request_timeout', 'bunker_max_relays', 'bunker_rate_limit')`,
    ],
  },
  {
    name: "008_hosts_csp",
    statements: ["ALTER TABLE hosts ADD COLUMN csp INTEGER"],
  },
  {
    // 008 made `csp` a tri-state against an `nsite_csp` setting that could
    // switch the policy off for the whole instance. That setting is gone: an
    // admin who wanted one awkward site working could disarm every site with
    // it, which is the one mistake a security default must not make easy. The
    // policy is now simply on, and a site opts out for itself.
    //
    // NULL meant "follow the setting", and the setting's default was on, so
    // NULL folds into 1. An explicit 0 was a real decision about a real site
    // and is kept.
    name: "009_hosts_csp_default_on",
    statements: [
      "ALTER TABLE hosts ADD COLUMN csp_on INTEGER NOT NULL DEFAULT 1",
      "UPDATE hosts SET csp_on = 0 WHERE csp = 0",
      "ALTER TABLE hosts DROP COLUMN csp",
      "ALTER TABLE hosts RENAME COLUMN csp_on TO csp",
      "DELETE FROM settings WHERE key = 'nsite_csp'",
    ],
  },
  {
    // One onion service per host, holding the key Tor generated for it.
    //
    // The key is what makes the address stable: an onion service is named by
    // its public key, so keeping it is the only way a site keeps the address
    // people bookmarked across a restart, a disable, or a move to another box.
    // Tor itself stores nothing — npanel re-adds every service over the control
    // port each time it connects.
    //
    // ON DELETE CASCADE is real here (`PRAGMA foreign_keys = ON` in
    // src/db/sqlite.ts), so deleting a host disposes of its key with it. That
    // is the intended reading: the address belonged to that host, and handing
    // it to whoever registers the hostname next would be a different site
    // answering at an address people trusted.
    name: "010_tor_onions",
    statements: [
      `CREATE TABLE onion_services (
        host_id INTEGER PRIMARY KEY REFERENCES hosts (id) ON DELETE CASCADE,
        address TEXT NOT NULL UNIQUE,
        secret_key TEXT NOT NULL,
        created_at INTEGER NOT NULL
      )`,
    ],
  },
  {
    // NIP-05 names, per host, served at `/.well-known/nostr.json`.
    //
    // The primary key is what makes a name unique to one host rather than to
    // the instance: `bob` on two domains is two people, and there is nothing
    // odd about a gateway serving both.
    //
    // CASCADE for the same reason as the onion above. A name is an assertion
    // the domain makes about who someone is, so it should not outlive the
    // domain's registration here and greet whoever configures the hostname
    // next with a set of identities they never vouched for.
    name: "011_nip05_names",
    statements: [
      `CREATE TABLE nip05_names (
        host_id INTEGER NOT NULL REFERENCES hosts (id) ON DELETE CASCADE,
        name TEXT NOT NULL,
        pubkey TEXT NOT NULL,
        relays TEXT NOT NULL DEFAULT '[]',
        created_at INTEGER NOT NULL,
        updated_at INTEGER NOT NULL,
        PRIMARY KEY (host_id, name)
      )`,
    ],
  },
  {
    // Sizes of a blob's compressed copies, recorded once a deploy has settled
    // them, so serving a file reads no sizes off the disk — they ride along on
    // the `site_files` lookup instead.
    //
    // -1 is the settled absence of a copy: compression didn't pay, or the blob
    // isn't the kind worth trying. That is not the same answer as NULL, which
    // means nobody has looked yet — a row from before these columns existed —
    // and serving falls back to statting the sidecars until a deploy or
    // `bun run compress-blobs` fills the row in.
    name: "012_blob_variant_sizes",
    statements: ["ALTER TABLE blobs ADD COLUMN br_size INTEGER", "ALTER TABLE blobs ADD COLUMN gzip_size INTEGER"],
  },
  {
    // The serving path asks for the newest *ready* version, but
    // `site_events_site_idx` doesn't know about status, so that lookup walks
    // the index newest-first checking each row — and a site whose deploys keep
    // failing stacks rows on top of the one being served, making every request
    // pay for the failures. A partial index turns it back into a direct seek.
    // SQLite picks it because the query's literal `status = 'ready'` implies
    // the index predicate.
    name: "013_site_events_ready_idx",
    statements: [
      `CREATE INDEX site_events_ready_idx
        ON site_events (pubkey, kind, identifier, created_at DESC)
        WHERE status = 'ready'`,
    ],
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
