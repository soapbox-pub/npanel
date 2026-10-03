//! Publishes every enabled site as a Tor onion service.
//!
//! Tor runs inside npanel: Arti, the Tor Project's own implementation in
//! Rust, on a runtime and a thread of its own (`src/supervise.rs`), so neither
//! building circuits nor serving the sites' onion traffic runs on the threads
//! that serve clearnet visitors. There is no daemon to install and no control
//! port to secure.
//!
//! npanel keeps the keys and Tor keeps nothing. Arti's keystore is the
//! in-memory one, and every service is launched with the key stored in the
//! database, which is what makes an address survive a restart. What Arti does
//! keep on disk — its directory cache, its guards — makes it faster and safer
//! to come back, but losing it costs no address. Each service's own state is
//! cleared as Tor starts, since the keys it refers to are gone.
//!
//! Sites are reconciled rather than tracked. Every trigger — Tor starting, a
//! host being added or removed, a setting changing — asks the database which
//! hosts are enabled and makes the running services match. Nothing has to be
//! remembered between two of them, which is also what makes the runtime cheap
//! to restart if it ever dies.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context};
use arti_client::config::onion_service::OnionServiceConfigBuilder;
use arti_client::config::TorClientConfigBuilder;
use arti_client::TorClient;
use axum::body::Body;
use base64::Engine;
use futures_util::StreamExt;
use http::{header, Request, Response, StatusCode};
use parking_lot::Mutex;
use rusqlite::params;
use safelog::DisplayRedacted;
use serde::Serialize;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tor_cell::relaycell::msg::{Connected, End, EndReason};
use tor_config::ExplicitOrAuto;
use tor_hscrypto::pk::{HsIdKey, HsIdKeypair};
use tor_hsservice::{HsIdKeypairSpecifier, HsNickname, RunningOnionService, StreamRequest};
use tor_keymgr::config::ArtiKeystoreKind;
use tor_keymgr::KeystoreSelector;
use tor_llcrypto::pk::ed25519;
use tor_proto::stream::IncomingStreamRequest;

use crate::analytics::Peer;
use crate::db::Db;
use crate::deploy::Deployments;
use crate::fields;
use crate::hostname::request_hostname;
use crate::log::{with_err, LogLevel, Logger};
use crate::serve::{RequestOrigin, ServeCache, SiteServer};
use crate::server::Connections;
use crate::settings::Settings;

type Client = TorClient<tor_rtcompat::PreferredRuntime>;

const RETRY_MIN: Duration = Duration::from_secs(1);
const RETRY_MAX: Duration = Duration::from_secs(60);

/// The virtual port an onion answers on.
const ONION_PORT: u16 = 80;

/// How C Tor spells an onion service key, and so how npanel stores one.
const KEY_PREFIX: &str = "ED25519-V3:";

/// What the dashboard shows on the Settings page.
#[derive(Clone, Debug, Default, Serialize)]
pub struct TorStatus {
    pub enabled: bool,
    /// Whether npanel is on the Tor network and ready to answer.
    pub connected: bool,
    /// How far through joining the network it is, as a percentage.
    pub progress: u8,
    /// How many onion services are running right now.
    pub services: usize,
    /// Why Tor isn't working, as a sentence, or empty when nothing is wrong.
    pub error: String,
}

enum Command {
    /// Settings changed, or startup: start or stop Tor to match.
    Apply,
    Reconcile,
    Stop(oneshot::Sender<()>),
}

/// What the actor's own tasks report back.
enum Event {
    /// A client finished starting, or failed to, for the given generation.
    Started(u64, Result<Arc<Client>, String>),
    /// Time to try again whatever failed last.
    Retry(u64),
}

pub struct TorDeps {
    pub db: Db,
    pub settings: Arc<Settings>,
    pub log: Logger,
    /// Where Arti keeps its state and its directory cache.
    pub data_dir: PathBuf,
    /// The same site server the public listener uses.
    pub site: Arc<SiteServer>,
    /// Cleared when an onion is minted, so serving learns the new address.
    pub serve_cache: Arc<ServeCache>,
    /// Told when an onion is minted, since it is served from a build of its own.
    pub deployments: Arc<dyn Deployments>,
}

