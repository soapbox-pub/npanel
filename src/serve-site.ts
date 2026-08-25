import { extname } from "node:path/posix";
import type { Kysely } from "kysely";
import { type BlobStore, type BlobVariants, ENCODINGS, type Encoding } from "./blobs.ts";
import type { Database, SiteEvent } from "./db/schema.ts";
import { isOnionAddress, wwwApex } from "./hostname.ts";
import type { Logger } from "./log.ts";
import { contentType } from "./mime.ts";
import { NIP05_PATH, type Nip05Entry, listNip05Names, nip05Document } from "./nip05.ts";
import { encodeNaddr } from "./nsite-address.ts";
import type { ServeCache } from "./serve-cache.ts";
import type { SiteCsp } from "./site-csp.ts";
import { SitePaths } from "./site-paths.ts";
import type { ServedHost } from "./sites.ts";

export interface SiteServerDeps {
  db: Kysely<Database>;
  blobs: BlobStore;
  /** Reads the policy a site declares for itself. Shared, so its cache is. */
  siteCsp: SiteCsp;
  /** Answers the host and version lookups every request opens with. */
  serveCache: ServeCache;
  log: Logger;
}

/**
 * Where a request came from, as settled by the listener that accepted it.
 *
 * Neither field can be trusted to the request itself. The scheme is not in it
 * at all — npanel only ever speaks plain HTTP, so behind the proxy it has to
 * come from `X-Forwarded-Proto`, while a request that arrived over Tor has no
 * proxy in front of it and no header worth reading, because Tor hands a
 * visitor's bytes on unaltered. The hostname rides along because it is decided
 * at the same place, and because the Tor listener has already refused
 * everything that is not one of its onions by the time it gets here.
 *
 * Deciding both once, at the boundary, is what keeps the question of which
 * headers may be believed out of every line below.
 */
export interface RequestOrigin {
  /** The hostname the client asked for, lowercased and without a port. */
  hostname: string;
  /** The scheme the client used, which is not the one npanel was spoken to in. */
  scheme: "http" | "https";
}

/** Serve one request for a site, as it reached the listener described by {@link RequestOrigin}. */
export type SiteServer = (request: Request, origin: RequestOrigin) => Promise<Response>;

/**
 * How long browsers may cache a file.
 *
 * A site's URLs are stable across deploys — only the blobs behind them change
 * — so a long max-age would keep serving the previous deploy from browser
 * caches for as long as it lasts. Documents revalidate every time, which the
 * `ETag` makes cheap; fingerprinted assets are the ones worth caching, and
 * those are only ever fetched by a document that revalidated first.
 */
const CACHE_CONTROL = "public, no-cache";
const ASSET_CACHE_CONTROL = "public, max-age=3600, must-revalidate";

/** Extensions whose contents are the site itself, rather than its assets. */
const DOCUMENT_TYPES = new Set([".html", ".htm", ".xhtml", ".json", ".webmanifest", ".xml", ".txt", ".md"]);

/** How long a browser may reuse a preflight result, in seconds. */
const PREFLIGHT_MAX_AGE = "86400";

/**
 * The policy sites are served under, unless they declare one or it's switched
 * off.
 *
 * This is verbatim the `<meta>` policy mkstack ships in its `index.html`, which
 * is what most sites reaching an npanel gateway are built from. It stands in
 * for the site that declared nothing, because a static site is exactly the
 * thing that has no business running an inline script, and a gateway hosts
 * other people's code on domains you own.
 *
 * A site that does declare a policy gets that one instead, delivered as a
 * header — see {@link file://./site-csp.ts}. Sending it rather than leaving it
 * to the `<meta>` tag covers what a meta tag can't: responses that aren't HTML,
 * and a document's own `<meta>` being reached only after the parser has already
 * run whatever came before it.
 */
export const CSP = [
  "default-src 'none'",
  "script-src 'self'",
  "style-src 'self' 'unsafe-inline'",
  "frame-src 'self' https:",
  "font-src 'self'",
  "base-uri 'self'",
  "manifest-src 'self'",
  "connect-src 'self' blob: https: wss:",
  "img-src 'self' data: blob: https:",
  "media-src 'self' https:",
].join("; ");

