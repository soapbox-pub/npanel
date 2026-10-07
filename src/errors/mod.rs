//! npanel's error tracker, on a thread of its own.
//!
//! A site with error reporting on loads a small reporter in every page
//! (`reporter.rs`), which sends what the page throws to its own origin. Each
//! report is read on the listener that received it and handed across whole;
//! everything after that — parsing, mapping frames through the site's source
//! maps, grouping into issues, writing, and the dashboard's questions —
//! happens here, off the threads that serve sites, the way analytics do.
//!
//! The runtime owns its database file and its caches. If it dies it is
//! started again with backoff (`src/supervise.rs`), and sites go on being
//! served either way.

pub mod envelope;
pub mod grouping;
pub mod ingest;
pub mod queries;
pub mod reporter;
pub mod store;
pub mod symbolicate;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::response::IntoResponse;
use http::{header, HeaderValue, Method, Request, Response, StatusCode};
use parking_lot::{Mutex, RwLock};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

use self::ingest::{ErrorsHost, Ingest, Received, Sender};
use self::store::ErrorsStore;
use self::symbolicate::Symbolicator;
use crate::analytics::query::engine::QueryHost;
use crate::analytics::{client_ip, read_capped, Answer, Peer};
use crate::blobs::BlobStore;
use crate::db::Db;
use crate::fields;
use crate::log::{with_err, Logger};
use crate::settings::Settings;

/// The largest report accepted. The reporter keeps its own under 64 KiB; a
/// Sentry SDK's can be larger.
pub const MAX_REPORT_BODY: usize = 256 * 1024;

/// How often buffered reports are written.
const FLUSH_EVERY: Duration = Duration::from_secs(2);
/// How often reports past their time are forgotten.
const PRUNE_EVERY: Duration = Duration::from_secs(60 * 60);
/// How long a report may take to be handled, frames mapped and all.
const REPORT_TIMEOUT: Duration = Duration::from_secs(30);
/// How long the sender has to deliver a report's body.
const REPORT_READ_TIMEOUT: Duration = Duration::from_secs(10);
/// Reports waiting for, or being handled on, the errors runtime. Each holds
/// its body, and mapping frames can take a while, so past this the sender
/// is asked to come back.
const MAX_QUEUED_REPORTS: usize = 64;
const QUERY_TIMEOUT: Duration = Duration::from_secs(30);

fn starting() -> Answer {
    Answer::error(StatusCode::SERVICE_UNAVAILABLE, "Error tracking is starting. Try again in a moment.")
}

fn restarted() -> Answer {
    Answer::error(StatusCode::SERVICE_UNAVAILABLE, "Error tracking restarted before answering. Try again in a moment.")
}

fn too_slow() -> Answer {
    Answer::error(StatusCode::GATEWAY_TIMEOUT, "Error tracking took too long to answer. Try again in a moment.")
}

fn text(status: StatusCode, message: &'static str) -> Response<Body> {
    (status, [(header::CONTENT_TYPE, "text/plain; charset=utf-8")], message).into_response()
}

/// A Sentry SDK backs off on this: `Retry-After`, and the categories it
/// covers — all of them.
fn limited() -> Response<Body> {
    let mut response = text(StatusCode::TOO_MANY_REQUESTS, "This site has sent too many error reports. Try again in a minute.");
    response.headers_mut().insert(header::RETRY_AFTER, HeaderValue::from_static("60"));
    response.headers_mut().insert("x-sentry-rate-limits", HeaderValue::from_static("60::organization"));
    response
}

pub struct ErrorsConfig {
    /// Where `errors.sqlite3` lives.
    pub data_dir: PathBuf,
}

/// What the errors runtime has, once it is up.
struct Worker {
    runtime: tokio::runtime::Handle,
    ingest: Mutex<Ingest>,
    /// The one connection that writes.
    store: Mutex<ErrorsStore>,
    /// Taken for the whole of a flush, so two can't interleave.
    flushing: Mutex<()>,
    /// Connections for the dashboard's queries.
    reads: Db,
    symbolicator: Symbolicator,
}

struct Shared {
    db: Db,
    blobs: BlobStore,
    settings: Arc<Settings>,
    log: Logger,
    config: ErrorsConfig,
    /// Every host by id, once they have been read. Until then nothing is
    /// pruned for belonging to a host that's gone.
    hosts: RwLock<Option<Arc<HashMap<i64, ErrorsHost>>>>,
    worker: RwLock<Option<Arc<Worker>>>,
}

