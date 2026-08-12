import { createReadStream } from "node:fs";
import { stat } from "node:fs/promises";
import { join, normalize, resolve, sep } from "node:path";
import { Readable } from "node:stream";
import type { Logger } from "./log.ts";
import { contentType } from "./mime.ts";

export interface DashboardDeps {
  /** Directory holding the built dashboard (`dashboard/dist`). */
  dir: string;
  log: Logger;
}

/**
 * Serve the built dashboard as a single-page app.
 *
 * Unknown paths fall back to `index.html` so client-side routes work on a
 * hard refresh. Hashed Vite assets are immutable; `index.html` never is.
 */
export function createDashboardServer(deps: DashboardDeps) {
  const root = resolve(deps.dir);
  let warned = false;

  return async function serveDashboard(request: Request): Promise<Response> {
    const url = new URL(request.url);
    const file = (await find(decodePath(url.pathname))) ?? (await find("/index.html"));

    if (!file) {
      if (!warned) {
        warned = true;
        deps.log.warn("dashboard_not_built", { dir: root });
      }
      return new Response("The dashboard has not been built. Run `npm run build` in dashboard/.", {
        status: 503,
        headers: { "content-type": "text/plain; charset=utf-8" },
      });
    }

    const headers = new Headers({
      "content-type": contentType(file.path),
      "content-length": String(file.size),
      "cache-control": file.immutable ? "public, max-age=31536000, immutable" : "no-cache",
    });

    if (request.method === "HEAD") {
      return new Response(null, { headers });
    }

    return new Response(Readable.toWeb(createReadStream(file.path)) as ReadableStream<Uint8Array>, { headers });
  };

  /** Resolve a URL path to a file inside the dashboard directory. */
  async function find(path: string): Promise<{ path: string; size: number; immutable: boolean } | undefined> {
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
        // Vite fingerprints everything under assets/, so it can be cached forever.
        immutable: relative.startsWith("assets/"),
      };
    } catch {
      return undefined;
    }
  }
}

function decodePath(path: string): string {
  try {
    return decodeURIComponent(path);
  } catch {
    return path;
  }
}