pub struct TorService {
    tx: mpsc::UnboundedSender<Command>,
    status: Arc<Mutex<TorStatus>>,
    settings: Arc<Settings>,
    thread: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl TorService {
    pub fn start(deps: TorDeps) -> TorService {
        forward_logs(deps.log);
        let (tx, mut rx) = mpsc::unbounded_channel();
        let status = Arc::new(Mutex::new(TorStatus::default()));
        let settings = deps.settings.clone();

        let mut changes = settings.subscribe();
        let notify = tx.clone();
        tokio::spawn(async move {
            let mut seen = changes.borrow_and_update().tor_enabled;
            while changes.changed().await.is_ok() {
                let now = changes.borrow_and_update().tor_enabled;
                if now != seen && notify.send(Command::Apply).is_err() {
                    break;
                }
                seen = now;
            }
        });

        // Each runtime starts by applying the settings, so a restarted one
        // republishes everything without being asked.
        let shared = status.clone();
        let log = deps.log;
        let thread = crate::supervise::supervise("tor", 2, log, move |runtime| {
            runtime.block_on(Actor::new(&deps, shared.clone()).run(&mut rx));
            Ok(())
        });

        TorService { tx, status, settings, thread: Mutex::new(Some(thread)) }
    }

    /// Re-check which sites should have an onion. Returns at once.
    pub fn hosts_changed(&self) {
        let _ = self.tx.send(Command::Reconcile);
    }

    pub fn status(&self) -> TorStatus {
        let mut status = self.status.lock().clone();
        status.enabled = self.settings.get().tor_enabled;
        status
    }

    pub async fn stop(&self) {
        let (done, wait) = oneshot::channel();
        if self.tx.send(Command::Stop(done)).is_ok() {
            let _ = wait.await;
        }
        let thread = self.thread.lock().take();
        if let Some(thread) = thread {
            let _ = tokio::task::spawn_blocking(move || thread.join()).await;
        }
    }
}

/// A running Tor client, and what it is publishing.
struct Session {
    client: Arc<Client>,
    /// Running onion services, by host id.
    live: HashMap<i64, Published>,
    /// Every HTTP connection that arrived over Tor, so stopping can drain them.
    connections: Arc<Connections>,
    /// Joining the network, and reporting how that is going.
    background: Vec<JoinHandle<()>>,
}

struct Published {
    address: String,
    /// Dropping this is what takes the service down.
    _service: Arc<RunningOnionService>,
    /// Accepting its streams.
    accepting: JoinHandle<()>,
}

struct Actor<'a> {
    deps: &'a TorDeps,
    status: Arc<Mutex<TorStatus>>,
    events: (mpsc::UnboundedSender<Event>, mpsc::UnboundedReceiver<Event>),
    session: Option<Session>,
    /// Bumped whenever Tor is started or stopped, so that what an earlier
    /// start reports back is recognised as stale.
    generation: u64,
    retry: Option<JoinHandle<()>>,
    retry_delay: Duration,
}

