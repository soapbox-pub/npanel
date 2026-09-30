//! npanel's Plausible server, on a thread of its own.
//!
//! Serving sites is always the first priority, so nothing about counting
//! visits runs on the threads that serve them: not the parsing, not the
//! user-agent and location lookups, not the writes, and not the dashboard's
//! queries. Events arrive at `/.npanel/api/event` on every site, on whichever
//! listener serves it; each is read there and handed across whole, a few
//! hundred bytes.
//!
//! The analytics runtime owns its database file and its caches. If it dies it
//! is started again with backoff, and sites go on being served either way.

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
use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use http::{header, HeaderMap, Method, Request, Response};
use http_body_util::BodyExt;
use parking_lot::{Mutex, RwLock};
use tokio_util::sync::CancellationToken;

use self::geo::Geo;
use self::ingest::{AnalyticsHost, Ingest, Sender};
use self::query::engine::QueryHost;
use self::salts::Salts;
use self::sessions::{SessionCache, SESSION_TIMEOUT};
use self::store::AnalyticsStore;
use self::user_agent::UserAgents;
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

const RESTART_MIN: Duration = Duration::from_secs(1);
const RESTART_MAX: Duration = Duration::from_secs(60);
const HEALTHY_AFTER: Duration = Duration::from_secs(60);

/// How long an event forwarded to the analytics runtime may take.
const EVENT_TIMEOUT: Duration = Duration::from_secs(10);
/// How long a dashboard query may take. A year of a busy site is a lot of rows.
const QUERY_TIMEOUT: Duration = Duration::from_secs(60);

/// An answer, as an HTTP response to pass on as it is.
#[derive(Clone, Debug)]
pub struct WorkerResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl WorkerResponse {
    pub fn new(status: u16, content_type: &str, body: String) -> Self {
        Self { status, headers: vec![("content-type".into(), content_type.into())], body }
    }

    fn failure(status: u16, error: &str) -> Self {
        Self::new(status, "application/json", serde_json::json!({ "error": error }).to_string())
    }

    pub fn into_response(self, extra: &[(&str, &str)]) -> Response<Body> {
        let mut builder = Response::builder().status(self.status);
        for (name, value) in extra {
            builder = builder.header(*name, *value);
        }
        for (name, value) in &self.headers {
            builder = builder.header(name, value);
        }
        let body = if self.body.is_empty() { Body::empty() } else { Body::from(self.body) };
        builder.body(body).expect("response")
    }
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
    ingest: Mutex<Ingest>,
    /// Connections for the dashboard's queries, apart from the one that writes.
    reads: Db,
    geo: Geo,
}

struct Shared {
    db: Db,
    settings: Arc<Settings>,
    log: Logger,
    config: AnalyticsConfig,
    /// Every host by hostname, and whether events for it are kept.
    hosts: RwLock<HashMap<String, AnalyticsHost>>,
    worker: RwLock<Option<Arc<Worker>>>,
}

