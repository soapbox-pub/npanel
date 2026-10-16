//! The gateway's half of a tunnel: which hostnames are tunnels, the agents
//! connected for them, the agents waiting to be authorized, and passing a
//! visitor's request down a connection.
//!
//! Which hostnames are tunnels is reconciled, like everything else npanel
//! serves: every change to the table — an approval, an edit, a revocation —
//! reads the enabled tunnels afresh and drops the connections of any that are
//! no longer among them. A request looks its hostname up in a map swapped in
//! whole, and never waits on the database.

use std::collections::{HashMap, HashSet};
use std::net::IpAddr;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use anyhow::Result;
use arc_swap::ArcSwap;
use axum::body::Body;
use bytes::Bytes;
use http::{header, HeaderValue, Method, Request, Response, StatusCode, Version};
use hyper::body::{Frame, SizeHint};
use hyper::client::conn::http2::SendRequest;
use hyper_util::rt::{TokioExecutor, TokioIo, TokioTimer};
use parking_lot::Mutex;
use serde::Serialize;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use super::{is_websocket, strip_hop_by_hop, CODE_ALPHABET, KEEP_ALIVE, PROTOCOL};
use crate::db::{Db, Tunnel};
use crate::fields;
use crate::log::Logger;
use crate::rate_limit::{RateLimit, RateLimiter};
use crate::serve::RequestOrigin;

/// How long the origin has to start answering once the last of the request
/// has gone by. Cloudflare's figure: long enough for a slow page, short
/// enough that a hung one gets an answer.
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(100);

/// How long an agent waits to be authorized before its code is forgotten.
const REQUEST_LIFETIME: i64 = 15 * 60;

/// How many agents may wait at once. Asking costs nothing but a key, so the
/// list is bounded, and the oldest is forgotten to make room.
const MAX_PENDING: usize = 64;

/// How many of them may have asked from one address, so that one address
/// asking as fast as it can pushes out only its own requests.
const MAX_PENDING_PER_ADDRESS: usize = 3;

/// How often one address may ask: ten times, refilled over ten minutes. An
/// agent asks once when it starts and once when its code runs out.
const ASKING: RateLimit = RateLimit { capacity: 10.0, interval_ms: 10.0 * 60.0 * 1000.0 };

/// Every header a proxy or CDN might name the visitor's address in. The
/// origin is told one address, npanel's, and a visitor writing any of these
/// must not be able to tell it another.
const CLIENT_ADDRESS_HEADERS: &[&str] =
    &["forwarded", "x-forwarded-for", "x-real-ip", "true-client-ip", "cf-connecting-ip", "fastly-client-ip", "x-client-ip", "x-cluster-client-ip"];

pub struct Tunnels {
    db: Db,
    log: Logger,
    /// Enabled tunnels' hostnames, to their ids.
    routes: ArcSwap<HashMap<String, i64>>,
    /// Open connections, by tunnel.
    live: Mutex<HashMap<i64, Vec<Live>>>,
    /// Connections counted against each tunnel's limit: those open, and
    /// those still upgrading, which `live` doesn't have yet.
    slots: Mutex<HashMap<i64, usize>>,
    /// Numbers connections, and picks the next one to send a request down.
    counter: AtomicU64,
    pending: Mutex<Vec<Pending>>,
    /// Asking to be authorized, metered by address.
    asking: Mutex<RateLimiter>,
    /// The signatures connections were let in on, until each is too old to
    /// be accepted anyway, so that one seen in a log can't be used again.
    used: Mutex<HashMap<String, i64>>,
}

struct Live {
    id: u64,
    send: SendRequest<Body>,
    info: ConnectionInfo,
    stop: CancellationToken,
}

/// One open connection, as the dashboard shows it.
#[derive(Clone, Debug, Serialize)]
pub struct ConnectionInfo {
    pub connected_at: i64,
    /// Where it came from, as far as npanel can tell.
    pub remote: Option<String>,
    /// The agent's `User-Agent`.
    pub agent: Option<String>,
}

/// An agent waiting for an admin to authorize it.
#[derive(Clone, Debug, Serialize)]
pub struct Pending {
    /// What the agent shows, and the admin is asked to match.
    pub code: String,
    pub pubkey: String,
    pub name: String,
    pub target: String,
    pub remote: Option<String>,
    pub created_at: i64,
    pub expires_at: i64,
    /// Refused, and kept only until the agent has been told so.
    #[serde(skip)]
    pub denied: bool,
}

