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
 * `index.html` collapses into its directory, because `/foo`, `/foo/`, and
 * `/foo/index.html` all serve the same bytes here and a crawler scores them as
 * three competing pages. Every other path keeps the manifest's own spelling —
 * rewriting `/foo.html` to `/foo` would be a guess about somebody else's site.
 */
export function canonicalPath(filePath: string): string {
  if (filePath.endsWith("/index.html")) {
    return filePath.slice(0, -"index.html".length);
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