pub struct Errors {
    shared: Arc<Shared>,
    queued: Arc<Semaphore>,
    shutdown: CancellationToken,
    thread: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl Errors {
    /// Start the errors runtime. Returns at once; until it is ready,
    /// everything asked of it is answered `503`.
    pub fn start(db: Db, blobs: BlobStore, settings: Arc<Settings>, log: Logger, config: ErrorsConfig) -> Errors {
        let shared = Arc::new(Shared { db, blobs, settings, log, config, hosts: RwLock::new(None), worker: RwLock::new(None) });
        let shutdown = CancellationToken::new();

        let (s, stop) = (shared.clone(), shutdown.clone());
        let thread = crate::supervise::supervise("errors", 2, log, move |runtime| {
            let result = runtime.block_on(run(&s, &stop));
            *s.worker.write() = None;
            match result {
                _ if stop.is_cancelled() => Ok(()),
                Ok(()) => Err("stopped".into()),
                Err(error) => Err(error.to_string()),
            }
        });

        Errors { shared, queued: Arc::new(Semaphore::new(MAX_QUEUED_REPORTS)), shutdown, thread: Mutex::new(Some(thread)) }
    }

    /// Stop the runtime, letting it write what it has buffered first.
    pub async fn stop(&self) {
        self.shutdown.cancel();
        let thread = self.thread.lock().take();
        if let Some(thread) = thread {
            let _ = tokio::task::spawn_blocking(move || thread.join()).await;
        }
    }

    fn worker(&self) -> Option<Arc<Worker>> {
        self.shared.worker.read().clone()
    }

    /// Re-read which sites are collecting reports. Call after any host changes.
    pub async fn hosts_changed(&self) {
        refresh_hosts(&self.shared).await;
    }

    /// Receive a report, read on the listener that received it and handed
    /// to the errors runtime. `site` is the host `hostname` names, the only
    /// one it may be kept for; `peer` decides whether any header naming the
    /// visitor is believed.
    pub async fn forward_report(&self, request: Request<Body>, site: Option<i64>, hostname: &str, peer: Peer) -> Response<Body> {
        let mut response = self.receive(request, site, hostname, peer).await;
        let headers = response.headers_mut();
        headers.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, HeaderValue::from_static("*"));
        headers.insert(header::ACCESS_CONTROL_ALLOW_METHODS, HeaderValue::from_static("POST, OPTIONS"));
        headers.insert(header::ACCESS_CONTROL_ALLOW_HEADERS, HeaderValue::from_static("content-type"));
        headers.insert(header::ACCESS_CONTROL_EXPOSE_HEADERS, HeaderValue::from_static("x-sentry-rate-limits, retry-after"));
        response
    }