/// What an agent that isn't a tunnel yet is told about itself.
pub enum Waiting {
    Pending(Pending),
    Denied,
}

impl Tunnels {
    pub fn new(db: Db, log: Logger) -> Result<Tunnels> {
        let routes = db.with(|conn| Ok(load_routes(conn)?))?;
        Ok(Tunnels {
            db,
            log,
            routes: ArcSwap::from_pointee(routes),
            live: Mutex::new(HashMap::new()),
            slots: Mutex::new(HashMap::new()),
            counter: AtomicU64::new(0),
            pending: Mutex::new(Vec::new()),
            asking: Mutex::new(RateLimiter::new(4096)),
            used: Mutex::new(HashMap::new()),
        })
    }

    /// Read the enabled tunnels afresh, and drop the connections of every
    /// one that isn't among them any more.
    pub async fn reconcile(&self) -> Result<()> {
        let routes = self.db.call(|conn| Ok(load_routes(conn)?)).await?;
        let enabled: HashSet<i64> = routes.values().copied().collect();
        self.routes.store(Arc::new(routes));

        let dropped: Vec<Live> = {
            let mut live = self.live.lock();
            let gone: Vec<i64> = live.keys().filter(|id| !enabled.contains(id)).copied().collect();
            gone.into_iter().flat_map(|id| live.remove(&id).unwrap_or_default()).collect()
        };
        for connection in dropped {
            connection.stop.cancel();
        }
        Ok(())
    }

    /// The tunnel serving `hostname`, if one does.
    pub fn route(&self, hostname: &str) -> Option<i64> {
        self.routes.load().get(hostname).copied()
    }

    /// The connections a tunnel has open.
    pub fn connections(&self, tunnel: i64) -> Vec<ConnectionInfo> {
        self.live.lock().get(&tunnel).map(|live| live.iter().map(|c| c.info.clone()).collect()).unwrap_or_default()
    }

    /// Answer an agent's request to connect, which it has already proved
    /// is `tunnel`'s, by upgrading the connection and opening HTTP/2 on it.
    /// Every agent with its key shares `max_connections`.
    pub fn accept(self: &Arc<Self>, tunnel: &Tunnel, mut request: Request<Body>, remote: Option<IpAddr>, max_connections: usize) -> Response<Body> {
        let asks = request.headers().get(header::UPGRADE).and_then(|v| v.to_str().ok()).is_some_and(|v| v.eq_ignore_ascii_case(PROTOCOL));
        if !asks || request.version() != Version::HTTP_11 {
            return text(StatusCode::BAD_REQUEST, "Connect with npanel tunnel: this address only accepts its connections.");
        }
        let slot = {
            let mut slots = self.slots.lock();
            let taken = slots.entry(tunnel.id).or_default();
            if *taken >= max_connections {
                return text(
                    StatusCode::TOO_MANY_REQUESTS,
                    format!(
                        "This tunnel already has {max_connections} connections, as many as the server allows. Run fewer agents with its key or give them fewer --connections, or have an admin raise the limit in Settings."
                    ),
                );
            }
            *taken += 1;
            Slot { tunnels: self.clone(), tunnel: tunnel.id }
        };

        let info = ConnectionInfo {
            connected_at: crate::util::now(),
            remote: remote.map(|ip| ip.to_string()),
            agent: request.headers().get(header::USER_AGENT).and_then(|v| v.to_str().ok()).map(|v| v.chars().take(100).collect()),
        };
        let upgrade = hyper::upgrade::on(&mut request);
        let (this, id, hostname) = (self.clone(), tunnel.id, tunnel.hostname.clone());
        tokio::spawn(async move {
            let _slot = slot;
            let Ok(upgraded) = upgrade.await else { return };
            let handshake = hyper::client::conn::http2::Builder::new(TokioExecutor::new())
                .timer(TokioTimer::new())
                .keep_alive_interval(KEEP_ALIVE)
                .keep_alive_timeout(KEEP_ALIVE)
                .keep_alive_while_idle(true)
                .adaptive_window(true)
                .handshake::<_, Body>(upgraded)
                .await;
            let (send, connection) = match handshake {
                Ok(handshake) => handshake,
                Err(error) => {
                    this.log.warn("tunnel_handshake_failed", crate::log::with_err(fields! { "hostname" => hostname.as_str() }, error));
                    return;
                }
            };
            let stop = CancellationToken::new();
            let number = this.counter.fetch_add(1, Ordering::Relaxed);
            {
                let mut live = this.live.lock();
                // Disabled or revoked while it was upgrading: reconciling
                // has already been, and wouldn't see this one.
                if !this.routes.load().values().any(|t| *t == id) {
                    return;
                }
                live.entry(id).or_default().push(Live { id: number, send, info: info.clone(), stop: stop.clone() });
            }
            this.log.info("tunnel_connected", fields! { "hostname" => hostname.as_str(), "remote" => info.remote.clone() });

            tokio::select! {
                _ = connection => {}
                _ = stop.cancelled() => {}
            }

            {
                let mut live = this.live.lock();
                if let Some(list) = live.get_mut(&id) {
                    list.retain(|c| c.id != number);
                    if list.is_empty() {
                        live.remove(&id);
                    }
                }
            }
            this.log.info("tunnel_disconnected", fields! { "hostname" => hostname.as_str(), "remote" => info.remote });
        });

        Response::builder()
            .status(StatusCode::SWITCHING_PROTOCOLS)
            .header(header::CONNECTION, "upgrade")
            .header(header::UPGRADE, PROTOCOL)
            .body(Body::empty())
            .expect("response")
    }

