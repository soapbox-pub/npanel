import { randomBytes, randomInt } from "node:crypto";
import type { SqliteDatabase, SqliteStatement, SqliteValue } from "../db/sqlite.ts";
import type { Session } from "./sessions.ts";

/** What an event was. Stored as a number, since there are only three answers. */
export const EVENT_KIND = { pageview: 0, engagement: 1, custom: 2 } as const;
export type EventKind = (typeof EVENT_KIND)[keyof typeof EVENT_KIND];

/** One event, as it is stored. */
export interface EventRow {
  hostId: number;
  /** Epoch seconds. */
  timestamp: number;
  kind: EventKind;
  sessionId: number;
  userId: number;
  pathname: string;
  /** Milliseconds, on engagement events; 0 otherwise. */
  engagementTime: number;
  /** Percent, on engagement events; 255 where none was reported. */
  scrollDepth: number;
}

/**
 * Events held before a flush is forced, rather than waiting for the timer.
 *
 * And the most ever held: a store that can't write is a store whose buffer
 * would grow for as long as it can't, so past the ceiling new events are
 * dropped. They are dropped rather than the old ones because an old event is
 * one a visitor was already told was counted.
 */
const FLUSH_AT = 5_000;
const MAX_BUFFERED = 100_000;

/**
 * Schema migrations for the analytics database, applied in order.
 *
 * Its own database file beside npanel's, so the tables that grow with every
 * page anyone views are nowhere near the ones serving reads from, and backing
 * up or deleting one says nothing about the other. Append-only, like the main
 * database's migrations.
 *
 * Both tables are clustered by site and time — `WITHOUT ROWID` with those
 * leading the key — because every query reads one site over one date range,
 * and that makes the range one contiguous stretch of the file.
 */
const MIGRATIONS: { name: string; statements: string[] }[] = [
  {
    name: "001_initial",
    statements: [
      `CREATE TABLE salts (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        salt BLOB NOT NULL,
        created_at INTEGER NOT NULL
      )`,
      `CREATE TABLE sessions (
        host_id INTEGER NOT NULL,
        start INTEGER NOT NULL,
        session_id INTEGER NOT NULL,
        user_id INTEGER NOT NULL,
        timestamp INTEGER NOT NULL,
        entry_page TEXT NOT NULL,
        exit_page TEXT NOT NULL,
        pageviews INTEGER NOT NULL,
        events INTEGER NOT NULL,
        duration INTEGER NOT NULL,
        is_bounce INTEGER NOT NULL,
        referrer TEXT NOT NULL,
        source TEXT NOT NULL,
        channel TEXT NOT NULL,
        utm_medium TEXT NOT NULL,
        utm_source TEXT NOT NULL,
        utm_campaign TEXT NOT NULL,
        utm_content TEXT NOT NULL,
        utm_term TEXT NOT NULL,
        click_id_param TEXT NOT NULL,
        country TEXT NOT NULL,
        region TEXT NOT NULL,
        city TEXT NOT NULL,
        device TEXT NOT NULL,
        browser TEXT NOT NULL,
        browser_version TEXT NOT NULL,
        os TEXT NOT NULL,
        os_version TEXT NOT NULL,
        PRIMARY KEY (host_id, start, session_id)
      ) WITHOUT ROWID`,
      "CREATE INDEX sessions_timestamp_idx ON sessions (timestamp)",
      `CREATE TABLE events (
        host_id INTEGER NOT NULL,
        timestamp INTEGER NOT NULL,
        id INTEGER NOT NULL,
        kind INTEGER NOT NULL,
        session_id INTEGER NOT NULL,
        user_id INTEGER NOT NULL,
        pathname TEXT NOT NULL,
        engagement_time INTEGER NOT NULL,
        scroll_depth INTEGER NOT NULL,
        PRIMARY KEY (host_id, timestamp, id)
      ) WITHOUT ROWID`,
    ],
  },
];

/** Columns of `sessions`, in the order {@link sessionValues} fills them. */
const SESSION_COLUMNS = [
  "host_id",
  "start",
  "session_id",
  "user_id",
  "timestamp",
  "entry_page",
  "exit_page",
  "pageviews",
  "events",
  "duration",
  "is_bounce",
  "referrer",
  "source",
  "channel",
  "utm_medium",
  "utm_source",
  "utm_campaign",
  "utm_content",
  "utm_term",
  "click_id_param",
  "country",
  "region",
  "city",
  "device",
  "browser",
  "browser_version",
  "os",
  "os_version",
];

/**
 * Where analytics are kept: events and sessions, written in batches.
 *
 * Nothing is written per request. Events collect in memory and sessions are
 * marked as changed, and every few seconds the lot goes to disk in one
 * transaction — a session that saw ten pageviews in that time is written
 * once. A crash loses those seconds, which for counting visitors is an
 * acceptable price for never making a visitor's request wait on a disk.
 */
export class AnalyticsStore {
  private events: EventRow[] = [];
  private dirty = new Map<number, Session>();
  private insertEvent: SqliteStatement;
  private upsertSession: SqliteStatement;

  /** Events dropped because the buffer was full, since the last flush. */
  dropped = 0;

  constructor(readonly db: SqliteDatabase) {
    migrate(db);

    this.insertEvent = db.prepare(
      `INSERT OR IGNORE INTO events
        (host_id, timestamp, id, kind, session_id, user_id, pathname, engagement_time, scroll_depth)
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)`,
    );
    this.upsertSession = db.prepare(
      `INSERT OR REPLACE INTO sessions (${SESSION_COLUMNS.join(", ")})
        VALUES (${SESSION_COLUMNS.map(() => "?").join(", ")})`,
    );
  }

