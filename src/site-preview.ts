import { decodeHTML } from "entities";
import { parse } from "node-html-parser";
import type { BlobStore } from "./blobs.ts";

/**
 * A card's worth of a site, read from the `<head>` of its own `index.html`.
 *
 * Every field is optional because a site owes npanel nothing: plenty of nsites
 * ship a bare document with no Open Graph tags at all, and those still deserve
 * to be listed.
 */
export interface SitePreview {
  title: string | null;
  description: string | null;
  /** Absolute https URL, or null. */
  image: string | null;
}

/** Preview metadata as it was written, before URLs are resolved. */
interface RawMeta {
  title?: string;
  description?: string;
  image?: string;
}

/**
 * How much of a document npanel will read looking for meta tags.
 *
 * The tags that matter are in `<head>`, so this only has to cover a large one.
 * Sites are free to inline a megabyte of CSS below it; nothing is gained by
 * parsing that, and a homepage listing twenty sites would pay for all of it.
 */
const MAX_HTML_BYTES = 128 * 1024;

/** Caps on what a site can put on someone else's homepage. */
const MAX_TITLE = 200;
const MAX_DESCRIPTION = 300;

/** Parsed documents to keep. Each entry is a few hundred bytes of strings. */
const MAX_CACHED = 500;

/**
 * Preview metadata for served sites, cached by the sha256 of the document it
 * came from.
 *
 * Keying on content rather than on a host or a version means the cache can
 * never go stale: a deploy that changes `index.html` changes the hash, and one
 * that doesn't genuinely has the same metadata. Two hosts serving the same
 * document share the entry, and each still resolves relative URLs against its
 * own hostname.
 */
export class SitePreviews {
  private cache = new Map<string, RawMeta>();

  constructor(private readonly blobs: BlobStore) {}

  /** Read and parse `sha256`, then resolve its URLs against `origin`. */
  async get(sha256: string, origin: URL): Promise<SitePreview> {
    let meta = this.cache.get(sha256);

    if (!meta) {
      meta = extractMeta(await this.readHtml(sha256));

      // Oldest first, so this drops the least recently added entry.
      if (this.cache.size >= MAX_CACHED) {
        const oldest = this.cache.keys().next().value;
        if (oldest !== undefined) this.cache.delete(oldest);
      }
      this.cache.set(sha256, meta);
    }

    return {
      title: meta.title ?? null,
      description: meta.description ?? null,
      image: resolveImage(meta.image, origin),
    };
  }

  /** The first {@link MAX_HTML_BYTES} of a stored blob, as text. */
  private async readHtml(sha256: string): Promise<string> {
    const size = await this.blobs.size(sha256);
    if (size === undefined || size === 0) return "";

    const stream = this.blobs.read(sha256, { start: 0, end: Math.min(size, MAX_HTML_BYTES) - 1 });
    const chunks: Buffer[] = [];
    for await (const chunk of stream) {
      chunks.push(chunk as Buffer);
    }

    return Buffer.concat(chunks).toString("utf8");
  }
}

/**
 * Scrape the preview metadata out of an HTML document.
 *
 * Open Graph first, then Twitter's tags, then the plain document title and
 * description — the same order every other link-preview reader uses, so a site
 * that already looks right when shared looks right here too.
 */
export function extractMeta(html: string): RawMeta {
  const root = parse(html);
  const meta: RawMeta = {};

  const tags = new Map<string, string>();
  for (const element of root.querySelectorAll("meta")) {
    const name = (element.getAttribute("property") || element.getAttribute("name") || "").toLowerCase();
    const content = element.getAttribute("content");
    // First tag of a kind wins, matching how og:image is repeated for sizes.
    if (name && content && !tags.has(name)) {
      tags.set(name, decodeHTML(content));
    }
  }

  const documentTitle = root.querySelector("title")?.text;

  const title = tags.get("og:title") ?? tags.get("twitter:title") ?? (documentTitle && decodeHTML(documentTitle));
  const description = tags.get("og:description") ?? tags.get("twitter:description") ?? tags.get("description");
  const image = tags.get("og:image") ?? tags.get("og:image:url") ?? tags.get("twitter:image");

  if (title) meta.title = clamp(title, MAX_TITLE);
  if (description) meta.description = clamp(description, MAX_DESCRIPTION);
  if (image) meta.image = image;

  return meta;
}

/**
 * Resolve an image reference against the site that named it.
 *
 * Most are site-relative, which makes them npanel's own to serve. An absolute
 * one is honoured only over https: the dashboard's own content security policy
 * would block anything else, and a site can't be allowed to turn a homepage
 * visit into a plaintext request to somewhere else.
 */
export function resolveImage(value: string | undefined, origin: URL): string | null {
  if (!value) return null;

  try {
    const url = new URL(value, origin);
    return url.protocol === "https:" ? url.href : null;
  } catch {
    return null;
  }
}

/** Trim, and cut to length on a word boundary where one is close enough. */
function clamp(value: string, max: number): string {
  const text = value.replace(/\s+/g, " ").trim();
  if (text.length <= max) return text;

  const cut = text.slice(0, max);
  const space = cut.lastIndexOf(" ");
  return `${(space > max * 0.6 ? cut.slice(0, space) : cut).trimEnd()}…`;
}
