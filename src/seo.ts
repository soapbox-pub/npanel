import { extname, join } from "node:path/posix";
import type { Kysely } from "kysely";
import type { Database } from "./db/schema.ts";
import type { RequestOrigin } from "./serve-site.ts";
import { normalizeSitePath } from "./sites.ts";

/**
 * What npanel does for a site's standing in search engines, universally.
 *
 * Three things, all derived from data npanel already holds and none touching
 * the site's own bytes: a `sitemap.xml` generated from the version's file
 * list, a `robots.txt` that points at it, and a canonical URL on every page.
 * The generated files are fallbacks — a site that published its own is served
 * as-is, the same shadow-only-if-absent rule `nostr.json` follows in reverse.
 */

/** The pages a sitemap lists: real documents, not the assets they load. */
const HTML_EXTENSIONS = new Set([".html", ".htm"]);

/** The protocol's cap on URLs per sitemap file. */
const MAX_SITEMAP_URLS = 50_000;

/**
 * The one canonical spelling of a served file's URL path.
 *
 * Extensions come off: `/foo`, `/foo/`, `/foo.html` and `/foo/index.html` can
 * all serve the same bytes here, and a crawler scores them as competing pages.
 * The clean form is the one to keep, because it is the one static site
 * generators link to — VitePress with `cleanUrls` publishes `hacking.html` and
 * links to `/hacking` throughout, names that URL in its own sitemap, and puts
 * it in the page's `og:url`. Naming the file instead would have npanel
 * contradicting the site about its own address.
 *
 * It is also always servable, which is what makes this safe rather than a
 * guess: {@link file://./sites.ts} resolves an extensionless path to its
 * `.html` sibling for exactly this reason.
 *
 * Only `.html`, and not `.htm`, since only the former has that sibling step.
 */
export function canonicalPath(filePath: string): string {
  if (filePath.endsWith("/index.html")) {
    return filePath.slice(0, -"index.html".length);
  }
  if (filePath.endsWith(".html")) {
    return filePath.slice(0, -".html".length);
  }
  return filePath;
}

/**
 * The `Link: rel="canonical"` value for a response, or undefined when the
 * response shouldn't carry one.
 *
 * Only a real page gets it: a 200, for a document, reached under one of the
 * spellings that actually names it — the exact path, its clean-URL form, or
 * its directory. That last condition is what keeps the header off SPA-mode
 * fallbacks, where `/any/client/route` serves the shell under a 200 and a
 * canonical would tell crawlers the whole app is one page.
 */
export function canonicalLink(
  origin: RequestOrigin,
  requestPath: string,
  file: { path: string; status: number },
): string | undefined {
  if (file.status !== 200) return undefined;
  if (!HTML_EXTENSIONS.has(extname(file.path).toLowerCase())) return undefined;

  const normalized = normalizeSitePath(requestPath);
  const spellsFile =
    file.path === normalized || file.path === `${normalized}.html` || file.path === join(normalized, "index.html");
  if (!spellsFile) return undefined;

  return `<${absoluteUrl(origin, canonicalPath(file.path))}>; rel="canonical"`;
}

/** Every canonical page URL of a version, for its sitemap. */
export async function sitemapPaths(db: Kysely<Database>, eventId: string): Promise<string[]> {
  const rows = await db.selectFrom("site_files").select("path").where("event_id", "=", eventId).execute();

  const canonical = new Set<string>();
  for (const { path } of rows) {
    // The not-found page is the one document a crawler must not be invited to.
    if (path === "/404.html") continue;
    if (!HTML_EXTENSIONS.has(extname(path).toLowerCase())) continue;
    canonical.add(canonicalPath(path));
  }

  return [...canonical].sort().slice(0, MAX_SITEMAP_URLS);
}

/** Render a sitemap for the site as reached at `origin`. */
export function sitemapXml(origin: RequestOrigin, paths: string[], lastmod: string): string {
  const urls = paths.map(
    (path) => `<url><loc>${escapeXml(absoluteUrl(origin, path))}</loc><lastmod>${lastmod}</lastmod></url>`,
  );

  return [
    `<?xml version="1.0" encoding="UTF-8"?>`,
    `<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">`,
    ...urls,
    "</urlset>",
    "",
  ].join("\n");
}