impl<'a> Actor<'a> {
    fn new(deps: &'a TorDeps, status: Arc<Mutex<TorStatus>>) -> Actor<'a> {
        Actor {
            deps,
            status,
            events: mpsc::unbounded_channel(),
            session: None,
            generation: 0,
            retry: None,
            retry_delay: RETRY_MIN,
        }
    }

    async fn run(mut self, rx: &mut mpsc::UnboundedReceiver<Command>) {
        self.apply().await;
        loop {
            tokio::select! {
                command = rx.recv() => match command {
                    Some(Command::Apply) => self.apply().await,
                    Some(Command::Reconcile) => self.reconcile().await,
                    Some(Command::Stop(done)) => {
                        self.shut_down().await;
                        let _ = done.send(());
                        return;
                    }
                    None => {
                        self.shut_down().await;
                        return;
                    }
                },
                Some(event) = self.events.1.recv() => match event {
                    Event::Started(generation, result) if generation == self.generation => self.started(result).await,
                    Event::Retry(generation) if generation == self.generation => {
                        if self.session.is_some() {
                            self.reconcile().await;
                        } else {
                            self.start();
                        }
                    }
                    _ => {}
                },
            }
        }
    }

    async fn apply(&mut self) {
        self.shut_down().await;
        if !self.deps.settings.get().tor_enabled {
            self.deps.log.info("tor_disabled", fields! {});
            return;
        }
        self.retry_delay = RETRY_MIN;
        self.start();
    }

    /// Start a Tor client in the background; [`Event::Started`] says how it went.
    fn start(&mut self) {
        let (tx, generation, dir) = (self.events.0.clone(), self.generation, self.deps.data_dir.join("tor"));
        tokio::spawn(async move {
            let result = create_client(&dir)
                .await
                .map_err(|error| {
                    format!(
                        "Tor could not start: {error:#}. It keeps its state in {}, which npanel's user must be able to write and no other user may.",
                        dir.display()
                    )
                });
            let _ = tx.send(Event::Started(generation, result));
        });
    }

    async fn started(&mut self, result: Result<Arc<Client>, String>) {
        let client = match result {
            Ok(client) => client,
            Err(error) => {
                self.fail(error);
                return;
            }
        };

        self.status.lock().error.clear();
        let background = vec![
            tokio::spawn(bootstrap(client.clone(), self.deps.log)),
            tokio::spawn(report_progress(client.clone(), self.status.clone())),
        ];
        self.session = Some(Session { client, live: HashMap::new(), connections: Arc::new(Connections::new()), background });
        self.retry_delay = RETRY_MIN;
        self.deps.log.info("tor_started", fields! {});
        self.reconcile().await;
    }

    /// Note why Tor isn't working, and try again later.
    fn fail(&mut self, error: String) {
        self.deps.log.warn("tor_unavailable", fields! { "retry_in" => self.retry_delay.as_secs(), "err_msg" => error.as_str() });
        self.status.lock().error = error;
        self.schedule_retry();
    }

    fn schedule_retry(&mut self) {
        if let Some(retry) = self.retry.take() {
            retry.abort();
        }
        let (tx, delay, generation) = (self.events.0.clone(), self.retry_delay, self.generation);
        self.retry = Some(tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            let _ = tx.send(Event::Retry(generation));
        }));
        self.retry_delay = (self.retry_delay * 2).min(RETRY_MAX);
    }

    /// Make the running services match the enabled hosts. One site's service
    /// failing to start is not a reason to leave the rest unpublished, so
    /// failures are logged and retried later rather than returned.
    async fn reconcile(&mut self) {
        let Some(session) = self.session.as_mut() else {
            return;
        };

        let wanted = self
            .deps
            .db
            .call(|conn| {
                Ok(conn
                    .prepare(
                        "SELECT hosts.id, hosts.hostname, onion_services.address, onion_services.secret_key
                         FROM hosts LEFT JOIN onion_services ON onion_services.host_id = hosts.id
                         WHERE hosts.enabled = 1",
                    )?
                    .query_map([], |r| {
                        Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, Option<String>>(2)?, r.get::<_, Option<String>>(3)?))
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?)
            })
            .await;
        let wanted = match wanted {
            Ok(wanted) => wanted,
            Err(error) => {
                self.deps.log.error("tor_reconcile_failed", crate::log::err_fields(error));
                return;
            }
        };

        let keep: HashSet<i64> = wanted.iter().map(|(id, ..)| *id).collect();
        let stale: Vec<i64> = session.live.keys().filter(|id| !keep.contains(id)).copied().collect();
        for id in stale {
            if let Some(published) = session.live.remove(&id) {
                published.accepting.abort();
                self.deps.log.info("tor_onion_unpublished", fields! { "onion" => published.address });
            }
        }

        let mut failed = false;
        for (id, hostname, address, secret_key) in wanted {
            if session.live.contains_key(&id) {
                continue;
            }
            let (key, new) = match address.zip(secret_key) {
                Some((address, secret_key)) => (OnionKey { address, secret_key }, false),
                None => (OnionKey::generate(), true),
            };

            let published = match publish(&session.client, &key) {
                Ok((service, requests)) => {
                    let accepting = tokio::spawn(accept(
                        requests,
                        key.address.clone(),
                        self.deps.site.clone(),
                        session.connections.clone(),
                        self.deps.log,
                    ));
                    Published { address: key.address.clone(), _service: service, accepting }
                }
                Err(error) => {
                    self.deps.log.error("tor_onion_failed", with_err(fields! { "hostname" => hostname.as_str() }, format!("{error:#}")));
                    failed = true;
                    continue;
                }
            };

            if new {
                let (address, secret) = (key.address.clone(), key.secret_key.clone());
                let stored = self
                    .deps
                    .db
                    .call(move |conn| {
                        Ok(conn.execute(
                            "INSERT INTO onion_services (host_id, address, secret_key, created_at) VALUES (?, ?, ?, ?)",
                            params![id, address, secret, crate::util::now()],
                        )?)
                    })
                    .await;
                if let Err(error) = stored {
                    // An address that isn't stored would change at the next
                    // restart, so it is not handed out at all.
                    published.accepting.abort();
                    self.deps.log.error("tor_onion_failed", with_err(fields! { "hostname" => hostname.as_str() }, error));
                    failed = true;
                    continue;
                }
                // The host now answers at the onion too, and the onion needs a
                // build naming it before it serves anything.
                self.deps.serve_cache.clear();
                self.deps.deployments.rebuild(vec![id]);
            }

            self.deps.log.info(
                "tor_onion_published",
                fields! { "hostname" => hostname.as_str(), "onion" => key.address.as_str(), "new" => new },
            );
            session.live.insert(id, published);
        }

        self.status.lock().services = session.live.len();
        if failed {
            self.schedule_retry();
        } else {
            self.retry_delay = RETRY_MIN;
        }
    }

    async fn shut_down(&mut self) {
        self.generation += 1;
        if let Some(retry) = self.retry.take() {
            retry.abort();
        }
        if let Some(session) = self.session.take() {
            for task in &session.background {
                task.abort();
            }
            let accepting: Vec<JoinHandle<()>> = session.live.into_values().map(|published| published.accepting).collect();
            for task in &accepting {
                task.abort();
            }
            // Every task holding the connections is gone once these return,
            // which is what lets them be drained.
            for task in accepting {
                let _ = task.await;
            }
            drop(session.client);
            if let Some(connections) = Arc::into_inner(session.connections) {
                connections.drain(self.deps.log).await;
            }
        }
        *self.status.lock() = TorStatus::default();
    }
}