    async fn receive(&self, request: Request<Body>, site: Option<i64>, hostname: &str, peer: Peer) -> Response<Body> {
        let (parts, body) = request.into_parts();
        if parts.method == Method::OPTIONS {
            return StatusCode::NO_CONTENT.into_response();
        }
        if parts.method != Method::POST {
            return (StatusCode::METHOD_NOT_ALLOWED, [(header::ALLOW, "POST, OPTIONS")], "Send reports with POST.").into_response();
        }
        let Ok(permit) = self.queued.clone().try_acquire_owned() else {
            let mut response = text(StatusCode::TOO_MANY_REQUESTS, "Too many reports are waiting. Try again in a moment.");
            response.headers_mut().insert(header::RETRY_AFTER, HeaderValue::from_static("10"));
            return response;
        };

        let body = match tokio::time::timeout(REPORT_READ_TIMEOUT, read_capped(&parts.headers, body, MAX_REPORT_BODY)).await {
            Ok(Some(body)) => body,
            Ok(None) => return text(StatusCode::PAYLOAD_TOO_LARGE, "Request body is too large."),
            Err(_) => return text(StatusCode::REQUEST_TIMEOUT, "The request body took too long to arrive."),
        };
        let Some(worker) = self.worker() else {
            return starting().into_response();
        };

        let header = |name: &str| parts.headers.get(name).and_then(|v| v.to_str().ok()).map(str::to_string);
        let user_agent = header("user-agent").unwrap_or_default();
        let ip = client_ip(header("x-forwarded-for").as_deref(), header("x-real-ip").as_deref(), peer);
        let host = site.and_then(|id| self.shared.hosts.read().as_ref().and_then(|hosts| hosts.get(&id).cloned()));
        let hostname = hostname.to_string();
        let log = self.shared.log;

        let task = worker.runtime.spawn_blocking({
            let worker = worker.clone();
            move || {
                let _permit = permit;
                let cx = ingest::Context { ingest: &worker.ingest, store: &worker.store, symbolicator: &worker.symbolicator, log: &log };
                let sender = Sender { ip: &ip, user_agent: &user_agent };
                let (received, full) = ingest::handle(&cx, &body, &sender, host.as_ref(), &hostname, crate::util::now());
                if full {
                    flush(&worker, &log);
                }
                received
            }
        });

        match tokio::time::timeout(REPORT_TIMEOUT, task).await {
            Ok(Ok(Received::Accepted(id))) => {
                let body = match id {
                    Some(id) => serde_json::json!({ "id": id }),
                    None => serde_json::json!({}),
                };
                Answer { status: StatusCode::OK, body: body.to_string() }.into_response()
            }
            Ok(Ok(Received::Limited)) => limited(),
            Ok(Ok(Received::Invalid(message))) => Answer::error(StatusCode::BAD_REQUEST, &message).into_response(),
            Ok(Err(_)) => restarted().into_response(),
            Err(_) => too_slow().into_response(),
        }
    }

    /// A page of a site's issues in one state.
    pub async fn issues(&self, host_id: i64, status: String, cursor: Option<String>) -> Answer {
        self.read(move |conn| queries::issues(conn, host_id, &status, cursor.as_deref(), crate::util::now())).await
    }

    pub async fn issue(&self, host_id: i64, issue_id: i64) -> Answer {
        self.read(move |conn| queries::issue(conn, host_id, issue_id, crate::util::now())).await
    }

    pub async fn events(&self, host_id: i64, issue_id: i64, before: Option<i64>) -> Answer {
        self.read(move |conn| queries::events(conn, host_id, issue_id, before)).await
    }

    pub async fn event(&self, host_id: i64, issue_id: i64, id: i64) -> Answer {
        self.read(move |conn| queries::event(conn, host_id, issue_id, id)).await
    }

    /// Unresolved issues and the last day's reports of every site given.
    pub async fn summary(&self, hosts: Vec<QueryHost>) -> Answer {
        self.read(move |conn| queries::summary(conn, &hosts, crate::util::now())).await
    }

    /// Set an issue's status, returning what it was: `Ok(None)` when the
    /// site has no such issue.
    pub async fn set_status(&self, host_id: i64, issue_id: i64, status: &'static str) -> Result<Option<String>, Answer> {
        let Some(worker) = self.worker() else { return Err(starting()) };
        let log = self.shared.log;
        let task = worker.runtime.spawn_blocking({
            let worker = worker.clone();
            move || {
                // What's buffered first, so a brand new issue can be resolved.
                flush(&worker, &log);
                worker.store.lock().set_status(host_id, issue_id, status)
            }
        });
        match task.await {
            Ok(Ok(previous)) => Ok(previous),
            Ok(Err(error)) => {
                log.error("errors_update_failed", crate::log::err_fields(error));
                Err(Answer::error(StatusCode::INTERNAL_SERVER_ERROR, "The issue couldn't be updated. The server log has the details."))
            }
            Err(_) => Err(restarted()),
        }
    }

    /// Run a question on the read connections, after writing what's
    /// buffered, so a report sent a moment ago is in the answer.
    async fn read<F>(&self, run: F) -> Answer
    where
        F: FnOnce(&rusqlite::Connection) -> anyhow::Result<Answer> + Send + 'static,
    {
        let Some(worker) = self.worker() else {
            return starting();
        };
        let log = self.shared.log;
        let task = worker.runtime.spawn({
            let worker = worker.clone();
            async move {
                let w = worker.clone();
                let _ = tokio::task::spawn_blocking(move || flush(&w, &log)).await;
                worker.reads.call_within(QUERY_TIMEOUT, move |conn| run(conn)).await
            }
        });

        match task.await {
            Ok(Ok(Some(answer))) => answer,
            Ok(Ok(None)) => too_slow(),
            Ok(Err(error)) => {
                log.error("errors_query_failed", crate::log::err_fields(error));
                Answer::error(StatusCode::INTERNAL_SERVER_ERROR, "The error tracking query failed. The server log has the details.")
            }
            Err(_) => restarted(),
        }
    }
}

