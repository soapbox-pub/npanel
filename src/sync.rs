//! The sync worker: subscribe, resync, deploy.
//!
//! Three things drive it:
//!
//! 1. A live subscription to the configured relays for new manifest events.
//! 2. A periodic full re-fetch, so a manifest missed on the wire (relay
//!    downtime, a restart) is still picked up.
//! 3. A blob queue that downloads every file of a new version before that
//!    version is allowed to serve — which it does once the builder has made
//!    each host serving it a build of it.
//!
//! Every step also writes to the `deploy_log` table, which the dashboard shows
//! per version — a failed deploy explains itself without touching the server.

use std::collections::{HashMap, HashSet};
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use futures_util::stream::FuturesUnordered;
use futures_util::future::BoxFuture;
use futures_util::{FutureExt, StreamExt};
use parking_lot::Mutex;
use rusqlite::{params, OptionalExtension};
use tokio::sync::mpsc;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::blobs::BlobStore;
use crate::builds::HostBuilder;
use crate::db::{DeployStatus, Db};
use crate::fields;
use crate::hash::BlobHash;
use crate::log::{with_err, Fields, Logger};
use crate::manifest::{is_site_manifest, manifest_address};
use crate::mime::is_compressible;
use crate::nostr::pool::Filter;
use crate::nostr::service::{blob_url, dedupe, NostrSource};
use crate::nostr::Event;
use crate::nsite_address::{NsiteAddress, SITE_KINDS};
use crate::settings::Settings;
use crate::sites::{
    list_configured_sites, newest_event, prune_deploy_log, record_blob, record_variant_sizes, site_event, store_manifest,
    write_deploy_log,
};

/// Blobs downloaded in parallel within one deploy.
const BLOB_CONCURRENCY: usize = 4;

/// Wait before a second attempt at a deploy. It doubles after that.
pub const RETRY_BACKOFF: Duration = Duration::from_secs(5);

pub struct SyncWorkerDeps {
    pub db: Db,
    pub blobs: BlobStore,
    pub nostr: Arc<dyn NostrSource>,
    pub settings: Arc<Settings>,
    /// Told when a version becomes ready, so the hosts serving it are rebuilt.
    pub builder: HostBuilder,
    pub log: Logger,
    pub retry_backoff: Duration,
}

/// A version waiting its turn to deploy.
struct QueuedDeploy {
    event_id: String,
    /// When it may next be attempted, for a retry's backoff.
    after: Instant,
}

/// The two tasks a running worker has, and how to reach them.
struct Tasks {
    /// Versions to deploy, each with a fresh run of attempts.
    deploys: mpsc::UnboundedSender<String>,
    /// Requests for another resync pass.
    resyncs: mpsc::UnboundedSender<()>,
    handles: Vec<tokio::task::JoinHandle<()>>,
}

#[derive(Default)]
struct State {
    running: bool,
    subscription: Option<CancellationToken>,
    tasks: Option<Tasks>,
}

#[derive(Clone)]
pub struct SyncWorker {
    inner: Arc<Inner>,
}

struct Inner {
    deps: SyncWorkerDeps,
    state: Mutex<State>,
    /// Cancels downloads and relay lookups when the worker stops.
    cancel: Mutex<CancellationToken>,
}

/// What a deploy of one version came to, short of crashing.
enum Outcome {
    Ready,
    Failed(String),
    Refused { reason: String, msg: &'static str, fields: Fields },
    Interrupted(usize),
}

impl SyncWorker {
    pub fn new(deps: SyncWorkerDeps) -> SyncWorker {
        SyncWorker { inner: Arc::new(Inner { deps, state: Mutex::new(State::default()), cancel: Mutex::new(CancellationToken::new()) }) }
    }

    fn deps(&self) -> &SyncWorkerDeps {
        &self.inner.deps
    }

    fn running(&self) -> bool {
        self.inner.state.lock().running
    }

    fn cancel_token(&self) -> CancellationToken {
        self.inner.cancel.lock().clone()
    }

    async fn db<T: Send + 'static>(&self, f: impl FnOnce(&mut rusqlite::Connection) -> Result<T> + Send + 'static) -> Result<T> {
        self.deps().db.call(f).await
    }

    async fn deploy_log(&self, event_id: &str, level: &'static str, msg: &'static str, fields: Fields) {
        let event_id = event_id.to_string();
        let _ = self.db(move |conn| Ok(write_deploy_log(conn, &event_id, level, msg, fields)?)).await;
    }