/**
 * Serve a request for one of the configured hostnames.
 *
 * Everything is read from the local blob store — npanel never proxies a
 * Blossom server at request time — so a served site keeps working even if
 * every relay and Blossom server it came from is offline.
 */
export function createSiteServer(deps: SiteServerDeps): SiteServer {
  const { db, blobs, siteCsp, serveCache, log } = deps;

  // Built here rather than injected: the path map is only ever read to serve a
  // file, so nothing outside this server has a lookup to share with it.
  const sitePaths = new SitePaths(db);

  /**
   * The redirect `www.example.com` gets when `example.com` asked for one.
   *
   * Only reached when no host of that name is configured, so a `www` host
   * someone added deliberately is served rather than redirected away.
   */
  async function wwwRedirect(url: URL, origin: RequestOrigin): Promise<Response | undefined> {
    const apex = wwwApex(origin.hostname);
    if (!apex) return undefined;

    const host = (await serveCache.lookup(apex))?.host;
    if (!host || host.enabled !== 1 || host.redirect_www !== 1) return undefined;

    const location = `${origin.scheme}://${apex}${url.pathname}${url.search}`;

    log.debug("site_www_redirect", { hostname: origin.hostname, location });

    return new Response(null, {
      status: 301,
      headers: {
        location,
        // A 301 with no freshness of its own is cached by browsers until they
        // are restarted, which would outlive an admin switching the redirect
        // back off. An hour is long enough that nobody pays for the hop twice
        // in a session, and short enough that undoing this is not a support
        // call. Crawlers take the 301 itself as the permanent answer.
        "cache-control": "public, max-age=3600",
      },
    });
  }

  async function handle(request: Request, origin: RequestOrigin): Promise<Response> {
    // Parsed once for everything below; three callees want the path.
    const url = new URL(request.url);

    // Both answers in one cache lookup. The version comes along because the
    // policy below is the version's and applies to every answer this host
    // gives — including the ones that never reach a file.
    const site = await serveCache.lookup(origin.hostname);
    const host = site?.host;
    const version = site?.version;
    const response = await respond(request, url, origin, host, version);

    // On unless this one site turned it off. A hostname with no host row has
    // nobody to have turned it off, so it gets the policy too — and gets the
    // gateway's, since there is no version to have declared another.
    if (host?.csp !== 0) {
      const declared = version && (await siteCsp.policy(version.event_id));
      response.headers.set("content-security-policy", declared || CSP);
    }

    // Set on everything rather than only on what varied, because a cache has
    // to be told before it stores the first response, and by then it is too
    // late to find out that some other path would have negotiated.
    response.headers.set("vary", "accept-encoding");

    advertiseOnion(url, origin, host, response);

    return response;
  }

  /**
   * A host's NIP-05 names, as the document {@link NIP05_PATH} serves.
   *
   * Undefined when the host has none, which is what lets a site that published
   * its own `nostr.json` go on serving it: npanel shadows that file only once
   * somebody has actually put a name in the database, and removing the last one
   * hands the path straight back.
   *
   * A request naming one gets that one; a request naming nothing gets the lot,
   * which is what makes a domain's names discoverable by anything that wants to
   * know who a gateway hosts. That is the same list a static `nostr.json` would
   * have handed over, so the choice was made when the names were added.
   */
  async function nip05Names(request: Request, host: ServedHost, url: URL): Promise<Response | undefined> {
    const entries = await listNip05Names(db, host.id);
    if (!entries.length) return undefined;

    const requested = url.searchParams.get("name");
    const wanted: Nip05Entry[] =
      requested === null ? entries : entries.filter((entry) => entry.name === requested.trim().toLowerCase());

    // A name nobody here answers for is an empty document rather than a 404:
    // the question was answered, and the answer is that this domain does not
    // know them.
    const body = new TextEncoder().encode(JSON.stringify(nip05Document(wanted)));

    const headers = new Headers({
      "content-type": "application/json; charset=utf-8",
      // Generated per request, so there is no blob hash to make an `ETag` from.
      // A minute bounds how long a name survives being changed, and absorbs the
      // repeat lookups every client that renders the identifier performs.
      "cache-control": "public, max-age=60",
      "content-length": String(body.byteLength),
    });

    log.debug("site_nip05", { hostname: host.hostname, name: requested, names: wanted.length });

    return new Response(request.method === "HEAD" ? null : body, { status: 200, headers });
  }

  async function respond(
    request: Request,
    url: URL,
    origin: RequestOrigin,
    host: ServedHost | undefined,
    version: SiteEvent | undefined,
  ): Promise<Response> {
    const { hostname } = origin;

    if (request.method !== "GET" && request.method !== "HEAD") {
      // An nsite is a pile of files. There is nothing to POST to.
      return new Response("This site only serves GET and HEAD requests.", {
        status: 405,
        headers: { "content-type": "text/plain; charset=utf-8", allow: "GET, HEAD" },
      });
    }

    if (!host) {
      const redirect = await wwwRedirect(url, origin);
      if (redirect) return redirect;
    }

    if (!host || host.enabled !== 1) {
      log.debug("site_unknown_host", { hostname });
      return text(404, "This domain is not configured.");
    }

    // Ahead of the deploy check below, because these names are npanel's and not
    // the site's: one added here resolves straight away, whether or not the
    // site behind the hostname has ever finished deploying.
    if (url.pathname === NIP05_PATH) {
      const names = await nip05Names(request, host, url);
      if (names) return names;
    }

    if (!version) {
      // Nothing servable yet: either the first deploy is still running or it failed.
      log.debug("site_not_deployed", { hostname });
      return new Response("This site has not finished deploying yet.", {
        status: 503,
        headers: { "content-type": "text/plain; charset=utf-8", "retry-after": "30" },
      });
    }

    const file = await sitePaths.resolve(version.event_id, url.pathname, { spa: host.spa === 1 });

    if (!file) {
      log.debug("site_path_missing", { hostname, path: url.pathname });
      return text(404, "Not found.");
    }

    // Sizes come with the path lookup when a deploy recorded them; the stat is
    // only for rows that predate that, until `compress-blobs` backfills them.
    const variants = file.variants ?? (await blobs.variants(file.sha256));
    if (!variants) {
      // The version is marked ready, so this means the blob was removed underneath us.
      log.error("blob_missing", { hostname, path: file.path, sha256: file.sha256 });
      return text(500, "This file is missing from local storage.");
    }

    // Everything below is about the representation this picks, not the blob:
    // its size, its `ETag`, and the bytes a `Range` counts from.
    const encoding = negotiateEncoding(request.headers.get("accept-encoding"), variants);
    const size = encoding ? (variants[encoding] as number) : variants.identity;

    const etag = etagFor(file.sha256, encoding);
    const headers = new Headers({
      "content-type": contentType(file.path),
      "cache-control": cacheControl(file.path),
      etag,
      "last-modified": lastModified(version.created_at, version.fetched_at),
      "accept-ranges": "bytes",
      // Which nsite this hostname is serving, in the one form a client can
      // paste anywhere: `naddr` carries the pubkey, kind, and `d` tag at once.
      "x-nsite-addr": encodeNaddr(host),
    });

    if (encoding) {
      headers.set("content-encoding", encoding);
    }

    if (matchesEtag(request.headers.get("if-none-match"), etag)) {
      return new Response(null, { status: 304, headers });
    }

    const range = parseRange(request.headers.get("range"), size);

    if (range === "unsatisfiable") {
      headers.set("content-range", `bytes */${size}`);
      return new Response(null, { status: 416, headers });
    }

    const status = file.status === 404 ? 404 : range ? 206 : 200;

    if (range) {
      headers.set("content-range", `bytes ${range.start}-${range.end}/${size}`);
      headers.set("content-length", String(range.end - range.start + 1));
    } else {
      headers.set("content-length", String(size));
    }

    if (request.method === "HEAD") {
      return new Response(null, { status, headers });
    }

    const body = await blobs.open(file.sha256, { encoding, range: range ?? undefined });

    log.debug("site_request", {
      hostname,
      path: url.pathname,
      status,
      sha256: file.sha256,
      bytes: size,
      encoding,
    });

    return new Response(body, { status, headers });
  }

  return async function serveSite(request: Request, origin: RequestOrigin): Promise<Response> {
    if (isPreflight(request)) return preflight(request);

    const response = await handle(request, origin);
    allowCrossOrigin(response.headers);

    return response;
  };
}

