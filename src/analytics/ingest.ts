import { randomInt } from "node:crypto";
import { getDomain } from "tldts";
import { channel } from "./channel.ts";
import type { Geo } from "./geo.ts";
import { type EventRequest, parseEventRequest } from "./request.ts";
import type { Salts } from "./salts.ts";
import type { Session, SessionAttributes, SessionCache } from "./sessions.ts";
import { acquisition, isSpamReferrer } from "./source.ts";
import { type AnalyticsStore, EVENT_KIND } from "./store.ts";
import type { Device, UserAgents } from "./user-agent.ts";

/** Ad click ids a landing URL may carry, in the order Plausible checks them. */
const CLICK_IDS = ["gclid", "gbraid", "wbraid", "msclkid", "fbclid", "twclid"];

/** A site events may be recorded for, as the main thread describes it. */
export interface AnalyticsHost {
  id: number;
  hostname: string;
  /** Whether it is enabled and counting visits: the only hosts events are kept for. */
  counting: boolean;
}

/** What an event request is answered with. */
export interface IngestResponse {
  status: number;
  headers: Record<string, string>;
  body: string;
}

/** Who sent an event, as far as the request says. */
export interface Sender {
  ip: string;
  userAgent: string;
}

export interface IngestDeps {
  store: AnalyticsStore;
  sessions: SessionCache;
  salts: Salts;
  userAgents: UserAgents;
  geo: Geo;
  /** The site an event names, if there is one by that name. */
  host(domain: string): AnalyticsHost | undefined;
  /** Called when enough events are buffered that they should be written now. */
  flush(): void;
}

/**
 * Plausible's ingestion: validate an event, decide which site and visit it
 * belongs to, and record it.
 *
 * The answers are Plausible's too. An event recorded is a `202 ok`. One that
 * was well-formed but isn't recorded — a bot, a site that isn't counting, an
 * engagement ping for a visit that already ended — is still a `202`, with
 * `x-plausible-dropped` saying how many were dropped, because nothing the
 * sender could change would make it count. Only a malformed request is a 400.
 */
export class Ingest {
  constructor(private readonly deps: IngestDeps) {}

  handle(body: string, sender: Sender, now = Math.floor(Date.now() / 1000)): IngestResponse {
    const parsed = parseEventRequest(body);
    if ("errors" in parsed) {
      return {
        status: 400,
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ errors: parsed.errors }),
      };
    }

    const { event } = parsed;
    let dropped = 0;

    // Spam is spam whichever site it was sent to.
    if (isSpamReferrer(event.referrer)) {
      dropped = event.domains.length;
    } else {
      for (const domain of event.domains) {
        if (!this.record(event, domain, sender, now)) dropped++;
      }
    }

    const headers: Record<string, string> = { "content-type": "text/plain; charset=utf-8" };
    if (dropped) headers["x-plausible-dropped"] = String(dropped);

    return { status: 202, headers, body: "ok" };
  }

  /** Record one event for one site. Returns whether it was kept. */
  private record(event: EventRequest, domain: string, sender: Sender, now: number): boolean {
    const host = this.deps.host(domain);
    if (!host?.counting) return false;

    const device = sender.userAgent ? this.deps.userAgents.parse(sender.userAgent) : undefined;
    if (device === null) return false;

    const rootDomain = getDomain(event.url.hostname) || event.url.hostname || "(none)";
    const ids = this.deps.salts.userIds(now, [sender.userAgent, sender.ip, domain, rootDomain]);

    const found =
      this.deps.sessions.find(host.id, ids.current, now) ??
      (ids.previous === undefined ? undefined : this.deps.sessions.find(host.id, ids.previous, now));

    const pageview = event.name === "pageview";

    let session: Session;
    if (event.name === "engagement") {
      // Engagement only ever continues a visit: it keeps the session alive and
      // is stored against it, but changes nothing a session counts.
      if (!found) return false;
      found.timestamp = now;
      this.deps.sessions.put(found);
      session = found;
    } else if (found) {
      session = found;
      session.timestamp = now;
      session.duration = Math.abs(now - session.start);
      session.events++;
      if (pageview) {
        session.pageviews++;
        if (!session.entryPage) session.entryPage = event.pathname;
        session.exitPage = event.pathname;
      }
      // A bounce stops being one on a second pageview or anything the visitor
      // actively did, and never becomes one again.
      if (session.isBounce) {
        session.isBounce = !(session.pageviews >= 2 || (event.interactive && !pageview));
      }
      this.deps.sessions.put(session);
      this.deps.store.touchSession(session);
    } else {
      session = {
        id: sessionId(),
        hostId: host.id,
        userId: ids.current,
        start: now,
        timestamp: now,
        entryPage: pageview ? event.pathname : "",
        exitPage: pageview ? event.pathname : "",
        pageviews: pageview ? 1 : 0,
        events: 1,
        duration: 0,
        isBounce: pageview || !event.interactive,
        ...attributes(event, device, this.deps.geo.lookup(sender.ip)),
      };
      this.deps.sessions.put(session);
      this.deps.store.touchSession(session);
    }

    const full = this.deps.store.addEvent({
      hostId: host.id,
      timestamp: now,
      kind: pageview ? EVENT_KIND.pageview : event.name === "engagement" ? EVENT_KIND.engagement : EVENT_KIND.custom,
      sessionId: session.id,
      userId: session.userId,
      pathname: event.pathname,
      engagementTime: event.engagementTime,
      scrollDepth: event.scrollDepth,
    });
    if (full) this.deps.flush();

    return true;
  }
}

/** A session's attributes, all taken from the event that began it. */
function attributes(
  event: EventRequest,
  device: Device | undefined,
  location: { country: string; region: string; city: string },
): SessionAttributes {
  const query = event.url.searchParams;
  const { source, referrer } = acquisition(event.url, event.referrer);
  const clickIdParam = CLICK_IDS.find((param) => query.has(param)) ?? "";

  let utmMedium = query.get("utm_medium") ?? "";
  // An auto-tagged ad click says what it was without a medium to say it.
  if (!utmMedium && source === "Google" && clickIdParam === "gclid") utmMedium = "(gclid)";
  if (!utmMedium && source === "Bing" && clickIdParam === "msclkid") utmMedium = "(msclkid)";

  const utmSource = query.get("utm_source") || query.get("source") || query.get("ref") || "";
  const utmCampaign = query.get("utm_campaign") ?? "";

  return {
    referrer,
    source,
    channel: channel({ source, utmMedium, utmCampaign, utmSource, clickIdParam }),
    utmMedium,
    utmSource,
    utmCampaign,
    utmContent: query.get("utm_content") ?? "",
    utmTerm: query.get("utm_term") ?? "",
    clickIdParam,
    ...location,
    device: device?.device ?? "",
    browser: device?.browser ?? "",
    browserVersion: device?.browserVersion ?? "",
    os: device?.os ?? "",
    osVersion: device?.osVersion ?? "",
  };
}

function sessionId(): number {
  return randomInt(2 ** 48 - 1);
}
