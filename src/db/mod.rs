//! SQLite: connections, migrations, and the rows npanel reads back.
//!
//! One database file, opened by a bounded pool of connections. WAL keeps
//! readers — serving — from ever waiting on a writer, and a busy timeout
//! makes two writers take turns rather than fail.

mod migrations;
pub mod rows;

use std::ops::{Deref, DerefMut};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use parking_lot::{Condvar, Mutex};
use rusqlite::{params, Connection, OpenFlags};
use tokio::sync::Semaphore;

pub use rows::*;

/// The most connections open at once. A caller past it waits for one to come
/// back rather than opening another: opening one costs far more than any
/// query npanel makes, and more connections than cores buys no concurrency.
/// Without the cap, a burst of queries — every hostname missing the serving
/// cache at once, the moment a build goes live — opened a connection per
/// query and threw most of them away again.
const MAX_CONNECTIONS: usize = 16;

/// A pool of connections to one database file.
#[derive(Clone)]
pub struct Db {
    inner: Arc<Inner>,
}

struct Inner {
    path: PathBuf,
    pool: Mutex<Pool>,
    /// Signalled whenever a connection is returned or a slot freed.
    returned: Condvar,
    /// Async callers queue here, before the blocking pool, so a burst waits
    /// as futures rather than as parked blocking threads.
    permits: Arc<Semaphore>,
}

struct Pool {
    idle: Vec<Connection>,
    /// Connections in existence, idle or checked out.
    open: usize,
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
        let db = Db {
            inner: Arc::new(Inner {
                path: path.to_path_buf(),
                pool: Mutex::new(Pool { idle: Vec::new(), open: 0 }),
                returned: Condvar::new(),
                permits: Arc::new(Semaphore::new(MAX_CONNECTIONS)),
            }),
        };
        // Fail now, at startup, rather than on the first request.
        db.checkout()?;
        Ok(db)
    }

    /// Run `f` with a connection, on the calling thread, waiting for one if
    /// every connection is in use.
    pub fn with<T>(&self, f: impl FnOnce(&mut Connection) -> Result<T>) -> Result<T> {
        f(self.checkout()?.deref_mut())
    }

    /// Run `f` with a connection on the blocking pool, so a query never holds
    /// up a runtime thread.
    pub async fn call<T, F>(&self, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<T> + Send + 'static,
    {
        let permit = self.inner.permits.clone().acquire_owned().await.expect("the semaphore is never closed");
        let db = self.clone();
        tokio::task::spawn_blocking(move || {
            // Held until the work is done, not until the caller stops
            // waiting for it.
            let _permit = permit;
            db.with(f)
        })
        .await
        .context("database task panicked")?
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
        let permit = self.inner.permits.clone().acquire_owned().await.expect("the semaphore is never closed");
        let db = self.clone();
        let (handle_tx, handle_rx) = tokio::sync::oneshot::channel();
        let mut task = tokio::task::spawn_blocking(move || {
            let _permit = permit;
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

    /// An idle connection, a new one if there is room for it, or else the
    /// next one returned.
    fn checkout(&self) -> Result<Lease<'_>> {
        let mut pool = self.inner.pool.lock();
        loop {
            if let Some(conn) = pool.idle.pop() {
                return Ok(Lease { inner: &self.inner, conn: Some(conn) });
            }
            if pool.open < MAX_CONNECTIONS {
                break;
            }
            self.inner.returned.wait(&mut pool);
        }
        // The slot is taken before the open and given back if it fails, so
        // nobody else can take it in between.
        pool.open += 1;
        drop(pool);
        match open_connection(&self.inner.path) {
            Ok(conn) => Ok(Lease { inner: &self.inner, conn: Some(conn) }),
            Err(error) => {
                self.inner.release_slot();
                Err(error)
            }
        }
    }

    #[cfg(test)]
    fn open_connections(&self) -> usize {
        self.inner.pool.lock().open
    }
}

impl Inner {
    fn checkin(&self, conn: Connection) {
        // A connection left mid-transaction — by an error, or by a panic
        // unwinding through it — is not one to reuse.
        if !conn.is_autocommit() {
            drop(conn);
            self.release_slot();
            return;
        }
        self.pool.lock().idle.push(conn);
        self.returned.notify_one();
    }

    fn release_slot(&self) {
        self.pool.lock().open -= 1;
        self.returned.notify_one();
    }
}

/// A checked-out connection, returned to the pool however its user exits —
/// a panic included, since a connection that never came back would hold its
/// slot for good.
struct Lease<'a> {
    inner: &'a Inner,
    conn: Option<Connection>,
}

impl Deref for Lease<'_> {
    type Target = Connection;

    fn deref(&self) -> &Connection {
        self.conn.as_ref().expect("held until dropped")
    }
}

impl DerefMut for Lease<'_> {
    fn deref_mut(&mut self) -> &mut Connection {
        self.conn.as_mut().expect("held until dropped")
    }
}

impl Drop for Lease<'_> {
    fn drop(&mut self) {
        if let Some(conn) = self.conn.take() {
            self.inner.checkin(conn);
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
    apply_migrations(conn, migrations::MIGRATIONS, log)
}

/// Apply whichever of `migrations` a database is missing, each in a
/// transaction of its own, recording each by name. Shared by every database
/// npanel keeps, each with a list of its own.
pub fn apply_migrations(conn: &mut Connection, migrations: &[(&str, &[&str])], log: Option<&crate::log::Logger>) -> Result<()> {
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

    for (name, statements) in migrations {
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

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_burst_waits_for_connections_rather_than_opening_more() {
        let t = testing::temp_db();
        let calls: Vec<_> = (0..256)
            .map(|_| {
                let db = t.db.clone();
                tokio::spawn(async move {
                    let inner = db.clone();
                    db.call(move |conn| {
                        let open = inner.open_connections();
                        std::thread::sleep(std::time::Duration::from_millis(1));
                        Ok((open, conn.query_row("SELECT 1", [], |r| r.get::<_, i64>(0))?))
                    })
                    .await
                })
            })
            .collect();
        for call in calls {
            let (open, one) = call.await.unwrap().unwrap();
            assert!(open <= MAX_CONNECTIONS, "{open} connections open");
            assert_eq!(one, 1);
        }
        assert!(t.db.open_connections() <= MAX_CONNECTIONS);
    }

    #[tokio::test]
    async fn a_panic_gives_its_connection_back() {
        let t = testing::temp_db();
        for i in 0..MAX_CONNECTIONS * 2 {
            let result = t
                .db
                .call(move |conn| -> Result<()> {
                    // Half of them mid-transaction, which can't be reused.
                    if i % 2 == 0 {
                        conn.execute_batch("BEGIN")?;
                    }
                    panic!("boom");
                })
                .await;
            assert!(result.is_err());
        }
        let one = tokio::time::timeout(std::time::Duration::from_secs(5), t.db.call(|conn| Ok(conn.query_row("SELECT 1", [], |r| r.get::<_, i64>(0))?)))
            .await
            .expect("a connection came back");
        assert_eq!(one.unwrap(), 1);
        assert!(t.db.open_connections() <= MAX_CONNECTIONS);
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
