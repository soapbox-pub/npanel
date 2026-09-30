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

use std::collections::{HashMap, HashSet, VecDeque};
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use futures_util::FutureExt;
use parking_lot::Mutex;
use rusqlite::{params, OptionalExtension};
use tokio::sync::{watch, Notify};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::blobs::BlobStore;
use crate::builds::HostBuilder;
use crate::db::{status, Db};
use crate::fields;
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

#[derive(Default)]
struct State {
    running: bool,
    queue: Vec<QueuedDeploy>,
    /// Attempts already spent on a version, while it is being retried.
    attempts: HashMap<String, u32>,
    pumping: bool,
    resyncing: bool,
    resync_again: bool,
    subscription: Option<CancellationToken>,
    settings_listener: Option<tokio::task::JoinHandle<()>>,
    resync_timer: Option<tokio::task::JoinHandle<()>>,
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
    /// Cuts short the wait for the next retry, when there's work sooner.
    wake: Notify,
    pumping: watch::Sender<bool>,
    resyncing: watch::Sender<bool>,
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
        SyncWorker {
            inner: Arc::new(Inner {
                deps,
                state: Mutex::new(State::default()),
                cancel: Mutex::new(CancellationToken::new()),
                wake: Notify::new(),
                pumping: watch::channel(false).0,
                resyncing: watch::channel(false).0,
            }),
        }
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

    /// Begin syncing.
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

        let mut changes = self.deps().settings.subscribe();
        let worker = self.clone();
        let listener = tokio::spawn(async move {
            while let Ok(keys) = changes.recv().await {
                if keys.contains(&"relays") {
                    // Connections to relays that were removed close themselves
                    // once the restarted subscription stops asking them anything.
                    worker.deps().nostr.clear_caches();
                    worker.restart_subscription().await;
                }
                if keys.contains(&"resync_interval") {
                    worker.schedule_resync();
                }
                if ["blossom_fallback_servers", "max_file_size", "max_site_size", "max_site_files"]
                    .iter()
                    .any(|key| keys.contains(key))
                {
                    // New servers or a bigger limit may be exactly what a
                    // failed deploy needed.
                    worker.retry_incomplete().await;
                }
            }
        });
        self.inner.state.lock().settings_listener = Some(listener);

        self.schedule_resync();
        self.restart_subscription().await;

        let worker = self.clone();
        tokio::spawn(async move { worker.resync_all().await });

