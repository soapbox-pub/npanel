//! Where error reports are kept: issues, with running counts, and a bounded
//! sample of each one's events.
//!
//! A database of its own, `DATA_DIR/errors.sqlite3`, beside npanel's and the
//! analytics', for the same reason analytics have one: what grows with every
//! report a broken page sends stays away from what serving reads. Reports are
//! written in batches every few seconds, never one request at a time.
//!
//! Every count is exact; the events themselves are not all kept. An issue
//! keeps its first event, for what it looked like when it started, and its
//! latest [`KEEP_EVENTS`], for what it looks like now: a loop throwing on
//! every frame of every visitor's page is a lot of copies of one report.

use std::collections::{HashSet, BTreeSet};
use std::io::{Read, Write};

use anyhow::Result;
use rand::Rng;
use rusqlite::{params, Connection, OptionalExtension};

use super::grouping::Grouping;
use crate::analytics::salts::{SaltStore, DAY};

/// The latest events kept per issue, besides its first.
pub const KEEP_EVENTS: i64 = 100;

const MIGRATIONS: &[(&str, &[&str])] = &[(
    "001_initial",
    &[
        "CREATE TABLE salts (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        salt BLOB NOT NULL,
        created_at INTEGER NOT NULL
      )",
        "CREATE TABLE issues (
        id INTEGER PRIMARY KEY,
        host_id INTEGER NOT NULL,
        fingerprint TEXT NOT NULL,
        status TEXT NOT NULL DEFAULT 'unresolved' CHECK (status IN ('unresolved', 'resolved', 'ignored')),
        title TEXT NOT NULL,
        culprit TEXT NOT NULL,
        level TEXT NOT NULL,
        first_seen INTEGER NOT NULL,
        last_seen INTEGER NOT NULL,
        events INTEGER NOT NULL,
        visitors INTEGER NOT NULL,
        first_release TEXT,
        last_release TEXT,
        UNIQUE (host_id, fingerprint)
      )",
        // What the dashboard lists: one site's issues in one state, latest first.
        "CREATE INDEX issues_list_idx ON issues (host_id, status, last_seen DESC, id DESC)",
        "CREATE TABLE issue_hours (
        issue_id INTEGER NOT NULL REFERENCES issues (id) ON DELETE CASCADE,
        hour INTEGER NOT NULL,
        events INTEGER NOT NULL,
        PRIMARY KEY (issue_id, hour)
      ) WITHOUT ROWID",
        // Who an issue reached, a day at a time, under that day's salt.
        "CREATE TABLE issue_visitors (
        issue_id INTEGER NOT NULL REFERENCES issues (id) ON DELETE CASCADE,
        day INTEGER NOT NULL,
        visitor INTEGER NOT NULL,
        PRIMARY KEY (issue_id, day, visitor)
      ) WITHOUT ROWID",
        "CREATE TABLE events (
        id INTEGER PRIMARY KEY,
        issue_id INTEGER NOT NULL REFERENCES issues (id) ON DELETE CASCADE,
        event_id TEXT NOT NULL,
        received_at INTEGER NOT NULL,
        release TEXT,
        url TEXT,
        browser TEXT NOT NULL,
        os TEXT NOT NULL,
        payload BLOB NOT NULL
      )",
        "CREATE INDEX events_issue_idx ON events (issue_id, id DESC)",
        "CREATE INDEX events_received_idx ON events (received_at)",
    ],
)];

/// One report, ready to be written.
#[derive(Clone, Debug)]
pub struct Occurrence {
    pub host_id: i64,
    pub grouping: Grouping,
    pub level: String,
    pub received_at: i64,
    pub release: Option<String>,
    /// Who sent it, under the day's salt.
    pub visitor: i64,
    pub event_id: String,
    pub url: Option<String>,
    pub browser: String,
    pub os: String,
    /// The report, as [`compress`] made it.
    pub payload: Vec<u8>,
}

impl Occurrence {
    /// Roughly what it costs to hold in memory.
    pub fn weight(&self) -> usize {
        self.payload.len() + self.grouping.title.len() + self.grouping.culprit.len() + 256
    }
}

/// The connection reports are written through, which only it holds.
pub struct ErrorsStore {
    conn: Connection,
}

impl ErrorsStore {
    pub fn new(mut conn: Connection) -> Result<ErrorsStore> {
        crate::db::apply_migrations(&mut conn, MIGRATIONS, None)?;
        Ok(ErrorsStore { conn })
    }

