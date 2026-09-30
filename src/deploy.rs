//! The thread sync and builds run on, and its handle.
//!
//! Everything expensive about deploying a site happens here: checking the
//! signature of every event a relay sends, downloading and hashing every
//! blob, compressing at brotli's slowest setting, rewriting pages for a build,
//! and the database writes that record each step. It runs on a runtime of its
//! own, on threads of its own, so serving goes on answering requests for the
//! whole of a large deploy.
//!
//! None of what the rest of npanel says to it has to arrive. Sync and builds
//! are reconciled rather than tracked, so a deploy runtime starting up does
//! everything a missed message would have asked for: it resyncs every site
//! and rebuilds every host whose build is out of date. That is what makes it
//! cheap to restart, which is what happens if it ever dies.

use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use tokio::sync::{mpsc, oneshot};

use crate::blobs::BlobStore;
use crate::builds::{HostBuilder, HostBuilderDeps};
use crate::db::Db;
use crate::fields;
use crate::log::{with_err, Logger};
use crate::nostr::service::{NostrService, NostrSource};
use crate::nsite_address::NsiteAddress;
use crate::settings::Settings;
use crate::sync::{SyncWorker, SyncWorkerDeps};

/// First and longest wait before restarting a deploy runtime that died.
const RESTART_MIN: Duration = Duration::from_secs(1);
const RESTART_MAX: Duration = Duration::from_secs(60);

/// A runtime that ran this long before dying is restarted without the backoff
/// it had built up.
const HEALTHY_AFTER: Duration = Duration::from_secs(60);

/// What the rest of npanel asks of deployments. Every method is a
/// notification: none waits for the work it starts, which can take minutes.
pub trait Deployments: Send + Sync {
    /// Hosts were added, removed, retargeted or toggled: resubscribe, and
    /// fetch what's new.
    fn hosts_changed(&self);
    /// Reconcile these hosts' builds, since something they're made of may
    /// have changed.
    fn rebuild(&self, host_ids: Vec<i64>);
    /// Fetch a site's newest manifest now, and deploy it if it isn't ready.
    fn sync_site(&self, address: NsiteAddress);
}

/// Sync and builds, wired together: everything the deploy runtime does.
#[derive(Clone)]
pub struct Deployer {
    pub builder: HostBuilder,
    pub sync: SyncWorker,
    log: Logger,
}

pub struct DeployerDeps {
    pub db: Db,
    pub blobs: BlobStore,
    pub nostr: Arc<dyn NostrSource>,
    pub settings: Arc<Settings>,
    pub log: Logger,
    pub serving_changed: Arc<dyn Fn() + Send + Sync>,
    pub retry_backoff: Option<Duration>,
}

impl Deployer {
    pub fn new(deps: DeployerDeps) -> Deployer {
        let builder = HostBuilder::new(HostBuilderDeps {
            db: deps.db.clone(),
            blobs: deps.blobs.clone(),
            settings: deps.settings.clone(),
            serving_changed: deps.serving_changed,
            log: deps.log,
            retry_backoff: deps.retry_backoff.unwrap_or(crate::builds::RETRY_BACKOFF),
        });
        let sync = SyncWorker::new(SyncWorkerDeps {
            db: deps.db,
            blobs: deps.blobs,
            nostr: deps.nostr,
            settings: deps.settings,
            builder: builder.clone(),
            log: deps.log,
            retry_backoff: deps.retry_backoff.unwrap_or(crate::sync::RETRY_BACKOFF),
        });
        Deployer { builder, sync, log: deps.log }
    }

    /// Builds first, so a deploy that finishes early finds nothing stale left
    /// to clear.
    pub async fn start(&self) {
        if let Err(error) = self.builder.start().await {
            self.log.error("build_start_failed", crate::log::err_fields(error));
        }
        self.sync.start().await;
    }

    pub async fn stop(&self) {
        self.sync.stop().await;
        self.builder.stop().await;
    }
}

impl Deployments for Deployer {
    fn hosts_changed(&self) {
        let sync = self.sync.clone();
        tokio::spawn(async move { sync.hosts_changed().await });
    }

    fn rebuild(&self, host_ids: Vec<i64>) {
        self.builder.hosts_changed(Some(&host_ids));
    }