    /// Begin syncing: resync everything, subscribe, and deploy what turns up.
    pub async fn start(&self) {
        {
            let mut state = self.inner.state.lock();
            if state.running {
                return;
            }
            state.running = true;
        }
        // A restarted worker needs a fresh token, or every fetch it makes is
        // cancelled before it begins.
        *self.inner.cancel.lock() = CancellationToken::new();

        // Temp files can only be leftovers from a download interrupted by a crash.
        let blobs = self.deps().blobs.clone();
        let _ = tokio::task::spawn_blocking(move || blobs.clear_temp()).await;

        let (deploys, deploy_requests) = mpsc::unbounded_channel();
        let (resyncs, resync_requests) = mpsc::unbounded_channel();
        let handles = vec![tokio::spawn(self.clone().work_deploys(deploy_requests)), tokio::spawn(self.clone().work_resyncs(resync_requests))];
        self.inner.state.lock().tasks = Some(Tasks { deploys, resyncs, handles });

        self.restart_subscription().await;
        self.deps().log.info("sync_started", fields! { "relays" => self.deps().nostr.relays().len() });
    }

    /// Stop syncing and release relay connections. Resolves once the deploy
    /// in progress, if any, has left itself resumable.
    pub async fn stop(&self) {
        let tasks = {
            let mut state = self.inner.state.lock();
            if !state.running {
                return;
            }
            state.running = false;
            if let Some(subscription) = state.subscription.take() {
                subscription.cancel();
            }
            state.tasks.take()
        };
        self.inner.cancel.lock().cancel();

        // Dropping the senders is what ends each task, once it is between
        // pieces of work — so before waiting on them, not after.
        if let Some(Tasks { deploys, resyncs, handles }) = tasks {
            drop((deploys, resyncs));
            futures_util::future::join_all(handles).await;
        }
        self.deps().log.info("sync_stopped", fields! {});
    }

    /// Called when hosts are added, retargeted, or removed. Restarts the live
    /// subscription and fetches anything newly configured right away.
    pub async fn hosts_changed(&self) {
        if !self.running() {
            return;
        }
        self.restart_subscription().await;
        self.request_resync();
    }

    /// Ask for a pass over every site. A pass under way when this is asked
    /// may already be past whatever prompted it, so one more follows it.
    fn request_resync(&self) {
        if let Some(tasks) = &self.inner.state.lock().tasks {
            let _ = tasks.resyncs.send(());
        }
    }

    /// Fetch a site's newest manifest now, and deploy it if it isn't ready.
    pub async fn sync_site(&self, address: &NsiteAddress) -> Result<()> {
        let cancel = self.cancel_token();
        let stored = match self.deps().nostr.fetch_manifest(address, &cancel).await {
            Some(event) => self.ingest(event).await?,
            None => {
                self.deps().log.debug(
                    "manifest_not_found",
                    fields! { "pubkey" => address.pubkey.as_str(), "identifier" => address.identifier.as_str() },
                );
                false
            }
        };
        // A new version is queued already, and queueing it twice would reset
        // the attempts its first deploy may have spent by now.
        if stored {
            return Ok(());
        }

        // Pick up a version that was stored earlier but never finished deploying.
        let address = address.clone();
        let newest = self.db(move |conn| Ok(newest_event(conn, &address)?)).await?;
        if let Some(newest) = newest.filter(|n| n.status != DeployStatus::Ready) {
            self.enqueue(&newest.event_id);
        }
        Ok(())
    }

    async fn run_resync(&self) {
        let sites = match self.db(|conn| Ok(list_configured_sites(conn)?)).await {
            Ok(sites) => sites,
            Err(error) => {
                self.deps().log.error("resync_failed", with_err(fields! {}, error));
                return;
            }
        };
        self.deps().log.debug("resync_started", fields! { "sites" => sites.len() });

        for site in sites {
            if !self.running() {
                return;
            }
            if let Err(error) = self.sync_site(&site).await {
                self.deps().log.warn(
                    "resync_site_failed",
                    with_err(fields! { "pubkey" => site.pubkey.as_str(), "identifier" => site.identifier.as_str() }, error),
                );
            }
        }
    }

