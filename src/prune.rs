//! Pruning: forgetting old versions, and deleting the blobs nothing needs.
//!
//! The deploy runtime prunes once a day and whenever `keep_versions` changes;
//! `npanel prune-blobs` runs the same code on demand. Both halves are safe to
//! run beside deploys and builds, which is what lets it happen unattended.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use parking_lot::Mutex;
use rusqlite::{params, Connection, TransactionBehavior};
use serde::Serialize;
use tokio_util::sync::CancellationToken;

use crate::blobs::BlobStore;
use crate::builds::HostBuilder;
use crate::db::{Db, DeployStatus, Host, HOST_COLUMNS};
use crate::fields;
use crate::hash::BlobHash;
use crate::log::{with_err, Logger};
use crate::settings::Settings;
use crate::sites::{active_event, site_key};

/// How long a blob nothing references yet is left alone. A build stores the
/// files it generates before it writes the rows naming them.
const GRACE_SECONDS: i64 = 60 * 60;

/// Blobs checked and deleted per transaction. Each holds the write lock, so
/// small enough that a deploy waiting on it barely notices.
const SWEEP_BATCH: usize = 500;

/// How long after starting the first prune waits, so it doesn't compete
/// with the resync and rebuilds every start begins with.
const FIRST_RUN: Duration = Duration::from_secs(10 * 60);

const INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

/// What forgetting versions came to.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Forgotten {
    pub versions: usize,
    /// Every merged host. A merged build holds the files of every version in
    /// its chain, so one whose versions were forgotten — now, or by an
    /// earlier run of the command — lets go of them only when it is rebuilt.
    /// Reconciling one that lost nothing rebuilds nothing.
    pub rebuild: Vec<i64>,
}

/// Forget the versions nobody can need: every version of a site no host
/// points at, and — when `keep` is set — every configured site's versions
/// beyond its newest `keep` ready ones.
///
/// Only ready versions are counted, because they are the ones a site can be
/// rolled back to. A version that never got there and is older than the
/// newest ready one can never be deployed — only a site's newest is — so it
/// goes too, while anything newer may still be deploying and is left alone.
/// The version a host serves, its pin, and every version a build names are
/// never forgotten.
///
/// One transaction, so a dry run is the same work rolled back.
pub fn forget_versions(conn: &mut Connection, keep: Option<usize>, dry_run: bool) -> Result<Forgotten> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;

    let hosts: Vec<Host> = tx.prepare(&format!("SELECT {HOST_COLUMNS} FROM hosts"))?.query_map([], Host::from_row)?.collect::<rusqlite::Result<_>>()?;
    let mut protected: HashSet<String> = HashSet::new();
    for host in &hosts {
        protected.extend(host.pinned_event_id.clone());
        if let Some(active) = active_event(&tx, host)? {
            protected.insert(active.event_id);
        }
    }
    // A build serves on without the versions it was made from, but the
    // dashboard describes the site by the version it serves.
    for event_id in tx.prepare("SELECT DISTINCT event_id FROM host_builds")?.query_map([], |r| r.get::<_, String>(0))? {
        protected.insert(event_id?);
    }
    // Disabled hosts count: turning one back on shouldn't find its history gone.
    let configured: HashSet<String> = hosts.iter().map(|host| site_key(&host.address())).collect();

    let sites: Vec<(String, i64, String)> = tx
        .prepare("SELECT DISTINCT pubkey, kind, identifier FROM site_events")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;

    let mut forgotten = Forgotten { versions: 0, rebuild: hosts.iter().filter(|host| host.merge_deployments).map(|host| host.id).collect() };
    for (pubkey, kind, identifier) in sites {
        let key = site_key(&crate::nsite_address::NsiteAddress { pubkey: pubkey.clone(), kind, identifier: identifier.clone() });
        let versions: Vec<(String, DeployStatus)> = tx
            .prepare_cached(
                "SELECT event_id, status FROM site_events WHERE pubkey = ? AND kind = ? AND identifier = ? ORDER BY created_at DESC, event_id DESC",
            )?
            .query_map(params![pubkey, kind, identifier], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;

        let doomed: Vec<String> = if !configured.contains(&key) {
            versions.into_iter().map(|(event_id, _)| event_id).collect()
        } else if let Some(keep) = keep {
            expired(versions, keep)
        } else {
            continue;
        };

        for event_id in doomed.into_iter().filter(|id| !protected.contains(id)) {
            // site_files goes with it: it cascades.
            tx.execute("DELETE FROM deploy_log WHERE event_id = ?", [&event_id])?;
            tx.execute("DELETE FROM site_events WHERE event_id = ?", [&event_id])?;
            forgotten.versions += 1;
        }
    }

    if !dry_run {
        tx.commit()?;
    }
    Ok(forgotten)
}

