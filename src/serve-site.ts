import { extname } from "node:path/posix";
import { Readable } from "node:stream";
import type { Kysely } from "kysely";
import type { BlobStore } from "./blobs.ts";
import type { Database } from "./db/schema.ts";
import { isOnionAddress, wwwApex } from "./hostname.ts";
import type { Logger } from "./log.ts";
import { contentType } from "./mime.ts";
import { encodeNaddr } from "./nsite-address.ts";
import { type ServedHost, activeEvent, findHost, resolveSitePath } from "./sites.ts";

export interface SiteServerDeps {
  db: Kysely<Database>;
  blobs: BlobStore;
  log: Logger;
}

/**
 * Serve one request for a site.
 *
 * The hostname is passed in rather than read from the request, because which
 * header carries it depends on how the request arrived: through the proxy, or
 * straight from Tor.
 */
export type SiteServer = (request: Request, hostname: string) => Promise<Response>;

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

/**
 * Response headers a cross-origin script may read.
 *
 * CORS otherwise hides everything outside its safelist, which is small enough
 * that a `fetch` could not read the `ETag` to revalidate with, or the
 * `Content-Range` to resume a download from.
 */
const EXPOSED_HEADERS = ["accept-ranges", "content-length", "content-range", "etag", "x-nsite-addr"].join(", ");

/** How long a browser may reuse a preflight result, in seconds. */
const PREFLIGHT_MAX_AGE = "86400";

/**
 * The policy sites are served under, unless it's switched off.
 *
 * This is verbatim the `<meta>` policy mkstack ships in its `index.html`, which
 * is what most sites reaching an npanel gateway are built from. Sent as a
 * header it also covers everything a meta tag can't: responses that aren't
 * HTML, and the document's own `<meta>` being reached only after the parser
 * has already run whatever came before it.
 *
 * A site that ships its own policy still gets it — a document under two
 * policies must satisfy both — so this can only tighten a site, never loosen
 * one. That is also why it can break a site that expected neither: `script-src
 * 'self'` forbids inline scripts and CDN script tags outright. A site that
 * needs either turns its own `csp` off; there is deliberately no way to turn
 * the policy off for the whole gateway at once.
 */
const CSP = [
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
  const { db, blobs, log } = deps;

  /**
   * The redirect `www.example.com` gets when `example.com` asked for one.
   *
   * Only reached when no host of that name is configured, so a `www` host
   * someone added deliberately is served rather than redirected away.
   */
  async function wwwRedirect(request: Request, hostname: string): Promise<Response | undefined> {
    const apex = wwwApex(hostname);
    if (!apex) return undefined;

    const host = await findHost(db, apex);
    if (!host || host.enabled !== 1 || host.redirect_www !== 1) return undefined;

    const url = new URL(request.url);
    const location = `${requestScheme(request, hostname)}://${apex}${url.pathname}${url.search}`;

    log.debug("site_www_redirect", { hostname, location });

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

  async function handle(request: Request, hostname: string): Promise<Response> {
    const host = await findHost(db, hostname);
    const response = await respond(request, hostname, host);

    // On unless this one site turned it off. A hostname with no host row has
    // nobody to have turned it off, so it gets the policy too.
    if (host?.csp !== 0) {
      response.headers.set("content-security-policy", CSP);
    }

    advertiseOnion(request, hostname, host, response);

    return response;
  }

  async function respond(request: Request, hostname: string, host: ServedHost | undefined): Promise<Response> {
    if (request.method !== "GET" && request.method !== "HEAD") {
      // An nsite is a pile of files. There is nothing to POST to.
      return new Response("This site only serves GET and HEAD requests.", {
        status: 405,
        headers: { "content-type": "text/plain; charset=utf-8", allow: "GET, HEAD" },
      });
    }

    if (!host) {
      const redirect = await wwwRedirect(request, hostname);
      if (redirect) return redirect;
    }

    if (!host || host.enabled !== 1) {
      log.debug("site_unknown_host", { hostname });
      return text(404, "This domain is not configured.");
    }

    const version = await activeEvent(db, host);
    if (!version) {
      // Nothing servable yet: either the first deploy is still running or it failed.
      log.debug("site_not_deployed", { hostname });
      return new Response("This site has not finished deploying yet.", {
        status: 503,
        headers: { "content-type": "text/plain; charset=utf-8", "retry-after": "30" },
      });
    }

    const url = new URL(request.url);
    const file = await resolveSitePath(db, version.event_id, url.pathname, { spa: host.spa === 1 });

    if (!file) {
      log.debug("site_path_missing", { hostname, path: url.pathname });
      return text(404, "Not found.");
    }

    const size = await blobs.size(file.sha256);
    if (size === undefined) {
      // The version is marked ready, so this means the blob was removed underneath us.
      log.error("blob_missing", { hostname, path: file.path, sha256: file.sha256 });
      return text(500, "This file is missing from local storage.");
    }

    const etag = `"${file.sha256}"`;
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

    const stream = blobs.read(file.sha256, range ?? undefined);
    const body = Readable.toWeb(stream) as ReadableStream<Uint8Array>;

    log.debug("site_request", {
      hostname,
      path: url.pathname,
      status,
      sha256: file.sha256,
      bytes: size,
    });

    return new Response(body, { status, headers });
  }

  return async function serveSite(request: Request, hostname: string): Promise<Response> {
    if (isPreflight(request)) return preflight(request);

    const response = await handle(request, hostname);
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
function advertiseOnion(request: Request, hostname: string, host: ServedHost | undefined, response: Response): void {
  if (!host?.onion || isOnionAddress(hostname)) return;
  if (response.status !== 200) return;
  if (requestScheme(request, hostname) !== "https") return;
  if (!response.headers.get("content-type")?.startsWith("text/html")) return;

  const url = new URL(request.url);
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
  headers.set("access-control-expose-headers", EXPOSED_HEADERS);
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
 * The scheme the client used, for building an absolute redirect.
 *
 * npanel is always behind a proxy that terminates TLS, so the URL this process
 * sees is `http://` even when the request that reached the proxy was `https://`
 * — redirecting to the scheme of the local request would downgrade every
 * redirect. `X-Forwarded-Proto` is the proxy's word for it, and `https` is the
 * assumption when the proxy didn't say, since a plain-HTTP deployment is the
 * unusual one and the wrong guess there costs a redirect rather than a
 * downgrade.
 *
 * An onion request is the exception, and inverts that reasoning: it reaches
 * npanel straight from the Tor daemon, with no proxy and no TLS anywhere, so
 * `https` would be wrong every time. Tor authenticates and encrypts the
 * connection itself, which is what the scheme would otherwise be claiming.
 */
export function requestScheme(request: Request, hostname: string): string {
  const forwarded = request.headers.get("x-forwarded-proto")?.split(",")[0]?.trim().toLowerCase();
  if (forwarded === "http" || forwarded === "https") return forwarded;

  return isOnionAddress(hostname) ? "http" : "https";
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
