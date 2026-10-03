//! npanel's Plausible server, on a thread of its own.
//!
//! Serving sites is always the first priority, so nothing about counting
//! visits runs on the threads that serve them: not the parsing, not the
//! user-agent and location lookups, not the writes, and not the dashboard's
//! queries. Events arrive at `/_npanel/api/event` on every site, on whichever
//! listener serves it; each is read there and handed across whole, a few
//! hundred bytes.
//!
//! The analytics runtime owns its database file and its caches. If it dies it
//! is started again with backoff (`src/supervise.rs`), and sites go on being
//! served either way.

pub mod channel;
pub mod geo;
pub mod ingest;
pub mod query;
pub mod request;
pub mod salts;
pub mod sessions;
pub mod source;
pub mod stats;
pub mod store;
pub mod tracker;
pub mod user_agent;

use std::collections::HashMap;
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::response::IntoResponse;
use http::{header, HeaderMap, HeaderValue, Method, Request, Response, StatusCode};
use http_body_util::BodyExt;
use parking_lot::{Mutex, RwLock};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

use self::geo::Geo;
use self::ingest::{AnalyticsHost, Ingest, Received, Sender};
use self::query::engine::QueryHost;
use self::sessions::{SessionCache, SESSION_TIMEOUT};
use self::store::AnalyticsStore;
use crate::db::Db;
use crate::fields;
use crate::log::{with_err, Logger};
use crate::settings::Settings;

/// The largest event accepted. A real one is a few hundred bytes.
pub const MAX_EVENT_BODY: usize = 64 * 1024;

/// How often buffered events are written.
const FLUSH_EVERY: Duration = Duration::from_secs(2);
/// How often expired sessions are forgotten.
const SWEEP_EVERY: Duration = Duration::from_secs(60);

/// How long an event forwarded to the analytics runtime may take.
const EVENT_TIMEOUT: Duration = Duration::from_secs(10);
/// How long the sender has to deliver an event's body.
const EVENT_READ_TIMEOUT: Duration = Duration::from_secs(10);

/// Events waiting for, or being handled on, the analytics runtime. Each
/// holds its body until it is handled, and past this the sender is asked to
/// come back rather than queued behind everyone else.
const MAX_QUEUED_EVENTS: usize = 256;
/// How long a dashboard query may take. A year of a busy site is a lot of rows.
const QUERY_TIMEOUT: Duration = Duration::from_secs(60);

/// A dashboard question's answer: a status and a JSON body, as Plausible's
/// endpoint would give it.
#[derive(Debug)]
pub struct Answer {
    pub status: StatusCode,
    pub body: String,
}

impl Answer {
    pub fn error(status: StatusCode, message: &str) -> Answer {
        Answer { status, body: serde_json::json!({ "error": message }).to_string() }
    }
}

impl IntoResponse for Answer {
    fn into_response(self) -> Response<Body> {
        (self.status, [(header::CONTENT_TYPE, "application/json")], self.body).into_response()
    }
}

fn starting() -> Answer {
    Answer::error(StatusCode::SERVICE_UNAVAILABLE, "Analytics are starting. Try again in a moment.")
}

fn restarted() -> Answer {
    Answer::error(StatusCode::SERVICE_UNAVAILABLE, "Analytics restarted before answering. Try again in a moment.")
}

fn too_many() -> Response<Body> {
    let mut response = (StatusCode::TOO_MANY_REQUESTS, [(header::CONTENT_TYPE, "text/plain; charset=utf-8")], "Too many events are waiting. Try again in a moment.")
        .into_response();
    response.headers_mut().insert(header::RETRY_AFTER, HeaderValue::from_static("10"));
    response
}

fn too_slow() -> Answer {
    Answer::error(StatusCode::GATEWAY_TIMEOUT, "Analytics took too long to answer. Try a shorter date range.")
}