/// A configured site's versions, newest first, that `keep` leaves no room for.
fn expired(versions: Vec<(String, DeployStatus)>, keep: usize) -> Vec<String> {
    // A site with nothing ready yet has nothing to roll back to, and its
    // failures are the only account of why.
    let Some(newest_ready) = versions.iter().position(|(_, status)| *status == DeployStatus::Ready) else {
        return Vec::new();
    };
    let mut ready = 0;
    versions
        .into_iter()
        .skip(newest_ready)
        .filter(|(_, status)| {
            if *status != DeployStatus::Ready {
                return true;
            }
            ready += 1;
            ready > keep
        })
        .map(|(event_id, _)| event_id)
        .collect()
}

/// What deleting blobs came to.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Swept {
    pub blobs: usize,
    /// Disk freed, compressed copies included.
    pub bytes: u64,
}

impl std::ops::AddAssign for Swept {
    fn add_assign(&mut self, other: Swept) {
        self.blobs += other.blobs;
        self.bytes += other.bytes;
    }
}

/// Delete whichever of these blobs nothing references.
///
/// The question is asked again here, per blob, inside a write transaction,
/// and the file is gone before it commits — which is what makes it safe
/// beside a deploy. A manifest that comes to need a blob either committed
/// first, and is seen here, or waits on this transaction and finds the blob
/// missing, and downloads it again. A deploy can never be marked ready on a
/// blob this has just deleted.
pub fn sweep_batch(conn: &mut Connection, blobs: &BlobStore, hashes: &[BlobHash], dry_run: bool) -> Result<Swept> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let recent = crate::util::now() - GRACE_SECONDS;
    let mut swept = Swept::default();
    {
        let mut wanted = tx.prepare_cached(
            "SELECT EXISTS (SELECT 1 FROM site_files WHERE sha256 = ?1)
                 OR EXISTS (SELECT 1 FROM host_build_files WHERE sha256 = ?1)
                 OR EXISTS (SELECT 1 FROM blobs WHERE sha256 = ?1 AND seen_at > ?2)",
        )?;
        let mut forget = tx.prepare_cached("DELETE FROM blobs WHERE sha256 = ?")?;
        for &sha256 in hashes {
            if wanted.query_row(params![sha256, recent], |r| r.get::<_, bool>(0))? {
                continue;
            }
            let Some(variants) = blobs.variants(sha256) else { continue };
            if !dry_run {
                blobs.delete(sha256);
                forget.execute([sha256])?;
            }
            swept.blobs += 1;
            swept.bytes += variants.on_disk();
        }
    }
    tx.commit()?;
    Ok(swept)
}

/// Delete every stored blob nothing references, a batch at a time. Listing
/// the store is the slow part, and holds no connection while it runs.
pub async fn sweep(db: &Db, blobs: &BlobStore, cancel: &CancellationToken) -> Result<Swept> {
    let store = blobs.clone();
    let hashes = tokio::task::spawn_blocking(move || store.list()).await?;
    let mut total = Swept::default();
    for batch in hashes.chunks(SWEEP_BATCH) {
        if cancel.is_cancelled() {
            break;
        }
        let (store, batch) = (blobs.clone(), batch.to_vec());
        total += db.call(move |conn| sweep_batch(conn, &store, &batch, false)).await?;
    }
    Ok(total)
}

/// The same sweep, for the command line.
pub fn sweep_now(db: &Db, blobs: &BlobStore, dry_run: bool) -> Result<Swept> {
    let mut total = Swept::default();
    for batch in blobs.list().chunks(SWEEP_BATCH) {
        total += db.with(|conn| sweep_batch(conn, blobs, batch, dry_run))?;
    }
    Ok(total)
}

/// How the last automatic prune went.
#[derive(Clone, Debug, Serialize)]
pub struct PruneReport {
    pub finished_at: i64,
    pub versions: usize,
    pub blobs: usize,
    pub bytes: u64,
}

/// The last report, kept where the API can read it. It outlives a deploy
/// runtime that restarts.
pub type LastPrune = Arc<Mutex<Option<PruneReport>>>;

pub struct PrunerDeps {
    pub db: Db,
    pub blobs: BlobStore,
    pub settings: Arc<Settings>,
    /// Told which merged hosts lost versions, so their builds let go of them.
    pub builder: HostBuilder,
    pub log: Logger,
    pub last: LastPrune,
}

/// The task that prunes on a schedule, on the deploy runtime.
#[derive(Clone)]
pub struct Pruner {
    inner: Arc<Inner>,
}