    fn sync_site(&self, address: NsiteAddress) {
        // Asked for by someone pressing a button, with nobody waiting on the
        // answer: the dashboard polls the host for how it went.
        let sync = self.sync.clone();
        let log = self.log;
        tokio::spawn(async move {
            if let Err(error) = sync.sync_site(&address).await {
                log.warn(
                    "sync_site_failed",
                    with_err(fields! { "pubkey" => address.pubkey.as_str(), "identifier" => address.identifier.as_str() }, error),
                );
            }
        });
    }
}

enum Command {
    HostsChanged,
    Rebuild(Vec<i64>),
    SyncSite(NsiteAddress),
    Stop(oneshot::Sender<()>),
}

/// Runs the deploy runtime on a thread of its own, and keeps it running.
///
/// Messages sent while it is starting wait in the channel and are delivered
/// once it is up. A runtime that dies is started again after a second, then
/// doubling, and what it was asked before it died needs no replaying, because
/// a new one reconciles everything as it starts.
pub struct DeployService {
    tx: mpsc::UnboundedSender<Command>,
    thread: Mutex<Option<std::thread::JoinHandle<()>>>,
}

pub struct DeployConfig {
    pub db: Db,
    pub blobs: BlobStore,
    pub settings: Arc<Settings>,
    pub log: Logger,
    pub serving_changed: Arc<dyn Fn() + Send + Sync>,
}

impl DeployService {
    pub fn start(config: DeployConfig) -> DeployService {
        let (tx, mut rx) = mpsc::unbounded_channel();

        let thread = std::thread::Builder::new()
            .name("npanel-deploy".into())
            .spawn(move || {
                let mut delay = RESTART_MIN;
                loop {
                    let started = Instant::now();
                    let runtime = tokio::runtime::Builder::new_multi_thread()
                        .worker_threads(2)
                        .thread_name("npanel-deploy")
                        .enable_all()
                        .build()
                        .expect("a deploy runtime");

                    let result = std::panic::catch_unwind(AssertUnwindSafe(|| runtime.block_on(run(&config, &mut rx))));
                    runtime.shutdown_timeout(Duration::from_secs(5));

                    match result {
                        // Stopped, or everyone who could send has gone.
                        Ok(()) => return,
                        Err(_) => {
                            if started.elapsed() > HEALTHY_AFTER {
                                delay = RESTART_MIN;
                            }
                            config.log.error("deploy_thread_exited", fields! { "restart_in_ms" => delay.as_millis() as u64 });
                            std::thread::sleep(delay);
                            delay = (delay * 2).min(RESTART_MAX);
                        }
                    }
                }
            })
            .expect("a deploy thread");

        DeployService { tx, thread: Mutex::new(Some(thread)) }
    }

    /// Stop the runtime, letting any deploy in progress leave itself resumable.
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

impl Deployments for DeployService {
    fn hosts_changed(&self) {
        let _ = self.tx.send(Command::HostsChanged);
    }

    fn rebuild(&self, host_ids: Vec<i64>) {
        let _ = self.tx.send(Command::Rebuild(host_ids));
    }

    fn sync_site(&self, address: NsiteAddress) {
        let _ = self.tx.send(Command::SyncSite(address));
    }
}

async fn run(config: &DeployConfig, rx: &mut mpsc::UnboundedReceiver<Command>) {
    let nostr: Arc<dyn NostrSource> = Arc::new(NostrService::new(config.settings.clone(), config.log));
    let deployer = Deployer::new(DeployerDeps {
        db: config.db.clone(),
        blobs: config.blobs.clone(),
        nostr,
        settings: config.settings.clone(),
        log: config.log,
        serving_changed: config.serving_changed.clone(),
        retry_backoff: None,
    });

    deployer.start().await;
    config.log.info("deploy_thread_started", fields! {});

    while let Some(command) = rx.recv().await {
        match command {
            Command::HostsChanged => deployer.hosts_changed(),
            Command::Rebuild(ids) => deployer.rebuild(ids),
            Command::SyncSite(address) => deployer.sync_site(address),
            Command::Stop(done) => {
                deployer.stop().await;
                let _ = done.send(());
                return;
            }
        }
    }

    deployer.stop().await;
}