  /** Queue an event. Returns whether the buffer has grown enough to flush now. */
  addEvent(event: EventRow): boolean {
    if (this.events.length >= MAX_BUFFERED) {
      this.dropped++;
      return false;
    }

    this.events.push(event);
    return this.events.length >= FLUSH_AT;
  }

  /** Mark a session as changed, so the next flush writes it. */
  touchSession(session: Session): void {
    this.dirty.set(session.id, session);
  }

  /** Write everything queued, in one transaction. */
  flush(): { events: number; sessions: number } {
    const events = this.events;
    const sessions = [...this.dirty.values()];
    if (!events.length && !sessions.length) return { events: 0, sessions: 0 };

    this.events = [];
    this.dirty.clear();

    this.db.exec("BEGIN");
    try {
      for (const event of events) {
        this.insertEvent.run(
          event.hostId,
          event.timestamp,
          // Only needs to be unique among one site's events in one second.
          randomInt(2 ** 48 - 1),
          event.kind,
          event.sessionId,
          event.userId,
          event.pathname,
          event.engagementTime,
          event.scrollDepth,
        );
      }
      for (const session of sessions) {
        this.upsertSession.run(...sessionValues(session));
      }
      this.db.exec("COMMIT");
    } catch (error) {
      this.db.exec("ROLLBACK");
      throw error;
    }

    return { events: events.length, sessions: sessions.length };
  }

  /** Sessions active since `since` (epoch seconds), to pick up where a restart left off. */
  openSessions(since: number): Session[] {
    const rows = this.db
      .prepare(`SELECT ${SESSION_COLUMNS.join(", ")} FROM sessions WHERE timestamp >= ?`)
      .all(since) as SessionRow[];

    return rows.map(fromRow);
  }

  /** The newest salts, newest first. */
  salts(): { salt: Uint8Array; createdAt: number }[] {
    const rows = this.db.prepare("SELECT salt, created_at FROM salts ORDER BY id DESC LIMIT 2").all() as {
      salt: Uint8Array;
      created_at: number;
    }[];

    return rows.map((row) => ({ salt: row.salt, createdAt: row.created_at }));
  }

  /**
   * Mint a salt, and destroy every one but it and the one before.
   *
   * Destroying is the point. A visitor's id is a hash of their address under
   * the day's salt, so once the salt is gone nothing — not this database, not
   * whoever copies it — can tell whether two days' visitors were the same
   * person.
   */
  rotateSalt(now: number): Uint8Array {
    const salt = randomBytes(16);
    this.db.prepare("INSERT INTO salts (salt, created_at) VALUES (?, ?)").run(salt, now);
    this.db.prepare("DELETE FROM salts WHERE id NOT IN (SELECT id FROM salts ORDER BY id DESC LIMIT 2)").run();
    return salt;
  }
}

type SessionRow = Record<string, SqliteValue>;

function sessionValues(session: Session): SqliteValue[] {
  return [
    session.hostId,
    session.start,
    session.id,
    session.userId,
    session.timestamp,
    session.entryPage,
    session.exitPage,
    session.pageviews,
    session.events,
    session.duration,
    session.isBounce ? 1 : 0,
    session.referrer,
    session.source,
    session.channel,
    session.utmMedium,
    session.utmSource,
    session.utmCampaign,
    session.utmContent,
    session.utmTerm,
    session.clickIdParam,
    session.country,
    session.region,
    session.city,
    session.device,
    session.browser,
    session.browserVersion,
    session.os,
    session.osVersion,
  ];
}

function fromRow(row: SessionRow): Session {
  const text = (column: string) => String(row[column] ?? "");
  const number = (column: string) => Number(row[column] ?? 0);

  return {
    id: number("session_id"),
    hostId: number("host_id"),
    userId: number("user_id"),
    start: number("start"),
    timestamp: number("timestamp"),
    entryPage: text("entry_page"),
    exitPage: text("exit_page"),
    pageviews: number("pageviews"),
    events: number("events"),
    duration: number("duration"),
    isBounce: number("is_bounce") === 1,
    referrer: text("referrer"),
    source: text("source"),
    channel: text("channel"),
    utmMedium: text("utm_medium"),
    utmSource: text("utm_source"),
    utmCampaign: text("utm_campaign"),
    utmContent: text("utm_content"),
    utmTerm: text("utm_term"),
    clickIdParam: text("click_id_param"),
    country: text("country"),
    region: text("region"),
    city: text("city"),
    device: text("device"),
    browser: text("browser"),
    browserVersion: text("browser_version"),
    os: text("os"),
    osVersion: text("os_version"),
  };
}

/** Bring the analytics schema up to date. */
function migrate(db: SqliteDatabase): void {
  db.exec("CREATE TABLE IF NOT EXISTS schema_migrations (name TEXT PRIMARY KEY, applied_at INTEGER NOT NULL)");
  const applied = new Set(
    (db.prepare("SELECT name FROM schema_migrations").all() as { name: string }[]).map((row) => row.name),
  );

  for (const migration of MIGRATIONS) {
    if (applied.has(migration.name)) continue;

    db.exec("BEGIN");
    try {
      for (const statement of migration.statements) db.exec(statement);
      db.prepare("INSERT INTO schema_migrations (name, applied_at) VALUES (?, ?)").run(
        migration.name,
        Math.floor(Date.now() / 1000),
      );
      db.exec("COMMIT");
    } catch (error) {
      db.exec("ROLLBACK");
      throw error;
    }
  }
}
