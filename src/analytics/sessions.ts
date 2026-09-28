/** What a session carries from the event that began it. */
export interface SessionAttributes {
  referrer: string;
  source: string;
  channel: string;
  utmMedium: string;
  utmSource: string;
  utmCampaign: string;
  utmContent: string;
  utmTerm: string;
  clickIdParam: string;
  /** ISO 3166-1 alpha-2, or empty. */
  country: string;
  /** The region's English name, or empty. */
  region: string;
  /** The city's English name, or empty. */
  city: string;
  device: string;
  browser: string;
  browserVersion: string;
  os: string;
  osVersion: string;
}

/** One visit: everything a visitor did on a site until they were gone half an hour. */
export interface Session extends SessionAttributes {
  id: number;
  hostId: number;
  userId: number;
  /** Epoch seconds of the first event. */
  start: number;
  /** Epoch seconds of the latest event, which is what keeps it alive. */
  timestamp: number;
  entryPage: string;
  exitPage: string;
  pageviews: number;
  events: number;
  /** Seconds from the first event to the latest. */
  duration: number;
  isBounce: boolean;
}

/**
 * How long a visitor may be idle before their next event starts a new visit.
 *
 * Plausible compares whole minutes and allows thirty of them, so anything
 * short of thirty-one minutes still counts.
 */
export const SESSION_TIMEOUT = 31 * 60;

/**
 * Most sessions held at once.
 *
 * A bound for the process's sake rather than a number anyone should reach: a
 * session lives half an hour, so this is a hundred thousand visitors arriving
 * within thirty minutes on one gateway. Past it the stalest session is
 * forgotten, and that visitor's next event starts a new one — a miscount, and
 * a far smaller one than running out of memory.
 */
const MAX_SESSIONS = 100_000;

/**
 * The sessions still open, in memory, keyed by site and visitor.
 *
 * This is Plausible's session cache. Events are matched to a session here
 * rather than in the database, which is written to only in batches: a visit
 * is one row that is rewritten as it goes, and asking SQLite for it on every
 * pageview would put a read on the hot path for no gain.
 */
export class SessionCache {
  private sessions = new Map<string, Session>();

  /** The open session of one visitor on one site, if they were seen recently enough. */
  find(hostId: number, userId: number, now: number): Session | undefined {
    const session = this.sessions.get(key(hostId, userId));
    if (session && now - session.timestamp < SESSION_TIMEOUT) return session;
    return undefined;
  }

  /** Remember a session under the visitor it belongs to. */
  put(session: Session): void {
    const k = key(session.hostId, session.userId);

    // Re-inserted so the map stays in order of last activity, which is what
    // lets both the sweep and the cap below stop at the first live entry.
    this.sessions.delete(k);
    this.sessions.set(k, session);

    if (this.sessions.size > MAX_SESSIONS) {
      const oldest = this.sessions.keys().next().value;
      if (oldest !== undefined) this.sessions.delete(oldest);
    }
  }

  /** Forget every session that can no longer be continued. */
  sweep(now: number): void {
    for (const [k, session] of this.sessions) {
      if (now - session.timestamp < SESSION_TIMEOUT) break;
      this.sessions.delete(k);
    }
  }

  get size(): number {
    return this.sessions.size;
  }
}

function key(hostId: number, userId: number): string {
  return `${hostId}:${userId}`;
}
