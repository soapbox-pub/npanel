/** URLs and referrers longer than this are refused or cut, as Plausible does. */
const MAX_URL = 2000;

/** Longest event name accepted. */
const MAX_NAME = 120;

/** An engagement event reporting no scroll depth stores this, which no real depth can be. */
export const MISSING_SCROLL_DEPTH = 255;

/** Engagement time longer than a month is a broken clock, not a visitor. */
const MAX_ENGAGEMENT_TIME = 30 * 24 * 60 * 60 * 1000;

/** An event, as the tracker described it and after Plausible's validation. */
export interface EventRequest {
  name: string;
  url: URL;
  /** The page, percent-decoded, with its fragment in hash-routing mode. */
  pathname: string;
  /** Every site the event was reported to, `www.` removed. */
  domains: string[];
  referrer: string | null;
  interactive: boolean;
  /** Milliseconds; 0 unless this is an engagement event. */
  engagementTime: number;
  /** Percent; {@link MISSING_SCROLL_DEPTH} unless reported. */
  scrollDepth: number;
}

/** Field errors, in the shape Plausible's `{"errors": …}` body takes. */
export type RequestErrors = Record<string, string[]>;

/**
 * Parse the body `POST /api/event` receives, following Plausible's
 * `Ingestion.Request` field by field: the same short and long names (`n` or
 * `name`, `u` or `url`, …), the same limits, and the same error messages, so
 * anything written against Plausible's endpoint behaves the same here.
 *
 * Props and revenue are accepted and ignored — npanel reports neither — and so
 * is anything else a newer tracker sends.
 */
export function parseEventRequest(body: string): { event: EventRequest } | { errors: RequestErrors } {
  let params: unknown;
  try {
    params = JSON.parse(body);
  } catch {
    params = undefined;
  }
  if (!params || typeof params !== "object" || Array.isArray(params)) {
    return { errors: { request: ["Unable to parse request body as json"] } };
  }

  const fields = params as Record<string, unknown>;
  const errors: RequestErrors = {};
  const error = (field: string, message: string) => {
    errors[field] = [...(errors[field] ?? []), message];
  };

  const rawName = fields.n ?? fields.name;
  const name = typeof rawName === "number" && Number.isInteger(rawName) ? String(rawName) : rawName;
  if (typeof name !== "string" || !name) {
    error("event_name", "can't be blank");
  } else if (name.length > MAX_NAME) {
    error("event_name", `should be at most ${MAX_NAME} character(s)`);
  }

  const url = parseUrl(fields.u ?? fields.url, error);
  const hashMode = (fields.h ?? fields.hashMode) === 1;

  const rawReferrer = fields.r ?? fields.referrer;
  const referrer = typeof rawReferrer === "string" ? rawReferrer.slice(0, MAX_URL) : null;

  let engagementTime = 0;
  let scrollDepth = MISSING_SCROLL_DEPTH;
  if (name === "engagement") {
    scrollDepth = parseScrollDepth(fields.sd);
    engagementTime = parseEngagementTime(fields.e);
    if (scrollDepth === MISSING_SCROLL_DEPTH && engagementTime === 0) {
      error("event_name", "engagement event requires a valid integer value for at least one of 'sd' or 'e' fields");
    }
  }

  const domains = parseDomains(fields.d ?? fields.domain, url, error);

  if (Object.keys(errors).length || typeof name !== "string" || !url) return { errors };

  return {
    event: {
      name,
      url,
      pathname: pathname(url, hashMode),
      domains,
      referrer,
      interactive: (fields.i ?? fields.interactive) !== false,
      engagementTime,
      scrollDepth,
    },
  };
}

function parseUrl(value: unknown, error: (field: string, message: string) => void): URL | undefined {
  if (value === undefined || value === null) {
    error("url", "is required");
    return undefined;
  }
  if (typeof value !== "string" || Buffer.byteLength(value) > MAX_URL) {
    error("url", "must be a valid url");
    return undefined;
  }

  let url: URL;
  try {
    url = new URL(value);
  } catch {
    error("url", "must be a valid url");
    return undefined;
  }

  if (url.protocol === "data:") {
    error("url", "scheme is not allowed");
    return undefined;
  }

  return url;
}

/**
 * The sites an event is for: `d`, split on commas, each without `www.`, or the
 * page's own host when the tracker named none.
 */
function parseDomains(value: unknown, url: URL | undefined, error: (field: string, message: string) => void): string[] {
  const raw = typeof value === "string" ? value.trim() : undefined;

  if (raw === "" || (raw === undefined && !url?.hostname)) {
    error("domain", "can't be blank");
    return [];
  }

  if (raw === undefined) return [sanitizeHostname(url?.hostname ?? "")];

  return raw.split(",").map(sanitizeHostname).filter(Boolean);
}

/** A page's path as Plausible records it: decoded, and with its route in hash mode. */
function pathname(url: URL, hashMode: boolean): string {
  const path = decode(url.pathname || "/").trimEnd();
  return hashMode && url.hash ? `${path}#${decode(url.hash.slice(1))}` : path;
}

function parseScrollDepth(value: unknown): number {
  const depth = integer(value);
  if (depth === undefined || depth < 0) return MISSING_SCROLL_DEPTH;
  return Math.min(depth, 100);
}

function parseEngagementTime(value: unknown): number {
  const time = integer(value);
  if (time === undefined || time < 0 || time >= MAX_ENGAGEMENT_TIME) return 0;
  return time;
}

/** An integer, or a string that is exactly one. */
function integer(value: unknown): number | undefined {
  if (typeof value === "number") return Number.isInteger(value) ? value : undefined;
  if (typeof value === "string" && /^-?\d+$/.test(value)) return Number(value);
  return undefined;
}

export function sanitizeHostname(hostname: string): string {
  return hostname
    .trim()
    .toLowerCase()
    .replace(/^www\./, "");
}

function decode(value: string): string {
  try {
    return decodeURIComponent(value);
  } catch {
    return value;
  }
}
