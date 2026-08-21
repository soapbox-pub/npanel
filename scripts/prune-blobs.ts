import process from "node:process";
import { BlobStore } from "../src/blobs.ts";
import { Config } from "../src/config.ts";
import { openDatabase } from "../src/db/index.ts";
import { Logger } from "../src/log.ts";
import { activeEvent } from "../src/sites.ts";

/**
 * Reclaim disk space.
 *
 * With `--keep N`, old versions of each site are forgotten first (never the
 * version a host is currently serving, and never a pinned one). Then every
 * blob no remaining version references is deleted.
 *
 *   bun run prune-blobs --keep 10
 *   bun run prune-blobs --dry-run
 */
const args = process.argv.slice(2);
const dryRun = args.includes("--dry-run");
const keep = parseKeep(args);

const config = new Config({
  get(key) {
    return process.env[key];
  },
});

const log = new Logger(config.logLevel);
const db = await openDatabase(config.databasePath, log);
const blobs = new BlobStore(config.blobsDir);

if (keep !== undefined) {
  await pruneVersions(keep);
}

await pruneBlobs();

await db.destroy();

/** Drop all but the newest `keep` versions of each site. */
async function pruneVersions(keep: number): Promise<void> {
  const hosts = await db.selectFrom("hosts").selectAll().execute();
  const protectedIds = new Set<string>();

  for (const host of hosts) {
    if (host.pinned_event_id) protectedIds.add(host.pinned_event_id);
    const active = await activeEvent(db, host);
    if (active) protectedIds.add(active.event_id);
  }

  const sites = await db.selectFrom("site_events").select(["pubkey", "kind", "identifier"]).distinct().execute();

  let removed = 0;

  for (const site of sites) {
    const versions = await db
      .selectFrom("site_events")
      .select("event_id")
      .where("pubkey", "=", site.pubkey)
      .where("kind", "=", site.kind)
      .where("identifier", "=", site.identifier)
      .orderBy("created_at", "desc")
      .execute();

    const stale = versions.slice(keep).filter((version) => !protectedIds.has(version.event_id));

    for (const version of stale) {
      if (!dryRun) {
        // site_files and deploy_log rows go with it (site_files cascades).
        await db.deleteFrom("deploy_log").where("event_id", "=", version.event_id).execute();
        await db.deleteFrom("site_events").where("event_id", "=", version.event_id).execute();
      }
      removed++;
    }
  }

  console.log(`${dryRun ? "Would remove" : "Removed"} ${removed} old version(s), keeping ${keep} per site.`);
}

/** Delete blobs no stored version references. */
async function pruneBlobs(): Promise<void> {
  const rows = await db.selectFrom("site_files").select("sha256").distinct().execute();
  const referenced = new Set(rows.map((row) => row.sha256));

  let deleted = 0;
  let bytes = 0;

  for await (const sha256 of blobs.list()) {
    if (referenced.has(sha256)) continue;

    // Compressed copies go with the blob, so they count towards what deleting
    // it reclaims.
    const variants = await blobs.variants(sha256);
    bytes += (variants?.identity ?? 0) + (variants?.br ?? 0) + (variants?.gzip ?? 0);

    if (!dryRun) {
      await blobs.delete(sha256);
      await db.deleteFrom("blobs").where("sha256", "=", sha256).execute();
    }
    deleted++;
  }

  console.log(`${dryRun ? "Would delete" : "Deleted"} ${deleted} unreferenced blob(s), ${formatBytes(bytes)}.`);
}

function parseKeep(args: string[]): number | undefined {
  const index = args.indexOf("--keep");
  if (index === -1) return undefined;

  const value = Number(args[index + 1]);
  if (!Number.isInteger(value) || value < 1) {
    console.error("--keep requires a positive integer.");
    process.exit(1);
  }

  return value;
}

function formatBytes(bytes: number): string {
  const units = ["B", "KiB", "MiB", "GiB", "TiB"];
  let value = bytes;
  let unit = 0;

  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit++;
  }

  return `${value.toFixed(unit === 0 ? 0 : 1)} ${units[unit]}`;
}
