//! Publishes every enabled site as a Tor onion service.
//!
//! npanel drives a Tor daemon over its control port rather than writing
//! `torrc`: `ADD_ONION` registers a service immediately, with no restart and
//! no configuration file shared between the two processes. Tor generates the
//! key, npanel keeps it, and that is what makes an address survive a restart —
//! Tor stores nothing, so every service is re-registered from the database
//! each time npanel connects.
//!
//! Sites are reconciled rather than tracked. Every trigger — connecting, a
//! host being added or removed, a setting changing — asks the database which
//! hosts are enabled and makes Tor match. Nothing has to be remembered
//! between two of them.

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use http::{header, Response, StatusCode};
use parking_lot::Mutex;
use rusqlite::params;
use serde::Serialize;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::db::Db;
use crate::deploy::Deployments;
use crate::fields;
use crate::hostname::{is_onion_address, request_hostname};
use crate::log::{with_err, Logger};
use crate::serve::{RequestOrigin, ServeCache, SiteServer};
use crate::settings::{Settings, SettingsValues};
use crate::tor_control::{OnionKey, TorControl};

const RETRY_MIN: Duration = Duration::from_secs(1);
const RETRY_MAX: Duration = Duration::from_secs(30);

/// What the dashboard shows on the Settings page.
#[derive(Clone, Debug, Default, Serialize)]
pub struct TorStatus {
    pub enabled: bool,
    pub connected: bool,
    /// How many onion services are published right now.
    pub services: usize,
    /// Why Tor is unreachable, as a sentence, or empty when it isn't.
    pub error: String,
}

enum Command {
    /// Settings changed, or startup: reconnect to whatever is configured now.
    Apply,
    Connect,
    Reconcile,
    /// A control connection closed; the number says which one.
    Closed(u64),
    Stop(tokio::sync::oneshot::Sender<()>),
}

pub struct TorDeps {
    pub db: Db,
    pub settings: Arc<Settings>,
    pub log: Logger,
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
}

impl TorService {
    pub fn start(deps: TorDeps) -> TorService {
        let (tx, rx) = mpsc::unbounded_channel();
        let status = Arc::new(Mutex::new(TorStatus::default()));
        let settings = deps.settings.clone();

        // Reconnecting on any Tor setting is deliberate: a changed address or
        // password is a different Tor.
        let mut changes = settings.subscribe();
        let notify = tx.clone();
        tokio::spawn(async move {
            let tor = |s: &SettingsValues| (s.tor_enabled, s.tor_control_url.clone(), s.tor_control_password.clone());
            let mut seen = tor(&changes.borrow_and_update());
            while changes.changed().await.is_ok() {
                let now = tor(&changes.borrow_and_update());
                if now != seen && notify.send(Command::Apply).is_err() {
                    break;
                }
                seen = now;
            }
        });

        let _ = tx.send(Command::Apply);
        let actor = Actor {
            deps,
            tx: tx.clone(),
            status: status.clone(),
            control: None,
            generation: 0,
            live: HashMap::new(),
            server: None,
            retry: None,
            retry_delay: RETRY_MIN,
            last_error: String::new(),
        };
        tokio::spawn(actor.run(rx));

        TorService { tx, status, settings }
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
        let (done, wait) = tokio::sync::oneshot::channel();
        if self.tx.send(Command::Stop(done)).is_ok() {
            let _ = wait.await;
        }
    }
}

struct Actor {
    deps: TorDeps,
    tx: mpsc::UnboundedSender<Command>,
    status: Arc<Mutex<TorStatus>>,
    control: Option<Arc<TorControl>>,
    generation: u64,
    /// Onion services this control connection has published, by host id.
    live: HashMap<i64, OnionKey>,
    /// The listener Tor forwards to: its shutdown, and its port.
    server: Option<(CancellationToken, u16)>,
    retry: Option<tokio::task::JoinHandle<()>>,
    retry_delay: Duration,
    last_error: String,
}

impl Actor {
    async fn run(mut self, mut rx: mpsc::UnboundedReceiver<Command>) {
        while let Some(command) = rx.recv().await {
            match command {
                Command::Apply => self.apply().await,
                Command::Connect => self.connect().await,
                Command::Reconcile => self.reconcile().await,
                Command::Closed(generation) => {
                    if generation == self.generation && self.control.is_some() {
                        self.control = None;
                        // Every service went with the connection.
                        self.live.clear();
                        self.fail("The Tor control connection closed.".into());
                    }
                }
                Command::Stop(done) => {
                    self.shut_down();
                    let _ = done.send(());
                    return;
                }
            }
            self.publish_status();
        }
    }

    fn publish_status(&self) {
        let mut status = self.status.lock();
        status.connected = self.control.is_some();
        status.services = self.live.len();
        status.error = self.last_error.clone();
    }

    async fn apply(&mut self) {
        self.shut_down();
        if !self.deps.settings.get().tor_enabled {
            self.deps.log.info("tor_disabled", fields! {});
            return;
        }
        self.retry_delay = RETRY_MIN;
        self.connect().await;
    }

