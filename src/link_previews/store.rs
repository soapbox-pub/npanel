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

/// What a lookup found.
#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    Card(Arc<Card>),
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
        // What sites' templates' queries found, before scripts replaced them.
        conn.execute_batch(
            "BEGIN;
             ALTER TABLE previews ADD COLUMN results TEXT;
             PRAGMA user_version = 3;
             COMMIT;",
        )?;
    }
    if version < 4 {
        // Which request headers a site's script read for a page, which its
        // rows are keyed by.
        conn.execute_batch(
            "BEGIN;
             ALTER TABLE previews DROP COLUMN results;
             CREATE TABLE varies (
               base TEXT PRIMARY KEY,
               names TEXT NOT NULL,
               updated_at INTEGER NOT NULL
             );
             CREATE INDEX varies_updated_idx ON varies (updated_at);
             PRAGMA user_version = 4;
             COMMIT;",
        )?;
    }
    Ok(())
}

/// The request headers a script read for a page, as last recorded.
pub fn get_vary(conn: &Connection, base: &str) -> Result<Option<Vec<String>>> {
    let names: Option<String> = conn.prepare_cached("SELECT names FROM varies WHERE base = ?")?.query_row([base], |r| r.get(0)).optional()?;
    Ok(names.and_then(|names| serde_json::from_str(&names).ok()))
}

pub fn put_vary(conn: &Connection, base: &str, names: &[String]) -> Result<()> {
    conn.prepare_cached(
        "INSERT INTO varies (base, names, updated_at) VALUES (?1, ?2, ?3)
         ON CONFLICT (base) DO UPDATE SET names = ?2, updated_at = ?3",
    )?
    .execute(params![base, serde_json::to_string(names)?, crate::util::now()])?;
    Ok(())
}

pub fn get(conn: &Connection, key: &str) -> Result<Option<Cached>> {
    let row: Option<(Option<String>, Option<i64>, i64)> = conn
        .prepare_cached("SELECT card, unsupported, fetched_at FROM previews WHERE key = ?")?
        .query_row([key], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .optional()?;
    let Some((card, unsupported, fetched_at)) = row else { return Ok(None) };
    let outcome = match (card, unsupported) {
        // A row an older npanel wrote in a shape this one can't read is as
        // good as missing, and is replaced by the next lookup.
        (Some(json), _) => match serde_json::from_str::<Card>(&json) {
            Ok(card) => Outcome::Card(Arc::new(card)),
            Err(_) => return Ok(None),
        },
        (None, Some(kind)) => Outcome::Unsupported(kind),
        (None, None) => Outcome::NotFound,
    };
    Ok(Some(Cached { outcome, fetched_at }))
}

pub fn put(conn: &Connection, key: &str, cached: &Cached) -> Result<()> {
    let (card, unsupported) = match &cached.outcome {
        Outcome::Card(card) => (Some(serde_json::to_string(card.as_ref())?), None),
        Outcome::Unsupported(kind) => (None, Some(*kind)),
        Outcome::NotFound => (None, None),
    };
    conn.prepare_cached(
        "INSERT INTO previews (key, card, unsupported, fetched_at) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT (key) DO UPDATE SET card = ?2, unsupported = ?3, fetched_at = ?4",
    )?
    .execute(params![key, card, unsupported, cached.fetched_at])?;
    Ok(())
}

/// Forget the oldest rows past `max`.
pub fn evict(conn: &Connection, max: i64) -> Result<usize> {
    evict_varies(conn, max)?;
    let count: i64 = conn.query_row("SELECT COUNT(*) FROM previews", [], |r| r.get(0))?;
    if count <= max {
        return Ok(0);
    }
    Ok(conn.execute(
        "DELETE FROM previews WHERE key IN (SELECT key FROM previews ORDER BY fetched_at ASC LIMIT ?)",
        [count - max],
    )?)
}

/// Forget the oldest records of what scripts read past `max`, as previews
/// are forgotten.
fn evict_varies(conn: &Connection, max: i64) -> Result<()> {
    let count: i64 = conn.query_row("SELECT COUNT(*) FROM varies", [], |r| r.get(0))?;
    if count > max {
        conn.execute("DELETE FROM varies WHERE base IN (SELECT base FROM varies ORDER BY updated_at ASC LIMIT ?)", [count - max])?;
    }
    Ok(())
}

/// Forget everything, and give the space back.
pub fn clear(conn: &Connection) -> Result<usize> {
    let deleted = conn.execute("DELETE FROM previews", [])?;
    conn.execute("DELETE FROM varies", [])?;
    conn.execute_batch("VACUUM; PRAGMA wal_checkpoint(TRUNCATE);")?;
    Ok(deleted)
}

/// How many rows hold a card, how many an event that isn't previewed, and
/// how many remember finding nothing.
pub fn counts(conn: &Connection) -> Result<(i64, i64, i64)> {
    Ok(conn.query_row("SELECT COUNT(card), COUNT(unsupported), COUNT(*) - COUNT(card) - COUNT(unsupported) FROM previews", [], |r| {
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

        put_vary(&conn, "s:x", &["user-agent".into()]).unwrap();
        assert_eq!(get_vary(&conn, "s:x").unwrap(), Some(vec!["user-agent".into()]));
        assert_eq!(get_vary(&conn, "s:y").unwrap(), None);

        assert_eq!(clear(&conn).unwrap(), 2);
        assert_eq!(get_vary(&conn, "s:x").unwrap(), None);
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
