import { Readable } from "node:stream";
import type { Kysely } from "kysely";
import type { BlobStore } from "./blobs.ts";
import type { Database } from "./db/schema.ts";
import type { Logger } from "./log.ts";
import { contentType } from "./mime.ts";
import { activeEvent, findHost, resolveSitePath } from "./sites.ts";

export interface SiteServerDeps {
  db: Kysely<Database>;
  blobs: BlobStore;
  log: Logger;
}

/** How long browsers may cache a file. Content is addressed by hash anyway. */
const CACHE_CONTROL = "public, max-age=3600";

/**
 * Serve a request for one of the configured hostnames.
 *
 * Everything is read from the local blob store — npanel never proxies a
 * Blossom server at request time — so a served site keeps working even if
 * every relay and Blossom server it came from is offline.
 */
export function createSiteServer(deps: SiteServerDeps) {
  const { db, blobs, log } = deps;

  return async function serveSite(request: Request, hostname: string): Promise<Response> {
    const host = await findHost(db, hostname);

    if (!host || host.enabled !== 1) {
      log.debug("site_unknown_host", { hostname });
      return text(404, "This hostname is not configured.");
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
    const file = await resolveSitePath(db, version.event_id, url.pathname);

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
      "cache-control": CACHE_CONTROL,
      etag,
      "last-modified": new Date(version.created_at * 1000).toUTCString(),
      "accept-ranges": "bytes",
      "x-npanel-pubkey": host.pubkey,
      "x-npanel-event": version.event_id,
    });
    if (host.identifier) {
      headers.set("x-npanel-identifier", host.identifier);
    }

    if (matchesEtag(request.headers.get("if-none-match"), etag)) {
      return new Response(null, { status: 304, headers });
    }

    const range = parseRange(request.headers.get("range"), size);

    if (range === "unsatisfiable") {
      headers.set("content-range", `bytes */${size}`);
      return new Response(null, { status: 416, headers });
    }

    const status = file.is404 ? 404 : range ? 206 : 200;

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
  };
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
