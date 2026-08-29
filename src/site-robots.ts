import type { BlobStore } from "./blobs.ts";
import { MAX_ROBOTS_BYTES, augmentRobots, robotsNeedsSitemap } from "./seo.ts";
import type { RequestOrigin } from "./serve-site.ts";

/**
 * Files to keep. A `robots.txt` is a few dozen bytes, and there is one per
 * deployed version rather than one per request path.
 */
const MAX_CACHED = 200;

/**
 * A site's own `robots.txt`, with npanel's `Sitemap:` line appended.
 *
 * Keyed by the hash of the file rather than by host or version, for the same
 * reason {@link file://./site-preview.ts} is: content that hashes the same is
 * the same content, so nothing can go stale, and the many sites sharing one
 * boilerplate `robots.txt` share the entry.
 *
 * What is held is the file, never the answer. The `Sitemap:` line names the
 * hostname it is served at, so six hosts sharing one blob must each get their
 * own URL — caching the finished body under the hash is precisely how they
 * would end up advertising somebody else's sitemap.
 */
export class SiteRobots {
  /** The file to append to, or null for one to serve exactly as published. */
  private cache = new Map<string, string | null>();

  constructor(private readonly blobs: BlobStore) {}

  /**
   * The body to serve for this `robots.txt`, or undefined to serve the blob
   * itself.
   *
   * `size` comes from the lookup the request already made, so a file too large
   * to read is refused without opening it.
   */
  async body(sha256: string, size: number, origin: RequestOrigin): Promise<string | undefined> {
    if (size > MAX_ROBOTS_BYTES) return undefined;

    let text = this.cache.get(sha256);

    if (text === undefined) {
      const published = await this.blobs.readText(sha256, MAX_ROBOTS_BYTES);
      text = robotsNeedsSitemap(published) ? published : null;

      // Oldest first, so this drops the least recently added entry.
      if (this.cache.size >= MAX_CACHED) {
        const oldest = this.cache.keys().next().value;
        if (oldest !== undefined) this.cache.delete(oldest);
      }
      this.cache.set(sha256, text);
    }

    return text === null ? undefined : augmentRobots(text, origin);
  }
}
