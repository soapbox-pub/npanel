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

use super::card::Card;

/// How long a lookup that found nothing is believed, at most. Shorter than a
/// card's, since nothing found may only mean the relays were slow.
pub const NOT_FOUND_TTL: i64 = 10 * 60;

/// What is known about one entity, and when it was learned.
#[derive(Clone, Debug)]
pub struct Cached {
    /// `None` when nothing was found.
    pub card: Option<Arc<Card>>,
    pub fetched_at: i64,
}

impl Cached {
    pub fn fresh(&self, ttl: i64, now: i64) -> bool {
        let ttl = if self.card.is_some() { ttl } else { ttl.min(NOT_FOUND_TTL) };
        now - self.fetched_at < ttl
    }

    pub fn weight(&self) -> usize {
        self.card.as_ref().map_or(64, |card| card.weight())
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
    Ok(())
}

pub fn get(conn: &Connection, key: &str) -> Result<Option<Cached>> {
    let row: Option<(Option<String>, i64)> = conn
        .prepare_cached("SELECT card, fetched_at FROM previews WHERE key = ?")?
        .query_row([key], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()?;
    let Some((card, fetched_at)) = row else { return Ok(None) };
    // A row an older npanel wrote in a shape this one can't read is as good as
    // missing, and is replaced by the next lookup.
    let card = match card {
        Some(json) => match serde_json::from_str::<Card>(&json) {
            Ok(card) => Some(Arc::new(card)),
            Err(_) => return Ok(None),
        },
        None => None,
    };
    Ok(Some(Cached { card, fetched_at }))
}

pub fn put(conn: &Connection, key: &str, cached: &Cached) -> Result<()> {
    let card = cached.card.as_ref().map(|card| serde_json::to_string(card.as_ref())).transpose()?;
    conn.prepare_cached(
        "INSERT INTO previews (key, card, fetched_at) VALUES (?1, ?2, ?3)
         ON CONFLICT (key) DO UPDATE SET card = ?2, fetched_at = ?3",
    )?
    .execute(params![key, card, cached.fetched_at])?;
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

/// How many rows hold a card, and how many remember finding nothing.
pub fn counts(conn: &Connection) -> Result<(i64, i64)> {
    Ok(conn.query_row("SELECT COUNT(card), COUNT(*) - COUNT(card) FROM previews", [], |r| Ok((r.get(0)?, r.get(1)?)))?)
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
        put(&conn, "p:a", &Cached { card: Some(card.clone()), fetched_at: 10 }).unwrap();
        put(&conn, "e:b", &Cached { card: None, fetched_at: 20 }).unwrap();
        put(&conn, "e:c", &Cached { card: None, fetched_at: 30 }).unwrap();
        assert_eq!(get(&conn, "p:a").unwrap().unwrap().card.as_deref(), Some(card.as_ref()));
        assert!(get(&conn, "e:b").unwrap().unwrap().card.is_none());
        assert!(get(&conn, "e:x").unwrap().is_none());
        assert_eq!(counts(&conn).unwrap(), (1, 2));

        assert_eq!(evict(&conn, 2).unwrap(), 1);
        assert!(get(&conn, "p:a").unwrap().is_none());
        assert!(disk_bytes(&path) > 0);

        assert_eq!(clear(&conn).unwrap(), 2);
        assert_eq!(counts(&conn).unwrap(), (0, 0));
    }

    #[test]
    fn freshness() {
        let found = Cached { card: Some(Arc::new(profile_card(&Event::sign("0000000000000000000000000000000000000000000000000000000000000003", 1, 0, vec![], "{}")))), fetched_at: 0 };
        let missing = Cached { card: None, fetched_at: 0 };
        assert!(found.fresh(3600, 3599));
        assert!(!found.fresh(3600, 3600));
        assert!(missing.fresh(3600, NOT_FOUND_TTL - 1));
        assert!(!missing.fresh(3600, NOT_FOUND_TTL));
        assert!(!missing.fresh(60, 60));
    }
}