/**
 * Tell Tor Browser that this site has an onion address of its own.
 *
 * Sent only over HTTPS, which is what Tor Browser requires before it acts on
 * the header: on a plain-HTTP hop anything in the path could rewrite it and
 * send the visitor to an onion of its choosing, and the whole point is that the
 * address comes from the site. Only on a document, since that is what the
 * browser navigates, and never on a request that already arrived over Tor —
 * that visitor is where the header would be sending them.
 */
function advertiseOnion(url: URL, origin: RequestOrigin, host: ServedHost | undefined, response: Response): void {
  if (!host?.onion || isOnionAddress(origin.hostname)) return;
  if (response.status !== 200) return;
  if (origin.scheme !== "https") return;
  if (!response.headers.get("content-type")?.startsWith("text/html")) return;

  response.headers.set("onion-location", `http://${host.onion}${url.pathname}${url.search}`);
}

/**
 * Let any origin read any site.
 *
 * A site is a pile of public files, served identically to everyone and with no
 * cookie or credential anywhere in the request — so the same-origin policy
 * protects nothing here, and withholding these headers only breaks the callers
 * with a legitimate reason to read one site from another: viewers that render
 * a site's `feed.xml`, importers, archivers. `*` also forbids credentialed
 * requests outright, which is the behavior we want regardless.
 */
