import process from "node:process";
import { BlobStore } from "../src/blobs.ts";
import { Config } from "../src/config.ts";
import { openDatabase } from "../src/db/index.ts";
import { Logger } from "../src/log.ts";
import { isCompressible } from "../src/mime.ts";

/**
 * Store compressed copies of blobs that predate npanel making them.
 *
 * Deploys compress as they go, but only when they run — a site that is already
 * `ready` has no reason to deploy again, and would go on being served raw
 * forever. This walks everything currently referenced and fills in the gaps.
 * It is safe to re-run: blobs that already have their copies are skipped.
 *
 * It also records every blob's sidecar sizes in the database — including the
 * blobs it doesn't compress — which is what lets serving skip the disk when
 * answering `Accept-Encoding`. Rows that predate the size columns stay NULL
 * until either this script or a deploy fills them in.
 *
 *   bun run compress-blobs
 *   bun run compress-blobs --dry-run
 */
const args = process.argv.slice(2);
const dryRun = args.includes("--dry-run");

/** Blobs compressed in parallel, matching what a deploy allows itself. */
const CONCURRENCY = 4;

const config = new Config({
  get(key) {
    return process.env[key];
  },
});

const log = new Logger(config.logLevel);
const db = await openDatabase(config.databasePath, log);
const blobs = new BlobStore(config.blobsDir);

await compressBlobs();

await db.destroy();

async function compressBlobs(): Promise<void> {
  // The same rule a deploy uses: the store is keyed by hash and can't tell
  // markup from a JPEG, so the paths a blob is served under decide for it.
  const rows = [
    ...(await db.selectFrom("site_files").select(["sha256", "path"]).execute()),
    ...(await db.selectFrom("host_build_files").select(["sha256", "path"]).execute()),
  ];

  const candidates = new Set<string>();
  const referenced = new Set<string>();
  for (const row of rows) {
    referenced.add(row.sha256);
    if (isCompressible(row.path)) candidates.add(row.sha256);
  }

  // Every referenced blob, not just the compressible ones: the incompressible
  // ones get their sizes recorded too, so serving them skips the disk as well.
  const queue = [...referenced];
  const total = candidates.size;

  let compressed = 0;
  let before = 0;
  let after = 0;
  let added = 0;

  const worker = async (): Promise<void> => {
    for (let sha256 = queue.shift(); sha256; sha256 = queue.shift()) {
      let variants = await blobs.variants(sha256);
      // Referenced by a version whose deploy never finished, or pruned since.
      if (!variants) continue;

      const wanted = candidates.has(sha256) && (variants.br === undefined || variants.gzip === undefined);

      if (dryRun) {
        if (wanted) compressed++;
        continue;
      }

      if (wanted) {
        const written = await blobs.ensureVariants(sha256);
        if (written.length) {
          variants = (await blobs.variants(sha256)) ?? variants;

          const smallest = Math.min(
            variants.identity,
            variants.br ?? Number.POSITIVE_INFINITY,
            variants.gzip ?? Number.POSITIVE_INFINITY,
          );

          compressed++;
          before += variants.identity;
          after += smallest;
          for (const encoding of written) added += variants[encoding] ?? 0;

          console.log(`${sha256}  ${formatBytes(variants.identity)} → ${formatBytes(smallest)}`);
        }
      }

      // The same record a deploy writes: what is on disk, with -1 for a copy
      // that is settled as absent. A no-op for blobs with no row.
      await db
        .updateTable("blobs")
        .set({ br_size: variants.br ?? -1, gzip_size: variants.gzip ?? -1 })
        .where("sha256", "=", sha256)
        .execute();
    }
  };

  await Promise.all(Array.from({ length: Math.min(CONCURRENCY, queue.length) }, worker));

  if (dryRun) {
    console.log(`Would compress ${compressed} of ${total} compressible blob(s).`);
    return;
  }

  if (!compressed) {
    console.log(`Nothing to do: all ${total} compressible blob(s) already have their copies.`);
    return;
  }

  console.log(
    `Compressed ${compressed} of ${total} compressible blob(s): ` +
      `${formatBytes(before)} → ${formatBytes(after)} on the wire, for ${formatBytes(added)} of disk.`,
  );
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