struct Inner {
    deps: PrunerDeps,
    task: Mutex<Option<(CancellationToken, tokio::task::JoinHandle<()>)>>,
}

impl Pruner {
    pub fn new(deps: PrunerDeps) -> Pruner {
        Pruner { inner: Arc::new(Inner { deps, task: Mutex::new(None) }) }
    }

    pub fn start(&self) {
        let cancel = CancellationToken::new();
        let handle = tokio::spawn(self.clone().run(cancel.clone()));
        if let Some((previous, _)) = self.inner.task.lock().replace((cancel, handle)) {
            previous.cancel();
        }
    }

    /// Stop, letting a prune under way finish the batch it is on.
    pub async fn stop(&self) {
        let task = self.inner.task.lock().take();
        if let Some((cancel, handle)) = task {
            cancel.cancel();
            let _ = handle.await;
        }
    }

    async fn run(self, cancel: CancellationToken) {
        let mut changes = self.inner.deps.settings.subscribe();
        let mut keep = changes.borrow_and_update().keep_versions;
        let mut next = tokio::time::Instant::now() + FIRST_RUN;

        loop {
            tokio::select! {
                () = cancel.cancelled() => return,
                () = tokio::time::sleep_until(next) => {}
                changed = changes.changed() => {
                    if changed.is_err() {
                        return;
                    }
                    let now = changes.borrow_and_update().keep_versions;
                    if now == keep {
                        continue;
                    }
                    keep = now;
                }
            }

            if let Err(error) = self.prune(&cancel).await {
                self.inner.deps.log.error("prune_failed", with_err(fields! {}, error));
            }
            next = tokio::time::Instant::now() + INTERVAL;
        }
    }