    /// Store a manifest event and queue it for deployment. Whether it was
    /// new, and for a site someone configured.
    pub async fn ingest(&self, event: Event) -> Result<bool> {
        if !is_site_manifest(&event) {
            return Ok(false);
        }

        let address = manifest_address(&event);
        let log = self.deps().log;
        let stored = {
            let event = event.clone();
            self.db(move |conn| {
                let configured: Option<i64> = conn
                    .prepare_cached("SELECT id FROM hosts WHERE pubkey = ? AND kind = ? AND identifier = ? AND enabled = 1")?
                    .query_row(params![address.pubkey, address.kind, address.identifier], |row| row.get(0))
                    .optional()?;
                if configured.is_none() {
                    log.debug("manifest_ignored", fields! { "pubkey" => event.pubkey.as_str(), "kind" => event.kind });
                    return Ok(false);
                }
                Ok(store_manifest(conn, &event)?)
            })
            .await?
        };
        if !stored {
            return Ok(false);
        }

        let address = manifest_address(&event);
        log.info(
            "manifest_received",
            fields! {
                "event_id" => event.id.as_str(),
                "pubkey" => event.pubkey.as_str(),
                "identifier" => address.identifier.as_str(),
                "created_at" => event.created_at,
            },
        );
        self.deploy_log(&event.id, "info", "manifest_received", fields! { "created_at" => event.created_at }).await;

        self.enqueue(&event.id);
        Ok(true)
    }

    /// Queue every version that isn't ready yet.
    async fn retry_incomplete(&self) {
        let pending = self
            .db(|conn| {
                let mut ids = Vec::new();
                for site in list_configured_sites(conn)? {
                    if let Some(newest) = newest_event(conn, &site)?.filter(|n| n.status != DeployStatus::Ready) {
                        ids.push(newest.event_id);
                    }
                }
                Ok(ids)
            })
            .await
            .unwrap_or_else(|error| {
                self.deps().log.error("retry_incomplete_failed", with_err(fields! {}, error));
                Vec::new()
            });

        for event_id in pending {
            self.enqueue(&event_id);
        }
    }

    async fn restart_subscription(&self) {
        let token = {
            let mut state = self.inner.state.lock();
            if let Some(previous) = state.subscription.take() {
                previous.cancel();
            }
            if !state.running {
                return;
            }
            let token = CancellationToken::new();
            state.subscription = Some(token.clone());
            token
        };

        let sites = match self.db(|conn| Ok(list_configured_sites(conn)?)).await {
            Ok(sites) => sites,
            Err(error) => {
                self.deps().log.error("subscription_failed", with_err(fields! {}, error));
                return;
            }
        };
        if sites.is_empty() {
            self.deps().log.debug("subscription_idle", fields! { "reason" => "no_hosts" });
            return;
        }

        let worker = self.clone();
        tokio::spawn(async move { worker.run_subscription(sites, token).await });
    }

    async fn run_subscription(&self, sites: Vec<NsiteAddress>, cancel: CancellationToken) {
        let authors = dedupe(sites.iter().map(|site| site.pubkey.clone()));
        let mut kinds: Vec<i64> = Vec::new();
        for site in &sites {
            if SITE_KINDS.contains(&site.kind) && !kinds.contains(&site.kind) {
                kinds.push(site.kind);
            }
        }

        // Watch the authors' own write relays too — that's where they publish.
        let hints = self.deps().nostr.authors_relays(&authors, &cancel).await;
        let hints: Vec<String> = authors.iter().flat_map(|a| hints.get(a).cloned().unwrap_or_default()).collect();
        let relays = dedupe(self.deps().nostr.relays().into_iter().chain(hints));
        if cancel.is_cancelled() {
            return;
        }

        self.deps().log.info("subscription_started", fields! { "authors" => authors.len(), "relays" => relays.len() });

        let filter = Filter { kinds, authors, since: Some(crate::util::now()), ..Default::default() };
        let mut events = self.deps().nostr.subscribe(vec![filter], &relays, cancel.clone());
        while let Some(event) = events.recv().await {
            let id = event.id.clone();
            if let Err(error) = self.ingest(event).await {
                self.deps().log.error("ingest_failed", with_err(fields! { "event_id" => id }, error));
            }
        }
    }

    /// Queue a version with a fresh run of attempts: everything that calls
    /// this is a reason to believe a deploy that gave up could now succeed.
    fn enqueue(&self, event_id: &str) {
        if let Some(tasks) = &self.inner.state.lock().tasks {
            let _ = tasks.deploys.send(event_id.to_string());
        }
    }