        self.deps().log.info("sync_started", fields! { "relays" => self.deps().nostr.relays().len() });
    }

    /// Stop syncing and release relay connections.
    pub async fn stop(&self) {
        {
            let mut state = self.inner.state.lock();
            if !state.running {
                return;
            }
            state.running = false;
            for task in [state.settings_listener.take(), state.resync_timer.take()].into_iter().flatten() {
                task.abort();
            }
            if let Some(subscription) = state.subscription.take() {
                subscription.cancel();
            }
        }
        self.inner.cancel.lock().cancel();
        self.inner.wake.notify_waiters();

        self.when_idle().await;
        self.inner.state.lock().attempts.clear();
        self.deps().log.info("sync_stopped", fields! {});
    }

    /// Called when hosts are added, retargeted, or removed. Restarts the live
    /// subscription and fetches anything newly configured right away.
    pub async fn hosts_changed(&self) {
        if !self.running() {
            return;
        }
        self.restart_subscription().await;
        let worker = self.clone();
        tokio::spawn(async move { worker.resync_all().await });
    }

    /// Resolves once the deploy queue has drained.
    pub async fn when_idle(&self) {
        let mut pumping = self.inner.pumping.subscribe();
        let _ = pumping.wait_for(|busy| !busy).await;
    }

    /// Fetch a site's newest manifest now, and deploy it if it isn't ready.
    pub async fn sync_site(&self, address: &NsiteAddress) -> Result<()> {
        let cancel = self.cancel_token();
        match self.deps().nostr.fetch_manifest(address, &cancel).await {
            Some(event) => self.ingest(event).await?,
            None => self.deps().log.debug(
                "manifest_not_found",
                fields! { "pubkey" => address.pubkey.as_str(), "identifier" => address.identifier.as_str() },
            ),
        }

        // Pick up a version that was stored earlier but never finished deploying.
        let address = address.clone();
        let newest = self.db(move |conn| Ok(newest_event(conn, &address)?)).await?;
        if let Some(newest) = newest.filter(|n| n.status != status::READY) {
            self.enqueue(&newest.event_id);
        }
        Ok(())
    }

    /// Re-fetch every configured site from relays.
    ///
    /// Passes never overlap, but a call made mid-pass isn't dropped either:
    /// the walk may already be past whatever prompted it, so one more pass is
    /// queued behind the current one.
    pub async fn resync_all(&self) {
        let already = {
            let mut state = self.inner.state.lock();
            if state.resyncing {
                state.resync_again = true;
            }
            std::mem::replace(&mut state.resyncing, true)
        };
        if already {
            let mut resyncing = self.inner.resyncing.subscribe();
            let _ = resyncing.wait_for(|busy| !busy).await;
            return;
        }
        self.inner.resyncing.send_replace(true);

        loop {
            self.inner.state.lock().resync_again = false;
            self.run_resync().await;
            let again = {
                let mut state = self.inner.state.lock();
                let again = state.resync_again && state.running;
                if !again {
                    state.resyncing = false;
                }
                again
            };
            if !again {
                break;
            }
        }
        self.inner.resyncing.send_replace(false);
    }

    async fn run_resync(&self) {
        let sites = self.db(|conn| Ok(list_configured_sites(conn)?)).await.unwrap_or_default();
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

    /// Store a manifest event and queue it for deployment.
    pub async fn ingest(&self, event: Event) -> Result<()> {
        if !is_site_manifest(&event) {
            return Ok(());
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
                Ok(store_manifest(conn, &event, status::PENDING)?)
            })
            .await?
        };
        if !stored {
            return Ok(());
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
        Ok(())
    }

    /// Queue every version that isn't ready yet.
    async fn retry_incomplete(&self) {
        let pending = self
            .db(|conn| {
                let mut ids = Vec::new();
                for site in list_configured_sites(conn)? {
                    if let Some(newest) = newest_event(conn, &site)?.filter(|n| n.status != status::READY) {
                        ids.push(newest.event_id);
                    }
                }
                Ok(ids)
            })
            .await
            .unwrap_or_default();

        for event_id in pending {
            self.enqueue(&event_id);
        }
    }

    fn schedule_resync(&self) {
        let interval = Duration::from_secs(self.deps().settings.get().resync_interval as u64);
        let worker = self.clone();
        let timer = tokio::spawn(async move {
            let mut ticker = tokio::time::interval_at(Instant::now() + interval, interval);
            loop {
                ticker.tick().await;
                let worker = worker.clone();
                tokio::spawn(async move { worker.resync_all().await });
            }
        });

        if let Some(previous) = self.inner.state.lock().resync_timer.replace(timer) {
            previous.abort();
        }

        self.deps().log.debug("resync_scheduled", fields! { "interval_seconds" => interval.as_secs() });
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

        let sites = self.db(|conn| Ok(list_configured_sites(conn)?)).await.unwrap_or_default();
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
        self.inner.state.lock().attempts.remove(event_id);
        self.queue_deploy(event_id, Instant::now());
    }

    /// Queue a version, to be deployed no earlier than `after`.
    fn queue_deploy(&self, event_id: &str, after: Instant) {
        let start = {
            let mut state = self.inner.state.lock();
            match state.queue.iter_mut().find(|entry| entry.event_id == event_id) {
                // A fresh reason to deploy overtakes a retry waiting out its backoff.
                Some(pending) => pending.after = pending.after.min(after),
                None => state.queue.push(QueuedDeploy { event_id: event_id.to_string(), after }),
            }
            !std::mem::replace(&mut state.pumping, true)
        };
        self.inner.wake.notify_waiters();

        if start {
            self.inner.pumping.send_replace(true);
            let worker = self.clone();
            tokio::spawn(async move { worker.pump().await });
        }
    }

    /// Deploy queued versions one at a time. A version waiting out a retry's
    /// backoff holds up nothing: whatever is ready goes first, and only an
    /// otherwise empty queue actually waits.
    async fn pump(&self) {
        loop {
            let next = {
                let mut state = self.inner.state.lock();
                if !state.running {
                    None
                } else {
                    let now = Instant::now();
                    match state.queue.iter().position(|entry| entry.after <= now) {
                        Some(index) => Some(Ok(state.queue.remove(index).event_id)),
                        None => state.queue.iter().map(|entry| entry.after).min().map(Err),
                    }
                }
            };

            match next {
                None => break,
                Some(Err(soonest)) => {
                    let cancel = self.cancel_token();
                    tokio::select! {
                        _ = tokio::time::sleep_until(soonest) => {}
                        _ = self.inner.wake.notified() => {}
                        _ = cancel.cancelled() => {}
                    }
                }
                Some(Ok(event_id)) => {
                    let outcome = AssertUnwindSafe(self.deploy(&event_id)).catch_unwind().await;
                    let outcome = match outcome {
                        Ok(outcome) => outcome,
                        Err(_) => Err(anyhow::anyhow!("the deploy panicked")),
                    };
                    self.settle(&event_id, outcome).await;
                }
            }
        }

        let mut state = self.inner.state.lock();
        state.pumping = false;
        drop(state);
        self.inner.pumping.send_replace(false);
    }

    async fn settle(&self, event_id: &str, outcome: Result<Option<Outcome>>) {
        match outcome {
            Ok(None) => {}
            Ok(Some(Outcome::Ready)) => self.finish(event_id, status::READY, None).await,
            Ok(Some(Outcome::Failed(reason))) => self.fail(event_id, &reason).await,
            Ok(Some(Outcome::Refused { reason, msg, fields })) => self.refuse(event_id, &reason, msg, fields).await,
            Ok(Some(Outcome::Interrupted(remaining))) => self.interrupt(event_id, remaining).await,
            Err(error) => {
                if !self.running() {
                    // A consequence of shutting down, not of the version.
                    self.interrupt(event_id, 0).await;
                    return;
                }
                self.deps().log.error("deploy_crashed", with_err(fields! { "event_id" => event_id }, &error));
                self.fail(event_id, &format!("Deploy failed: {error}")).await;
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
        if row.status == status::READY {
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
        let files: Vec<(String, String)> = self
            .db(move |conn| {
                conn.execute("UPDATE site_events SET status = 'fetching_blobs', error = NULL WHERE event_id = ?", [&id])?;
                Ok(conn
                    .prepare("SELECT sha256, path FROM site_files WHERE event_id = ?")?
                    .query_map([&id], |r| Ok((r.get(0)?, r.get(1)?)))?
                    .collect::<rusqlite::Result<_>>()?)
            })
            .await?;

        // Paths come along because the store is keyed by hash and so can't
        // tell markup from a JPEG, while whether a blob is worth compressing
        // is a question about the file it is served as.
        let compressible: HashSet<String> =
            files.iter().filter(|(_, path)| is_compressible(path)).map(|(sha, _)| sha.clone()).collect();
        let hashes = dedupe(files.iter().map(|(sha, _)| sha.clone()));

        self.deploy_log(event_id, "info", "deploy_started", fields! { "files" => row.files_total, "blobs" => hashes.len() }).await;

        // Bytes of this version already on disk. Blobs other sites share count
        // too: the limit is on how big a version is, not on what it costs to add.
        let mut missing = Vec::new();
        let mut stored: u64 = 0;
        for sha256 in &hashes {
            match deps.blobs.size(sha256) {
                None => missing.push(sha256.clone()),
                Some(size) => {
                    stored += size;
                    let sha = sha256.clone();
                    self.db(move |conn| Ok(record_blob(conn, &sha, size)?)).await?;
                    // Already on disk, but not necessarily compressed.
                    if compressible.contains(sha256) {
                        self.compress(event_id, sha256).await;
                    }
                    self.record_variants(sha256).await?;
                }
            }
        }

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
            let sample = failed.iter().take(3).cloned().collect::<Vec<_>>().join(", ");
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
        missing: Vec<String>,
        servers: &[String],
        compressible: &HashSet<String>,
        already: i64,
        budget: u64,
    ) -> (Vec<String>, usize, u64) {
        let max_file_size = self.deps().settings.get().max_file_size as u64;
        let workers = BLOB_CONCURRENCY.min(missing.len());
        let queue = Mutex::new(VecDeque::from(missing));
        let failed = Mutex::new(Vec::new());
        let bytes = std::sync::atomic::AtomicU64::new(0);
        let ready = std::sync::atomic::AtomicI64::new(already);

        let worker = || async {
            while self.running() && bytes.load(std::sync::atomic::Ordering::SeqCst) <= budget {
                let Some(sha256) = queue.lock().pop_front() else { return };
                let compress = compressible.contains(&sha256);
                match self.download_blob(event_id, &sha256, servers, max_file_size, compress).await {
                    Some(size) => {
                        bytes.fetch_add(size, std::sync::atomic::Ordering::SeqCst);
                        let count = ready.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                        let _ = self.set_progress(event_id, count).await;
                    }
                    None => failed.lock().push(sha256),
                }
            }
        };

        futures_util::future::join_all((0..workers).map(|_| worker())).await;

        let abandoned = queue.lock().len();
        (failed.into_inner(), abandoned, bytes.into_inner())
    }

    /// Try each server in turn for one blob. Returns its size, or `None` if no
    /// server had it.
    async fn download_blob(&self, event_id: &str, sha256: &str, servers: &[String], max_size: u64, compress: bool) -> Option<u64> {
        let cancel = self.cancel_token();

        for server in servers {
            let url = blob_url(server, sha256);
            match self.deps().blobs.download(&url, sha256, max_size, &cancel).await {
                Ok(size) => {
                    let sha = sha256.to_string();
                    let _ = self.db(move |conn| Ok(record_blob(conn, &sha, size)?)).await;
                    self.deploy_log(event_id, "debug", "blob_downloaded", fields! { "sha256" => sha256, "server" => server.as_str(), "size" => size })
                        .await;
                    if compress {
                        self.compress(event_id, sha256).await;
                    }
                    let _ = self.record_variants(sha256).await;
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
    async fn compress(&self, event_id: &str, sha256: &str) {
        let blobs = self.deps().blobs.clone();
        let sha = sha256.to_string();
        let result = tokio::task::spawn_blocking(move || blobs.ensure_variants(&sha)).await;
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

    async fn compress_failed(&self, event_id: &str, sha256: &str, message: String) {
        let fields = fields! { "sha256" => sha256, "err_msg" => message };
        self.deploy_log(event_id, "warn", "blob_compress_failed", fields.clone()).await;
        self.deps().log.warn("blob_compress_failed", fields);
    }

    /// Record which compressed copies a blob has, read back from the disk.
    async fn record_variants(&self, sha256: &str) -> Result<()> {
        let Some(variants) = self.deps().blobs.variants(sha256) else {
            return Ok(());
        };
        let sha = sha256.to_string();
        self.db(move |conn| Ok(record_variant_sizes(conn, &sha, variants)?)).await
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
    async fn fail(&self, event_id: &str, reason: &str) {
        let limit = self.deps().settings.get().deploy_attempts as u32;
        let (attempt, again) = {
            let mut state = self.inner.state.lock();
            let attempt = state.attempts.get(event_id).copied().unwrap_or(0) + 1;
            let again = attempt < limit && state.running;
            if again {
                state.attempts.insert(event_id.to_string(), attempt);
            } else {
                state.attempts.remove(event_id);
            }
            (attempt, again)
        };

        let backoff = self.deps().retry_backoff * 2u32.pow(attempt - 1);

        let mut explanation = reason.to_string();
        if limit > 1 {
            let next = if again { format!("Trying again in {}.", duration(backoff)) } else { "The next sync will try again.".into() };
            explanation = format!("{} Attempt {attempt} of {limit}. {next}", terminate(reason));
        }

        self.finish(event_id, status::FAILED, Some(&explanation)).await;

        if again {
            self.deploy_log(
                event_id,
                "info",
                "deploy_retrying",
                fields! { "attempt" => attempt + 1, "of" => limit, "wait_ms" => backoff.as_millis() as u64 },
            )
            .await;
            self.queue_deploy(event_id, Instant::now() + backoff);
        }
    }

    /// Fail a version for a reason retrying can't fix: it was superseded, or
    /// it breaks a limit.
    async fn refuse(&self, event_id: &str, reason: &str, msg: &'static str, fields: Fields) {
        self.inner.state.lock().attempts.remove(event_id);
        self.deploy_log(event_id, "warn", msg, fields).await;
        self.finish(event_id, status::FAILED, Some(reason)).await;
    }

    /// Leave an interrupted deploy incomplete, so the next sync resumes it.
    async fn interrupt(&self, event_id: &str, remaining: usize) {
        self.inner.state.lock().attempts.remove(event_id);
        let id = event_id.to_string();
        let _ = self
            .db(move |conn| {
                Ok(conn.execute(
                    "UPDATE site_events SET status = 'pending', error = ? WHERE event_id = ?",
                    params!["Deploy was interrupted while npanel was stopping. It resumes on the next sync.", id],
                )?)
            })
            .await;
        self.deploy_log(event_id, "warn", "deploy_interrupted", fields! { "remaining" => remaining }).await;
        self.deps().log.warn("deploy_interrupted", fields! { "event_id" => event_id, "remaining" => remaining });
    }

    async fn finish(&self, event_id: &str, status: &'static str, error: Option<&str>) {
        let now = crate::util::now();
        let id = event_id.to_string();
        let err = error.map(str::to_string);
        let ready = status == status::READY;

        let site = self
            .db(move |conn| {
                conn.execute(
                    "UPDATE site_events SET status = ?, error = ?, ready_at = ? WHERE event_id = ?",
                    params![status, err, ready.then_some(now), id],
                )?;
                Ok(site_event(conn, &id)?.map(|row| row.address()))
            })
            .await
            .ok()
            .flatten();

        if ready {
            self.inner.state.lock().attempts.remove(event_id);
            self.deps().log.info("deploy_ready", fields! { "event_id" => event_id });
            self.deploy_log(event_id, "info", "deploy_ready", fields! {}).await;

            // The newest ready version may just have changed, and a version is
            // only served through a build of it.
            if let Some(site) = site {
                let builder = self.deps().builder.clone();
                tokio::spawn(async move { builder.site_changed(&site).await });
            }
        } else {
            self.deps().log.error("deploy_failed", fields! { "event_id" => event_id, "err_msg" => error });
            self.deploy_log(event_id, "error", "deploy_failed", fields! { "err_msg" => error }).await;
        }

        let id = event_id.to_string();
        let _ = self.db(move |conn| Ok(prune_deploy_log(conn, &id)?)).await;
    }
}

/// Why a version too big for `max_site_size` was refused.
fn oversized(limit: u64, bytes: u64) -> Outcome {
    Outcome::Refused {
        reason: format!(
            "This version is larger than the {} MB a site may take up here. An admin can raise the limit under Settings.",
            crate::util::js_round(limit as f64 / 1024f64.powi(2)) as i64
        ),
        msg: "site_too_large",
        fields: fields! { "bytes" => bytes, "limit" => limit },
    }
}

/// A wait, as someone reading about it would say it.
fn duration(wait: Duration) -> String {
    let seconds = crate::util::js_round(wait.as_millis() as f64 / 1000.0).max(1.0) as i64;
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
