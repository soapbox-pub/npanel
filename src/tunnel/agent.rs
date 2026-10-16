//! The agent's half of a tunnel: `npanel tunnel <server> <target>`.
//!
//! The first time it is pointed at a server, the agent makes itself a key,
//! asks to be authorized and shows the code an admin will see, and waits. Once
//! authorized, and every time after, it holds connections open to the server
//! and answers what comes down them by asking the target. A connection that
//! drops is made again, backing off while the server can't be reached.
//!
//! Its keys live under `~/.config/npanel/tunnels/<server>/`, one per target,
//! so two servers on this machine are two tunnels, each with a hostname of its
//! own, and running the same command again is the same tunnel.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use axum::body::Body;
use bytes::Bytes;
use http::uri::Authority;
use http::{header, HeaderValue, Method, Request, Response, StatusCode, Uri, Version};
use http_body_util::{BodyExt, Empty};
use hyper::body::Incoming;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::client::legacy::Client;
use hyper_util::rt::{TokioExecutor, TokioIo, TokioTimer};
use parking_lot::Mutex;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use url::Url;

use super::{is_code, strip_hop_by_hop, CONNECTION_WINDOW, CONNECT_PATH, KEEP_ALIVE, PROTOCOL, REQUEST_PATH, STATUS_PATH, STREAM_WINDOW, TARGET_HEADER, WEBSOCKET_BUFFER};
use crate::auth::sign_nip98;
use crate::nostr::event::{generate_secret_key, is_secret_key};

/// How many connections the agent keeps open unless told otherwise: two, so
/// that either end can lose one without the site going dark while it is made
/// again.
pub const DEFAULT_CONNECTIONS: usize = 2;

/// How many requests and WebSockets one connection may carry at once. Each is
/// a stream, and hyper's default of 200 would cap a tunnel at a few hundred
/// open sockets: every one past it waits, then times out. An idle stream
/// costs a few kilobytes, so the limit is set far above any real load.
const MAX_STREAMS: u32 = 10_000;

/// How often a waiting agent asks whether it has been authorized.
const POLL_INTERVAL: Duration = Duration::from_secs(2);

/// The longest the agent waits between attempts to reach the server.
const MAX_BACKOFF: Duration = Duration::from_secs(30);

/// A connection that stayed up this long was a working one, and the next
/// attempt after it closes starts the backoff over.
const STABLE: Duration = Duration::from_secs(30);

/// How long reaching the server and being let in may take.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// What `npanel tunnel` was given.
pub struct Options {
    pub server: String,
    pub target: String,
    pub name: Option<String>,
    pub connections: usize,
}

