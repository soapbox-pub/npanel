//! The cards npanel has fetched, in a database of their own,
//! `DATA_DIR/previews.sqlite3`.
//!
//! A file apart from `npanel.sqlite3` so that clearing it — and the `VACUUM`
//! that gives the space back — never touches the gateway's own data, and so
//! that what it takes up on disk is simply its size.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

use super::card::Card;
use super::templates::Subject;
use crate::nostr::Event;

/// How long a lookup that found nothing is believed, at most. Shorter than a
/// card's, since nothing found may only mean the relays were slow.
pub const NOT_FOUND_TTL: i64 = 10 * 60;

/// What a site's queries found about one event: what is known of it, and
/// each query's events, newest first.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Found {
    pub subject: Subject,
    pub results: IndexMap<String, Vec<Event>>,
}

impl Found {
    fn weight(&self) -> usize {
        let event = |e: &Event| e.content.len() + e.tags.iter().flatten().map(String::len).sum::<usize>() + 256;
        self.results.values().flatten().map(event).sum::<usize>() + 512
    }
}

/// What a lookup found.
#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    Card(Arc<Card>),
    /// For a site's template: what its queries found.
    Results(Arc<Found>),
    /// An event of a kind that isn't previewed. Its page is served as
    /// published, and since an event's kind never changes, that is known
    /// for as long as a card would be.
    Unsupported(i64),
    NotFound,
}

/// What is known about one entity, and when it was learned.
#[derive(Clone, Debug)]
pub struct Cached {
    pub outcome: Outcome,
    pub fetched_at: i64,
}

impl Cached {
    pub fn fresh(&self, ttl: i64, now: i64) -> bool {
        let ttl = if self.outcome == Outcome::NotFound { ttl.min(NOT_FOUND_TTL) } else { ttl };
        now - self.fetched_at < ttl
    }

    pub fn weight(&self) -> usize {
        match &self.outcome {
            Outcome::Card(card) => card.weight(),
            Outcome::Results(found) => found.weight(),
            _ => 64,
        }
    }
}

/// Create the table, or bring an older one up to date.
pub fn migrate(conn: &Connection) -> Result<()> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if version < 1 {
        conn.execute_batch(
            "BEGIN;
             CREATE TABLE previews (
               key TEXT PRIMARY KEY,
               card TEXT,
               fetched_at INTEGER NOT NULL
             );
             CREATE INDEX previews_fetched_idx ON previews (fetched_at);
             PRAGMA user_version = 1;
             COMMIT;",
        )?;
    }
    if version < 2 {
        // The kind of an event that isn't previewed, on a row with no card.
        conn.execute_batch(
            "BEGIN;
             ALTER TABLE previews ADD COLUMN unsupported INTEGER;
             PRAGMA user_version = 2;
             COMMIT;",
        )?;
    }
    if version < 3 {
        // What a site's queries found, on a row with no card.
        conn.execute_batch(
            "BEGIN;
             ALTER TABLE previews ADD COLUMN results TEXT;
             PRAGMA user_version = 3;
             COMMIT;",
        )?;
    }
    Ok(())
}

/// A row: its card, its results, the kind it doesn't preview, and when it
/// was fetched.
type Row = (Option<String>, Option<String>, Option<i64>, i64);