pub struct Analytics {
    shared: Arc<Shared>,
    shutdown: CancellationToken,
    thread: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl Analytics {
    /// Start the analytics runtime. Returns at once; until it is ready,
    /// everything asked of it is answered `503`.
    pub fn start(db: Db, settings: Arc<Settings>, log: Logger, config: AnalyticsConfig) -> Analytics {
        let shared = Arc::new(Shared {
            db,
            settings,
            log,
            config,
            hosts: RwLock::new(HashMap::new()),
            worker: RwLock::new(None),
        });
        let shutdown = CancellationToken::new();

        let (s, stop) = (shared.clone(), shutdown.clone());
        let thread = std::thread::Builder::new()
            .name("npanel-analytics".into())
            .spawn(move || {
                let mut delay = RESTART_MIN;
                loop {
                    let started = Instant::now();
                    let runtime = tokio::runtime::Builder::new_multi_thread()
                        .worker_threads(2)
                        .thread_name("npanel-analytics")
                        .enable_all()
                        .build()
                        .expect("an analytics runtime");

                    let result = std::panic::catch_unwind(AssertUnwindSafe(|| runtime.block_on(run(&s, &stop))));
                    *s.worker.write() = None;
                    runtime.shutdown_timeout(Duration::from_secs(5));

                    if stop.is_cancelled() {
                        return;
                    }
                    let reason = match result {
                        Ok(Ok(())) => "stopped".to_string(),
                        Ok(Err(error)) => error.to_string(),
                        Err(_) => "panicked".to_string(),
                    };
                    if started.elapsed() > HEALTHY_AFTER {
                        delay = RESTART_MIN;
                    }
                    s.log.error("analytics_thread_exited", fields! { "err_msg" => reason, "restart_in_ms" => delay.as_millis() as u64 });
                    std::thread::sleep(delay);
                    delay = (delay * 2).min(RESTART_MAX);
                }
            })
            .expect("an analytics thread");

        Analytics { shared, shutdown, thread: Mutex::new(Some(thread)) }
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
    /// the analytics runtime. `peer` is the connection's address, which only
    /// counts when the proxy didn't name the visitor; a listener whose peer
    /// is never the visitor — Tor's — passes `None`.
    pub async fn forward_event(&self, request: Request<Body>, peer: Option<IpAddr>) -> Response<Body> {
        let cors = [
            ("access-control-allow-origin", "*"),
            ("access-control-allow-methods", "POST, OPTIONS"),
            ("access-control-allow-headers", "content-type"),
        ];
        let (parts, body) = request.into_parts();

        let body = if parts.method == Method::POST {
            match read_capped(&parts.headers, body, MAX_EVENT_BODY).await {
                Some(body) => body,
                None => {
                    return WorkerResponse::new(413, "text/plain; charset=utf-8", "Request body is too large.".into()).into_response(&cors);
                }
            }
        } else {
            String::new()
        };

        let Some(worker) = self.worker() else {
            return WorkerResponse::failure(503, "Analytics are starting. Try again in a moment.").into_response(&cors);
        };

        let shared = self.shared.clone();
        let headers = parts.headers;
        let method = parts.method;
        let peer = peer.map(|ip| ip.to_string()).unwrap_or_default();
        let task = worker.runtime.spawn(async move { receive(&shared, &method, &headers, body, &peer).await });
        let response = match tokio::time::timeout(EVENT_TIMEOUT, task).await {
            Ok(Ok(response)) => response,
            Ok(Err(_)) => WorkerResponse::failure(503, "Analytics restarted before answering. Try again in a moment."),
            Err(_) => WorkerResponse::failure(504, "Analytics took too long to answer. Try a shorter date range."),
        };
        response.into_response(&cors)
    }

    /// Answer a dashboard query, Plausible's `POST /api/stats/:domain/query`.
    pub async fn query(&self, host: QueryHost, body: serde_json::Value) -> WorkerResponse {
        let timezone = self.shared.settings.get().analytics_timezone.clone();
        self.read(true, move |conn, log| stats::query(conn, log, crate::util::now(), &host, &body, &timezone)).await
    }

    /// Visitors in the last five minutes.
    pub async fn current_visitors(&self, host: QueryHost) -> WorkerResponse {
        self.read(true, move |conn, log| stats::current_visitors(conn, log, crate::util::now(), &host)).await
    }

    /// Each site's visitors over the last 24 hours, hour by hour.
    pub async fn sparklines(&self, hosts: Vec<QueryHost>) -> WorkerResponse {
        let timezone = self.shared.settings.get().analytics_timezone.clone();
        self.read(true, move |conn, log| stats::sparklines(conn, log, crate::util::now(), &hosts, &timezone)).await
    }

    /// Filter autocomplete.
    pub async fn suggestions(&self, host: QueryHost, filter: String, params: HashMap<String, String>) -> WorkerResponse {
        let timezone = self.shared.settings.get().analytics_timezone.clone();
        self.read(false, move |conn, log| stats::suggestions(conn, log, crate::util::now(), &host, &filter, &params, &timezone)).await
    }

    /// Run a question on the analytics runtime's read connections, writing
    /// what's buffered first when the answer should include it.
    async fn read<F>(&self, flush_first: bool, run: F) -> WorkerResponse
    where
        F: FnOnce(&rusqlite::Connection, &Logger) -> WorkerResponse + Send + 'static,
    {
        let Some(worker) = self.worker() else {
            return WorkerResponse::failure(503, "Analytics are starting. Try again in a moment.");
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
            Ok(Ok(Some(response))) => response,
            Ok(Ok(None)) => WorkerResponse::failure(504, "Analytics took too long to answer. Try a shorter date range."),
            Ok(Err(error)) => {
                log.error("analytics_query_failed", crate::log::err_fields(error));
                WorkerResponse::failure(500, "The analytics query failed. The server log has the details.")
            }
            Err(_) => WorkerResponse::failure(503, "Analytics restarted before answering. Try again in a moment."),
        }
    }
}

/// Everything the analytics runtime does, until it is told to stop.
async fn run(shared: &Arc<Shared>, stop: &CancellationToken) -> anyhow::Result<()> {
    let log = shared.log;
    let path = shared.config.data_dir.join("analytics.sqlite3");
    std::fs::create_dir_all(&shared.config.data_dir)?;

    let store = AnalyticsStore::new(crate::db::open_connection(&path)?)?;
    let reads = Db::connect(&path)?;
    let geo = Geo::new(&shared.config.data_dir.join("geo"), log);
    {
        let geo = geo.clone();
        tokio::task::spawn_blocking(move || geo.load()).await?;
    }

    let mut sessions = SessionCache::default();
    let mut open = store.open_sessions(crate::util::now() - SESSION_TIMEOUT)?;
    // Oldest first, so the cache ends up in the order it keeps itself in.
    open.sort_by_key(|s| s.timestamp);
    for session in open {
        sessions.put(session);
    }
    let session_count = sessions.len();

    let worker = Arc::new(Worker {
        runtime: tokio::runtime::Handle::current(),
        ingest: Mutex::new(Ingest { store, sessions, salts: Salts::default(), user_agents: UserAgents::default(), geo: geo.clone() }),
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
                worker.ingest.lock().sessions.sweep(crate::util::now());
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

/// Answer one request for the event endpoint, however it arrived. The tracker
/// posts `text/plain`, which spares it a preflight, but a client of
/// Plausible's API may send JSON and preflight it, so both are met.
async fn receive(shared: &Arc<Shared>, method: &Method, headers: &HeaderMap, body: String, socket_ip: &str) -> WorkerResponse {
    if method == Method::OPTIONS {
        return WorkerResponse { status: 204, headers: Vec::new(), body: String::new() };
    }
    if method != Method::POST {
        return WorkerResponse {
            status: 405,
            headers: vec![("allow".into(), "POST, OPTIONS".into())],
            body: "Send events with POST.".into(),
        };
    }

    let Some(worker) = shared.worker.read().clone() else {
        return WorkerResponse::failure(503, "Analytics are starting. Try again in a moment.");
    };

    let header = |name: &str| headers.get(name).and_then(|v| v.to_str().ok()).map(str::to_string);
    let user_agent = header("user-agent").unwrap_or_default();
    let ip = client_ip(header("x-forwarded-for").as_deref(), header("x-real-ip").as_deref(), socket_ip);
    let hosts = shared.hosts.read().clone();
    let log = shared.log;

    // User-agent parsing is real work, so it stays off the runtime threads.
    tokio::task::spawn_blocking(move || {
        let (response, full) =
            worker.ingest.lock().handle(&body, &Sender { ip: &ip, user_agent: &user_agent }, &hosts, crate::util::now());
        if full {
            flush(&worker, &log);
        }
        response
    })
    .await
    .unwrap_or_else(|_| WorkerResponse::failure(500, "Something went wrong."))
}

/// The visitor's address, used to tell visitors apart and to place them, and
/// then forgotten. The proxy's word for it, since behind one the socket is
/// always the proxy.
fn client_ip(forwarded_for: Option<&str>, real_ip: Option<&str>, socket: &str) -> String {
    let forwarded = forwarded_for.and_then(|f| f.split(',').next()).map(str::trim).filter(|f| !f.is_empty());
    let real = real_ip.map(str::trim).filter(|r| !r.is_empty());
    forwarded.or(real).unwrap_or(socket).to_string()
}

fn flush(worker: &Worker, log: &Logger) {
    let mut ingest = worker.ingest.lock();
    match ingest.store.flush() {
        Ok((events, sessions)) => {
            if events > 0 || sessions > 0 {
                log.debug("analytics_flushed", fields! { "events" => events, "sessions" => sessions });
            }
        }
        Err(error) => log.error("analytics_flush_failed", with_err(fields! {}, error)),
    }
    if ingest.store.dropped > 0 {
        log.warn("analytics_buffer_full", fields! { "dropped" => ingest.store.dropped });
        ingest.store.dropped = 0;
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
                    Ok(AnalyticsHost {
                        id: r.get(0)?,
                        hostname: r.get(1)?,
                        counting: r.get::<_, i64>(2)? == 1 && r.get::<_, i64>(3)? == 1,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .await;

    let Ok(rows) = rows else { return };
    let counting = rows.iter().any(|h| h.counting);
    *shared.hosts.write() = rows.into_iter().map(|h| (h.hostname.clone(), h)).collect();

    // Fetched only once something is counting: a gateway that never turns
    // analytics on never downloads a location database.
    if counting && shared.config.download_geo {
        if let Some(worker) = shared.worker.read().clone() {
            let geo = worker.geo.clone();
            worker.runtime.spawn(async move { geo.keep_updated() });
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
        assert_eq!(client_ip(Some("1.2.3.4, 10.0.0.1"), Some("5.6.7.8"), "9.9.9.9"), "1.2.3.4");
        assert_eq!(client_ip(None, Some(" 5.6.7.8 "), "9.9.9.9"), "5.6.7.8");
        assert_eq!(client_ip(Some(""), None, "9.9.9.9"), "9.9.9.9");
    }
}