/**
 * `<lastmod>` for a version: the manifest's own date, capped at when npanel
 * saw it — the same rule its `Last-Modified` follows, since an author can date
 * a manifest however they please.
 */
export function versionDate(version: { created_at: number; fetched_at: number }): string {
  const seconds = Number.isFinite(version.created_at)
    ? Math.min(version.created_at, version.fetched_at)
    : version.fetched_at;

  return new Date(Math.max(seconds, 0) * 1000).toISOString().slice(0, 10);
}

/** The `robots.txt` a site gets when it published none: allow all, name the sitemap. */
export function robotsTxt(origin: RequestOrigin): string {
  return `User-agent: *\nAllow: /\n\nSitemap: ${absoluteUrl(origin, "/sitemap.xml")}\n`;
}

/**
 * The most of a `robots.txt` npanel will read before serving it as it is.
 *
 * This runs on the serving path, and a site may publish a file of any size at
 * that name. Anything larger goes out as the blob, untouched.
 */
export const MAX_ROBOTS_BYTES = 64 * 1024;

/** A `Sitemap:` line anywhere in the file, whatever its case or indentation. */
const SITEMAP_DIRECTIVE = /^[ \t]*sitemap[ \t]*:/im;

/**
 * A rule shutting the whole site to some crawler.
 *
 * Matched loosely on purpose — any group's blanket `Disallow: /` is enough to
 * leave the file alone. Reading which group it belongs to would be a robots
 * parser, and the question here is only whether the site looks like it wants
 * to be indexed at all.
 */
const DISALLOW_EVERYTHING = /^[ \t]*disallow[ \t]*:[ \t]*\/[ \t]*$/im;

/**
 * Whether npanel should append its `Sitemap:` line to a site's own
 * `robots.txt`.
 *
 * False when the file already names a sitemap, whichever file that is: a site
 * pointing at its own `sitemap-index.xml` has answered the question, and a
 * second pointer would be npanel arguing with it. False too when the file
 * shuts a crawler out of the whole site, where an invitation to index would be
 * a strange thing to add on the site's behalf.
 */
export function robotsNeedsSitemap(text: string): boolean {
  return !SITEMAP_DIRECTIVE.test(text) && !DISALLOW_EVERYTHING.test(text);
}

/**
 * A site's own `robots.txt` with npanel's `Sitemap:` line appended.
 *
 * This is the one place npanel adds to a site's bytes rather than standing in
 * for a file it never published, and it stays narrow to earn that. `Sitemap`
 * is a non-group directive: it grants no crawler anything, and appending one
 * cannot change what an `Allow` or `Disallow` above it means. The original is
 * copied through verbatim — never reflowed, reordered, or rewritten — so the
 * only difference is the line at the end.
 *
 * Only for a file {@link robotsNeedsSitemap} accepted.
 */
export function augmentRobots(text: string, origin: RequestOrigin): string {
  const sitemap = `Sitemap: ${absoluteUrl(origin, "/sitemap.xml")}\n`;

  // An empty file has no last group for a blank line to separate this from.
  const body = text.replace(/\s*$/, "");
  if (!body) return sitemap;

  // A blank line before, so the directive can't be read as part of the last
  // group, and one after, because a file ending mid-line is how the site's own
  // last directive would get lost if anything ever appended again.
  return `${body}\n\n${sitemap}`;
}

/**
 * A site path as an absolute URL at `origin`.
 *
 * Through `URL` rather than concatenation, because stored paths are
 * percent-decoded and a sitemap or header needs them encoded again.
 */
function absoluteUrl(origin: RequestOrigin, path: string): string {
  return new URL(path, `${origin.scheme}://${origin.hostname}`).href;
}

function escapeXml(value: string): string {
  return value.replaceAll("&", "&amp;").replaceAll("<", "&lt;").replaceAll(">", "&gt;");
}