function allowCrossOrigin(headers: Headers): void {
  headers.set("access-control-allow-origin", "*");

  // Every header, rather than the handful a `fetch` needs today. CORS hides
  // everything outside a safelist too small to revalidate an `ETag` or resume
  // from a `Content-Range`, and a list of exceptions drifts from what the
  // responses actually set — as it did for `Onion-Location`. The wildcard
  // degrades to a literal header name on a credentialed request, which is moot
  // here: the `*` above already refuses those, and a site has no credential to
  // send anyway.
  headers.set("access-control-expose-headers", "*");
}

/**
 * Whether a request is a CORS preflight rather than something to serve.
 *
 * A plain `OPTIONS` is not one, and keeps falling through to the 405 that
 * tells the client which methods a site does answer.
 */
function isPreflight(request: Request): boolean {
  return request.method === "OPTIONS" && request.headers.has("access-control-request-method");
}

/**
 * Answer a preflight.
 *
 * A simple `GET` never triggers one, but a `fetch` that sets `Range` or
 * `If-None-Match` does, and failing it would fail the request that follows.
 * The requested headers are echoed back because a static file server has no
 * request header it needs to refuse.
 */
function preflight(request: Request): Response {
  const headers = new Headers({
    "access-control-allow-origin": "*",
    "access-control-allow-methods": "GET, HEAD, OPTIONS",
    "access-control-max-age": PREFLIGHT_MAX_AGE,
  });

  const requested = request.headers.get("access-control-request-headers");
  if (requested) {
    headers.set("access-control-allow-headers", requested);
  }

  return new Response(null, { status: 204, headers });
}

/**
 * Pick which stored copy of a blob to serve.
 *
 * `q` is honored the way the header defines it — a ranking, in which zero is a
 * refusal — but ties are broken by size rather than by any preference of
 * npanel's own. That tiebreak is the case that actually happens: browsers send
 * their encodings at equal quality, so this serves the smallest copy the
 * client can read. Tor Browser reaching an onion offers only `gzip`, because
 * Firefox withholds `br` from plain HTTP, and gets the gzip copy.
 *
 * Returns undefined for the blob itself. A client that refuses `identity` and
 * accepts nothing stored gets it anyway rather than a 406 — for a static file
 * a slower answer beats no answer.
 */
