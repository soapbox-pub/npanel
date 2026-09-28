import { decodeHTML } from "entities";
import { parse } from "node-html-parser";
import type { BlobStore } from "./blobs.ts";
import { readHtmlHead } from "./html.ts";

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
  /** The site's own icon, as an absolute https URL, or null. */
  icon: string | null;
}

/** What one stored document says about itself; the icon takes the rest of the build. */
export type DocumentPreview = Omit<SitePreview, "icon">;

/** An icon a document links to, as written. */
interface IconLink {
  href: string;
  /** `apple-touch-icon` is drawn full-bleed on a rounded square, which is how npanel draws icons too. */
  apple: boolean;
  /** The largest edge it claims, in pixels; a vector scales to anything. */
  size: number;
}

/** Preview metadata as it was written, before URLs are resolved. */
interface RawMeta {
  title?: string;
  description?: string;
  image?: string;
  icons?: IconLink[];
  manifest?: string;
}

/**
 * Where a file sits in the build being described, by its path: its hash, or
 * undefined when the build has no such file.
 */
export type BuildLookup = (path: string) => Promise<string | undefined>;

/** Caps on what a site can put on someone else's homepage. */
const MAX_TITLE = 200;
const MAX_DESCRIPTION = 300;

/** Parsed documents to keep. Each entry is a few hundred bytes of strings. */
const MAX_CACHED = 500;

/** How much of a web app manifest is read. A real one is a few kilobytes. */
const MAX_MANIFEST_BYTES = 64 * 1024;

/** What a vector icon counts as, against icons that give their size in pixels. */
const VECTOR_SIZE = 4096;

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
  private documents = new Map<string, RawMeta>();
  /** The icon each manifest names, as written, by the manifest's hash. */
  private manifests = new Map<string, string | null>();

  constructor(private readonly blobs: BlobStore) {}

  /** Read and parse `sha256`, then resolve its URLs against `origin`. */
  async get(sha256: string, origin: URL): Promise<DocumentPreview> {
    const meta = await this.meta(sha256);

    return {
      title: meta.title ?? null,
      description: meta.description ?? null,
      image: resolveImage(meta.image, origin),
    };
  }

  /**
   * The site's icon, the best one it offers, in the order a home screen would
   * take them: an `apple-touch-icon`, then the largest icon in its web app
   * manifest, then whatever `<link rel="icon">` it has — a vector first — and
   * finally a `/favicon.ico` that is simply there.
   *
   * `page` is the landing page's hash, when it has one. An icon on the site's
   * own domain counts only if the build really has the file, so a stale link
   * never becomes a broken image.
   */
  async icon(page: string | undefined, origin: URL, lookup: BuildLookup): Promise<string | null> {
    const meta = page ? await this.meta(page) : {};
    const links = [...(meta.icons ?? [])].sort((a, b) => b.size - a.size);

    for (const link of links.filter((link) => link.apple)) {
      const url = await usable(link.href, origin, lookup);
      if (url) return url;
    }

    if (meta.manifest) {
      const url = await this.manifestIcon(meta.manifest, origin, lookup);
      if (url) return url;
    }

    for (const link of links.filter((link) => !link.apple)) {
      const url = await usable(link.href, origin, lookup);
      if (url) return url;
    }

    return await usable("/favicon.ico", origin, lookup);
  }

  private async manifestIcon(href: string, origin: URL, lookup: BuildLookup): Promise<string | null> {
    // A manifest elsewhere would have to be fetched, and nothing here fetches.
    const url = sameOrigin(href, origin);
    const sha256 = url && (await lookup(sitePath(url)));
    if (!url || !sha256) return null;

    let src = this.manifests.get(sha256);
    if (src === undefined) {
      src = extractManifestIcon(await this.blobs.readText(sha256, MAX_MANIFEST_BYTES));
      remember(this.manifests, sha256, src);
    }

    // Icons in a manifest are relative to the manifest, not to the page.
    return src ? await usable(src, url, lookup) : null;
  }

  private async meta(sha256: string): Promise<RawMeta> {
    let meta = this.documents.get(sha256);
    if (!meta) {
      meta = extractMeta(await readHtmlHead(this.blobs, sha256));
      remember(this.documents, sha256, meta);
    }
    return meta;
  }
}