    /// Deploy queued versions one at a time, until the worker stops. A version
    /// waiting out a retry's backoff holds up nothing: whatever is ready goes
    /// first, and only an otherwise empty queue actually waits.
    ///
    /// The queue and the attempts spent belong to this task alone, so taking
    /// work and going to sleep can't race a request arriving in between.
    async fn work_deploys(self, mut requests: mpsc::UnboundedReceiver<String>) {
        let mut queue: Vec<QueuedDeploy> = Vec::new();
        let mut attempts: HashMap<String, u32> = HashMap::new();
        let admit = |queue: &mut Vec<QueuedDeploy>, attempts: &mut HashMap<String, u32>, event_id: String| {
            attempts.remove(&event_id);
            let now = Instant::now();
            match queue.iter_mut().find(|entry| entry.event_id == event_id) {
                // A fresh reason to deploy overtakes a retry waiting out its backoff.
                Some(pending) => pending.after = pending.after.min(now),
                None => queue.push(QueuedDeploy { event_id, after: now }),
            }
        };

        loop {
            while let Ok(event_id) = requests.try_recv() {
                admit(&mut queue, &mut attempts, event_id);
            }
            if !self.running() {
                break;
            }

            let now = Instant::now();
            if let Some(index) = queue.iter().position(|entry| entry.after <= now) {
                let event_id = queue.remove(index).event_id;
                let outcome = match AssertUnwindSafe(self.deploy(&event_id)).catch_unwind().await {
                    Ok(outcome) => outcome,
                    Err(_) => Err(anyhow::anyhow!("the deploy panicked")),
                };
                match AssertUnwindSafe(self.settle(&event_id, outcome, &mut attempts)).catch_unwind().await {
                    Ok(Some(after)) => queue.push(QueuedDeploy { event_id, after }),
                    Ok(None) => {}
                    Err(_) => self.deps().log.error("deploy_settle_panicked", fields! { "event_id" => event_id }),
                }
                continue;
            }

            let soonest = queue.iter().map(|entry| entry.after).min();
            tokio::select! {
                request = requests.recv() => match request {
                    Some(event_id) => admit(&mut queue, &mut attempts, event_id),
                    None => break,
                },
                _ = sleep_until(soonest) => {}
            }
        }
    }

    /// Pass over every site on the resync timer and whenever asked, and
    /// reconfigure as the settings change. A pass runs alongside the rest, so
    /// a relay list saved mid-pass takes effect at once.
    async fn work_resyncs(self, mut requests: mpsc::UnboundedReceiver<()>) {
        let mut changes = self.deps().settings.subscribe();
        let mut seen = changes.borrow_and_update().clone();
        let mut timer = resync_timer(seen.resync_interval, self.deps().log);
        // Every start is a reason to look: whatever was missed while stopped.
        let mut wanted = true;
        let mut pass: Option<BoxFuture<'static, ()>> = None;

        loop {
            if wanted && pass.is_none() && self.running() {
                wanted = false;
                let worker = self.clone();
                pass = Some(Box::pin(async move { worker.run_resync().await }));
            }

            tokio::select! {
                () = async { pass.as_mut().expect("a pass").await }, if pass.is_some() => pass = None,
                request = requests.recv() => match request {
                    Some(()) => wanted = true,
                    None => break,
                },
                _ = timer.tick() => wanted = true,
                changed = changes.changed() => {
                    if changed.is_err() {
                        break;
                    }
                    let now = changes.borrow_and_update().clone();
                    if now.relays != seen.relays {
                        // Connections to relays that were removed close themselves
                        // once the restarted subscription stops asking them anything.
                        self.deps().nostr.clear_caches();
                        self.restart_subscription().await;
                    }
                    if now.resync_interval != seen.resync_interval {
                        timer = resync_timer(now.resync_interval, self.deps().log);
                    }
                    // New servers or a bigger limit may be exactly what a failed
                    // deploy needed.
                    if now.blossom_fallback_servers != seen.blossom_fallback_servers
                        || now.max_file_size != seen.max_file_size
                        || now.max_site_size != seen.max_site_size
                        || now.max_site_files != seen.max_site_files
                    {
                        self.retry_incomplete().await;
                    }
                    seen = now;
                }
            }
        }

        // A pass in progress is abandoned where it is: the cancelled token has
        // already cut its lookups short.
        drop(pass);
    }