export function negotiateEncoding(header: string | null, variants: BlobVariants): Encoding | undefined {
  if (!header) return undefined;

  const quality = parseAcceptEncoding(header);
  const candidates = ENCODINGS.flatMap((encoding) => {
    const size = variants[encoding];
    if (size === undefined) return [];

    const q = quality.get(encoding) ?? quality.get("*") ?? 0;
    return q > 0 ? [{ encoding, size, q }] : [];
  });

  if (!candidates.length) return undefined;

  candidates.sort((a, b) => b.q - a.q || a.size - b.size);
  const best = candidates[0];

  // A copy is only stored when it beat the blob by a margin, but serving one
  // that doesn't would be worse than pointless.
  return best.size < variants.identity ? best.encoding : undefined;
}

/** Quality values by encoding name, lowercased, from an `Accept-Encoding`. */
function parseAcceptEncoding(header: string): Map<string, number> {
  const quality = new Map<string, number>();

  for (const entry of header.split(",")) {
    const [token, ...params] = entry.split(";");
    const name = token.trim().toLowerCase();
    if (!name) continue;

    const q = params.map((param) => param.trim().toLowerCase()).find((param) => param.startsWith("q="));
    const value = q === undefined ? 1 : Number(q.slice(2));

    // A malformed q is treated as absent, which is what the header's own
    // grammar says to do with a parameter that can't be read.
    quality.set(name, Number.isFinite(value) ? Math.min(Math.max(value, 0), 1) : 1);
  }

  return quality;
}

/**
 * The `ETag` for one representation of a blob.
 *
 * Encodings need tags of their own: with one tag across all three, a shared
 * cache that stored the brotli copy would hand it to the next client whose
 * `If-None-Match` matched, however little brotli that client speaks.
 */
export function etagFor(sha256: string, encoding: Encoding | undefined): string {
  return encoding ? `"${sha256}-${encoding}"` : `"${sha256}"`;
}

/** Parse a single-range `Range` header. Multi-range requests are ignored. */
export function parseRange(
  header: string | null,
  size: number,
): { start: number; end: number } | "unsatisfiable" | undefined {
  if (!header) return undefined;

  const match = header.match(/^bytes=(\d*)-(\d*)$/);
  if (!match) return undefined;

  const [, rawStart, rawEnd] = match;

  if (!rawStart && !rawEnd) return undefined;

  let start: number;
  let end: number;

  if (!rawStart) {
    // Suffix range: the last N bytes.
    const suffix = Number(rawEnd);
    if (suffix === 0) return "unsatisfiable";
    start = Math.max(size - suffix, 0);
    end = size - 1;
  } else {
    start = Number(rawStart);
    end = rawEnd ? Math.min(Number(rawEnd), size - 1) : size - 1;
  }

  if (start > end || start >= size) return "unsatisfiable";

  return { start, end };
}

/** How long a file may be cached, by what kind of file it is. */
export function cacheControl(path: string): string {
  return DOCUMENT_TYPES.has(extname(path).toLowerCase()) ? CACHE_CONTROL : ASSET_CACHE_CONTROL;
}

/**
 * `Last-Modified` for a version.
 *
 * A manifest's `created_at` is whatever its author put there, including dates
 * in the future or beyond what `Date` can represent, so it is capped at the
 * moment npanel actually saw the event.
 */
export function lastModified(createdAt: number, fetchedAt: number): string {
  const seconds = Number.isFinite(createdAt) ? Math.min(createdAt, fetchedAt) : fetchedAt;
  return new Date(Math.max(seconds, 0) * 1000).toUTCString();
}

/** Whether an `If-None-Match` header matches the current ETag. */
export function matchesEtag(header: string | null, etag: string): boolean {
  if (!header) return false;
  if (header.trim() === "*") return true;

  return header
    .split(",")
    .map((value) => value.trim().replace(/^W\//, ""))
    .includes(etag);
}

function text(status: number, body: string): Response {
  return new Response(body, {
    status,
    headers: { "content-type": "text/plain; charset=utf-8" },
  });
}