/// Send what Arti logs to npanel's log, as `tor_log` entries. Arti reports
/// through `tracing`, and without this everything it has to say — a
/// descriptor that wouldn't upload, an introduction point that failed — would
/// go nowhere. Its warnings are kept at their level; its info lines, which
/// narrate every service's routine, only show at `LOG_LEVEL=debug`.
///
/// `tracing` has one subscriber for the whole process. The first call
/// installs it, and any later one finds it there, which is fine: every
/// `Logger` writes to the same place.
fn forward_logs(log: Logger) {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::Layer;

    let filter = if log.enabled(LogLevel::Debug) { tracing::level_filters::LevelFilter::INFO } else { tracing::level_filters::LevelFilter::WARN };
    let subscriber = tracing_subscriber::registry().with(ArtiLogs(log).with_filter(filter));
    let _ = tracing::subscriber::set_global_default(subscriber);
}

struct ArtiLogs(Logger);

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for ArtiLogs {
    fn on_event(&self, event: &tracing::Event<'_>, _: tracing_subscriber::layer::Context<'_, S>) {
        let metadata = event.metadata();
        let mut fields = fields! { "target" => metadata.target() };
        event.record(&mut FieldVisitor(&mut fields));
        match *metadata.level() {
            tracing::Level::ERROR => self.0.error("tor_log", fields),
            tracing::Level::WARN => self.0.warn("tor_log", fields),
            _ => self.0.debug("tor_log", fields),
        }
    }
}

/// Flattens an event's fields into log fields, its message as `tor_msg` so it
/// can't be mistaken for the entry's own `msg`.
struct FieldVisitor<'a>(&'a mut crate::log::Fields);

impl tracing::field::Visit for FieldVisitor<'_> {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        let key = match field.name() {
            "message" => "tor_msg",
            "level" | "msg" | "target" => return,
            name => name,
        };
        self.0.push((key, format!("{value:?}").into()));
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        self.record_debug(field, &format_args!("{value}"));
    }
}

/// A Tor client whose keys exist only in memory. It joins the network when
/// [`bootstrap`] asks it to.
async fn create_client(dir: &std::path::Path) -> anyhow::Result<Arc<Client>> {
    // What Arti records about each service's introduction points is only
    // half of them: the keys they were made with were in memory, and went
    // with the last client. Left in place, every service would start out
    // reviving points it can no longer prove it owns. Starting each from
    // nothing is what C Tor does with a service that isn't detached.
    match tokio::fs::remove_dir_all(dir.join("state").join("hss")).await {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => return Err(error.into()),
        _ => {}
    }

    let mut config = TorClientConfigBuilder::from_directories(dir.join("state"), dir.join("cache"));
    // The database is the only place a site's key is kept. Arti's own
    // keystore would be a second copy, on disk, that nothing backs up.
    config.storage().keystore().primary().kind(ExplicitOrAuto::Explicit(ArtiKeystoreKind::Ephemeral));
    let config = config.build()?;
    Ok(TorClient::builder().config(config).create_unbootstrapped_async().await?)
}