pub fn get(conn: &Connection, key: &str) -> Result<Option<Cached>> {
    let row: Option<Row> = conn
        .prepare_cached("SELECT card, results, unsupported, fetched_at FROM previews WHERE key = ?")?
        .query_row([key], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .optional()?;
    let Some((card, results, unsupported, fetched_at)) = row else { return Ok(None) };
    let outcome = match (card, results, unsupported) {
        // A row an older npanel wrote in a shape this one can't read is as
        // good as missing, and is replaced by the next lookup.
        (Some(json), _, _) => match serde_json::from_str::<Card>(&json) {
            Ok(card) => Outcome::Card(Arc::new(card)),
            Err(_) => return Ok(None),
        },
        (None, Some(json), _) => match serde_json::from_str::<Found>(&json) {
            Ok(found) => Outcome::Results(Arc::new(found)),
            Err(_) => return Ok(None),
        },
        (None, None, Some(kind)) => Outcome::Unsupported(kind),
        (None, None, None) => Outcome::NotFound,
    };
    Ok(Some(Cached { outcome, fetched_at }))
}

pub fn put(conn: &Connection, key: &str, cached: &Cached) -> Result<()> {
    let (card, results, unsupported) = match &cached.outcome {
        Outcome::Card(card) => (Some(serde_json::to_string(card.as_ref())?), None, None),
        Outcome::Results(found) => (None, Some(serde_json::to_string(found.as_ref())?), None),
        Outcome::Unsupported(kind) => (None, None, Some(*kind)),
        Outcome::NotFound => (None, None, None),
    };
    conn.prepare_cached(
        "INSERT INTO previews (key, card, results, unsupported, fetched_at) VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT (key) DO UPDATE SET card = ?2, results = ?3, unsupported = ?4, fetched_at = ?5",
    )?
    .execute(params![key, card, results, unsupported, cached.fetched_at])?;
    Ok(())
}

/// Forget the oldest rows past `max`.
pub fn evict(conn: &Connection, max: i64) -> Result<usize> {
    let count: i64 = conn.query_row("SELECT COUNT(*) FROM previews", [], |r| r.get(0))?;
    if count <= max {
        return Ok(0);
    }
    Ok(conn.execute(
        "DELETE FROM previews WHERE key IN (SELECT key FROM previews ORDER BY fetched_at ASC LIMIT ?)",
        [count - max],
    )?)
}

/// Forget everything, and give the space back.
pub fn clear(conn: &Connection) -> Result<usize> {
    let deleted = conn.execute("DELETE FROM previews", [])?;
    conn.execute_batch("VACUUM; PRAGMA wal_checkpoint(TRUNCATE);")?;
    Ok(deleted)
}

/// How many rows hold a card or what a site's queries found, how many an
/// event that isn't previewed, and how many remember finding nothing.
pub fn counts(conn: &Connection) -> Result<(i64, i64, i64)> {
    let sql = "SELECT COUNT(card) + COUNT(results), COUNT(unsupported), COUNT(*) - COUNT(card) - COUNT(results) - COUNT(unsupported) FROM previews";
    Ok(conn.query_row(sql, [], |r| {
        Ok((r.get(0)?, r.get(1)?, r.get(2)?))
    })?)
}

/// The database's size on disk, its write-ahead log included.
pub fn disk_bytes(path: &Path) -> u64 {
    let with_suffix = |suffix: &str| -> PathBuf {
        let mut name = path.as_os_str().to_owned();
        name.push(suffix);
        PathBuf::from(name)
    };
    [path.to_path_buf(), with_suffix("-wal"), with_suffix("-shm")].iter().filter_map(|p| std::fs::metadata(p).ok()).map(|m| m.len()).sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::link_previews::card::profile_card;
    use crate::nostr::Event;

    #[test]
    fn stores_evicts_and_clears() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("previews.sqlite3");
        let conn = crate::db::open_connection(&path).unwrap();
        migrate(&conn).unwrap();
        migrate(&conn).unwrap();

        let profile = Event::sign("0000000000000000000000000000000000000000000000000000000000000003", 1, 0, vec![], r#"{"name":"a"}"#);
        let card = Arc::new(profile_card(&profile));
        put(&conn, "p:a", &Cached { outcome: Outcome::Card(card.clone()), fetched_at: 10 }).unwrap();
        put(&conn, "e:b", &Cached { outcome: Outcome::NotFound, fetched_at: 20 }).unwrap();
        put(&conn, "e:c", &Cached { outcome: Outcome::Unsupported(31337), fetched_at: 30 }).unwrap();
        let found = Arc::new(Found { subject: Subject { kind: 0, pubkey: Some(profile.pubkey.clone()), ..Default::default() }, results: IndexMap::from([("p".into(), vec![profile.clone()])]) });
        put(&conn, "t:x:p:a", &Cached { outcome: Outcome::Results(found.clone()), fetched_at: 35 }).unwrap();
        assert_eq!(get(&conn, "t:x:p:a").unwrap().unwrap().outcome, Outcome::Results(found));
        conn.execute("DELETE FROM previews WHERE key = 't:x:p:a'", []).unwrap();
        assert_eq!(get(&conn, "p:a").unwrap().unwrap().outcome, Outcome::Card(card));
        assert_eq!(get(&conn, "e:b").unwrap().unwrap().outcome, Outcome::NotFound);
        assert_eq!(get(&conn, "e:c").unwrap().unwrap().outcome, Outcome::Unsupported(31337));
        assert!(get(&conn, "e:x").unwrap().is_none());
        assert_eq!(counts(&conn).unwrap(), (1, 1, 1));

        // A card that was found is written over by what is found next.
        put(&conn, "e:c", &Cached { outcome: Outcome::NotFound, fetched_at: 40 }).unwrap();
        assert_eq!(counts(&conn).unwrap(), (1, 0, 2));

        assert_eq!(evict(&conn, 2).unwrap(), 1);
        assert!(get(&conn, "p:a").unwrap().is_none());
        assert!(disk_bytes(&path) > 0);

        assert_eq!(clear(&conn).unwrap(), 2);
        assert_eq!(counts(&conn).unwrap(), (0, 0, 0));
    }

    #[test]
    fn freshness() {
        let found = Cached { outcome: Outcome::Card(Arc::new(profile_card(&Event::sign("0000000000000000000000000000000000000000000000000000000000000003", 1, 0, vec![], "{}")))), fetched_at: 0 };
        let unsupported = Cached { outcome: Outcome::Unsupported(31337), fetched_at: 0 };
        let missing = Cached { outcome: Outcome::NotFound, fetched_at: 0 };
        assert!(found.fresh(3600, 3599));
        assert!(!found.fresh(3600, 3600));
        assert!(unsupported.fresh(3600, 3599));
        assert!(missing.fresh(3600, NOT_FOUND_TTL - 1));
        assert!(!missing.fresh(3600, NOT_FOUND_TTL));
        assert!(!missing.fresh(60, 60));
    }
}
