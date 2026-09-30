//! SQLite: connections, migrations, and the rows npanel reads back.
//!
//! One database file, opened by as many connections as there are threads
//! asking. WAL keeps readers — serving — from ever waiting on a writer, and a
//! busy timeout makes two writers take turns rather than fail.

mod migrations;
pub mod rows;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use parking_lot::Mutex;
use rusqlite::{params, Connection, OpenFlags};

pub use rows::*;

/// Connections kept open between uses.
const MAX_IDLE: usize = 16;

/// A pool of connections to one database file.
#[derive(Clone)]
pub struct Db {
    inner: Arc<Inner>,
}

struct Inner {
    path: PathBuf,
    idle: Mutex<Vec<Connection>>,
}

impl Db {
    /// Open the database at `path`, creating and migrating it if necessary.
    pub fn open(path: &Path, log: Option<&crate::log::Logger>) -> Result<Db> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        let db = Db::connect(path)?;
        db.with(|conn| migrate(conn, log))?;
        Ok(db)
    }

    /// Connect to a database [`Db::open`] has already migrated.
    pub fn connect(path: &Path) -> Result<Db> {
        let db = Db { inner: Arc::new(Inner { path: path.to_path_buf(), idle: Mutex::new(Vec::new()) }) };
        // Fail now, at startup, rather than on the first request.
        let conn = db.checkout()?;
        db.checkin(conn);
        Ok(db)
    }

    /// Run `f` with a connection, on the calling thread.
    pub fn with<T>(&self, f: impl FnOnce(&mut Connection) -> Result<T>) -> Result<T> {
        let mut conn = self.checkout()?;
        let result = f(&mut conn);
        self.checkin(conn);
        result
    }

    /// Run `f` with a connection on the blocking pool, so a query never holds
    /// up a runtime thread.
    pub async fn call<T, F>(&self, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<T> + Send + 'static,
    {
        let db = self.clone();
        tokio::task::spawn_blocking(move || db.with(f)).await.context("database task panicked")?
    }

    /// [`Db::call`], with the query interrupted if it is still running after
    /// `limit` — not just abandoned, which would leave it holding a blocking
    /// thread and a connection for as long as it cared to run. `None` when
    /// it was cut short.
    pub async fn call_within<T, F>(&self, limit: std::time::Duration, f: F) -> Result<Option<T>>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<T> + Send + 'static,
    {
        let db = self.clone();
        let (handle_tx, handle_rx) = tokio::sync::oneshot::channel();
        let mut task = tokio::task::spawn_blocking(move || {
            db.with(|conn| {
                let _ = handle_tx.send(conn.get_interrupt_handle());
                f(conn)
            })
        });

        match tokio::time::timeout(limit, &mut task).await {
            Ok(result) => Ok(Some(result.context("database task panicked")??)),
            Err(_) => {
                if let Ok(handle) = handle_rx.await {
                    handle.interrupt();
                }
                // Whatever it came to, it came to it too late.
                let _ = task.await;
                Ok(None)
            }
        }
    }

    fn checkout(&self) -> Result<Connection> {
        if let Some(conn) = self.inner.idle.lock().pop() {
            return Ok(conn);
        }
        open_connection(&self.inner.path)
    }

    fn checkin(&self, conn: Connection) {
        // A connection left mid-transaction by an error is not one to reuse.
        if !conn.is_autocommit() {
            return;
        }
        let mut idle = self.inner.idle.lock();
        if idle.len() < MAX_IDLE {
            idle.push(conn);
        }
    }
}

/// Open one connection, set up the way every connection must be.
pub fn open_connection(path: &Path) -> Result<Connection> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .with_context(|| format!("opening {}", path.display()))?;

    // First, because the statements below it can block: switching journal
    // modes takes a brief exclusive lock, and without a timeout already in
    // force an open that collides with one fails outright rather than waiting.
    conn.busy_timeout(std::time::Duration::from_millis(5000))?;
    // WAL keeps readers (blob serving) from blocking on the deploy thread's
    // writes, which come through a connection of their own.
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    // Map the database into memory, so point lookups read pages straight out
    // of the OS page cache instead of paying a syscall and a copy for each.
    // A ceiling, not a reservation.
    conn.pragma_update(None, "mmap_size", 268_435_456)?;
    conn.set_prepared_statement_cache_capacity(128);

    Ok(conn)
}