/// Join the Tor network, trying again until it works.
async fn bootstrap(client: Arc<Client>, log: Logger) {
    let mut delay = RETRY_MIN;
    while let Err(error) = client.bootstrap().await {
        log.warn("tor_bootstrap_failed", fields! { "retry_in" => delay.as_secs(), "err_msg" => error.to_string() });
        tokio::time::sleep(delay).await;
        delay = (delay * 2).min(RETRY_MAX);
    }
    log.info("tor_bootstrapped", fields! {});
}

/// Keep the status in step with how joining the network is going.
async fn report_progress(client: Arc<Client>, status: Arc<Mutex<TorStatus>>) {
    let mut events = client.bootstrap_events();
    let mut current = client.bootstrap_status();
    loop {
        {
            let mut status = status.lock();
            status.connected = current.ready_for_traffic();
            status.progress = (current.as_frac() * 100.0).round().clamp(0.0, 100.0) as u8;
            status.error = match current.blocked() {
                Some(blockage) => format!("Tor is stuck joining the network: {blockage}."),
                None => String::new(),
            };
        }
        match events.next().await {
            Some(next) => current = next,
            None => return,
        }
    }
}

/// Launch the onion service `key` names.
fn publish(
    client: &Client,
    key: &OnionKey,
) -> anyhow::Result<(Arc<RunningOnionService>, impl futures_util::Stream<Item = tor_hsservice::RendRequest> + Send + 'static)> {
    let keypair = key.keypair()?;
    // Arti finds a service's key, and keeps its state on disk, by nickname.
    // Naming each service by its own address means a nickname can only ever
    // mean one key, whatever happened to the host that had it.
    let label = key.address.strip_suffix(".onion").unwrap_or(&key.address);
    let nickname = HsNickname::new(label.to_string()).map_err(|_| anyhow!("{} is not an onion address", key.address))?;
    client.keymgr()?.insert(keypair, &HsIdKeypairSpecifier::new(nickname.clone()), KeystoreSelector::Primary, true)?;

    let config = OnionServiceConfigBuilder::default().nickname(nickname).build()?;
    let (service, requests) = client.launch_onion_service(config)?.context("Tor would not launch the service")?;
    // Launching a service with no key makes up a new one, which would be an
    // address nobody was given.
    let address = service.onion_address().map(|id| id.display_unredacted().to_string());
    if address.as_deref() != Some(key.address.as_str()) {
        return Err(anyhow!("Tor launched {} as {address:?}", key.address));
    }
    Ok((service, requests))
}

/// Serve every stream that arrives at the onion `address`.
async fn accept(
    requests: impl futures_util::Stream<Item = tor_hsservice::RendRequest>,
    address: String,
    site: Arc<SiteServer>,
    connections: Arc<Connections>,
    log: Logger,
) {
    let mut streams = std::pin::pin!(tor_hsservice::handle_rend_requests(requests));
    while let Some(request) = streams.next().await {
        let Some(stream) = open(request).await else {
            continue;
        };
        connections.serve(stream, onion_handler(site.clone(), address.clone(), log));
    }
}

/// What answers a connection that arrived at the onion `address`.
pub fn onion_handler(
    site: Arc<SiteServer>,
    address: String,
    log: Logger,
) -> impl Fn(Request<Body>) -> std::pin::Pin<Box<dyn std::future::Future<Output = Response<Body>> + Send>> + Send + 'static {
    move |request| {
        let (site, address) = (site.clone(), address.clone());
        Box::pin(async move {
            // A stream arrives at one onion, so that is the only name it can
            // ask for: never the dashboard, never another site. Tor hands the
            // visitor's bytes on unaltered, so the Host header is theirs to
            // write. `http` is the truthful scheme: there is no TLS on this
            // path, and Tor encrypts the connection itself.
            let hostname = request_hostname(request.headers().get(header::HOST).and_then(|v| v.to_str().ok()));
            if hostname != address {
                log.debug("tor_unknown_host", fields! { "hostname" => hostname, "onion" => address });
                return Response::builder()
                    .status(StatusCode::NOT_FOUND)
                    .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
                    .body(Body::from("Not found."))
                    .expect("response");
            }
            // There is no visitor address to give: the peer is Tor.
            site.serve(request, RequestOrigin { hostname, scheme: "http" }, Peer::Unknown).await
        })
    }
}

/// Accept a stream if it is for the web port. Only a `BEGIN` is answered, and
/// only with `DONE` when refused, as C Tor does, so that nothing about the
/// refusal tells this service apart from any other.
async fn open(request: StreamRequest) -> Option<arti_client::DataStream> {
    if !matches!(request.request(), IncomingStreamRequest::Begin(begin) if begin.port() == ONION_PORT) {
        let _ = request.reject(End::new_with_reason(EndReason::DONE)).await;
        return None;
    }
    request.accept(Connected::new_empty()).await.ok()
}