/// What the agent has to say, as it happens.
pub enum Event<'a> {
    /// Waiting for an admin, who should open `url` and see `code`.
    Code { code: &'a str, url: &'a str },
    Authorized { hostname: &'a str },
    /// How many connections are open now.
    Connections(usize),
    /// Something went wrong that the agent will try again.
    Retrying(&'a str),
    /// A request the origin couldn't answer. Visitors are told only that it
    /// didn't; this is what whoever runs the agent needs to know.
    Origin(&'a str),
}

pub type Report = Arc<dyn Fn(Event<'_>) + Send + Sync>;

/// Run `npanel tunnel` until it is interrupted or the tunnel is removed.
pub fn main(options: Options) -> Result<()> {
    let server = parse_server(&options.server)?;
    let target = parse_target(&options.target)?;
    let label = options.name.clone().unwrap_or_else(|| target.to_string());
    let secret = load_or_create_key(&key_file(&server, &label)?)?;

    tokio::runtime::Builder::new_multi_thread().enable_all().thread_name("npanel-tunnel").build()?.block_on(async {
        let agent = Arc::new(
            Agent::new(Config {
                server,
                secret,
                name: options.name.unwrap_or_else(machine_name),
                report: terminal(target.clone(), options.connections),
                target,
                connections: options.connections,
                resolve: None,
            })
            .await?,
        );
        tokio::select! {
            result = async {
                agent.authorize().await?;
                agent.clone().run().await
            } => result,
            _ = tokio::signal::ctrl_c() => Ok(()),
        }
    })
}

/// An agent, ready to be authorized and run.
pub struct Config {
    pub server: Url,
    pub secret: String,
    pub target: Authority,
    /// What the dashboard calls this agent.
    pub name: String,
    /// How many connections to hold open.
    pub connections: usize,
    /// Where to reach the server, in place of looking its name up.
    pub resolve: Option<SocketAddr>,
    pub report: Report,
}

pub struct Agent {
    server: Url,
    /// The name the server's dashboard answers at, which every request is
    /// signed for: not necessarily the name it was reached by.
    dashboard_host: String,
    /// Where an admin approves this agent: built here rather than taken from
    /// the server, since it is handed to whatever opens URLs on this desktop.
    authorize_url: Url,
    secret: String,
    target: Authority,
    name: String,
    connections: usize,
    resolve: Option<SocketAddr>,
    http: reqwest::Client,
    tls: Arc<rustls::ClientConfig>,
    origin: Arc<Origin>,
    report: Report,
}

#[derive(Deserialize)]
struct Instance {
    dashboard_host: String,
    version: Option<String>,
}

/// Where an agent stands, as the server says.
#[derive(Deserialize)]
struct Status {
    status: String,
    hostname: Option<String>,
    code: Option<String>,
}

/// Why a connection couldn't be made.
enum Failure {
    /// The tunnel was removed: nothing will let this key in again.
    Gone(String),
    /// Turned off in the dashboard, for now.
    Disabled,
    Other(anyhow::Error),
}

impl Agent {
    pub async fn new(config: Config) -> Result<Agent> {
        let host = config.server.host_str().ok_or_else(|| anyhow!("The server address needs a hostname."))?.to_string();
        // No proxy from the environment: the connection that carries the tunnel
        // is made directly, and the API calls go the same way.
        let mut http = reqwest::Client::builder().user_agent(user_agent()).timeout(CONNECT_TIMEOUT).no_proxy();
        if let Some(address) = config.resolve {
            http = http.resolve(&host, address);
        }
        let http = http.build()?;

        let server = config.server.as_str().trim_end_matches('/').to_string();
        let instance = http.get(config.server.join("/api/instance")?).send().await.with_context(|| format!("Could not reach {server}"))?;
        let instance: Instance = match instance.error_for_status() {
            Ok(response) => response.bytes().await.ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()),
            Err(_) => None,
        }
        .ok_or_else(|| anyhow!("{server} doesn't look like an npanel server."))?;
        let authorize_url = Url::parse(&format!("https://{}/tunnels/authorize", instance.dashboard_host))
            .ok()
            .filter(|url| url.host_str() == Some(instance.dashboard_host.as_str()) && url.path() == "/tunnels/authorize")
            .ok_or_else(|| anyhow!("{server} names its dashboard {}, which isn't a hostname.", printable(&instance.dashboard_host)))?;

        let mut connector = HttpConnector::new();
        connector.set_nodelay(true);
        let client = Client::builder(TokioExecutor::new()).pool_timer(TokioTimer::new()).build(connector);

        let agent = Agent {
            dashboard_host: instance.dashboard_host,
            authorize_url,
            secret: config.secret,
            target: config.target.clone(),
            name: config.name,
            connections: config.connections,
            resolve: config.resolve,
            http,
            tls: tls_config(),
            origin: Arc::new(Origin { client, target: config.target, report: config.report.clone() }),
            report: config.report,
            server: config.server,
        };
        // A server too old to know of tunnels says so here, rather than
        // after the agent has asked to be authorized.
        let (status, _) = agent.call(Method::GET, STATUS_PATH, None).await?;
        if status == StatusCode::NOT_FOUND {
            bail!("{server} runs npanel {}, which has no tunnels. It needs upgrading first.", instance.version.as_deref().unwrap_or("older than 0.2.0"));
        }
        Ok(agent)
    }

    /// A signed request to the server's API.
    async fn call(&self, method: Method, path: &str, body: Option<Value>) -> Result<(StatusCode, Value)> {
        let url = format!("https://{}{path}", self.dashboard_host);
        let raw = body.map(|b| b.to_string());
        let auth = sign_nip98(&self.secret, method.as_str(), &url, raw.as_deref().map(str::as_bytes), crate::util::now());
        let mut request = self.http.request(method, self.server.join(path)?).header(header::AUTHORIZATION, auth);
        if let Some(raw) = raw {
            request = request.header(header::CONTENT_TYPE, "application/json").body(raw);
        }
        let response = request.send().await.with_context(|| format!("Could not reach {}", self.server))?;
        let status = response.status();
        let body = response.bytes().await.unwrap_or_default();
        Ok((status, serde_json::from_slice(&body).unwrap_or(Value::Null)))
    }

    async fn status(&self) -> Result<Status> {
        let (status, body) = self.call(Method::GET, STATUS_PATH, None).await?;
        if !status.is_success() {
            bail!("{}", error_message(&body, status));
        }
        Ok(serde_json::from_value(body)?)
    }

    /// Wait until an admin has authorized this agent, asking to be if it
    /// hasn't yet, and hand back the hostname it serves.
    pub async fn authorize(&self) -> Result<String> {
        let mut shown = false;
        loop {
            let status = self.status().await?;
            match status.status.as_str() {
                "authorized" => {
                    let hostname = status.hostname.unwrap_or_default();
                    (self.report)(Event::Authorized { hostname: &hostname });
                    return Ok(hostname);
                }
                "pending" => {
                    if !shown {
                        self.show_code(status.code.as_deref())?;
                        shown = true;
                    }
                }
                "denied" => bail!("An admin turned this tunnel down."),
                _ if shown => bail!("The code expired before anyone approved it. Run the command again for a new one."),
                _ => {
                    let body = json!({ "name": self.name, "target": format!("http://{}", self.target) });
                    let (code, body) = self.call(Method::POST, REQUEST_PATH, Some(body)).await?;
                    if !code.is_success() {
                        bail!("{}", error_message(&body, code));
                    }
                    let pending: Status = serde_json::from_value(body)?;
                    if pending.status == "pending" {
                        self.show_code(pending.code.as_deref())?;
                        shown = true;
                    }
                    continue;
                }
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    /// Show the code an admin is to check, with where to approve it.
    fn show_code(&self, code: Option<&str>) -> Result<()> {
        let code = code.filter(|code| is_code(code)).ok_or_else(|| anyhow!("{} sent a code npanel doesn't make.", self.server_name()))?;
        let mut url = self.authorize_url.clone();
        url.query_pairs_mut().append_pair("code", code);
        (self.report)(Event::Code { code, url: url.as_str() });
        Ok(())
    }

    /// Hold the tunnel's connections open until it is removed.
    pub async fn run(self: Arc<Self>) -> Result<()> {
        // Counted and reported under one lock, so two connections changing
        // at once are reported in the order they changed.
        let open = Arc::new(Mutex::new(0));
        let mut connections = tokio::task::JoinSet::new();
        for _ in 0..self.connections {
            let (agent, open) = (self.clone(), open.clone());
            connections.spawn(async move { agent.hold(&open).await });
        }
        // Each one ends only when the tunnel is gone, so the first to end
        // speaks for them all.
        let first = connections.join_next().await.expect("connections were spawned");
        connections.abort_all();
        first?
    }

    /// Keep one connection open, making it again whenever it closes.
    async fn hold(&self, open: &Mutex<usize>) -> Result<()> {
        let mut backoff = Duration::from_secs(1);
        loop {
            match self.connect(open).await {
                Ok(lasted) => {
                    if lasted >= STABLE {
                        backoff = Duration::from_secs(1);
                        continue;
                    }
                }
                Err(Failure::Gone(message)) => bail!("{message}"),
                Err(Failure::Disabled) => {
                    (self.report)(Event::Retrying("This tunnel is turned off in the dashboard. Waiting for it to be turned back on…"));
                    tokio::time::sleep(MAX_BACKOFF).await;
                    continue;
                }
                Err(Failure::Other(error)) => {
                    let message = format!("{error:#}. Trying again…");
                    (self.report)(Event::Retrying(&message));
                }
            }
            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(MAX_BACKOFF);
        }
    }

    /// Open one connection and serve the requests that come down it until
    /// it closes, which is how long it lasted.
    async fn connect(&self, open: &Mutex<usize>) -> Result<Duration, Failure> {
        let other = |error: anyhow::Error| Failure::Other(error);
        let upgraded = tokio::time::timeout(CONNECT_TIMEOUT, self.upgrade())
            .await
            .map_err(|_| other(anyhow!("{} took too long to let the tunnel in", self.server_name())))??;

        self.count(open, |n| n + 1);
        let started = Instant::now();
        let origin = self.origin.clone();
        let service = hyper::service::service_fn(move |request| {
            let origin = origin.clone();
            async move { Ok::<_, Infallible>(origin.forward(request).await) }
        });
        let _ = hyper::server::conn::http2::Builder::new(TokioExecutor::new())
            .timer(TokioTimer::new())
            .keep_alive_interval(KEEP_ALIVE)
            .keep_alive_timeout(KEEP_ALIVE)
            .initial_connection_window_size(CONNECTION_WINDOW)
            .initial_stream_window_size(STREAM_WINDOW)
            .max_concurrent_streams(MAX_STREAMS)
            .enable_connect_protocol()
            .serve_connection(upgraded, service)
            .await;
        self.count(open, |n| n - 1);
        Ok(started.elapsed())
    }

    fn count(&self, open: &Mutex<usize>, change: impl FnOnce(usize) -> usize) {
        let mut open = open.lock();
        *open = change(*open);
        (self.report)(Event::Connections(*open));
    }

    /// Reach the server and have the connection upgraded to a tunnel.
    async fn upgrade(&self) -> Result<hyper::upgrade::Upgraded, Failure> {
        let other = |error: anyhow::Error| Failure::Other(error);
        let io = self.dial().await.map_err(other)?;
        let (mut send, connection) = hyper::client::conn::http1::handshake::<_, Empty<Bytes>>(TokioIo::new(io)).await.map_err(|e| other(e.into()))?;
        tokio::spawn(async move {
            let _ = connection.with_upgrades().await;
        });

        let url = format!("https://{}{CONNECT_PATH}", self.dashboard_host);
        let request = Request::get(CONNECT_PATH)
            .header(header::HOST, self.server_authority())
            .header(header::AUTHORIZATION, sign_nip98(&self.secret, "GET", &url, None, crate::util::now()))
            .header(header::CONNECTION, "upgrade")
            .header(header::UPGRADE, PROTOCOL)
            .header(header::USER_AGENT, user_agent())
            .header(TARGET_HEADER, format!("http://{}", self.target))
            .body(Empty::new())
            .map_err(|e| other(e.into()))?;
        let response = send.send_request(request).await.map_err(|e| other(e.into()))?;

        match response.status() {
            StatusCode::SWITCHING_PROTOCOLS => hyper::upgrade::on(response).await.map_err(|e| other(e.into())),
            StatusCode::NOT_FOUND => Err(Failure::Gone(response_error(response).await)),
            StatusCode::FORBIDDEN => Err(Failure::Disabled),
            _ => Err(other(anyhow!("{}", response_error(response).await))),
        }
    }

    /// A connection to the server, over TLS unless it was given as `http://`.
    async fn dial(&self) -> Result<Box<dyn Io>> {
        let host = self.server_name();
        let port = self.server.port_or_known_default().unwrap_or(443);
        let addresses: Vec<SocketAddr> = match self.resolve {
            Some(address) => vec![address],
            None => tokio::net::lookup_host((host.as_str(), port)).await.with_context(|| format!("Could not look up {host}"))?.collect(),
        };
        let tcp = TcpStream::connect(&addresses[..]).await.with_context(|| format!("Could not reach {host}"))?;
        tcp.set_nodelay(true)?;
        if self.server.scheme() == "http" {
            return Ok(Box::new(tcp));
        }
        let name = rustls::pki_types::ServerName::try_from(host.clone()).with_context(|| format!("{host} can't be checked against a certificate"))?;
        let tls = tokio_rustls::TlsConnector::from(self.tls.clone()).connect(name, tcp).await.with_context(|| format!("Could not make a secure connection to {host}"))?;
        Ok(Box::new(tls))
    }

    /// The server's hostname or address, without brackets or port.
    fn server_name(&self) -> String {
        match self.server.host() {
            Some(url::Host::Domain(domain)) => domain.to_string(),
            Some(url::Host::Ipv4(ip)) => ip.to_string(),
            Some(url::Host::Ipv6(ip)) => ip.to_string(),
            None => String::new(),
        }
    }

    /// The server as its `Host` header names it.
    fn server_authority(&self) -> String {
        let host = self.server.host_str().unwrap_or_default();
        match self.server.port() {
            Some(port) => format!("{host}:{port}"),
            None => host.to_string(),
        }
    }
}

trait Io: AsyncRead + AsyncWrite + Send + Unpin {}
impl<T: AsyncRead + AsyncWrite + Send + Unpin> Io for T {}

/// The server this agent passes requests on to.
struct Origin {
    client: Client<HttpConnector, Body>,
    target: Authority,
    report: Report,
}

impl Origin {
    async fn forward(&self, mut request: Request<Incoming>) -> Response<Body> {
        let websocket =
            request.method() == Method::CONNECT && request.extensions().get::<hyper::ext::Protocol>().is_some_and(|p| p.as_str().eq_ignore_ascii_case("websocket"));
        let tunnel_upgrade = websocket.then(|| hyper::upgrade::on(&mut request));
        let (mut parts, body) = request.into_parts();

        strip_hop_by_hop(&mut parts.headers);
        // The hostname the visitor asked for, as any reverse proxy passes it
        // on: the origin is a production server, and routes, redirects and
        // builds its URLs by it. The target is only where to dial.
        let Some(public) = parts.uri.authority() else {
            return text(StatusCode::BAD_REQUEST, "The request named no hostname.".into());
        };
        parts.headers.insert(header::HOST, HeaderValue::from_str(public.as_str()).expect("an authority is a header value"));
        let path = parts.uri.path_and_query().map_or("/", |pq| pq.as_str());
        let Ok(uri) = Uri::builder().scheme("http").authority(self.target.clone()).path_and_query(path).build() else {
            return text(StatusCode::BAD_REQUEST, "That path can't be passed on.".into());
        };

        let mut outbound = Request::builder().uri(uri).version(Version::HTTP_11);
        *outbound.headers_mut().expect("a builder with no error yet") = parts.headers;
        let outbound = match tunnel_upgrade {
            Some(_) => outbound.method(Method::GET).header(header::CONNECTION, "upgrade").header(header::UPGRADE, "websocket").body(Body::empty()),
            None => outbound.method(parts.method).body(Body::new(body)),
        }
        .expect("request");

        let mut response = match self.client.request(outbound).await {
            Ok(response) => response,
            Err(error) => {
                (self.report)(Event::Origin(&format!("Couldn't reach http://{}: {}.", self.target, root_cause(&error))));
                return text(StatusCode::BAD_GATEWAY, "This site's server isn't answering. Try again in a moment.".into());
            }
        };

        match tunnel_upgrade {
            Some(tunnel_upgrade) if response.status() == StatusCode::SWITCHING_PROTOCOLS => {
                let origin_upgrade = hyper::upgrade::on(&mut response);
                tokio::spawn(async move {
                    let (Ok(tunnel), Ok(origin)) = tokio::join!(tunnel_upgrade, origin_upgrade) else { return };
                    let (mut tunnel, mut origin) = (TokioIo::new(tunnel), TokioIo::new(origin));
                    let _ = tokio::io::copy_bidirectional_with_sizes(&mut tunnel, &mut origin, WEBSOCKET_BUFFER, WEBSOCKET_BUFFER).await;
                });
                let (mut parts, _) = response.into_parts();
                strip_hop_by_hop(&mut parts.headers);
                parts.status = StatusCode::OK;
                parts.version = Version::HTTP_2;
                Response::from_parts(parts, Body::empty())
            }
            Some(_) if response.status().is_success() => {
                (self.report)(Event::Origin(&format!("http://{} answered a WebSocket request without upgrading it.", self.target)));
                text(StatusCode::BAD_GATEWAY, "This site's server didn't accept the WebSocket connection.".into())
            }
            _ => {
                strip_hop_by_hop(response.headers_mut());
                response.map(Body::new)
            }
        }
    }
}

/// The server to connect to, `https://` unless it says otherwise.
pub fn parse_server(input: &str) -> Result<Url> {
    let input = input.trim();
    let url = if input.contains("://") { Url::parse(input) } else { Url::parse(&format!("https://{input}")) }
        .map_err(|_| anyhow!("{input} isn't a server address. Give the hostname of the npanel dashboard, like npanel.example.com."))?;
    if !matches!(url.scheme(), "https" | "http") || url.host().is_none() {
        bail!("{input} isn't a server address. Give the hostname of the npanel dashboard, like npanel.example.com.");
    }
    Ok(url)
}

/// What to forward to: a port on this machine, or a host and port.
pub fn parse_target(input: &str) -> Result<Authority> {
    let input = input.trim();
    if let Ok(port) = input.parse::<u16>() {
        return Ok(format!("localhost:{port}").parse()?);
    }
    let address = match input.split_once("://") {
        Some(("http", rest)) => rest,
        Some(("https", _)) => bail!("npanel tunnel forwards plain HTTP. Give the server's http:// address, or just its port."),
        Some(_) => bail!("{input} isn't an HTTP server. Give a port, like 8080, or an address, like localhost:8080."),
        None => input,
    };
    let address = address.strip_suffix('/').unwrap_or(address);
    if address.contains('/') {
        bail!("The target is a server, not a page: give its address without a path, like localhost:8080.");
    }
    address.parse().map_err(|_| anyhow!("{input} isn't an address. Give a port, like 8080, or an address, like localhost:8080."))
}

/// Where the key for `label` on `server` is kept.
fn key_file(server: &Url, label: &str) -> Result<PathBuf> {
    let dir = crate::config::Environment::current()
        .user_config_dir()
        .ok_or_else(|| anyhow!("Neither HOME nor XDG_CONFIG_HOME is set, so there's nowhere to keep this tunnel's key."))?;
    let server = match server.port() {
        Some(port) => format!("{}_{port}", server.host_str().unwrap_or_default()),
        None => server.host_str().unwrap_or_default().to_string(),
    };
    Ok(dir.join("tunnels").join(file_name(&server)).join(format!("{}.key", file_name(label))))
}

/// `value` with anything that can't be in a file name made a dash.
fn file_name(value: &str) -> String {
    value.chars().map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') { c } else { '-' }).collect()
}

/// The key at `path`, made and stored there if there isn't one yet.
fn load_or_create_key(path: &Path) -> Result<String> {
    use std::io::Write;
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};

    match std::fs::read_to_string(path) {
        Ok(contents) => {
            let secret = contents.trim().to_string();
            if !is_secret_key(&secret) {
                bail!("{} isn't a key npanel can use. Delete it to make a new one, which will need authorizing again.", path.display());
            }
            return Ok(secret);
        }
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
            return Err(error).with_context(|| format!("Could not read {}", path.display()));
        }
        Err(_) => {}
    }

    let dir = path.parent().expect("a key file is in a directory");
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir).with_context(|| format!("Could not create {}", dir.display()))?;
    let secret = generate_secret_key();
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("Could not create {}", path.display()))?;
    writeln!(file, "{secret}")?;
    Ok(secret)
}

/// This machine's name, for the dashboard to call the agent by.
fn machine_name() -> String {
    let mut buffer = [0u8; 256];
    // SAFETY: the buffer is valid for its length, which is what gethostname
    // is told it may write.
    let ok = unsafe { libc::gethostname(buffer.as_mut_ptr().cast(), buffer.len()) } == 0;
    let end = buffer.iter().position(|b| *b == 0).unwrap_or(buffer.len());
    let name = String::from_utf8_lossy(&buffer[..end]).trim().to_string();
    if ok && !name.is_empty() { name } else { "agent".into() }
}

fn user_agent() -> String {
    format!("npanel/{}", env!("CARGO_PKG_VERSION"))
}

fn tls_config() -> Arc<rustls::ClientConfig> {
    let roots = rustls::RootCertStore { roots: webpki_roots::TLS_SERVER_ROOTS.to_vec() };
    let mut config = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .expect("ring supports the default protocol versions")
        .with_root_certificates(roots)
        .with_no_client_auth();
    // The upgrade is HTTP/1.1's.
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Arc::new(config)
}

/// The sentence an API error carries, or the status if it carried none.
fn error_message(body: &Value, status: StatusCode) -> String {
    body["error"].as_str().map_or_else(|| format!("The server answered {status}"), str::to_string)
}

async fn response_error(response: Response<Incoming>) -> String {
    let status = response.status();
    let body = response.into_body().collect().await.map(|b| b.to_bytes()).unwrap_or_default();
    error_message(&serde_json::from_slice(&body).unwrap_or(Value::Null), status)
}

/// What actually went wrong, under the layers of context a client wraps
/// it in: "Connection refused" rather than "client error (Connect)".
fn root_cause(error: &(dyn std::error::Error + 'static)) -> String {
    let mut cause = error;
    while let Some(source) = cause.source() {
        cause = source;
    }
    cause.to_string()
}

fn text(status: StatusCode, body: String) -> Response<Body> {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .body(Body::from(body))
        .expect("response")
}

/// `text` as it is safe to print: what the server says could otherwise move
/// the cursor, retitle the window, or reorder what is shown, with control
/// characters and bidirectional overrides.
pub fn printable(text: &str) -> String {
    text.chars().filter(|c| !c.is_control() && !matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')).collect()
}

/// What the agent says on a terminal: each thing once, as it changes.
fn terminal(target: Authority, connections: usize) -> Report {
    let last = Mutex::new(String::new());
    Arc::new(move |event| {
        let line = match event {
            Event::Code { code, url } => {
                open_browser(url);
                format!("To authorize this tunnel, open\n\n    {url}\n\nand check that it shows the code {code}.\n\nWaiting for an admin to approve it…")
            }
            Event::Authorized { hostname } => format!("https://{} → http://{target}", printable(hostname)),
            Event::Connections(0) => "Disconnected. Reconnecting…".into(),
            Event::Connections(open) => format!("Connected ({open} of {connections} connections)."),
            Event::Retrying(message) | Event::Origin(message) => printable(message),
        };
        let mut last = last.lock();
        if *last != line {
            eprintln!("{line}");
            *last = line;
        }
    })
}

/// Open the authorization page, when there is a desktop to open it on.
fn open_browser(url: &str) {
    let desktop = std::env::var_os("DISPLAY").is_some() || std::env::var_os("WAYLAND_DISPLAY").is_some();
    if desktop {
        let _ = std::process::Command::new("xdg-open")
            .arg(url)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn targets() {
        assert_eq!(parse_target("8080").unwrap().as_str(), "localhost:8080");
        assert_eq!(parse_target("http://127.0.0.1:3000/").unwrap().as_str(), "127.0.0.1:3000");
        assert_eq!(parse_target("devbox:5173").unwrap().as_str(), "devbox:5173");
        assert!(parse_target("https://localhost:3000").unwrap_err().to_string().contains("plain HTTP"));
        assert!(parse_target("localhost:3000/app").unwrap_err().to_string().contains("without a path"));
        assert!(parse_target("ftp://x").is_err());
    }

    #[test]
    fn servers() {
        assert_eq!(parse_server("npanel.example.com").unwrap().as_str(), "https://npanel.example.com/");
        assert_eq!(parse_server("http://localhost:3000").unwrap().as_str(), "http://localhost:3000/");
        assert!(parse_server("ftp://x").is_err());
    }

    #[test]
    fn prints_only_what_is_safe_to() {
        assert_eq!(printable("dev.example.com\u{1b}]0;owned\u{7}\r\n"), "dev.example.com]0;owned");
        assert_eq!(printable("abc\u{202e}fed"), "abcfed");
        assert_eq!(printable("Connected → ok…"), "Connected → ok…");
    }

    #[test]
    fn keys_are_made_once_and_kept() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tunnels/npanel.example.com/localhost-8080.key");
        let made = load_or_create_key(&path).unwrap();
        assert_eq!(load_or_create_key(&path).unwrap(), made);
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);

        std::fs::write(&path, "nonsense").unwrap();
        assert!(load_or_create_key(&path).unwrap_err().to_string().contains("Delete it"));
        assert_eq!(file_name("localhost:8080"), "localhost-8080");
    }
}