    async fn connect(&mut self) {
        let settings = self.deps.settings.get();
        if !settings.tor_enabled || self.control.is_some() {
            return;
        }

        let control = match TorControl::connect(&settings.tor_control_url, &settings.tor_control_password).await {
            Ok(control) => Arc::new(control),
            Err(error) => {
                self.fail(error.to_string());
                return;
            }
        };

        // Bound only once Tor is known to be reachable, so an instance with Tor
        // switched off holds no port at all.
        if let Err(error) = self.listen().await {
            control.close();
            self.fail(error.to_string());
            return;
        }

        self.generation += 1;
        let (generation, closed, tx) = (self.generation, control.closed.clone(), self.tx.clone());
        tokio::spawn(async move {
            closed.cancelled().await;
            let _ = tx.send(Command::Closed(generation));
        });

        self.control = Some(control);
        self.last_error.clear();
        self.retry_delay = RETRY_MIN;
        self.deps.log.info(
            "tor_connected",
            fields! { "control_url" => settings.tor_control_url.as_str(), "port" => self.server.as_ref().map(|(_, p)| *p) },
        );
        self.reconcile().await;
    }

    fn fail(&mut self, error: String) {
        self.last_error = error.clone();
        if !self.deps.settings.get().tor_enabled {
            return;
        }
        self.deps.log.warn("tor_unavailable", fields! { "retry_in" => self.retry_delay.as_secs(), "err_msg" => error });

        if let Some(retry) = self.retry.take() {
            retry.abort();
        }
        let (tx, delay) = (self.tx.clone(), self.retry_delay);
        self.retry = Some(tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            let _ = tx.send(Command::Connect);
        }));
        self.retry_delay = (self.retry_delay * 2).min(RETRY_MAX);
    }

    /// Make Tor's published services match the enabled hosts. Never fails: one
    /// site's key being unusable is not a reason to leave the rest unpublished.
    async fn reconcile(&mut self) {
        let (Some(control), Some((_, port))) = (self.control.clone(), self.server.clone()) else {
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
        let stale: Vec<(i64, OnionKey)> = self.live.iter().filter(|(id, _)| !keep.contains(id)).map(|(id, k)| (*id, k.clone())).collect();
        for (id, key) in stale {
            if let Err(error) = control.del_onion(&key.address).await {
                // A stale service is cleared by the reconnect that follows any
                // real control failure.
                self.deps.log.warn("tor_onion_unpublish_failed", with_err(fields! { "onion" => key.address.as_str() }, error));
            }
            self.live.remove(&id);
            self.deps.log.info("tor_onion_unpublished", fields! { "onion" => key.address });
        }

        for (id, hostname, address, secret_key) in wanted {
            if self.live.contains_key(&id) {
                continue;
            }
            let existing = address.zip(secret_key).map(|(address, secret_key)| OnionKey { address, secret_key });
            match control.add_onion(port, existing.as_ref()).await {
                Ok(key) => {
                    if existing.is_none() {
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
                            self.deps.log.error("tor_onion_failed", with_err(fields! { "hostname" => hostname.as_str() }, error));
                            continue;
                        }
                        // The host now answers at the onion too, and the onion
                        // needs a build naming it before it serves anything.
                        self.deps.serve_cache.clear();
                        self.deps.deployments.rebuild(vec![id]);
                    }
                    self.deps.log.info(
                        "tor_onion_published",
                        fields! { "hostname" => hostname.as_str(), "onion" => key.address.as_str(), "new" => existing.is_none() },
                    );
                    self.live.insert(id, key);
                }
                Err(error) => self.deps.log.error("tor_onion_failed", with_err(fields! { "hostname" => hostname.as_str() }, error)),
            }
        }
    }

    /// Bind the listener Tor forwards to, on loopback and a port the kernel
    /// picks: npanel is the one telling Tor where to send traffic, so nobody
    /// else ever needs the number.
    async fn listen(&mut self) -> anyhow::Result<()> {
        if self.server.is_some() {
            return Ok(());
        }
        let listener = crate::server::bind(SocketAddr::from(([127, 0, 0, 1], 0))).await?;
        let port = listener.local_addr()?.port();
        let shutdown = CancellationToken::new();

        let (site, log) = (self.deps.site.clone(), self.deps.log);
        tokio::spawn(crate::server::serve(
            listener,
            move |request, _| {
                let site = site.clone();
                async move {
                    // Tor hands a visitor's bytes on unaltered, so onion
                    // addresses are the only names this listener answers for:
                    // the dashboard and the API can't be reached through one
                    // however a request dresses itself up. `http` is the
                    // truthful scheme: there is no proxy and no TLS on this
                    // path, and Tor encrypts the connection itself.
                    let hostname = request_hostname(request.headers().get(header::HOST).and_then(|v| v.to_str().ok()));
                    if !is_onion_address(&hostname) {
                        log.debug("tor_unknown_host", fields! { "hostname" => hostname });
                        return Response::builder()
                            .status(StatusCode::NOT_FOUND)
                            .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
                            .body(Body::from("Not found."))
                            .expect("response");
                    }
                    // The peer is Tor's own proxy, never the visitor.
                    site.serve(request, RequestOrigin { hostname, scheme: "http" }, None).await
                }
            },
            shutdown.clone(),
            log,
        ));

        self.server = Some((shutdown, port));
        Ok(())
    }

    fn shut_down(&mut self) {
        if let Some(retry) = self.retry.take() {
            retry.abort();
        }
        // Closing the control connection unregisters every service with it,
        // since none of them were detached.
        if let Some(control) = self.control.take() {
            control.close();
        }
        self.live.clear();
        self.last_error.clear();
        if let Some((shutdown, _)) = self.server.take() {
            shutdown.cancel();
        }
    }
}