/// An onion service's stable identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OnionKey {
    /// The address, with its `.onion` suffix.
    pub address: String,
    /// `ED25519-V3:<base64>`: the expanded secret key, as C Tor spells it in
    /// `ADD_ONION`. Keys minted before Tor was built in are in this form, and
    /// keeping it means a key can still be handed to C Tor if that is ever
    /// wanted.
    pub secret_key: String,
}

impl OnionKey {
    pub fn generate() -> OnionKey {
        let keypair = ed25519::ExpandedKeypair::from(&ed25519::Keypair::from(&rand::random::<[u8; 32]>()));
        let secret_key = format!("{KEY_PREFIX}{}", base64::engine::general_purpose::STANDARD.encode(keypair.to_secret_key_bytes()));
        OnionKey { address: address_of(&keypair), secret_key }
    }

    /// The key, checked against the address it is stored with.
    fn keypair(&self) -> anyhow::Result<HsIdKeypair> {
        let encoded = self.secret_key.strip_prefix(KEY_PREFIX).context("the stored key is not an ED25519-V3 key")?;
        // C Tor pads its base64, but accepts it either way; so does this.
        let lenient = base64::engine::GeneralPurpose::new(
            &base64::alphabet::STANDARD,
            base64::engine::GeneralPurposeConfig::new().with_decode_padding_mode(base64::engine::DecodePaddingMode::Indifferent),
        );
        let bytes: [u8; 64] = lenient
            .decode(encoded)
            .ok()
            .and_then(|bytes| bytes.try_into().ok())
            .context("the stored key is not 64 bytes of base64")?;
        let keypair = ed25519::ExpandedKeypair::from_secret_key_bytes(bytes).context("the stored key is not a valid ed25519 key")?;
        if address_of(&keypair) != self.address {
            return Err(anyhow!("the stored key is for {}, not {}", address_of(&keypair), self.address));
        }
        Ok(HsIdKeypair::from(keypair))
    }
}

fn address_of(keypair: &ed25519::ExpandedKeypair) -> String {
    HsIdKey::from(*keypair.public()).id().display_unredacted().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Made by C Tor 0.4.8 from a `HiddenServiceDir`: its `hostname`, and the
    /// last 64 bytes of `hs_ed25519_secret_key`, which are what `ADD_ONION`
    /// returns after `ED25519-V3:`.
    const C_TOR_ADDRESS: &str = "dl6nglbofs66w2ctcqp3hdgw7lgql7b5xc3dqtj3oqjrcihztrqrkjyd.onion";
    const C_TOR_KEY: &str = "ED25519-V3:MEaMJctlsgmU3HCYqZAda76Xvu0XFmMELLWNDm/8qWq6AYZW0AiNB8uF0+BTPKF/8eFTfIr9ZBF/TGWo1waDcA==";

    #[test]
    fn reads_keys_c_tor_made() {
        let key = OnionKey { address: C_TOR_ADDRESS.into(), secret_key: C_TOR_KEY.into() };
        let keypair = key.keypair().unwrap();
        assert_eq!(HsIdKey::from(&keypair).id().display_unredacted().to_string(), C_TOR_ADDRESS);

        let unpadded = OnionKey { secret_key: C_TOR_KEY.trim_end_matches('=').into(), ..key.clone() };
        assert!(unpadded.keypair().is_ok());
    }

    #[test]
    fn rejects_a_key_stored_against_the_wrong_address() {
        let other = OnionKey::generate();
        let swapped = OnionKey { address: other.address, secret_key: C_TOR_KEY.into() };
        assert!(swapped.keypair().unwrap_err().to_string().contains("not"));
        assert!(OnionKey { address: C_TOR_ADDRESS.into(), secret_key: "ED25519-V3:short".into() }.keypair().is_err());
        assert!(OnionKey { address: C_TOR_ADDRESS.into(), secret_key: "RSA1024:abc".into() }.keypair().is_err());
    }

    #[test]
    fn generated_keys_round_trip() {
        let key = OnionKey::generate();
        assert!(crate::hostname::is_onion_address(&key.address));
        assert!(key.secret_key.starts_with(KEY_PREFIX));
        assert!(key.keypair().is_ok());
        assert_ne!(OnionKey::generate(), key);
    }
}
