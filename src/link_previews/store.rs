//! The previews sites' scripts made, in a database of their own,
//! `data_dir/previews.sqlite3`.
//!
//! A file apart from `npanel.sqlite3` so that clearing it — and the `VACUUM`
//! that gives the space back — never touches the gateway's own data, and so
//! that what it takes up on disk is simply its size. It's a cache, so its
//! layout is versioned as a whole: one an older npanel wrote is thrown away,
//! and refilled as pages are asked for.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};

use super::preview::Preview;

/// The layout this npanel writes.
const SCHEMA: i64 = 5;

/// How long a script having nothing for a page is believed, at most. Shorter
/// than a preview's, since it may only mean the relays were slow.
pub const EMPTY_TTL: i64 = 10 * 60;

/// What a script made of a page, and when.
#[derive(Clone, Debug)]
pub struct Cached {
    /// `None` when it had nothing for it.
    pub preview: Option<Arc<Preview>>,
    pub fetched_at: i64,
}

impl Cached {
    pub fn fresh(&self, ttl: i64, now: i64) -> bool {
        let ttl = if self.preview.is_none() { ttl.min(EMPTY_TTL) } else { ttl };
        now - self.fetched_at < ttl
    }

    pub fn weight(&self) -> usize {
        self.preview.as_ref().map_or(64, |preview| preview.weight())
    }
}

/// Create the tables, throwing away any an older npanel made.
pub fn migrate(conn: &Connection) -> Result<()> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if version == SCHEMA {
        return Ok(());
    }
    conn.execute_batch(&format!(
        "BEGIN;
         DROP TABLE IF EXISTS previews;
         DROP TABLE IF EXISTS varies;
         CREATE TABLE previews (
           key TEXT PRIMARY KEY,
           preview TEXT,
           fetched_at INTEGER NOT NULL
         );
         CREATE INDEX previews_fetched_idx ON previews (fetched_at);
         -- Which request headers a page's script read, which its rows are
         -- keyed by.
         CREATE TABLE varies (
           base TEXT PRIMARY KEY,
           names TEXT NOT NULL,
           updated_at INTEGER NOT NULL
         );
         CREATE INDEX varies_updated_idx ON varies (updated_at);
         PRAGMA user_version = {SCHEMA};
         COMMIT;"
    ))?;
    Ok(())
}

pub fn get(conn: &Connection, key: &str) -> Result<Option<Cached>> {
    let row: Option<(Option<String>, i64)> = conn
        .prepare_cached("SELECT preview, fetched_at FROM previews WHERE key = ?")?
        .query_row([key], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()?;
    let Some((preview, fetched_at)) = row else { return Ok(None) };
    let preview = match preview {
        // A row this npanel can't read is as good as missing, and is
        // replaced by the next run.
        Some(json) => match serde_json::from_str::<Preview>(&json) {
            Ok(preview) => Some(Arc::new(preview)),
            Err(_) => return Ok(None),
        },
        None => None,
    };
    Ok(Some(Cached { preview, fetched_at }))
}

pub fn put(conn: &Connection, key: &str, cached: &Cached) -> Result<()> {
    let preview = cached.preview.as_deref().map(serde_json::to_string).transpose()?;
    conn.prepare_cached(
        "INSERT INTO previews (key, preview, fetched_at) VALUES (?1, ?2, ?3)
         ON CONFLICT (key) DO UPDATE SET preview = ?2, fetched_at = ?3",
    )?
    .execute(params![key, preview, cached.fetched_at])?;
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

/// Forget the oldest rows past `max`, of previews and of what scripts read.
pub fn evict(conn: &Connection, max: i64) -> Result<usize> {
    let mut deleted = 0;
    for (table, key, at) in [("previews", "key", "fetched_at"), ("varies", "base", "updated_at")] {
        let count: i64 = conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))?;
        if count > max {
            deleted += conn.execute(&format!("DELETE FROM {table} WHERE {key} IN (SELECT {key} FROM {table} ORDER BY {at} ASC LIMIT ?)"), [count - max])?;
        }
    }
    Ok(deleted)
}

/// Forget everything, and give the space back. Returns how many previews
/// there were.
pub fn clear(conn: &Connection) -> Result<usize> {
    let deleted = conn.execute("DELETE FROM previews", [])?;
    conn.execute("DELETE FROM varies", [])?;
    conn.execute_batch("VACUUM; PRAGMA wal_checkpoint(TRUNCATE);")?;
    Ok(deleted)
}

/// How many rows hold a preview, and how many remember a script having
/// nothing for a page.
pub fn counts(conn: &Connection) -> Result<(i64, i64)> {
    Ok(conn.query_row("SELECT COUNT(preview), COUNT(*) - COUNT(preview) FROM previews", [], |r| Ok((r.get(0)?, r.get(1)?)))?)
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

    #[test]
    fn stores_evicts_and_clears() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("previews.sqlite3");
        let conn = crate::db::open_connection(&path).unwrap();
        // A cache an older npanel left is thrown away.
        conn.execute_batch("CREATE TABLE previews (key TEXT, card TEXT); PRAGMA user_version = 4;").unwrap();
        migrate(&conn).unwrap();
        migrate(&conn).unwrap();

        let preview = Arc::new(Preview { title: Some("A".into()), ..Default::default() });
        put(&conn, "a", &Cached { preview: Some(preview.clone()), fetched_at: 10 }).unwrap();
        put(&conn, "b", &Cached { preview: None, fetched_at: 20 }).unwrap();
        assert_eq!(get(&conn, "a").unwrap().unwrap().preview, Some(preview));
        assert_eq!(get(&conn, "b").unwrap().unwrap().preview, None);
        assert!(get(&conn, "x").unwrap().is_none());
        assert_eq!(counts(&conn).unwrap(), (1, 1));

        put_vary(&conn, "s:x", &["user-agent".into()]).unwrap();
        assert_eq!(get_vary(&conn, "s:x").unwrap(), Some(vec!["user-agent".into()]));
        assert_eq!(get_vary(&conn, "s:y").unwrap(), None);

        assert_eq!(evict(&conn, 1).unwrap(), 1);
        assert!(get(&conn, "a").unwrap().is_none());
        assert!(disk_bytes(&path) > 0);

        assert_eq!(clear(&conn).unwrap(), 1);
        assert_eq!(counts(&conn).unwrap(), (0, 0));
        assert_eq!(get_vary(&conn, "s:x").unwrap(), None);
    }

    #[test]
    fn freshness() {
        let found = Cached { preview: Some(Arc::new(Preview::default())), fetched_at: 0 };
        let empty = Cached { preview: None, fetched_at: 0 };
        assert!(found.fresh(3600, 3599));
        assert!(!found.fresh(3600, 3600));
        assert!(empty.fresh(3600, EMPTY_TTL - 1));
        assert!(!empty.fresh(3600, EMPTY_TTL));
        assert!(!empty.fresh(60, 60));
    }
}