pub struct AnalyticsConfig {
    /// Where `analytics.sqlite3` and the location database live.
    pub data_dir: PathBuf,
    /// Whether the location database may be downloaded. Off only in tests.
    pub download_geo: bool,
}

/// What the analytics runtime has, once it is up.
struct Worker {
    runtime: tokio::runtime::Handle,
    /// What is in memory: open visits, and what waits to be written. Held
    /// only for that; nothing waits on a disk while holding it.
    ingest: Mutex<Ingest>,
    /// The one connection that writes.
    store: Mutex<AnalyticsStore>,
    /// Taken for the whole of a flush, so two can't write a visit's state out
    /// of order.
    flushing: Mutex<()>,
    /// Connections for the dashboard's queries, apart from the one that writes.
    reads: Db,
    geo: Geo,
}

struct Shared {
    db: Db,
    settings: Arc<Settings>,
    log: Logger,
    config: AnalyticsConfig,
    /// Every host by hostname, and whether events for it are kept. Swapped
    /// whole, so an event takes a reference rather than a copy.
    hosts: RwLock<Arc<HashMap<String, AnalyticsHost>>>,
    worker: RwLock<Option<Arc<Worker>>>,
}

pub struct Analytics {
    shared: Arc<Shared>,
    queued: Arc<Semaphore>,
    shutdown: CancellationToken,
    thread: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl Analytics {
    /// Start the analytics runtime. Returns at once; until it is ready,
    /// everything asked of it is answered `503`.
    pub fn start(db: Db, settings: Arc<Settings>, log: Logger, config: AnalyticsConfig) -> Analytics {
        let shared = Arc::new(Shared { db, settings, log, config, hosts: RwLock::default(), worker: RwLock::new(None) });
        let shutdown = CancellationToken::new();

        let (s, stop) = (shared.clone(), shutdown.clone());
        let thread = crate::supervise::supervise("analytics", 2, log, move |runtime| {
            let result = runtime.block_on(run(&s, &stop));
            *s.worker.write() = None;
            match result {
                _ if stop.is_cancelled() => Ok(()),
                Ok(()) => Err("stopped".into()),
                Err(error) => Err(error.to_string()),
            }
        });

        Analytics {
            shared,
            queued: Arc::new(Semaphore::new(MAX_QUEUED_EVENTS)),
            shutdown,
            thread: Mutex::new(Some(thread)),
        }
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

    /// Re-read which sites are counting. Call after any host changes.
    pub async fn hosts_changed(&self) {
        refresh_hosts(&self.shared).await;
    }

    /// Record an event, read on the listener that received it and handed to
    /// the analytics runtime. `site` is the host the request's hostname
    /// names, the only one it may record for. `peer` is who the connection
    /// came from, which decides whether any header naming the visitor is
    /// believed (see [`Peer`]).
    ///
    /// The tracker posts `text/plain`, which spares it a preflight, but a
    /// client of Plausible's API may send JSON and preflight it, so both are
    /// met.
    pub async fn forward_event(&self, request: Request<Body>, site: Option<i64>, peer: Peer) -> Response<Body> {
        let mut response = self.receive(request, site, peer).await;
        let headers = response.headers_mut();
        headers.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, HeaderValue::from_static("*"));
        headers.insert(header::ACCESS_CONTROL_ALLOW_METHODS, HeaderValue::from_static("POST, OPTIONS"));
        headers.insert(header::ACCESS_CONTROL_ALLOW_HEADERS, HeaderValue::from_static("content-type"));
        response
    }

    async fn receive(&self, request: Request<Body>, site: Option<i64>, peer: Peer) -> Response<Body> {
        let (parts, body) = request.into_parts();
        if parts.method == Method::OPTIONS {
            return StatusCode::NO_CONTENT.into_response();
        }
        if parts.method != Method::POST {
            return (StatusCode::METHOD_NOT_ALLOWED, [(header::ALLOW, "POST, OPTIONS")], "Send events with POST.").into_response();
        }
        let Ok(permit) = self.queued.clone().try_acquire_owned() else {
            return too_many();
        };

        let body = match tokio::time::timeout(EVENT_READ_TIMEOUT, read_capped(&parts.headers, body, MAX_EVENT_BODY)).await {
            Ok(Some(body)) => body,
            Ok(None) => {
                return (StatusCode::PAYLOAD_TOO_LARGE, [(header::CONTENT_TYPE, "text/plain; charset=utf-8")], "Request body is too large.")
                    .into_response()
            }
            Err(_) => {
                return (StatusCode::REQUEST_TIMEOUT, [(header::CONTENT_TYPE, "text/plain; charset=utf-8")], "The request body took too long to arrive.")
                    .into_response()
            }
        };
        let Some(worker) = self.worker() else {
            return starting().into_response();
        };

        let header = |name: &str| parts.headers.get(name).and_then(|v| v.to_str().ok()).map(str::to_string);
        let user_agent = header("user-agent").unwrap_or_default();
        let ip = client_ip(header("x-forwarded-for").as_deref(), header("x-real-ip").as_deref(), peer);
        let hosts = self.shared.hosts.read().clone();
        let log = self.shared.log;

        // Parsing the user agent is real work, and the lock is the analytics
        // runtime's, so all of it happens there, off the runtime threads.
        let task = worker.runtime.spawn_blocking({
            let worker = worker.clone();
            move || {
                let _permit = permit;
                let sender = Sender { ip: &ip, user_agent: &user_agent };
                let (received, full) = worker.ingest.lock().handle(&worker.store, &body, &sender, &hosts, site, crate::util::now());
                if full {
                    flush(&worker, &log);
                }
                received
            }
        });

        match tokio::time::timeout(EVENT_TIMEOUT, task).await {
            Ok(Ok(Received::Accepted { dropped })) => {
                let mut response = (StatusCode::ACCEPTED, [(header::CONTENT_TYPE, "text/plain; charset=utf-8")], "ok").into_response();
                if dropped > 0 {
                    response.headers_mut().insert("x-plausible-dropped", HeaderValue::from(dropped));
                }
                response
            }
            Ok(Ok(Received::Invalid(errors))) => {
                Answer { status: StatusCode::BAD_REQUEST, body: serde_json::json!({ "errors": errors }).to_string() }.into_response()
            }
            Ok(Err(_)) => restarted().into_response(),
            Err(_) => too_slow().into_response(),
        }
    }

    /// Answer a dashboard query, Plausible's `POST /api/stats/:domain/query`.
    pub async fn query(&self, host: QueryHost, body: serde_json::Value) -> Answer {
        let timezone = self.shared.settings.get().analytics_timezone.clone();
        self.read(true, move |conn, log| stats::query(conn, log, crate::util::now(), &host, &body, &timezone)).await
    }

    /// Visitors in the last five minutes.
    pub async fn current_visitors(&self, host: QueryHost) -> Answer {
        self.read(true, move |conn, log| stats::current_visitors(conn, log, crate::util::now(), &host)).await
    }

    /// Each site's visitors over the last 24 hours, hour by hour.
    pub async fn sparklines(&self, hosts: Vec<QueryHost>) -> Answer {
        let timezone = self.shared.settings.get().analytics_timezone.clone();
        self.read(true, move |conn, log| stats::sparklines(conn, log, crate::util::now(), &hosts, &timezone)).await
    }

    /// Filter autocomplete.
    pub async fn suggestions(&self, host: QueryHost, filter: String, params: HashMap<String, String>) -> Answer {
        let timezone = self.shared.settings.get().analytics_timezone.clone();
        self.read(false, move |conn, log| stats::suggestions(conn, log, crate::util::now(), &host, &filter, &params, &timezone)).await
    }

    /// Run a question on the analytics runtime's read connections, writing
    /// what's buffered first when the answer should include it.
    async fn read<F>(&self, flush_first: bool, run: F) -> Answer
    where
        F: FnOnce(&rusqlite::Connection, &Logger) -> Answer + Send + 'static,
    {
        let Some(worker) = self.worker() else {
            return starting();
        };

        let log = self.shared.log;
        let task = worker.runtime.spawn({
            let worker = worker.clone();
            async move {
                if flush_first {
                    // Whatever arrived in the last few seconds is still in memory.
                    let w = worker.clone();
                    let _ = tokio::task::spawn_blocking(move || flush(&w, &log)).await;
                }
                worker.reads.call_within(QUERY_TIMEOUT, move |conn| Ok(run(conn, &log))).await
            }
        });

        match task.await {
            Ok(Ok(Some(answer))) => answer,
            Ok(Ok(None)) => too_slow(),
            Ok(Err(error)) => {
                log.error("analytics_query_failed", crate::log::err_fields(error));
                Answer::error(StatusCode::INTERNAL_SERVER_ERROR, "The analytics query failed. The server log has the details.")
            }
            Err(_) => restarted(),
        }
    }
}

/// Everything the analytics runtime does, until it is told to stop.
async fn run(shared: &Arc<Shared>, stop: &CancellationToken) -> anyhow::Result<()> {
    let log = shared.log;
    let path = shared.config.data_dir.join("analytics.sqlite3");
    tokio::fs::create_dir_all(&shared.config.data_dir).await?;

    let (store, open, geo) = {
        let (path, geo) = (path.clone(), Geo::new(&shared.config.data_dir.join("geo"), log));
        tokio::task::spawn_blocking(move || {
            geo.load();
            let store = AnalyticsStore::new(crate::db::open_connection(&path)?)?;
            let mut open = store.open_sessions(crate::util::now() - SESSION_TIMEOUT)?;
            // Oldest first, so the cache ends up in the order it keeps itself in.
            open.sort_by_key(|s| s.timestamp);
            anyhow::Ok((store, open, geo))
        })
        .await??
    };
    let reads = Db::connect(&path)?;

    let mut sessions = SessionCache::default();
    let session_count = open.len();
    for session in open {
        sessions.put(session);
    }

    let worker = Arc::new(Worker {
        runtime: tokio::runtime::Handle::current(),
        ingest: Mutex::new(Ingest::new(sessions, geo.clone())),
        store: Mutex::new(store),
        flushing: Mutex::new(()),
        reads,
        geo: geo.clone(),
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
    let sweeper = {
        let worker = worker.clone();
        tokio::spawn(async move {
            let mut ticks = tokio::time::interval(SWEEP_EVERY);
            loop {
                ticks.tick().await;
                let w = worker.clone();
                let _ = tokio::task::spawn_blocking(move || w.ingest.lock().sessions.sweep(crate::util::now())).await;
            }
        })
    };

    log.info("analytics_started", fields! { "sessions" => session_count });

    stop.cancelled().await;

    flusher.abort();
    sweeper.abort();
    geo.stop();
    let w = worker.clone();
    tokio::task::spawn_blocking(move || flush(&w, &log)).await?;
    Ok(())
}

/// Who is at the other end of the connection an event arrived on, which
/// decides where the visitor's address comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Peer {
    /// The reverse proxy, whose `X-Forwarded-For` names the visitor.
    Proxy(IpAddr),
    /// The visitor: npanel terminated TLS itself, so there is no proxy and
    /// every header is the visitor's to write.
    Visitor(IpAddr),
    /// Nobody whose address means anything: Tor.
    Unknown,
}

/// The visitor's address, used to tell visitors apart and to place them, and
/// then forgotten.
///
/// Behind a proxy it is the proxy's word for it, since the socket is always
/// the proxy: the first `X-Forwarded-For` entry, which behind a CDN is still
/// the visitor where the last would be the CDN. Without one, the headers are
/// the sender's to write and only the socket can be believed — and over Tor
/// there is not even that, so there is no address at all.
fn client_ip(forwarded_for: Option<&str>, real_ip: Option<&str>, peer: Peer) -> String {
    match peer {
        Peer::Proxy(proxy) => {
            let forwarded = forwarded_for.and_then(|f| f.split(',').next()).map(str::trim).filter(|f| !f.is_empty());
            let real = real_ip.map(str::trim).filter(|r| !r.is_empty());
            forwarded.or(real).map_or_else(|| proxy.to_string(), str::to_string)
        }
        Peer::Visitor(visitor) => visitor.to_string(),
        Peer::Unknown => String::new(),
    }
}

/// Write what's pending. The in-memory state is only held long enough to take
/// it, so events go on being recorded while the transaction runs.
fn flush(worker: &Worker, log: &Logger) {
    let _flushing = worker.flushing.lock();
    let pending = worker.ingest.lock().take_pending();
    if pending.dropped > 0 {
        log.warn("analytics_buffer_full", fields! { "dropped" => pending.dropped });
    }
    if pending.events.is_empty() && pending.sessions.is_empty() {
        return;
    }
    match worker.store.lock().write(&pending.events, &pending.sessions) {
        Ok(()) => log.debug("analytics_flushed", fields! { "events" => pending.events.len(), "sessions" => pending.sessions.len() }),
        Err(error) => log.error("analytics_flush_failed", with_err(fields! {}, error)),
    }
}

/// Read which hosts are counting, and start keeping the location database
/// current the first time any is.
async fn refresh_hosts(shared: &Arc<Shared>) {
    let rows = shared
        .db
        .call(|conn| {
            Ok(conn
                .prepare("SELECT id, hostname, enabled, analytics FROM hosts")?
                .query_map([], |r| {
                    Ok(AnalyticsHost { id: r.get(0)?, hostname: r.get(1)?, counting: r.get::<_, bool>(2)? && r.get::<_, bool>(3)? })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .await;

    let rows = match rows {
        Ok(rows) => rows,
        Err(error) => {
            shared.log.error("analytics_hosts_failed", with_err(fields! {}, error));
            return;
        }
    };
    let counting = rows.iter().any(|h| h.counting);
    *shared.hosts.write() = Arc::new(rows.into_iter().map(|h| (h.hostname.clone(), h)).collect());

    // Fetched only once something is counting: a gateway that never turns
    // analytics on never downloads a location database.
    if counting && shared.config.download_geo {
        if let Some(worker) = shared.worker.read().clone() {
            worker.geo.keep_updated(&worker.runtime);
        }
    }
}

/// A request body as text, or `None` if it is longer than `max` bytes. A
/// declared length over the limit is refused before a byte is read.
pub async fn read_capped(headers: &HeaderMap, body: Body, max: usize) -> Option<String> {
    let declared = headers.get(header::CONTENT_LENGTH).and_then(|v| v.to_str().ok()).and_then(|v| v.parse::<usize>().ok());
    if declared.is_some_and(|d| d > max) {
        return None;
    }
    let limited = http_body_util::Limited::new(body, max);
    let bytes = limited.collect().await.ok()?.to_bytes();
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_ips() {
        let proxy = Peer::Proxy(IpAddr::from([9, 9, 9, 9]));
        assert_eq!(client_ip(Some("1.2.3.4, 10.0.0.1"), Some("5.6.7.8"), proxy), "1.2.3.4");
        assert_eq!(client_ip(None, Some(" 5.6.7.8 "), proxy), "5.6.7.8");
        assert_eq!(client_ip(Some(""), None, proxy), "9.9.9.9");
        let visitor = Peer::Visitor(IpAddr::from([9, 9, 9, 9]));
        assert_eq!(client_ip(Some("1.2.3.4"), Some("5.6.7.8"), visitor), "9.9.9.9");
        assert_eq!(client_ip(Some("1.2.3.4"), Some("5.6.7.8"), Peer::Unknown), "");
    }
}