    /// What a deploy came to, recorded. Returns when to try it again, if it
    /// is to be.
    async fn settle(&self, event_id: &str, outcome: Result<Option<Outcome>>, attempts: &mut HashMap<String, u32>) -> Option<Instant> {
        match outcome {
            Ok(None) => None,
            Ok(Some(Outcome::Ready)) => {
                attempts.remove(event_id);
                self.finish(event_id, DeployStatus::Ready, None).await;
                None
            }
            Ok(Some(Outcome::Failed(reason))) => self.fail(event_id, &reason, attempts).await,
            Ok(Some(Outcome::Refused { reason, msg, fields })) => {
                attempts.remove(event_id);
                self.refuse(event_id, &reason, msg, fields).await;
                None
            }
            Ok(Some(Outcome::Interrupted(remaining))) => {
                attempts.remove(event_id);
                self.interrupt(event_id, remaining).await;
                None
            }
            Err(error) => {
                if !self.running() {
                    // A consequence of shutting down, not of the version.
                    attempts.remove(event_id);
                    self.interrupt(event_id, 0).await;
                    return None;
                }
                self.deps().log.error("deploy_crashed", with_err(fields! { "event_id" => event_id }, &error));
                self.fail(event_id, &format!("Deploy failed: {error}"), attempts).await
            }
        }
    }

    /// Download every blob a version needs. Only when this completes does the
    /// version become servable, so a host is never left half-updated.
    async fn deploy(&self, event_id: &str) -> Result<Option<Outcome>> {
        let deps = self.deps();
        let id = event_id.to_string();
        let Some(row) = self.db(move |conn| Ok(site_event(conn, &id)?)).await? else {
            return Ok(None);
        };
        if row.status == DeployStatus::Ready {
            return Ok(None);
        }

        // Only the newest version of a site is worth downloading: an author who
        // publishes faster than npanel deploys would otherwise have every
        // version fetched in full, on the gateway's disk rather than their own.
        let address = row.address();
        let newest = self.db(move |conn| Ok(newest_event(conn, &address)?)).await?;
        if let Some(newest) = newest.filter(|n| n.created_at > row.created_at) {
            return Ok(Some(Outcome::Refused {
                reason: "A newer version was published before this one was deployed, so npanel deployed that one instead.".into(),
                msg: "deploy_superseded",
                fields: fields! { "newer" => newest.event_id },
            }));
        }

        let settings = deps.settings.get();
        if row.files_total > settings.max_site_files {
            return Ok(Some(Outcome::Refused {
                reason: format!(
                    "This version has {} files, more than the {} a site may have here. An admin can raise the limit under Settings.",
                    row.files_total, settings.max_site_files
                ),
                msg: "too_many_files",
                fields: fields! { "files" => row.files_total, "limit" => settings.max_site_files },
            }));
        }

        let event: Event = serde_json::from_str(&row.event)?;

        let id = event_id.to_string();
        let files: Vec<(BlobHash, String)> = self
            .db(move |conn| {
                conn.execute("UPDATE site_events SET status = ?, error = NULL WHERE event_id = ?", params![DeployStatus::FetchingBlobs, id])?;
                Ok(conn
                    .prepare("SELECT sha256, path FROM site_files WHERE event_id = ?")?
                    .query_map([&id], |r| Ok((r.get(0)?, r.get(1)?)))?
                    .collect::<rusqlite::Result<_>>()?)
            })
            .await?;

        // Paths come along because the store is keyed by hash and so can't
        // tell markup from a JPEG, while whether a blob is worth compressing
        // is a question about the file it is served as.
        let compressible: HashSet<BlobHash> = files.iter().filter(|(_, path)| is_compressible(path)).map(|(sha, _)| *sha).collect();
        let hashes = dedupe(files.iter().map(|(sha, _)| *sha));

        self.deploy_log(event_id, "info", "deploy_started", fields! { "files" => row.files_total, "blobs" => hashes.len() }).await;

        // Bytes of this version already on disk. Blobs other sites share count
        // too: the limit is on how big a version is, not on what it costs to add.
        // Looked up and recorded in one pass on the blocking pool, since a
        // version already on disk can be tens of thousands of stats.
        let (blobs, all) = (deps.blobs.clone(), hashes.clone());
        let (present, missing, stored) = self
            .db(move |conn| {
                let (mut present, mut missing, mut stored) = (Vec::new(), Vec::new(), 0u64);
                let tx = conn.transaction()?;
                for sha256 in all {
                    match blobs.size(sha256) {
                        None => missing.push(sha256),
                        Some(size) => {
                            record_blob(&tx, sha256, size)?;
                            stored += size;
                            present.push(sha256);
                        }
                    }
                }
                tx.commit()?;
                Ok((present, missing, stored))
            })
            .await?;

        // Already on disk, but not necessarily compressed.
        for &sha256 in present.iter().filter(|sha| compressible.contains(*sha)) {
            self.compress(event_id, sha256).await;
        }
        let blobs = deps.blobs.clone();
        self.db(move |conn| {
            let tx = conn.transaction()?;
            for &sha256 in &present {
                if let Some(variants) = blobs.variants(sha256) {
                    record_variant_sizes(&tx, sha256, variants)?;
                }
            }
            tx.commit()?;
            Ok(())
        })
        .await?;

        self.set_progress(event_id, (hashes.len() - missing.len()) as i64).await?;

        let max_size = settings.max_site_size as u64;
        if stored > max_size {
            return Ok(Some(oversized(max_size, stored)));
        }

        if missing.is_empty() {
            return Ok(Some(Outcome::Ready));
        }

        let cancel = self.cancel_token();
        let servers = deps.nostr.blob_sources(&event, &settings.blossom_fallback_servers, &cancel).await;

        if servers.is_empty() {
            if !self.running() {
                // The lookup was aborted by the shutdown, not answered with nothing.
                return Ok(Some(Outcome::Interrupted(missing.len())));
            }
            self.deploy_log(event_id, "error", "no_blob_servers", fields! { "missing" => missing.len() }).await;
            return Ok(Some(Outcome::Failed(
                "No Blossom servers to download from. The manifest has no server tags and the author has no kind 10063 list — add fallback servers in Settings."
                    .into(),
            )));
        }

        self.deploy_log(event_id, "info", "downloading_blobs", fields! { "missing" => missing.len(), "servers" => servers.join(" ") })
            .await;

        let already = (hashes.len() - missing.len()) as i64;
        let (failed, abandoned, bytes) =
            self.download_all(event_id, missing, &servers, &compressible, already, max_size.saturating_sub(stored)).await;

        // Ahead of the shutdown check: a deploy stopped for its size is
        // finished, and resuming it would only download more of what it can't have.
        if stored + bytes > max_size {
            return Ok(Some(oversized(max_size, stored + bytes)));
        }

        if abandoned > 0 {
            // Shutting down mid-deploy. Marking this ready would serve a site
            // whose files aren't all on disk.
            return Ok(Some(Outcome::Interrupted(abandoned + failed.len())));
        }

        if !failed.is_empty() {
            let sample = failed.iter().take(3).map(BlobHash::to_string).collect::<Vec<_>>().join(", ");
            return Ok(Some(Outcome::Failed(format!(
                "{} of {} blobs could not be downloaded from {} server(s): {sample}{}",
                failed.len(),
                hashes.len(),
                servers.len(),
                if failed.len() > 3 { ", …" } else { "" }
            ))));
        }

        Ok(Some(Outcome::Ready))
    }

