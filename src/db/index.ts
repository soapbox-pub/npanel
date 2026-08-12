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

  const sqlite = await openSqlite(path);
  const db = new Kysely<Database>({ dialect: new NPanelSqliteDialect(sqlite) });

  await migrate(db, log);

  return db;
}

export type { Database } from "./schema.ts";
