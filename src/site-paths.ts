import type { Kysely } from "kysely";
import type { Database } from "./db/schema.ts";
import { type ResolvedFile, resolveSitePath } from "./sites.ts";

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
 * Which file a request path resolves to within a build, cached.
 *
 * This is the last database query on a warm site request, and it can be held
 * for the same reason {@link file://./site-csp.ts} holds a policy: a build is
 * immutable. Its file rows are written in the same transaction that makes it
 * ready and nothing edits them afterwards, and a build id is never reused, so
 * a rebuild is a different key rather than a changed one. Nothing has to
 * invalidate this cache, and nothing does.
 *
 * The blob sizes that ride along on the lookup are the one part that isn't
 * settled when the build is — `compress-blobs` can record them for a blob
 * stored before npanel recorded any. A row still missing them is left
 * uncached rather than pinned to the stat it falls back to, which is what
 * keeps every entry here immutable and the invalidation story empty.
 */
export class SitePaths {
  private cache = new Map<string, ResolvedFile | null>();

  constructor(private readonly db: Kysely<Database>) {}

  /** The file this path names within a build, as {@link resolveSitePath} answers it. */
  async resolve(buildId: number, requestPath: string, options: { spa: boolean }): Promise<ResolvedFile | undefined> {
    // The SPA flag is part of the key because it is read per request rather
    // than built in: switching it must not have to wait for a rebuild.
    const key = `${buildId}:${options.spa ? "spa" : ""}:${requestPath}`;

    const cached = this.cache.get(key);
    if (cached !== undefined) {
      // Re-insert on a hit, so the eviction below drops the least recently used.
      this.cache.delete(key);
      this.cache.set(key, cached);
      return cached ?? undefined;
    }

    const file = await resolveSitePath(this.db, buildId, requestPath, options);
    if (file && !file.variants) return file;

    if (this.cache.size >= MAX_CACHED) {
      const oldest = this.cache.keys().next().value;
      if (oldest !== undefined) this.cache.delete(oldest);
    }
    // A path the build has no file for is cached too — the file list that
    // settled that is as immutable as the one that resolves a hit.
    this.cache.set(key, file ?? null);

    return file;
  }
}