    /// Download missing blobs with bounded concurrency. Returns the blobs no
    /// server could supply, how many were never attempted because the worker
    /// stopped, and the bytes downloaded.
    ///
    /// Nothing new is started once the downloads pass `budget` bytes: a
    /// manifest doesn't say how big its blobs are, so the limit is found by
    /// crossing it, and the downloads under way bound the overshoot.
    async fn download_all(
        &self,
        event_id: &str,
        missing: Vec<BlobHash>,
        servers: &[String],
        compressible: &HashSet<BlobHash>,
        already: i64,
        budget: u64,
    ) -> (Vec<BlobHash>, usize, u64) {
        let max_file_size = self.deps().settings.get().max_file_size as u64;
        let mut queue = missing.into_iter();
        let mut downloading = FuturesUnordered::new();
        let (mut failed, mut bytes, mut ready) = (Vec::new(), 0u64, already);

        loop {
            while downloading.len() < BLOB_CONCURRENCY && self.running() && bytes <= budget {
                let Some(sha256) = queue.next() else { break };
                let compress = compressible.contains(&sha256);
                downloading.push(async move {
                    let size = self.download_blob(event_id, sha256, servers, max_file_size, compress).await;
                    (sha256, size)
                });
            }
            let Some((sha256, size)) = downloading.next().await else { break };
            match size {
                Some(size) => {
                    bytes += size;
                    ready += 1;
                    let _ = self.set_progress(event_id, ready).await;
                }
                None => failed.push(sha256),
            }
        }

        (failed, queue.count(), bytes)
    }