    async fn prune(&self, cancel: &CancellationToken) -> Result<()> {
        let deps = &self.inner.deps;
        let keep = usize::try_from(deps.settings.get().keep_versions).ok().filter(|keep| *keep > 0);

        let forgotten = deps.db.call(move |conn| forget_versions(conn, keep, false)).await?;
        if !forgotten.rebuild.is_empty() {
            deps.builder.hosts_changed(Some(&forgotten.rebuild));
        }
        let swept = sweep(&deps.db, &deps.blobs, cancel).await?;

        let report = PruneReport { finished_at: crate::util::now(), versions: forgotten.versions, blobs: swept.blobs, bytes: swept.bytes };
        deps.log.info("prune_finished", fields! { "versions" => report.versions, "blobs" => report.blobs, "bytes" => report.bytes });
        *deps.last.lock() = Some(report);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nsite_address::NsiteAddress;
    use crate::sites::testing::{insert_build, insert_host, insert_version};

    fn site(pubkey: char) -> NsiteAddress {
        NsiteAddress { pubkey: pubkey.to_string().repeat(64), kind: 15128, identifier: String::new() }
    }

    fn id(n: u32) -> String {
        format!("{n:064x}")
    }

    fn set_status(conn: &Connection, event_id: &str, status: DeployStatus) {
        conn.execute("UPDATE site_events SET status = ? WHERE event_id = ?", params![status, event_id]).unwrap();
    }

    fn remaining(conn: &Connection) -> Vec<String> {
        conn.prepare("SELECT event_id FROM site_events ORDER BY created_at").unwrap().query_map([], |r| r.get(0)).unwrap().map(Result::unwrap).collect()
    }

    #[test]
    fn counts_only_ready_versions() {
        let ids = |ns: &[u32]| ns.iter().map(|n| id(*n)).collect::<Vec<_>>();
        let version = |n: u32, status| (id(n), status);
        let (ready, failed, pending) = (DeployStatus::Ready, DeployStatus::Failed, DeployStatus::Pending);

        // Newest first.
        let versions = vec![version(9, pending), version(8, ready), version(7, failed), version(6, ready), version(5, ready), version(4, failed)];
        assert_eq!(expired(versions.clone(), 2), ids(&[7, 5, 4]));
        assert_eq!(expired(versions, 10), ids(&[7, 4]));
        // Nothing ready, nothing expires.
        assert!(expired(vec![version(2, failed), version(1, failed)], 1).is_empty());
    }

    #[test]
    fn forgets_versions() {
        let t = crate::db::testing::temp_db();
        t.db.with(|conn| {
            let (a, b, orphan) = (site('a'), site('b'), site('c'));
            let host = insert_host(conn, "a.example.com", &a);
            insert_host(conn, "b.example.com", &b);
            for n in 1..=5 {
                insert_version(conn, &id(n), &a, n.into(), &[]);
            }
            // Pinned to the oldest, with the newest still deploying and an
            // old failure behind it.
            conn.execute("UPDATE hosts SET pinned_event_id = ? WHERE id = ?", params![id(1), host])?;
            insert_version(conn, &id(6), &a, 6, &[]);
            set_status(conn, &id(6), DeployStatus::Pending);
            set_status(conn, &id(3), DeployStatus::Failed);
            insert_build(conn, host, &id(2), &[]);

            insert_version(conn, &id(10), &b, 10, &[]);
            insert_version(conn, &id(20), &orphan, 20, &[]);
            insert_version(conn, &id(21), &orphan, 21, &[]);

            // Without a limit, only the site no host points at goes.
            assert_eq!(forget_versions(conn, None, true)?.versions, 2);
            assert_eq!(remaining(conn).len(), 9);

            let forgotten = forget_versions(conn, Some(1), false)?;
            // 5 is the newest ready; 3 failed; 4 is past the limit; 2 has a
            // build and 1 is pinned; 6 may still be deploying.
            assert_eq!(remaining(conn), [id(1), id(2), id(5), id(6), id(10)]);
            assert_eq!(forgotten, Forgotten { versions: 4, rebuild: vec![] });
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn rebuilds_merged_hosts() {
        let t = crate::db::testing::temp_db();
        t.db.with(|conn| {
            let a = site('a');
            let host = insert_host(conn, "a.example.com", &a);
            conn.execute("UPDATE hosts SET merge_deployments = 1 WHERE id = ?", [host])?;
            for n in 1..=3 {
                insert_version(conn, &id(n), &a, n.into(), &[]);
            }
            insert_host(conn, "b.example.com", &site('b'));
            assert_eq!(forget_versions(conn, Some(2), false)?, Forgotten { versions: 1, rebuild: vec![host] });
            // Even when this run forgot nothing: an earlier one may have.
            assert_eq!(forget_versions(conn, Some(2), false)?, Forgotten { versions: 0, rebuild: vec![host] });
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn sweeps_only_what_nothing_needs() {
        let t = crate::db::testing::temp_db();
        let blobs = BlobStore::new(&t.dir.path().join("blobs"));
        let put = |bytes: &[u8]| {
            let sha = BlobHash::of(bytes);
            blobs.put(sha, bytes).unwrap();
            sha
        };
        let (versioned, built, fresh, stale, unrecorded) = (put(b"versioned"), put(b"built"), put(b"fresh"), put(b"stale"), put(b"unrecorded"));
        std::fs::write(blobs.variant_path(stale, crate::blobs::Encoding::Br), b"br").unwrap();

        t.db.with(|conn| {
            let a = site('a');
            let host = insert_host(conn, "a.example.com", &a);
            insert_version(conn, &id(1), &a, 1, &[("/index.html", &versioned.to_string())]);
            insert_build(conn, host, &id(1), &[("/sitemap.xml", &built.to_string())]);
            let now = crate::util::now();
            for (sha, seen) in [(fresh, now), (stale, now - GRACE_SECONDS - 1)] {
                conn.execute("INSERT INTO blobs (sha256, size, seen_at) VALUES (?, 5, ?)", params![sha, seen])?;
            }
            Ok(())
        })
        .unwrap();

        let dry = sweep_now(&t.db, &blobs, true).unwrap();
        assert_eq!(dry, Swept { blobs: 2, bytes: 5 + 2 + 10 });
        assert!(blobs.has(stale));

        assert_eq!(sweep_now(&t.db, &blobs, false).unwrap(), dry);
        for sha in [versioned, built, fresh] {
            assert!(blobs.has(sha));
        }
        for sha in [stale, unrecorded] {
            assert!(!blobs.has(sha));
        }
        assert!(blobs.variants(stale).is_none() && !blobs.variant_path(stale, crate::blobs::Encoding::Br).exists());
        let rows: i64 = t.db.with(|conn| Ok(conn.query_row("SELECT COUNT(*) FROM blobs WHERE sha256 = ?", [stale], |r| r.get(0))?)).unwrap();
        assert_eq!(rows, 0);
    }

    #[test]
    fn keeps_a_blob_that_gains_a_reference_after_listing() {
        let t = crate::db::testing::temp_db();
        let blobs = BlobStore::new(&t.dir.path().join("blobs"));
        let sha = BlobHash::of(b"reused");
        blobs.put(sha, b"reused").unwrap();
        let listed = blobs.list();

        // A manifest naming it lands between the listing and the batch.
        t.db.with(|conn| {
            insert_version(conn, &id(1), &site('a'), 1, &[("/index.html", &sha.to_string())]);
            Ok(())
        })
        .unwrap();

        let swept = t.db.with(|conn| sweep_batch(conn, &blobs, &listed, false)).unwrap();
        assert_eq!(swept, Swept::default());
        assert!(blobs.has(sha));
    }
}
