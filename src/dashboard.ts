import { openAsBlob } from "node:fs";
import { readFile, stat } from "node:fs/promises";
import { join, normalize, resolve, sep } from "node:path";
import type { Logger } from "./log.ts";
import { contentType } from "./mime.ts";

export interface DashboardDeps {
  /** Directory holding the built dashboard (`dashboard/dist`). */
  dir: string;
  /** Hostname the dashboard is served from, used to build absolute URLs. */
  dashboardHost: string;
  log: Logger;
}

/**
 * Link-preview meta tags whose content has to be an absolute URL.
 *
 * The built `index.html` is a static file that can't know its own origin, so
 * it ships site-relative paths and they're rewritten here. Crawlers are
 * inconsistent about resolving relative `og:image` values, and the ones that
 * don't simply show no image.
 */
const PREVIEW_URL_META = /(<meta\s+(?:property|name)="(?:og:image|og:url|twitter:image)"\s+content=")(\/[^"]*)(")/g;

/** Hostnames that aren't reachable over TLS, so previews should say `http`. */
const LOOPBACK = /^(localhost|127(\.\d+){3}|\[?::1\]?)$/;

/** Shared, since an encoder holds no state between calls. */
const UTF8 = new TextEncoder();

/** A file resolved inside the dashboard directory. */
interface DashboardFile {
  path: string;
  size: number;
  mtimeMs: number;
  /** Fingerprinted by Vite, so it can be cached forever. */
  immutable: boolean;
  html: boolean;
}

/**
 * Serve the built dashboard as a single-page app.
 *
 * Unknown paths fall back to `index.html` so client-side routes work on a
 * hard refresh. Hashed Vite assets are immutable; `index.html` never is.
 */
export function createDashboardServer(deps: DashboardDeps) {
  const root = resolve(deps.dir);
  const origin = originFor(deps.dashboardHost);
  /** Rendered HTML by path, reused until the file on disk changes. */
  const rendered = new Map<string, { mtimeMs: number; size: number; html: string; length: number }>();
  let warned = false;

  return async function serveDashboard(request: Request): Promise<Response> {
    const url = new URL(request.url);
    const file = (await find(decodePath(url.pathname))) ?? (await find("/index.html"));

    if (!file) {
      if (!warned) {
        warned = true;
        deps.log.warn("dashboard_not_built", { dir: root });
      }
      return new Response("The dashboard has not been built. Run `bun run dashboard:build`.", {
        status: 503,
        headers: { "content-type": "text/plain; charset=utf-8" },
      });
    }

    // HTML is rewritten before it goes out, so it can't be streamed from disk.
    const page = file.html ? await render(file) : undefined;

    const headers = new Headers({
      "content-type": contentType(file.path),
      "content-length": String(page ? page.length : file.size),
      "cache-control": file.immutable ? "public, max-age=31536000, immutable" : "no-cache",
    });

    if (request.method === "HEAD") {
      return new Response(null, { headers });
    }

    if (page) {
      return new Response(page.html, { headers });
    }

    // File-backed, so Bun sends it with sendfile rather than through JS.
    return new Response(await openAsBlob(file.path), { headers });
  };

  /** Read an HTML file and apply the runtime substitutions. */
  async function render(file: DashboardFile): Promise<{ html: string; length: number }> {
    const cached = rendered.get(file.path);
    if (cached && cached.mtimeMs === file.mtimeMs && cached.size === file.size) {
      return cached;
    }

    const source = await readFile(file.path, "utf8");
    const html = source.replace(PREVIEW_URL_META, (_match, head, path, tail) => `${head}${origin}${path}${tail}`);
    // Rewriting happens per character but Content-Length is per byte.
    const entry = { mtimeMs: file.mtimeMs, size: file.size, html, length: UTF8.encode(html).byteLength };

    rendered.set(file.path, entry);
    return entry;
  }

  /** Resolve a URL path to a file inside the dashboard directory. */
  async function find(path: string): Promise<DashboardFile | undefined> {
    // Normalize before joining so `..` can't escape the dashboard directory.
    const relative = normalize(path)
      .replace(/^(\.\.[/\\])+/, "")
      .replace(/^[/\\]+/, "");
    const candidate = join(root, relative);

    if (candidate !== root && !candidate.startsWith(root + sep)) return undefined;

    try {
      const stats = await stat(candidate);
      if (!stats.isFile()) return undefined;
      return {
        path: candidate,
        size: stats.size,
        mtimeMs: stats.mtimeMs,
        immutable: relative.startsWith("assets/"),
        html: candidate.endsWith(".html"),
      };
    } catch {
      return undefined;
    }
  }
}

/**
 * The dashboard's public origin.
 *
 * npanel expects a TLS-terminating proxy in front of it, so any real hostname
 * is assumed to be reachable over https. The port is not part of this: a
 * loopback dev server that isn't on port 80 gets a URL that only matters to
 * link-preview crawlers, which can't reach it either way.
 */
function originFor(hostname: string): string {
  const local = LOOPBACK.test(hostname) || hostname.endsWith(".localhost");
  return `${local ? "http" : "https"}://${hostname}`;
}

function decodePath(path: string): string {
  try {
    return decodeURIComponent(path);
  } catch {
    return path;
  }
}