/** Add to a bounded cache, dropping the entry added longest ago. */
function remember<T>(cache: Map<string, T>, key: string, value: T): void {
  if (cache.size >= MAX_CACHED) {
    const oldest = cache.keys().next().value;
    if (oldest !== undefined) cache.delete(oldest);
  }
  cache.set(key, value);
}

/**
 * An icon reference as a URL the dashboard can show: one on another https
 * host as it stands, one on the site's own domain only if the build has it.
 */
async function usable(href: string, base: URL, lookup: BuildLookup): Promise<string | null> {
  const url = resolveImage(href, base);
  if (!url) return null;

  const own = sameOrigin(url, base);
  if (!own) return url;
  return (await lookup(sitePath(own))) ? own.href : null;
}

function sameOrigin(href: string, base: URL): URL | null {
  try {
    const url = new URL(href, base);
    return url.origin === base.origin ? url : null;
  } catch {
    return null;
  }
}

/** The path a URL names within a build, which stores paths decoded. */
function sitePath(url: URL): string {
  try {
    return decodeURIComponent(url.pathname);
  } catch {
    return url.pathname;
  }
}

/**
 * The icon a web app manifest would put on a home screen: the largest one
 * meant for ordinary display. Maskable icons carry a safe zone of padding and
 * monochrome ones are a silhouette, so they are only taken when there is
 * nothing else.
 */
export function extractManifestIcon(json: string): string | null {
  let manifest: unknown;
  try {
    manifest = JSON.parse(json);
  } catch {
    return null;
  }

  const icons = (manifest as { icons?: unknown })?.icons;
  if (!Array.isArray(icons)) return null;

  const candidates = icons.flatMap((icon: unknown) => {
    const { src, sizes, purpose, type } = (icon ?? {}) as Record<string, unknown>;
    if (typeof src !== "string" || !src) return [];

    const purposes = typeof purpose === "string" ? purpose.toLowerCase().split(/\s+/) : ["any"];
    return [
      {
        src,
        plain: purposes.includes("any"),
        size: largestSize(
          typeof sizes === "string" ? sizes : undefined,
          src,
          typeof type === "string" ? type : undefined,
        ),
      },
    ];
  });

  candidates.sort((a, b) => Number(b.plain) - Number(a.plain) || b.size - a.size);
  return candidates[0]?.src ?? null;
}

/** The largest edge an icon claims, from its `sizes` or — for a vector — its type. */
function largestSize(sizes: string | undefined, href: string, type: string | undefined): number {
  if (type === "image/svg+xml" || /\.svg(?:[?#]|$)/i.test(href) || sizes?.toLowerCase().includes("any")) {
    return VECTOR_SIZE;
  }

  let largest = 0;
  for (const size of (sizes ?? "").matchAll(/(\d+)x(\d+)/gi)) {
    largest = Math.max(largest, Number(size[1]), Number(size[2]));
  }
  return largest;
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

  const icons: IconLink[] = [];
  for (const element of root.querySelectorAll("link")) {
    const rel = (element.getAttribute("rel") ?? "").toLowerCase().split(/\s+/);
    const href = element.getAttribute("href");
    if (!href) continue;

    if (rel.includes("manifest")) {
      meta.manifest ??= decodeHTML(href);
    } else if (rel.includes("icon") || rel.some((token) => token.startsWith("apple-touch-icon"))) {
      icons.push({
        href: decodeHTML(href),
        apple: rel.some((token) => token.startsWith("apple-touch-icon")),
        // An apple-touch-icon that doesn't say is the 180px Apple asks for.
        size:
          largestSize(element.getAttribute("sizes") ?? undefined, href, element.getAttribute("type") ?? undefined) ||
          (rel.includes("icon") ? 0 : 180),
      });
    }
  }
  if (icons.length) meta.icons = icons;

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
