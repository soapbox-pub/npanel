import { decodeHTML } from "entities";
import type { Kysely } from "kysely";
import { parse } from "node-html-parser";
import type { BlobStore } from "./blobs.ts";
import type { Database } from "./db/schema.ts";
import { readHtmlHead } from "./html.ts";
import type { Logger } from "./log.ts";

/**
 * Largest policy npanel will send, in characters.
 *
 * A policy long enough to matter here is one built from script hashes, and a
 * header field that outgrows what proxies accept would take the whole response
 * down with it. Over this, the site falls back to the gateway's own policy —
 * still protected, just not with what it asked for.
 */
const MAX_POLICY_LENGTH = 8 * 1024;

/**
 * Characters a header field value may carry: printable ASCII, plus tab.
 *
 * Anything else is either a policy that isn't one or an attempt to write a
 * second header from inside a site's markup, and both are refused rather than
 * escaped. `Headers.set` would throw on the worst of them anyway, which on the
 * serving path would be a 500 for a site whose only mistake was a stray
 * newline in an attribute.
 */
const INVALID_HEADER_CHARS = /[^\t\x20-\x7e]/;

/** Parsed policies to keep. Each entry is a build id and a short string. */
const MAX_CACHED = 500;

/**
 * The Content Security Policy a site declares for itself, read from the
 * `<meta>` tags of its own `index.html`.
 *
 * Cached by build rather than by content hash, because that is the lookup
 * being avoided: a build's files are written once and never edited, so the
 * policy for a build cannot change, and holding it keeps the file query off
 * the serving path entirely. Read from the build rather than the manifest, so
 * a document npanel rewrote and the header it is sent under always agree.
 */
export class SiteCsp {
  private cache = new Map<number, string | null>();

  constructor(
    private readonly db: Kysely<Database>,
    private readonly blobs: BlobStore,
    private readonly log: Logger,
  ) {}

  /** The policy this build declares, or null if it declares none. */
  async policy(buildId: number): Promise<string | null> {
    const cached = this.cache.get(buildId);
    if (cached !== undefined) return cached;

    const policy = await this.read(buildId);

    // Oldest first, so this drops the least recently added entry.
    if (this.cache.size >= MAX_CACHED) {
      const oldest = this.cache.keys().next().value;
      if (oldest !== undefined) this.cache.delete(oldest);
    }
    this.cache.set(buildId, policy);

    return policy;
  }

  /**
   * Read `/index.html` for this build and scrape its policy.
   *
   * Always the landing page, whatever was requested: one policy per build,
   * so a site's stylesheets and images are served under the same rules as the
   * document that named them. A site that publishes no `/index.html` — one
   * that is a single feed, say — declares nothing.
   */
  private async read(buildId: number): Promise<string | null> {
    const index = await this.db
      .selectFrom("host_build_files")
      .select("sha256")
      .where("build_id", "=", buildId)
      .where("path", "=", "/index.html")
      .executeTakeFirst();

    if (!index) return null;

    const policy = extractCsp(await readHtmlHead(this.blobs, index.sha256));
    if (policy) {
      this.log.debug("site_csp_adopted", { build_id: buildId, policy });
    }

    return policy;
  }
}

/**
 * The enforcing Content Security Policy an HTML document declares.
 *
 * Every matching `<meta http-equiv>` is taken, joined into the comma-separated
 * list the header defines, because that is what the document already meant: a
 * browser enforces each meta policy on top of the last, and comma-separated
 * policies in one header behave identically. A `-Report-Only` tag is skipped —
 * a policy the site asked only to be told about must not become the one it is
 * held to.
 */
export function extractCsp(html: string): string | null {
  const root = parse(html);
  const policies: string[] = [];

  for (const element of root.querySelectorAll("meta")) {
    if (element.getAttribute("http-equiv")?.trim().toLowerCase() !== "content-security-policy") continue;

    const content = element.getAttribute("content");
    if (!content) continue;

    const policy = decodeHTML(content).trim();
    if (!policy || INVALID_HEADER_CHARS.test(policy)) continue;

    policies.push(policy);
  }

  if (!policies.length) return null;

  const combined = policies.join(", ");

  return combined.length <= MAX_POLICY_LENGTH ? combined : null;
}