/// Create the schema and apply any migrations the database is missing.
pub fn migrate(conn: &mut Connection, log: Option<&crate::log::Logger>) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
            name TEXT PRIMARY KEY,
            applied_at INTEGER NOT NULL
        )",
    )?;

    let applied: std::collections::HashSet<String> = conn
        .prepare("SELECT name FROM schema_migrations")?
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;

    for (name, statements) in migrations::MIGRATIONS {
        if applied.contains(*name) {
            continue;
        }

        let tx = conn.transaction()?;
        for statement in *statements {
            tx.execute_batch(statement).with_context(|| format!("migration {name}"))?;
        }
        tx.execute("INSERT INTO schema_migrations (name, applied_at) VALUES (?, ?)", params![name, crate::util::now()])?;
        tx.commit()?;

        if let Some(log) = log {
            log.info("migration_applied", crate::fields! { "name" => *name });
        }
    }

    Ok(())
}

#[cfg(test)]
pub mod testing {
    use super::*;

    /// A migrated database in a temporary directory, kept alive with it.
    pub struct TempDb {
        pub db: Db,
        pub dir: tempfile::TempDir,
    }

    pub fn temp_db() -> TempDb {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("npanel.sqlite3"), None).unwrap();
        TempDb { db, dir }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrates_and_cascades() {
        let t = testing::temp_db();
        t.db
            .with(|conn| {
                conn.execute(
                    "INSERT INTO site_events (event_id, pubkey, kind, identifier, created_at, event, status, files_total, files_ready, error, fetched_at, ready_at)
                     VALUES (?, ?, 15128, '', 10, '{}', 'ready', 1, 1, NULL, 10, 11)",
                    params!["d".repeat(64), "a".repeat(64)],
                )?;
                conn.execute(
                    "INSERT INTO site_files (event_id, path, sha256) VALUES (?, '/index.html', ?)",
                    params!["d".repeat(64), "e".repeat(64)],
                )?;
                conn.execute("DELETE FROM site_events", [])?;
                let files: i64 = conn.query_row("SELECT COUNT(*) FROM site_files", [], |r| r.get(0))?;
                assert_eq!(files, 0);
                Ok(())
            })
            .unwrap();

        // Idempotent across opens.
        Db::open(&t.dir.path().join("npanel.sqlite3"), None).unwrap();
    }

    #[tokio::test]
    async fn interrupts_a_query_that_runs_long() {
        let t = testing::temp_db();
        let started = std::time::Instant::now();
        let result = t
            .db
            .call_within(std::time::Duration::from_millis(100), |conn| {
                Ok(conn.query_row(
                    "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n) SELECT count(*) FROM n",
                    [],
                    |r| r.get::<_, i64>(0),
                )?)
            })
            .await
            .unwrap();
        assert_eq!(result, None);
        assert!(started.elapsed() < std::time::Duration::from_secs(5));

        // The connection it ran on goes back into service unharmed.
        let one = t.db.call_within(std::time::Duration::from_secs(5), |conn| Ok(conn.query_row("SELECT 1", [], |r| r.get::<_, i64>(0))?)).await;
        assert_eq!(one.unwrap(), Some(1));
    }

    #[test]
    fn scrubs_ws_relays() {
        let t = testing::temp_db();
        t.db.with(|conn| {
            conn.execute(
                "INSERT INTO settings (key, value, updated_at) VALUES ('relays', ?, 1)",
                params![r#"["ws://plain.example.com","wss://relay.example.com"]"#],
            )?;
            conn.execute("DELETE FROM schema_migrations WHERE name = '019_relays_wss_only'", [])?;
            migrate(conn, None)?;
            let value: String = conn.query_row("SELECT value FROM settings WHERE key = 'relays'", [], |r| r.get(0))?;
            assert_eq!(value, r#"["wss://relay.example.com"]"#);
            Ok(())
        })
        .unwrap();
    }
}
