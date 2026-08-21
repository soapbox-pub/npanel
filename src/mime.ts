import { extname } from "node:path/posix";

/**
 * Content types for the file extensions that appear in web sites.
 *
 * A small table beats a dependency here: nsites are static sites, so the long
 * tail of media types never shows up, and anything unknown streams fine as
 * `application/octet-stream`.
 */
const TYPES: Record<string, string> = {
  ".html": "text/html; charset=utf-8",
  ".htm": "text/html; charset=utf-8",
  ".css": "text/css; charset=utf-8",
  ".js": "text/javascript; charset=utf-8",
  ".mjs": "text/javascript; charset=utf-8",
  ".json": "application/json; charset=utf-8",
  ".jsonld": "application/ld+json",
  ".map": "application/json; charset=utf-8",
  ".txt": "text/plain; charset=utf-8",
  ".md": "text/markdown; charset=utf-8",
  ".xml": "application/xml; charset=utf-8",
  ".xhtml": "application/xhtml+xml",
  ".rss": "application/rss+xml",
  ".atom": "application/atom+xml",
  ".csv": "text/csv; charset=utf-8",
  ".svg": "image/svg+xml",
  ".png": "image/png",
  ".jpg": "image/jpeg",
  ".jpeg": "image/jpeg",
  ".gif": "image/gif",
  ".webp": "image/webp",
  ".avif": "image/avif",
  ".ico": "image/x-icon",
  ".bmp": "image/bmp",
  ".tiff": "image/tiff",
  ".woff": "font/woff",
  ".woff2": "font/woff2",
  ".ttf": "font/ttf",
  ".otf": "font/otf",
  ".eot": "application/vnd.ms-fontobject",
  ".mp3": "audio/mpeg",
  ".ogg": "audio/ogg",
  ".oga": "audio/ogg",
  ".wav": "audio/wav",
  ".flac": "audio/flac",
  ".m4a": "audio/mp4",
  ".mp4": "video/mp4",
  ".webm": "video/webm",
  ".mov": "video/quicktime",
  ".m3u8": "application/vnd.apple.mpegurl",
  ".ts": "video/mp2t",
  ".pdf": "application/pdf",
  ".zip": "application/zip",
  ".gz": "application/gzip",
  ".br": "application/x-brotli",
  ".tar": "application/x-tar",
  ".wasm": "application/wasm",
  ".webmanifest": "application/manifest+json",
  ".manifest": "text/cache-manifest",
  ".appcache": "text/cache-manifest",
  ".epub": "application/epub+zip",
  ".apk": "application/vnd.android.package-archive",
  ".yaml": "text/yaml; charset=utf-8",
  ".yml": "text/yaml; charset=utf-8",
  ".toml": "text/plain; charset=utf-8",
};

/**
 * Types that are text underneath, without saying so in their name.
 *
 * `text/*` is the rule; these are the exceptions worth compressing anyway.
 * Fonts are here in their uncompressed forms only — `woff` and `woff2` carry
 * their own compression, and running another pass over them buys nothing.
 */
const COMPRESSIBLE_TYPES = new Set([
  "application/json",
  "application/ld+json",
  "application/xml",
  "application/xhtml+xml",
  "application/rss+xml",
  "application/atom+xml",
  "application/manifest+json",
  "application/wasm",
  "image/svg+xml",
  "font/ttf",
  "font/otf",
]);

/** Content type for a site path, defaulting to a safe binary type. */
export function contentType(path: string): string {
  return TYPES[extname(path).toLowerCase()] ?? "application/octet-stream";
}

/**
 * Whether a file is worth storing compressed copies of.
 *
 * Decided from the content type rather than a second list of extensions, so
 * this can't drift from the table above. An extension nobody recognizes ends
 * up as `application/octet-stream` and is left alone — the cost of guessing
 * wrong is deploy-time CPU spent on a file that was already compressed.
 */
export function isCompressible(path: string): boolean {
  const type = contentType(path).split(";")[0].trim();
  return type.startsWith("text/") || COMPRESSIBLE_TYPES.has(type);
}
