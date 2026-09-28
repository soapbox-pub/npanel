import type { Kysely } from "kysely";
import type { BlobStore } from "./blobs.ts";
import type { Database } from "./db/schema.ts";

/**
 * The `blobs` row for a stored blob: its size, and that something still wants
 * it.
 */
export async function recordBlob(db: Kysely<Database>, sha256: string, size: number): Promise<void> {
  const now = Math.floor(Date.now() / 1000);
  await db
    .insertInto("blobs")
    .values({ sha256, size, seen_at: now })
    .onConflict((oc) => oc.column("sha256").doUpdateSet({ seen_at: now, size }))
    .execute();
}

/**
 * Record which compressed copies a blob has, so serving never has to look.
 *
 * Read back from the disk rather than from what a compression pass reported,
 * because a sidecar may predate the caller — written by an earlier deploy, a
 * build, or the compress-blobs script. Called for every blob, not only the
 * compressible ones: a JPEG's row saying -1 is exactly what lets a request for
 * it skip the stat too.
 */
export async function recordVariantSizes(db: Kysely<Database>, blobs: BlobStore, sha256: string): Promise<void> {
  const variants = await blobs.variants(sha256);
  if (!variants) return;

  await db
    .updateTable("blobs")
    .set({ br_size: variants.br ?? -1, gzip_size: variants.gzip ?? -1 })
    .where("sha256", "=", sha256)
    .execute();
}
