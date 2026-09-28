import type { Kysely } from "kysely";
import type { Database } from "./db/schema.ts";
import type { LogFields } from "./log.ts";

/** Deploy log entries kept per version. Older entries are pruned. */
const DEPLOY_LOG_LIMIT = 500;

export type DeployLogLevel = "debug" | "info" | "warn" | "error";

/**
 * Append to the log the dashboard shows for a version.
 *
 * Shared by everything that works on a version — downloading it, and building
 * the hosts that serve it — so a site that isn't live explains why in one
 * place, whichever step stopped it.
 */
export async function writeDeployLog(
  db: Kysely<Database>,
  eventId: string,
  level: DeployLogLevel,
  msg: string,
  fields?: LogFields,
): Promise<void> {
  const cleaned = fields
    ? Object.fromEntries(Object.entries(fields).filter(([, value]) => value !== undefined))
    : undefined;

  await db
    .insertInto("deploy_log")
    .values({
      event_id: eventId,
      level,
      msg,
      fields: cleaned && Object.keys(cleaned).length ? JSON.stringify(cleaned) : null,
      created_at: Math.floor(Date.now() / 1000),
    })
    .execute();
}

/** Drop all but the newest {@link DEPLOY_LOG_LIMIT} entries for a version. */
export async function pruneDeployLog(db: Kysely<Database>, eventId: string): Promise<void> {
  const cutoff = await db
    .selectFrom("deploy_log")
    .select("id")
    .where("event_id", "=", eventId)
    .orderBy("id", "desc")
    .offset(DEPLOY_LOG_LIMIT)
    .limit(1)
    .executeTakeFirst();

  if (!cutoff) return;

  await db.deleteFrom("deploy_log").where("event_id", "=", eventId).where("id", "<=", cutoff.id).execute();
}