    /// Try each server in turn for one blob. Returns its size, or `None` if no
    /// server had it.
    async fn download_blob(&self, event_id: &str, sha256: BlobHash, servers: &[String], max_size: u64, compress: bool) -> Option<u64> {
        let cancel = self.cancel_token();

        for server in servers {
            let url = blob_url(server, sha256.hex().as_str());
            match self.deps().blobs.download(&url, sha256, max_size, &cancel).await {
                Ok(size) => {
                    self.deploy_log(event_id, "debug", "blob_downloaded", fields! { "sha256" => sha256, "server" => server.as_str(), "size" => size })
                        .await;
                    if compress {
                        self.compress(event_id, sha256).await;
                    }
                    // Recorded with whichever copies it now has, in one trip.
                    let blobs = self.deps().blobs.clone();
                    let recorded = self
                        .db(move |conn| {
                            record_blob(conn, sha256, size)?;
                            if let Some(variants) = blobs.variants(sha256) {
                                record_variant_sizes(conn, sha256, variants)?;
                            }
                            Ok(())
                        })
                        .await;
                    if let Err(error) = recorded {
                        self.deps().log.error("blob_record_failed", with_err(fields! { "sha256" => sha256 }, error));
                    }
                    return Some(size);
                }
                Err(error) => {
                    let fields = fields! {
                        "sha256" => sha256,
                        "server" => server.as_str(),
                        "reason" => error.reason.as_str(),
                        "status" => error.status,
                        "err_msg" => error.message.as_str(),
                    };
                    self.deps().log.debug("blob_download_failed", fields.clone());
                    self.deploy_log(event_id, "warn", "blob_download_failed", fields).await;
                }
            }
        }

        self.deploy_log(event_id, "error", "blob_unavailable", fields! { "sha256" => sha256, "servers" => servers.len() }).await;
        None
    }

    /// Store compressed copies of a blob. Never fatal: a site served
    /// uncompressed is a slower site rather than a broken one.
    async fn compress(&self, event_id: &str, sha256: BlobHash) {
        let blobs = self.deps().blobs.clone();
        let result = tokio::task::spawn_blocking(move || blobs.ensure_variants(sha256)).await;
        match result {
            Ok(Ok(written)) => {
                if !written.is_empty() {
                    let encodings = written.iter().map(|e| e.as_str()).collect::<Vec<_>>().join(" ");
                    self.deploy_log(event_id, "debug", "blob_compressed", fields! { "sha256" => sha256, "encodings" => encodings }).await;
                }
            }
            Ok(Err(error)) => self.compress_failed(event_id, sha256, error.to_string()).await,
            Err(error) => self.compress_failed(event_id, sha256, error.to_string()).await,
        }
    }

    async fn compress_failed(&self, event_id: &str, sha256: BlobHash, message: String) {
        let fields = fields! { "sha256" => sha256, "err_msg" => message };
        self.deploy_log(event_id, "warn", "blob_compress_failed", fields.clone()).await;
        self.deps().log.warn("blob_compress_failed", fields);
    }

    async fn set_progress(&self, event_id: &str, files_ready: i64) -> Result<()> {
        let id = event_id.to_string();
        self.db(move |conn| Ok(conn.execute("UPDATE site_events SET files_ready = ? WHERE event_id = ?", params![files_ready, id])?))
            .await?;
        Ok(())
    }

    /// Record a failed deploy, and queue another attempt if any are left.
    ///
    /// The version stays `failed` in the meantime, and a retry resumes rather
    /// than restarts, because the blobs of earlier attempts are still on
    /// disk. Each retry waits twice as long as the last. Attempts are counted
    /// per run: once a run is spent the resync timer still re-queues the
    /// version, which is what lets a site recover on its own.
    async fn fail(&self, event_id: &str, reason: &str, attempts: &mut HashMap<String, u32>) -> Option<Instant> {
        let limit = self.deps().settings.get().deploy_attempts as u32;
        let attempt = attempts.get(event_id).copied().unwrap_or(0) + 1;
        let again = attempt < limit && self.running();
        if again {
            attempts.insert(event_id.to_string(), attempt);
        } else {
            attempts.remove(event_id);
        }

        let backoff = self.deps().retry_backoff * 2u32.pow(attempt - 1);

        let mut explanation = reason.to_string();
        if limit > 1 {
            let next = if again { format!("Trying again in {}.", duration(backoff)) } else { "The next sync will try again.".into() };
            explanation = format!("{} Attempt {attempt} of {limit}. {next}", terminate(reason));
        }

        self.finish(event_id, DeployStatus::Failed, Some(&explanation)).await;

        if !again {
            return None;
        }
        self.deploy_log(event_id, "info", "deploy_retrying", fields! { "attempt" => attempt + 1, "of" => limit, "wait_ms" => backoff.as_millis() as u64 })
            .await;
        Some(Instant::now() + backoff)
    }