    /// A connection to send a request down: each in turn, skipping any that
    /// have closed.
    fn sender(&self, tunnel: i64) -> Option<SendRequest<Body>> {
        let live = self.live.lock();
        let open: Vec<&Live> = live.get(&tunnel)?.iter().filter(|c| !c.send.is_closed()).collect();
        if open.is_empty() {
            return None;
        }
        let turn = self.counter.fetch_add(1, Ordering::Relaxed) as usize;
        Some(open[turn % open.len()].send.clone())
    }

    /// Pass a visitor's request to the agent serving `tunnel`, and its
    /// answer back. `visitor` is the visitor's address as npanel believes
    /// it, and the only one the origin is told: it sees every request come
    /// from the agent, so a forwarding header is all it has to go on.
    pub async fn proxy(&self, tunnel: i64, mut request: Request<Body>, origin: &RequestOrigin, visitor: Option<IpAddr>) -> Response<Body> {
        let Some(mut send) = self.sender(tunnel) else {
            let mut response = text(StatusCode::BAD_GATEWAY, "This site is offline right now. Try again in a moment.");
            response.headers_mut().insert(header::RETRY_AFTER, HeaderValue::from_static("10"));
            return response;
        };

        let websocket = is_websocket(request.headers()).then(|| hyper::upgrade::on(&mut request));
        let (mut parts, body) = request.into_parts();
        strip_hop_by_hop(&mut parts.headers);
        parts.headers.remove(header::HOST);
        let headers = &mut parts.headers;
        for name in CLIENT_ADDRESS_HEADERS {
            headers.remove(*name);
        }
        if let Some(visitor) = visitor {
            let visitor = HeaderValue::from_str(&visitor.to_string()).expect("an address is a header value");
            headers.insert("x-forwarded-for", visitor.clone());
            headers.insert("x-real-ip", visitor);
        }
        headers.insert("x-forwarded-proto", HeaderValue::from_static(origin.scheme));
        headers.insert("x-forwarded-host", HeaderValue::from_str(&origin.hostname).expect("a hostname is a header value"));

        let path = parts.uri.path_and_query().map_or("/", |pq| pq.as_str());
        let Ok(uri) = format!("{}://{}{path}", origin.scheme, origin.hostname).parse::<http::Uri>() else {
            return text(StatusCode::BAD_REQUEST, "That path can't be passed on.");
        };
        let mut outbound = Request::builder().uri(uri).version(Version::HTTP_2);
        *outbound.headers_mut().expect("a builder with no error yet") = parts.headers;
        let outbound = match websocket {
            // The upgrade becomes a stream of its own, which the agent opens
            // to its origin as the upgrade it was.
            Some(_) => {
                let mut outbound = outbound.method(Method::CONNECT).body(Body::empty()).expect("request");
                outbound.extensions_mut().insert(hyper::ext::Protocol::from_static("websocket"));
                outbound
            }
            None => outbound.method(parts.method).body(body).expect("request"),
        };
        let (progress, sending) = watch::channel(());
        let outbound = outbound.map(|body| Body::new(Watched { body, progress }));

        let sent = tokio::select! {
            sent = async {
                send.ready().await?;
                send.send_request(outbound).await
            } => Some(sent),
            _ = answer_due(sending) => None,
        };
        let mut response = match sent {
            Some(Ok(response)) => response,
            Some(Err(error)) => {
                self.log.debug("tunnel_request_failed", crate::log::with_err(fields! { "hostname" => origin.hostname.as_str() }, error));
                return text(StatusCode::BAD_GATEWAY, "This site's connection dropped. Try again in a moment.");
            }
            None => return text(StatusCode::GATEWAY_TIMEOUT, "This site took too long to answer."),
        };

        if let Some(visitor_upgrade) = websocket {
            if !response.status().is_success() {
                return response.map(Body::new);
            }
            let agent_upgrade = hyper::upgrade::on(&mut response);
            tokio::spawn(async move {
                let (Ok(agent), Ok(visitor)) = tokio::join!(agent_upgrade, visitor_upgrade) else { return };
                let _ = tokio::io::copy_bidirectional(&mut TokioIo::new(agent), &mut TokioIo::new(visitor)).await;
            });
            let (mut parts, _) = response.into_parts();
            strip_hop_by_hop(&mut parts.headers);
            parts.status = StatusCode::SWITCHING_PROTOCOLS;
            parts.version = Version::HTTP_11;
            parts.headers.insert(header::CONNECTION, HeaderValue::from_static("upgrade"));
            parts.headers.insert(header::UPGRADE, HeaderValue::from_static("websocket"));
            return Response::from_parts(parts, Body::empty());
        }

        strip_hop_by_hop(response.headers_mut());
        response.map(Body::new)
    }