/// Everything the errors runtime does, until it is told to stop.
async fn run(shared: &Arc<Shared>, stop: &CancellationToken) -> anyhow::Result<()> {
    let log = shared.log;
    let path = shared.config.data_dir.join("errors.sqlite3");
    tokio::fs::create_dir_all(&shared.config.data_dir).await?;

    let store = {
        let path = path.clone();
        tokio::task::spawn_blocking(move || ErrorsStore::new(crate::db::open_connection(&path)?)).await??
    };
    let reads = Db::connect(&path)?;

    let worker = Arc::new(Worker {
        runtime: tokio::runtime::Handle::current(),
        ingest: Mutex::new(Ingest::default()),
        store: Mutex::new(store),
        flushing: Mutex::new(()),
        reads,
        symbolicator: Symbolicator::new(shared.db.clone(), shared.blobs.clone()),
    });
    *shared.worker.write() = Some(worker.clone());
    refresh_hosts(shared).await;

    let flusher = {
        let worker = worker.clone();
        tokio::spawn(async move {
            let mut ticks = tokio::time::interval(FLUSH_EVERY);
            loop {
                ticks.tick().await;
                let w = worker.clone();
                let _ = tokio::task::spawn_blocking(move || flush(&w, &log)).await;
            }
        })
    };
    let pruner = {
        let (worker, shared) = (worker.clone(), shared.clone());
        tokio::spawn(async move {
            let mut ticks = tokio::time::interval(PRUNE_EVERY);
            loop {
                ticks.tick().await;
                let Some(hosts) = shared.hosts.read().clone() else { continue };
                let cutoff = crate::util::now() - shared.settings.get().errors_retention_days * crate::analytics::salts::DAY;
                let w = worker.clone();
                let pruned = tokio::task::spawn_blocking(move || {
                    let known: HashSet<i64> = hosts.keys().copied().collect();
                    w.store.lock().prune(cutoff, &known)
                })
                .await;
                match pruned {
                    Ok(Ok(issues)) if issues > 0 => log.info("errors_pruned", fields! { "issues" => issues }),
                    Ok(Ok(_)) | Err(_) => {}
                    Ok(Err(error)) => log.error("errors_prune_failed", with_err(fields! {}, error)),
                }
            }
        })
    };

    log.info("errors_started", fields! {});

    stop.cancelled().await;

    flusher.abort();
    pruner.abort();
    let w = worker.clone();
    tokio::task::spawn_blocking(move || flush(&w, &log)).await?;
    Ok(())
}

/// Write what's pending. The in-memory state is only held long enough to
/// take it, so reports go on being received while the transaction runs.
fn flush(worker: &Worker, log: &Logger) {
    let _flushing = worker.flushing.lock();
    let pending = worker.ingest.lock().take_pending();
    if pending.dropped > 0 {
        log.warn("errors_buffer_full", fields! { "dropped" => pending.dropped });
    }
    if pending.occurrences.is_empty() {
        return;
    }
    match worker.store.lock().write(&pending.occurrences) {
        Ok(()) => log.debug("errors_flushed", fields! { "reports" => pending.occurrences.len() }),
        Err(error) => log.error("errors_flush_failed", with_err(fields! {}, error)),
    }
}

/// Read which hosts are collecting reports.
async fn refresh_hosts(shared: &Arc<Shared>) {
    let rows = shared
        .db
        .call(|conn| {
            Ok(conn
                .prepare("SELECT id, hostname, enabled, errors FROM hosts")?
                .query_map([], |r| Ok(ErrorsHost { id: r.get(0)?, hostname: r.get(1)?, collecting: r.get::<_, bool>(2)? && r.get::<_, bool>(3)? }))?
                .collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .await;

    match rows {
        Ok(rows) => *shared.hosts.write() = Some(Arc::new(rows.into_iter().map(|h| (h.id, h)).collect())),
        Err(error) => shared.log.error("errors_hosts_failed", with_err(fields! {}, error)),
    }
}
