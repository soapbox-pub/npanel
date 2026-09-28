import { readFileSync } from "node:fs";
import customSources from "./data/custom-sources.json" with { type: "json" };
import referers from "./data/referers.json" with { type: "json" };

/**
 * Where a visit came from: which source, and the referring page itself.
 *
 * A port of Plausible's `Ingestion.Source`, over the same data:
 *
 * - `referers.json` is snowplow's referer-parser database, which Plausible
 *   reads through RefInspector, flattened to one host (or host and path) per
 *   key. It is what turns `google.co.uk` into `Google`.
 * - `custom-sources.json` is Plausible's own list layered on top: sources
 *   referer-parser lacks, and the `utm_source` shorthands people actually use
 *   (`ig`, `adwords`).
 * - `spammers.txt` is Matomo's referrer spam list, which is public domain.
 *
 * Both of the first two are taken from Plausible at 8be0427 (AGPL-3.0, as
 * npanel is); the spam list from matomo-org/referrer-spam-list at eaaa5ea.
 */

/** Referrers kept, in characters, as Plausible truncates them. */
const MAX_REFERRER = 2000;

/** Families of hosts that are one source, however many subdomains they have. */
const CUSTOM_SUFFIXES: [string, string][] = [
  ["officeapps.live.com", "Microsoft 365"],
  ["wikipedia.org", "Wikipedia"],
];

const REFERERS = referers as Record<string, string>;
const CUSTOM = customSources as Record<string, string>;

/**
 * Every name a source is known by, lowercased, to the one it is shown as.
 *
 * Built the way Plausible builds it: every referer-parser name, then every
 * custom source by both its key and its name, so `utm_source=google` and a
 * referrer from `google.com` land on the same row.
 */
const CANONICAL = new Map<string, string>();
for (const name of Object.values(REFERERS)) CANONICAL.set(name.toLowerCase(), name);
for (const [key, name] of Object.entries(CUSTOM)) {
  CANONICAL.set(key, name);
  CANONICAL.set(name.toLowerCase(), name);
}

/**
 * `utm_source` values that mean paid traffic whatever the medium says, like
 * `facebook_ad` or `yt-ads`.
 */
export const PAID_SOURCES = new Set([
  "adwords",
  ...Object.keys(CUSTOM).filter((key) => key.endsWith("ads") || key.endsWith("ad")),
]);

const SPAMMERS = new Set(
  readFileSync(new URL("./data/spammers.txt", import.meta.url), "utf8")
    .split("\n")
    .map((line) => line.trim().toLowerCase())
    .filter(Boolean),
);

export interface Acquisition {
  /** The source, like `Google` or `news.example.com`, or empty for a direct visit. */
  source: string;
  /** The referring page, without scheme, query, or trailing slash, or empty. */
  referrer: string;
}

/**
 * Where a visit to `url` referred by `referrer` came from.
 *
 * A source tagged in the URL wins over the referrer — `utm_source`, then
 * `source`, then `ref` — and is folded into a known source when it names one.
 * Otherwise the referrer decides, unless it is the site itself, localhost, or
 * not a web page at all, in which case the visit is direct.
 */
export function acquisition(url: URL, referrer: string | null): Acquisition {
  const referral = validReferral(url, referrer);
  const tagged = url.searchParams.get("utm_source") || url.searchParams.get("source") || url.searchParams.get("ref");

  return {
    source: tagged ? canonical(tagged) : referral ? fromReferrer(referral) : "",
    referrer: referral ? referrerHost(referral) + referral.pathname.replace(/\/+$/, "") : "",
  };
}

/** Whether a referrer is on Matomo's spam list, by its host or any parent of it. */
export function isSpamReferrer(referrer: string | null): boolean {
  if (!referrer) return false;

  let host: string;
  try {
    host = new URL(referrer).hostname.toLowerCase().replace(/^www\./, "");
  } catch {
    return false;
  }

  for (let name = host; name.includes("."); name = name.slice(name.indexOf(".") + 1)) {
    if (SPAMMERS.has(name)) return true;
  }
  return false;
}

/** A source as it is shown, for a name someone typed into a URL. */
function canonical(source: string): string {
  return CANONICAL.get(source.toLowerCase()) ?? source;
}

/**
 * The referrer, if it points somewhere other than the site itself.
 *
 * Only web pages and Android apps count: a `file:` or `about:` referrer says
 * nothing about where a visitor was. A referral from the site's own hostname
 * is internal navigation, not a source.
 */
function validReferral(url: URL, referrer: string | null): URL | undefined {
  if (!referrer) return undefined;

  let parsed: URL;
  try {
    parsed = new URL(referrer.slice(0, MAX_REFERRER).trim());
  } catch {
    return undefined;
  }

  if (!["http:", "https:", "android-app:"].includes(parsed.protocol)) return undefined;
  if (!parsed.hostname || parsed.hostname === "localhost") return undefined;
  if (sanitize(parsed.hostname) === sanitize(url.hostname)) return undefined;

  return parsed;
}

/**
 * The source a referring page belongs to.
 *
 * Plausible's own overrides first — exact hosts, then the families above — so
 * `gemini.google.com` is Google Gemini rather than Google. Then referer-parser,
 * and failing all of it the bare host, which is still a perfectly good name.
 */
function fromReferrer(referral: URL): string {
  const host = referrerHost(referral);
  const lower = host.toLowerCase();

  const custom = CUSTOM[lower];
  if (custom) return custom;

  for (const [suffix, name] of CUSTOM_SUFFIXES) {
    if (lower === suffix || lower.endsWith(`.${suffix}`)) return name;
  }

  const known = refererParser(referral.hostname.toLowerCase(), referral.pathname);
  return known ? canonical(known) : host;
}

/**
 * The referer-parser name for a host and path.
 *
 * The database names hosts and, for a few, a path under one — Google Images
 * is `google.com/imgres`. The longest match wins: every leading part of the
 * path under the full host, then the host alone, then the same again for each
 * parent domain, so `mail.yahoo.co.jp` finds `yahoo.co.jp` if nothing closer.
 */
function refererParser(hostname: string, pathname: string): string | undefined {
  const segments = pathname.split("/").filter(Boolean);

  for (let host = hostname; host.includes("."); host = host.slice(host.indexOf(".") + 1)) {
    for (let length = segments.length; length > 0; length--) {
      const prefix = `${host}/${segments.slice(0, length).join("/")}`;
      const match = REFERERS[prefix] ?? REFERERS[`${prefix}/`];
      if (match) return match;
    }

    const match = REFERERS[host];
    if (match) return match;
  }

  return undefined;
}

/** A referrer's host as Plausible records it: no `www.`, and apps marked as such. */
function referrerHost(referral: URL): string {
  const host = referral.hostname.replace(/^www\./, "");
  return referral.protocol === "android-app:" ? `android-app://${host}` : host;
}

function sanitize(hostname: string): string {
  return hostname.trim().replace(/^www\./, "");
}
