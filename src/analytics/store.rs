//! Where analytics are kept: events and sessions, written in batches.
//!
//! Its own database file beside npanel's, so the tables that grow with every
//! page anyone views are nowhere near the ones serving reads from. Nothing is
//! written per request: events collect in memory (see [`super::ingest`]), and
//! every few seconds the lot goes to disk in one transaction. A crash loses
//! those seconds, which for counting visitors is an acceptable price for never
//! making a visitor's request wait on a disk.

use anyhow::Result;
use rand::Rng;
use rusqlite::{params, Connection};

use super::sessions::{Session, SessionAttributes};

/// What an event was. Stored as a number, since there are only three answers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(i64)]
pub enum EventKind {
    Pageview = 0,
    Engagement = 1,
    Custom = 2,
}

impl rusqlite::ToSql for EventKind {
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        Ok((*self as i64).into())
    }
}

/// One event, as it is stored.
#[derive(Clone, Debug)]
pub struct EventRow {
    pub host_id: i64,
    /// Epoch seconds.
    pub timestamp: i64,
    pub kind: EventKind,
    pub session_id: i64,
    pub user_id: i64,
    pub pathname: String,
    /// Milliseconds, on engagement events; 0 otherwise.
    pub engagement_time: i64,
    /// Percent, on engagement events; 255 where none was reported.
    pub scroll_depth: i64,
}

/// Migrations for the analytics database. Both tables are clustered by site and time, because every query
/// reads one site over one date range.
const MIGRATIONS: &[(&str, &[&str])] = &[(
    "001_initial",
    &[
        "CREATE TABLE salts (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        salt BLOB NOT NULL,
        created_at INTEGER NOT NULL
      )",
        "CREATE TABLE sessions (
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
      ) WITHOUT ROWID",
        "CREATE INDEX sessions_timestamp_idx ON sessions (timestamp)",
        // What an event is joined to its visit by.
        "CREATE INDEX sessions_session_idx ON sessions (host_id, session_id)",
        "CREATE TABLE events (
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
      ) WITHOUT ROWID",
    ],
)];

const SESSION_COLUMNS: &str = "host_id, start, session_id, user_id, timestamp, entry_page, exit_page, pageviews, events, \
    duration, is_bounce, referrer, source, channel, utm_medium, utm_source, utm_campaign, utm_content, utm_term, \
    click_id_param, country, region, city, device, browser, browser_version, os, os_version";

/// The connection analytics are written through, which only it holds.
pub struct AnalyticsStore {
    conn: Connection,
}

impl AnalyticsStore {
    pub fn new(mut conn: Connection) -> Result<AnalyticsStore> {
        crate::db::apply_migrations(&mut conn, MIGRATIONS, None)?;
        Ok(AnalyticsStore { conn })
    }

    /// Write a batch of events and the latest state of the sessions they
    /// changed, in one transaction.
    pub fn write(&mut self, events: &[EventRow], sessions: &[Session]) -> Result<()> {
        let tx = self.conn.transaction()?;
        {
            let mut insert = tx.prepare_cached(
                "INSERT OR IGNORE INTO events
                 (host_id, timestamp, id, kind, session_id, user_id, pathname, engagement_time, scroll_depth)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )?;
            let mut rng = rand::thread_rng();
            for e in events {
                // Only needs to be unique among one site's events in one second.
                let id: i64 = rng.gen_range(0..(1i64 << 48) - 1);
                insert.execute(params![
                    e.host_id,
                    e.timestamp,
                    id,
                    e.kind,
                    e.session_id,
                    e.user_id,
                    e.pathname,
                    e.engagement_time,
                    e.scroll_depth
                ])?;
            }

            let placeholders = vec!["?"; 28].join(", ");
            let mut upsert = tx.prepare_cached(&format!("INSERT OR REPLACE INTO sessions ({SESSION_COLUMNS}) VALUES ({placeholders})"))?;
            for s in sessions {
                let a = &s.attributes;
                upsert.execute(params![
                    s.host_id,
                    s.start,
                    s.id,
                    s.user_id,
                    s.timestamp,
                    s.entry_page,
                    s.exit_page,
                    s.pageviews,
                    s.events,
                    s.duration,
                    s.is_bounce as i64,
                    a.referrer,
                    a.source,
                    a.channel,
                    a.utm_medium,
                    a.utm_source,
                    a.utm_campaign,
                    a.utm_content,
                    a.utm_term,
                    a.click_id_param,
                    a.country,
                    a.region,
                    a.city,
                    a.device,
                    a.browser,
                    a.browser_version,
                    a.os,
                    a.os_version
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Sessions active since `since`, to pick up where a restart left off.
    pub fn open_sessions(&self, since: i64) -> Result<Vec<Session>> {
        let mut statement = self.conn.prepare(&format!("SELECT {SESSION_COLUMNS} FROM sessions WHERE timestamp >= ?"))?;
        let rows = statement.query_map([since], |row| {
            Ok(Session {
                host_id: row.get(0)?,
                start: row.get(1)?,
                id: row.get(2)?,
                user_id: row.get(3)?,
                timestamp: row.get(4)?,
                last_seen: row.get(4)?,
                entry_page: row.get(5)?,
                exit_page: row.get(6)?,
                pageviews: row.get(7)?,
                events: row.get(8)?,
                duration: row.get(9)?,
                is_bounce: row.get::<_, i64>(10)? == 1,
                attributes: SessionAttributes {
                    referrer: row.get(11)?,
                    source: row.get(12)?,
                    channel: row.get(13)?,
                    utm_medium: row.get(14)?,
                    utm_source: row.get(15)?,
                    utm_campaign: row.get(16)?,
                    utm_content: row.get(17)?,
                    utm_term: row.get(18)?,
                    click_id_param: row.get(19)?,
                    country: row.get(20)?,
                    region: row.get(21)?,
                    city: row.get(22)?,
                    device: row.get(23)?,
                    browser: row.get(24)?,
                    browser_version: row.get(25)?,
                    os: row.get(26)?,
                    os_version: row.get(27)?,
                },
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The newest salts, newest first.
    pub fn salts(&self) -> Result<Vec<(Vec<u8>, i64)>> {
        let mut statement = self.conn.prepare("SELECT salt, created_at FROM salts ORDER BY id DESC LIMIT 2")?;
        let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Mint a salt, and destroy every one but it and the one before.
    ///
    /// Destroying is the point: once a salt is gone nothing can tell whether
    /// two days' visitors were the same person.
    pub fn rotate_salt(&self, now: i64) -> Result<Vec<u8>> {
        let mut salt = vec![0u8; 16];
        rand::thread_rng().fill(&mut salt[..]);
        self.conn.execute("INSERT INTO salts (salt, created_at) VALUES (?, ?)", params![salt, now])?;
        self.conn.execute("DELETE FROM salts WHERE id NOT IN (SELECT id FROM salts ORDER BY id DESC LIMIT 2)", [])?;
        Ok(salt)
    }
}