    /// Note that an agent is waiting, or refresh what it said if it already
    /// was, and hand back what it should show.
    /// `None` when the address it asked from has asked too much.
    pub fn request(&self, pubkey: &str, name: String, target: String, remote: Option<IpAddr>) -> Option<Pending> {
        let remote = remote.map(|ip| ip.to_string());
        if !self.asking.lock().take(remote.as_deref().unwrap_or_default(), ASKING) {
            return None;
        }
        let now = crate::util::now();
        let mut pending = self.pending.lock();
        pending.retain(|p| p.expires_at > now && !(p.denied && p.pubkey == pubkey));
        if let Some(existing) = pending.iter_mut().find(|p| p.pubkey == pubkey) {
            existing.name = name;
            existing.target = target;
            existing.remote = remote;
            return Some(existing.clone());
        }
        if pending.iter().filter(|p| p.remote == remote).count() >= MAX_PENDING_PER_ADDRESS {
            return None;
        }
        if pending.len() >= MAX_PENDING {
            pending.remove(0);
        }
        let taken: HashSet<String> = pending.iter().map(|p| p.code.clone()).collect();
        let code = loop {
            let code = new_code();
            if !taken.contains(&code) {
                break code;
            }
        };
        let entry = Pending {
            code,
            pubkey: pubkey.to_string(),
            name,
            target,
            remote,
            created_at: now,
            expires_at: now + REQUEST_LIFETIME,
            denied: false,
        };
        pending.push(entry.clone());
        Some(entry)
    }

    /// Whether this is the first time a connection has been let in on
    /// `signature`, which is good until `expires_at`.
    pub fn first_use(&self, signature: &str, expires_at: i64) -> bool {
        let now = crate::util::now();
        let mut used = self.used.lock();
        used.retain(|_, expires| *expires >= now);
        used.insert(signature.to_string(), expires_at).is_none()
    }

    /// Every agent still waiting, oldest first.
    pub fn pending(&self) -> Vec<Pending> {
        let now = crate::util::now();
        let mut pending = self.pending.lock();
        pending.retain(|p| p.expires_at > now);
        pending.iter().filter(|p| !p.denied).cloned().collect()
    }

    /// The waiting agent showing `code`, however it was typed.
    pub fn find(&self, code: &str) -> Option<Pending> {
        let code = normalize_code(code)?;
        self.pending().into_iter().find(|p| p.code == code)
    }

    /// Forget an agent's request, now that it has been answered.
    pub fn resolve(&self, pubkey: &str) {
        self.pending.lock().retain(|p| p.pubkey != pubkey);
    }

