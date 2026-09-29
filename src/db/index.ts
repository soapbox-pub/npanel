import { mkdir } from "node:fs/promises";
import { dirname } from "node:path";
import { Kysely } from "kysely";
import type { Logger } from "../log.ts";
import { migrate } from "./migrations.ts";
import type { Database } from "./schema.ts";
import { NPanelSqliteDialect, openSqlite } from "./sqlite.ts";

/** Open the database at `path`, creating and migrating it if necessary. */
export async function openDatabase(path: string, log?: Logger): Promise<Kysely<Database>> {
  if (path !== ":memory:") {
    await mkdir(dirname(path), { recursive: true });
  }

  const db = await connectDatabase(path);
  await migrate(db, log);

  return db;
}

/**
 * A second connection to a database {@link openDatabase} has already migrated.
 *
 * For the deploy thread, which shares the main thread's file but must never
 * race it to a migration.
 */
export async function connectDatabase(path: string): Promise<Kysely<Database>> {
  return new Kysely<Database>({ dialect: new NPanelSqliteDialect(await openSqlite(path)) });
}

export type { Database } from "./schema.ts";
