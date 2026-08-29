import type { Kysely } from "kysely";
import type { Database } from "./db/schema.ts";
import { type ResolvedFile, type VersionFiles, resolveSitePath } from "./sites.ts";

/**
 * Resolved paths to keep. Each entry is one request path and the file it names.
 *
 * Sized for the paths a gateway is actually being asked for rather than for
 * every file of every site it hosts, since an entry costs a few hundred bytes.
 * Overflowing it is not a correctness problem: the least recently used path
 * goes back to costing a query, which is what every path cost before this
 * existed — so a scanner spraying paths at a real host can waste the cache but
 * cannot do worse than turn it off.
 */
const MAX_CACHED = 5000;

/**
 * Which file a request path resolves to within a version, cached.
 *
 * This is the last database query on a warm site request, and it can be held
 * for the same reason {@link file://./site-csp.ts} holds a policy: a version's
 * file list is immutable. `storeManifest` writes the `site_files` rows for an
 * `event_id` once and nothing edits them afterwards, and an event id is a
 * hash, so a redeploy is a different key rather than a changed one. Nothing
 * has to invalidate this cache, and nothing does.
 *
 * A merged host resolves across several versions at once, and the same
 * reasoning covers it: the answer is settled by *which* versions are in play,
 * so entries are keyed by {@link VersionFiles.filesKey} — one name for one
 * list of immutable file lists — rather than by an event id. A chain that
 * gains a version is a different key, not a changed one.
 *
 * The blob sizes that ride along on the lookup are the one part that isn't
 * settled at deploy time — `compress-blobs` can record them for a blob stored
 * before npanel recorded any. A row still missing them is left uncached rather
 * than pinned to the stat it falls back to, which is what keeps every entry
 * here immutable and the invalidation story empty.
 */
export class SitePaths {
  private cache = new Map<string, ResolvedFile | null>();

  constructor(private readonly db: Kysely<Database>) {}

  /** The file this path names within a version, as {@link resolveSitePath} answers it. */
  async resolve(
    version: VersionFiles,
    requestPath: string,
    options: { spa: boolean },
  ): Promise<ResolvedFile | undefined> {
    // The SPA flag is part of the key because it belongs to the host, not to
    // the version: two hosts pointing at one nsite may disagree about it, and
    // they resolve the same missing path to different answers.
    const key = `${version.filesKey}:${options.spa ? "spa" : ""}:${requestPath}`;

    const cached = this.cache.get(key);
    if (cached !== undefined) {
      // Re-insert on a hit, so the eviction below drops the least recently used.
      this.cache.delete(key);
      this.cache.set(key, cached);
      return cached ?? undefined;
    }

    const file = await resolveSitePath(this.db, version.eventIds, requestPath, options);
    if (file && !file.variants) return file;

    if (this.cache.size >= MAX_CACHED) {
      const oldest = this.cache.keys().next().value;
      if (oldest !== undefined) this.cache.delete(oldest);
    }
    // A path the version has no file for is cached too — the file list that
    // settled that is as immutable as the one that resolves a hit.
    this.cache.set(key, file ?? null);

    return file;
  }
}