    /// Refuse the agent showing `code`, which hears so the next time it asks.
    pub fn deny(&self, code: &str) -> Option<Pending> {
        let code = normalize_code(code)?;
        let now = crate::util::now();
        let mut pending = self.pending.lock();
        let entry = pending.iter_mut().find(|p| p.code == code && !p.denied && p.expires_at > now)?;
        entry.denied = true;
        Some(entry.clone())
    }

    /// Where an agent that isn't a tunnel stands. A refusal is told once.
    pub fn waiting(&self, pubkey: &str) -> Option<Waiting> {
        let now = crate::util::now();
        let mut pending = self.pending.lock();
        pending.retain(|p| p.expires_at > now);
        let at = pending.iter().position(|p| p.pubkey == pubkey)?;
        if pending[at].denied {
            pending.remove(at);
            return Some(Waiting::Denied);
        }
        Some(Waiting::Pending(pending[at].clone()))
    }
}

/// A connection's place under its tunnel's limit, from the moment it is
/// let in until it closes, however it ends.
struct Slot {
    tunnels: Arc<Tunnels>,
    tunnel: i64,
}

impl Drop for Slot {
    fn drop(&mut self) {
        let mut slots = self.tunnels.slots.lock();
        if let Some(taken) = slots.get_mut(&self.tunnel) {
            *taken -= 1;
            if *taken == 0 {
                slots.remove(&self.tunnel);
            }
        }
    }
}

fn load_routes(conn: &rusqlite::Connection) -> rusqlite::Result<HashMap<String, i64>> {
    conn.prepare_cached("SELECT hostname, id FROM tunnels WHERE enabled = 1")?.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect()
}

/// Eight letters, shown as two groups of four.
fn new_code() -> String {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    let letters: String = (0..8).map(|_| CODE_ALPHABET[rng.gen_range(0..CODE_ALPHABET.len())] as char).collect();
    format!("{}-{}", &letters[..4], &letters[4..])
}

/// A code as an admin might type it — lowercase, without its dash, with
/// spaces — in the form it was shown.
fn normalize_code(input: &str) -> Option<String> {
    let letters: String = input.chars().filter(|c| c.is_ascii_alphabetic()).map(|c| c.to_ascii_uppercase()).collect();
    (letters.len() == 8).then(|| format!("{}-{}", &letters[..4], &letters[4..]))
}

fn text(status: StatusCode, body: impl Into<Body>) -> Response<Body> {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .body(body.into())
        .expect("response")
}

/// A request body that reports each frame of it the agent takes, and is
/// dropped once it has all gone.
struct Watched {
    body: Body,
    progress: watch::Sender<()>,
}

impl hyper::body::Body for Watched {
    type Data = Bytes;
    type Error = axum::Error;

