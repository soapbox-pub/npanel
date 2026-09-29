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
  /** The largest edge it claims, in pixels; {@link LARGEST} for a vector, 0 if it doesn't say. */
  size: number;
}

/** An icon a web app manifest names, as written. */
interface ManifestIcon {
  src: string;
  size: number;
}

/** An icon from anywhere, ready to be ranked against the rest. */
interface IconCandidate {
  href: string;
  /** What `href` is relative to: the page, or the manifest that named it. */
  base: URL;
  score: number;
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

/**
 * The edge past which a bigger icon is no better, and what a vector counts as:
 * more than enough for any tile npanel draws.
 */
const LARGEST = 1024;

/**
 * Where an icon came from, ranked. A site can declare the same picture several
 * ways, and the declarations differ in what they promise: an apple-touch-icon
 * is drawn to be a home screen icon and fills its tile edge to edge, a web app
 * manifest icon is the same thing by another name, and a favicon is a glyph
 * meant to be read at 16px in a tab. Higher wins.
 */
const RANK = { appleTouch: 300, manifest: 200, favicon: 100 } as const;

/**
 * Files at the conventional names, for sites that ship an icon without ever
 * linking to it. Browsers probe `/favicon.ico` the same way.
 */
const CONVENTIONAL: Array<[path: string, score: number]> = [
  ["/apple-touch-icon.png", RANK.appleTouch],
  ["/icon.svg", RANK.favicon + 0.9],
  ["/icon.png", RANK.favicon + 0.8],
  ["/favicon.svg", RANK.favicon + 0.7],
  ["/favicon.png", RANK.favicon + 0.6],
  ["/favicon.ico", RANK.favicon + 0.5],
];

/**
 * Rank within a class by declared size, without ever outranking the class
 * above: a 1024px favicon is still a favicon.
 */
function score(rank: number, size: number): number {
  return rank + Math.min(size, LARGEST) / (LARGEST + 1);
}

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
  /** The icons each manifest names, as written, by the manifest's hash. */
  private manifests = new Map<string, ManifestIcon[]>();

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
   * The site's icon, the way a home screen would pick it: every icon the site
   * declares — its landing page's links, the web app manifest those link, and
   * files at the conventional names — ranked by {@link RANK} and then by size,
   * and the best one the build can actually serve.
   *
   * `page` is the landing page's hash, when it has one. An icon on the site's
   * own domain counts only if the build really has the file, so a stale link
   * never becomes a broken image.
   */
  async icon(page: string | undefined, origin: URL, lookup: BuildLookup): Promise<string | null> {
    const meta = page ? await this.meta(page) : {};

    const candidates: IconCandidate[] = (meta.icons ?? []).map((link) => ({
      href: link.href,
      base: origin,
      score: score(link.apple ? RANK.appleTouch : RANK.favicon, link.size),
    }));

    if (meta.manifest) candidates.push(...(await this.manifestIcons(meta.manifest, origin, lookup)));

    for (const [path, rank] of CONVENTIONAL) {
      candidates.push({ href: path, base: origin, score: rank });
    }

    candidates.sort((a, b) => b.score - a.score);

    for (const candidate of candidates) {
      const url = await usable(candidate.href, candidate.base, lookup);
      if (url) return url;
    }
    return null;
  }

  private async manifestIcons(href: string, origin: URL, lookup: BuildLookup): Promise<IconCandidate[]> {
    // A manifest elsewhere would have to be fetched, and nothing here fetches.
    const url = sameOrigin(href, origin);
    const sha256 = url && (await lookup(sitePath(url)));
    if (!url || !sha256) return [];

    let icons = this.manifests.get(sha256);
    if (!icons) {
      icons = extractManifestIcons(await this.blobs.readText(sha256, MAX_MANIFEST_BYTES));
      remember(this.manifests, sha256, icons);
    }

    // Icons in a manifest are relative to the manifest, not to the page.
    return icons.map((icon) => ({ href: icon.src, base: url, score: score(RANK.manifest, icon.size) }));
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
 * The icons a web app manifest names that could stand for the site.
 *
 * A `monochrome` icon is a silhouette meant to be tinted by whatever shows it,
 * which is useless as a tile, so one declared for nothing else is left out. A
 * site may declare one asset for several purposes, though, and that one stays.
 */
export function extractManifestIcons(json: string): ManifestIcon[] {
  let manifest: unknown;
  try {
    manifest = JSON.parse(json);
  } catch {
    return [];
  }

  const icons = (manifest as { icons?: unknown })?.icons;
  if (!Array.isArray(icons)) return [];

  return icons.flatMap((icon: unknown) => {
    const { src, sizes, purpose, type } = (icon ?? {}) as Record<string, unknown>;
    if (typeof src !== "string" || !src.trim()) return [];

    const purposes = typeof purpose === "string" ? purpose.toLowerCase().trim().split(/\s+/) : [];
    if (purposes.length > 0 && purposes.every((p) => p === "monochrome")) return [];

    return [
      {
        src: src.trim(),
        size: largestSize(
          typeof sizes === "string" ? sizes : undefined,
          src,
          typeof type === "string" ? type : undefined,
        ),
      },
    ];
  });
}

/** The largest edge an icon claims, from its `sizes` or — for a vector — its type. */
function largestSize(sizes: string | undefined, href: string, type: string | undefined): number {
  if (type === "image/svg+xml" || /\.svg(?:[?#]|$)/i.test(href) || sizes?.toLowerCase().includes("any")) {
    return LARGEST;
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
        size: largestSize(element.getAttribute("sizes") ?? undefined, href, element.getAttribute("type") ?? undefined),
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