    /// Fail a version for a reason retrying can't fix: it was superseded, or
    /// it breaks a limit.
    async fn refuse(&self, event_id: &str, reason: &str, msg: &'static str, fields: Fields) {
        self.deploy_log(event_id, "warn", msg, fields).await;
        self.finish(event_id, DeployStatus::Failed, Some(reason)).await;
    }

    /// Leave an interrupted deploy incomplete, so the next sync resumes it.
    async fn interrupt(&self, event_id: &str, remaining: usize) {
        let id = event_id.to_string();
        let _ = self
            .db(move |conn| {
                Ok(conn.execute(
                    "UPDATE site_events SET status = ?, error = ? WHERE event_id = ?",
                    params![DeployStatus::Pending, "Deploy was interrupted while npanel was stopping. It resumes on the next sync.", id],
                )?)
            })
            .await;
        self.deploy_log(event_id, "warn", "deploy_interrupted", fields! { "remaining" => remaining }).await;
        self.deps().log.warn("deploy_interrupted", fields! { "event_id" => event_id, "remaining" => remaining });
    }

    async fn finish(&self, event_id: &str, status: DeployStatus, error: Option<&str>) {
        let now = crate::util::now();
        let id = event_id.to_string();
        let err = error.map(str::to_string);
        let ready = status == DeployStatus::Ready;
        let (level, msg, fields) =
            if ready { ("info", "deploy_ready", fields! {}) } else { ("error", "deploy_failed", fields! { "err_msg" => error }) };

        // The verdict is logged in the same transaction as the status it
        // explains, so nobody reading one sees the other missing.
        let site = self
            .db(move |conn| {
                let tx = conn.transaction()?;
                tx.execute(
                    "UPDATE site_events SET status = ?, error = ?, ready_at = ? WHERE event_id = ?",
                    params![status, err, ready.then_some(now), id],
                )?;
                write_deploy_log(&tx, &id, level, msg, fields)?;
                prune_deploy_log(&tx, &id)?;
                let site = site_event(&tx, &id)?.map(|row| row.address());
                tx.commit()?;
                Ok(site)
            })
            .await
            .unwrap_or_else(|error| {
                self.deps().log.error("deploy_record_failed", with_err(fields! { "event_id" => event_id }, error));
                None
            });

        if ready {
            self.deps().log.info("deploy_ready", fields! { "event_id" => event_id });

            // The newest ready version may just have changed, and a version is
            // only served through a build of it.
            if let Some(site) = site {
                let builder = self.deps().builder.clone();
                tokio::spawn(async move { builder.site_changed(&site).await });
            }
        } else {
            self.deps().log.error("deploy_failed", fields! { "event_id" => event_id, "err_msg" => error });
        }
    }
}

async fn sleep_until(at: Option<Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}

/// The timer for the periodic pass, whose first tick is one interval away.
fn resync_timer(seconds: i64, log: Logger) -> tokio::time::Interval {
    let interval = Duration::from_secs(seconds.max(1) as u64);
    log.debug("resync_scheduled", fields! { "interval_seconds" => interval.as_secs() });
    tokio::time::interval_at(Instant::now() + interval, interval)
}

/// Why a version too big for `max_site_size` was refused.
fn oversized(limit: u64, bytes: u64) -> Outcome {
    Outcome::Refused {
        reason: format!(
            "This version is larger than the {} MB a site may take up here. An admin can raise the limit under Settings.",
            (limit as f64 / 1024f64.powi(2)).round() as i64
        ),
        msg: "site_too_large",
        fields: fields! { "bytes" => bytes, "limit" => limit },
    }
}

/// A wait, as someone reading about it would say it.
fn duration(wait: Duration) -> String {
    let seconds = (wait.as_millis() as f64 / 1000.0).round().max(1.0) as i64;
    if seconds == 1 {
        "a second".into()
    } else {
        format!("{seconds} seconds")
    }
}

/// End a sentence, so another can be appended to it.
fn terminate(text: &str) -> String {
    if text.ends_with(['.', '!', '?', '…']) {
        text.to_string()
    } else {
        format!("{text}.")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sentences() {
        assert_eq!(duration(Duration::from_millis(400)), "a second");
        assert_eq!(duration(Duration::from_secs(10)), "10 seconds");
        assert_eq!(terminate("Failed"), "Failed.");
        assert_eq!(terminate("Failed…"), "Failed…");
    }
}