    fn poll_frame(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Result<Frame<Bytes>, axum::Error>>> {
        let frame = hyper::body::Body::poll_frame(Pin::new(&mut self.body), cx);
        if let Poll::Ready(Some(Ok(_))) = frame {
            self.progress.send_replace(());
        }
        frame
    }

    fn is_end_stream(&self) -> bool {
        hyper::body::Body::is_end_stream(&self.body)
    }

    fn size_hint(&self) -> SizeHint {
        hyper::body::Body::size_hint(&self.body)
    }
}

/// When the origin has had [`RESPONSE_TIMEOUT`] to answer. The clock starts
/// over with each frame of the request body, and runs out for good once the
/// body has gone: an upload that takes ten minutes is the visitor's time,
/// not the origin's. An agent that stops taking the body runs it out too.
async fn answer_due(mut sending: watch::Receiver<()>) {
    loop {
        match tokio::time::timeout(RESPONSE_TIMEOUT, sending.changed()).await {
            Ok(Ok(())) => continue,
            // The body was dropped: sent, or abandoned.
            Ok(Err(_)) => return tokio::time::sleep(RESPONSE_TIMEOUT).await,
            Err(_) => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tunnels() -> (Tunnels, crate::db::testing::TempDb) {
        let t = crate::db::testing::temp_db();
        (Tunnels::new(t.db.clone(), Logger::new(crate::log::LogLevel::Error)).unwrap(), t)
    }

    #[test]
    fn codes_read_however_they_are_typed() {
        let code = new_code();
        assert_eq!(code.len(), 9);
        assert_eq!(normalize_code(&code.to_lowercase().replace('-', " ")), Some(code.clone()));
        assert_eq!(normalize_code("ABC"), None);
        assert!(super::super::is_code(&code));
        for other in ["bcdf-ghjk", "BCDFGHJK", "BCDF-GHJA", "BCDF-GHJK\u{1b}", "BCDF GHJK"] {
            assert!(!super::super::is_code(other), "{other}");
        }
    }

    #[tokio::test]
    async fn an_agent_waits_once_and_hears_a_refusal_once() {
        let (tunnels, _t) = tunnels();
        let first = tunnels.request("a", "laptop".into(), "http://localhost:3000".into(), None).unwrap();
        let again = tunnels.request("a", "laptop".into(), "http://localhost:4000".into(), None).unwrap();
        assert_eq!(first.code, again.code);
        assert_eq!(tunnels.find(&first.code.to_lowercase()).unwrap().target, "http://localhost:4000");

        assert!(tunnels.deny(&first.code).is_some());
        assert!(tunnels.pending().is_empty());
        assert!(matches!(tunnels.waiting("a"), Some(Waiting::Denied)));
        assert!(tunnels.waiting("a").is_none());

        // Asking again after a refusal is a fresh request.
        let fresh = tunnels.request("a", "laptop".into(), "http://localhost:3000".into(), None).unwrap();
        assert!(matches!(tunnels.waiting("a"), Some(Waiting::Pending(p)) if p.code == fresh.code));
        tunnels.resolve("a");
        assert!(tunnels.waiting("a").is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn the_origin_has_its_time_once_the_body_has_gone() {
        let (progress, sending) = watch::channel(());
        let due = tokio::spawn(answer_due(sending));
        // An upload that keeps moving takes as long as it takes.
        for _ in 0..30 {
            tokio::time::sleep(RESPONSE_TIMEOUT / 10).await;
            progress.send_replace(());
        }
        assert!(!due.is_finished());

        drop(progress);
        tokio::time::sleep(RESPONSE_TIMEOUT - Duration::from_secs(1)).await;
        assert!(!due.is_finished());
        tokio::time::sleep(Duration::from_secs(2)).await;
        assert!(due.is_finished());

        // One that stops moving runs it out as well.
        let (_progress, sending) = watch::channel(());
        let due = tokio::spawn(answer_due(sending));
        tokio::time::sleep(RESPONSE_TIMEOUT + Duration::from_secs(1)).await;
        assert!(due.is_finished());
    }

    #[tokio::test]
    async fn waiting_is_bounded() {
        let (tunnels, _t) = tunnels();
        for i in 0..MAX_PENDING + 5 {
            let address = IpAddr::from([10, 0, (i / 256) as u8, (i % 256) as u8]);
            assert!(tunnels.request(&i.to_string(), "x".into(), "y".into(), Some(address)).is_some());
        }
        let pending = tunnels.pending();
        assert_eq!(pending.len(), MAX_PENDING);
        assert_eq!(pending[0].pubkey, "5");
    }

    #[tokio::test]
    async fn one_address_can_crowd_out_only_itself() {
        let (tunnels, _t) = tunnels();
        let flood = Some(IpAddr::from([203, 0, 113, 9]));
        let mine = tunnels.request("mine", "laptop".into(), "y".into(), Some(IpAddr::from([198, 51, 100, 7]))).unwrap();
        for i in 0..MAX_PENDING_PER_ADDRESS {
            assert!(tunnels.request(&format!("flood{i}"), "alex-laptop".into(), "y".into(), flood).is_some());
        }
        assert!(tunnels.request("one more", "alex-laptop".into(), "y".into(), flood).is_none());
        // Asking again is metered too, however few are waiting.
        for _ in 0..10 {
            tunnels.request("flood0", "alex-laptop".into(), "y".into(), flood);
        }
        assert!(tunnels.request("flood0", "alex-laptop".into(), "y".into(), flood).is_none());
        assert!(tunnels.find(&mine.code).is_some());
    }

    #[test]
    fn a_signature_lets_a_connection_in_once() {
        let (tunnels, _t) = tunnels();
        let later = crate::util::now() + 60;
        assert!(tunnels.first_use("a", later));
        assert!(!tunnels.first_use("a", later));
        assert!(tunnels.first_use("b", later));
        // Forgotten once it would be refused as too old anyway.
        assert!(tunnels.first_use("old", crate::util::now() - 1));
        assert!(tunnels.first_use("c", later));
        assert!(!tunnels.used.lock().contains_key("old"));
    }
}