    /// Write a batch in one transaction: each report counted against its
    /// issue, which it creates if it is the first, and kept as an event.
    pub fn write(&mut self, occurrences: &[Occurrence]) -> Result<()> {
        let tx = self.conn.transaction()?;
        let mut touched = BTreeSet::new();
        {
            let mut upsert = tx.prepare_cached(
                "INSERT INTO issues (host_id, fingerprint, title, culprit, level, first_seen, last_seen, events, visitors, first_release, last_release)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6, 1, 0, ?7, ?7)
                 ON CONFLICT (host_id, fingerprint) DO UPDATE SET
                   title = excluded.title, culprit = excluded.culprit, level = excluded.level,
                   last_seen = MAX(last_seen, excluded.last_seen), events = events + 1,
                   last_release = COALESCE(excluded.last_release, last_release)
                 RETURNING id",
            )?;
            let mut hour = tx.prepare_cached(
                "INSERT INTO issue_hours (issue_id, hour, events) VALUES (?, ?, 1)
                 ON CONFLICT (issue_id, hour) DO UPDATE SET events = events + 1",
            )?;
            let mut visitor = tx.prepare_cached("INSERT OR IGNORE INTO issue_visitors (issue_id, day, visitor) VALUES (?, ?, ?)")?;
            let mut visited = tx.prepare_cached("UPDATE issues SET visitors = visitors + 1 WHERE id = ?")?;
            let mut event = tx.prepare_cached(
                "INSERT INTO events (issue_id, event_id, received_at, release, url, browser, os, payload) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            )?;

            for o in occurrences {
                let g = &o.grouping;
                let id: i64 =
                    upsert.query_row(params![o.host_id, g.fingerprint, g.title, g.culprit, o.level, o.received_at, o.release], |r| r.get(0))?;
                hour.execute(params![id, o.received_at - o.received_at.rem_euclid(3600)])?;
                if visitor.execute(params![id, o.received_at - o.received_at.rem_euclid(DAY), o.visitor])? > 0 {
                    visited.execute([id])?;
                }
                event.execute(params![id, o.event_id, o.received_at, o.release, o.url, o.browser, o.os, o.payload])?;
                touched.insert(id);
            }

            let mut trim = tx.prepare_cached(
                "DELETE FROM events WHERE issue_id = ?1
                   AND id != (SELECT MIN(id) FROM events WHERE issue_id = ?1)
                   AND id NOT IN (SELECT id FROM events WHERE issue_id = ?1 ORDER BY id DESC LIMIT ?2)",
            )?;
            for id in touched {
                trim.execute(params![id, KEEP_EVENTS])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Set an issue's status, returning what it was, or `None` if the site
    /// has no such issue.
    pub fn set_status(&self, host_id: i64, issue_id: i64, status: &str) -> Result<Option<String>> {
        let previous: Option<String> = self
            .conn
            .query_row("SELECT status FROM issues WHERE id = ? AND host_id = ?", params![issue_id, host_id], |r| r.get(0))
            .optional()?;
        if previous.is_some() {
            self.conn.execute("UPDATE issues SET status = ? WHERE id = ?", params![status, issue_id])?;
        }
        Ok(previous)
    }

    /// Forget everything older than `cutoff`, and every issue of a host that
    /// no longer exists. Returns how many issues went.
    pub fn prune(&self, cutoff: i64, hosts: &HashSet<i64>) -> Result<usize> {
        let mut removed = self.conn.execute("DELETE FROM issues WHERE last_seen < ?", [cutoff])?;
        let stored: Vec<i64> = self.conn.prepare("SELECT DISTINCT host_id FROM issues")?.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
        for host_id in stored.into_iter().filter(|id| !hosts.contains(id)) {
            removed += self.conn.execute("DELETE FROM issues WHERE host_id = ?", [host_id])?;
        }
        self.conn.execute("DELETE FROM events WHERE received_at < ?", [cutoff])?;
        self.conn.execute("DELETE FROM issue_hours WHERE hour < ?", [cutoff - cutoff.rem_euclid(3600)])?;
        self.conn.execute("DELETE FROM issue_visitors WHERE day < ?", [cutoff - cutoff.rem_euclid(DAY)])?;
        Ok(removed)
    }
}

impl SaltStore for ErrorsStore {
    fn salts(&self) -> Result<Vec<(Vec<u8>, i64)>> {
        let mut statement = self.conn.prepare("SELECT salt, created_at FROM salts ORDER BY id DESC LIMIT 2")?;
        let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    fn rotate_salt(&self, now: i64) -> Result<Vec<u8>> {
        let mut salt = vec![0u8; 16];
        rand::thread_rng().fill(&mut salt[..]);
        self.conn.execute("INSERT INTO salts (salt, created_at) VALUES (?, ?)", params![salt, now])?;
        self.conn.execute("DELETE FROM salts WHERE id NOT IN (SELECT id FROM salts ORDER BY id DESC LIMIT 2)", [])?;
        Ok(salt)
    }
}

/// A report as it is stored: its JSON, deflated. Reports are mostly frames
/// and breadcrumbs that repeat, and shrink several times over.
pub fn compress(json: &[u8]) -> Vec<u8> {
    let mut encoder = flate2::write::DeflateEncoder::new(Vec::with_capacity(json.len() / 4), flate2::Compression::default());
    // Writing to a Vec can't fail.
    let _ = encoder.write_all(json);
    encoder.finish().unwrap_or_default()
}

pub fn decompress(payload: &[u8]) -> std::io::Result<Vec<u8>> {
    let mut json = Vec::new();
    flate2::read::DeflateDecoder::new(payload).read_to_end(&mut json)?;
    Ok(json)
}

#[cfg(test)]
mod tests {
    use super::*;

    pub fn occurrence(host_id: i64, fingerprint: &str, at: i64, visitor: i64) -> Occurrence {
        Occurrence {
            host_id,
            grouping: Grouping { fingerprint: fingerprint.into(), title: format!("Error: {fingerprint}"), culprit: "f (a.js)".into() },
            level: "error".into(),
            received_at: at,
            release: Some(format!("r{at}")),
            visitor,
            event_id: format!("{at:032x}"),
            url: Some("https://docs.example.com/".into()),
            browser: "Chrome".into(),
            os: "Linux".into(),
            payload: compress(b"{}"),
        }
    }

    fn store() -> (tempfile::TempDir, ErrorsStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = ErrorsStore::new(crate::db::open_connection(&dir.path().join("errors.sqlite3")).unwrap()).unwrap();
        (dir, store)
    }

    fn count(store: &ErrorsStore, sql: &str) -> i64 {
        store.conn.query_row(sql, [], |r| r.get(0)).unwrap()
    }

    #[test]
    fn counts_and_samples() {
        let (_dir, mut store) = store();
        let batch: Vec<Occurrence> = (0..150).map(|i| occurrence(1, "a", 1000 + i, i % 3)).collect();
        store.write(&batch).unwrap();
        store.write(&[occurrence(1, "b", 5000, 1), occurrence(2, "a", 5000, 1)]).unwrap();

        let (events, visitors, first, last, first_release, last_release): (i64, i64, i64, i64, String, String) = store
            .conn
            .query_row("SELECT events, visitors, first_seen, last_seen, first_release, last_release FROM issues WHERE host_id = 1 AND fingerprint = 'a'", [], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?))
            })
            .unwrap();
        assert_eq!((events, visitors, first, last), (150, 3, 1000, 1149));
        assert_eq!((first_release.as_str(), last_release.as_str()), ("r1000", "r1149"));
        assert_eq!(count(&store, "SELECT COUNT(*) FROM issues"), 3);
        // The first event and the latest hundred.
        assert_eq!(count(&store, "SELECT COUNT(*) FROM events WHERE issue_id = 1"), 101);
        assert_eq!(count(&store, "SELECT MIN(received_at) FROM events WHERE issue_id = 1"), 1000);
        assert_eq!(count(&store, "SELECT MIN(received_at) FROM events WHERE issue_id = 1 AND received_at > 1000"), 1050);
        assert_eq!(count(&store, "SELECT SUM(events) FROM issue_hours WHERE issue_id = 1"), 150);

        assert_eq!(store.set_status(1, 1, "resolved").unwrap().as_deref(), Some("unresolved"));
        assert_eq!(store.set_status(2, 1, "resolved").unwrap(), None);
        // New events don't reopen it.
        store.write(&[occurrence(1, "a", 6000, 9)]).unwrap();
        assert_eq!(store.set_status(1, 1, "ignored").unwrap().as_deref(), Some("resolved"));

        assert_eq!(decompress(&compress(b"{\"a\":1}")).unwrap(), b"{\"a\":1}");
    }

    #[test]
    fn prunes() {
        let (_dir, mut store) = store();
        store.write(&[occurrence(1, "old", 1000, 1), occurrence(1, "kept", 1000, 1), occurrence(1, "kept", 200_000, 1), occurrence(2, "gone", 200_000, 1)]).unwrap();
        let removed = store.prune(100_000, &HashSet::from([1])).unwrap();
        assert_eq!(removed, 2);
        assert_eq!(count(&store, "SELECT COUNT(*) FROM issues"), 1);
        assert_eq!(count(&store, "SELECT COUNT(*) FROM events"), 1);
        assert_eq!(count(&store, "SELECT COUNT(*) FROM issue_hours"), 1);
        assert_eq!(count(&store, "SELECT COUNT(*) FROM issue_visitors"), 1);
        // The lifetime count stays.
        assert_eq!(count(&store, "SELECT events FROM issues"), 2);
    }
}
