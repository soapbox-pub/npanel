//! npanel: a self-hostable gateway for nsites, with a team dashboard.
//!
//! `npanel` (or `npanel serve`) runs the gateway; `make-admin`,
//! `prune-blobs` and `compress-blobs` are the maintenance commands; and
//! `npanel tunnel` is the agent that serves a hostname from another machine.

mod analytics;
mod api;
mod app;
mod auth;
mod blobs;
mod builds;
mod config;
mod dashboard;
mod db;
mod deploy;
mod dns;
mod hash;
#[cfg(test)]
mod e2e;
mod errors;
mod hostname;
mod html;
mod identifiers;
mod inject;
mod link_previews;
mod log;
mod manifest;
mod mime;
mod net;
mod nostr;
mod nsite_address;
mod prune;
mod psl;
mod rate_limit;
mod scripts;
mod send_file;
mod seo;
mod serve;
mod server;
mod settings;
mod site_csp;
mod site_env;
mod site_preview;
mod sites;
mod supervise;
mod sync;
mod tls;
mod tls_check;
mod tor;
mod tunnel;
mod util;

use std::net::SocketAddr;
use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use clap::builder::TypedValueParser as _;
use clap::Parser;
use tokio_util::sync::CancellationToken;

use crate::analytics::{Analytics, AnalyticsConfig};
use crate::app::{App, Via};
use crate::blobs::BlobStore;
use crate::config::Config;
use crate::db::Db;
use crate::deploy::{DeployConfig, DeployService, Deployments};
use crate::errors::{Errors, ErrorsConfig};
use crate::log::Logger;
use crate::serve::{ServeCache, SiteServer, SiteServerDeps};
use crate::settings::Settings;

#[derive(Parser)]
#[command(name = "npanel", version, about = "A self-hostable nsite gateway with a team dashboard")]
struct Cli {
    /// Configuration file [default: the first that exists of
    /// $XDG_CONFIG_HOME/npanel/npanel.toml (~/.config/npanel/npanel.toml)
    /// and /etc/npanel/npanel.toml]
    #[arg(short, long, global = true, value_name = "PATH")]
    config: Option<PathBuf>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(clap::Subcommand)]
enum Command {
    /// Run the gateway (the default)
    Serve,
    /// Make a pubkey an admin, adding it if it isn't a user yet
    MakeAdmin {
        /// An npub, nprofile, or hex pubkey
        pubkey: String,
    },
    /// Forget old versions of sites and delete the files nothing uses, as the
    /// server does once a day
    PruneBlobs {
        /// Ready versions to keep per site, in place of the setting
        #[arg(long, value_name = "N")]
        keep: Option<NonZeroUsize>,
        /// Say what would go, and remove nothing
        #[arg(long)]
        dry_run: bool,
    },
    /// Store compressed copies of files downloaded before npanel made them
    CompressBlobs {
        /// Say what would be compressed, and write nothing
        #[arg(long)]
        dry_run: bool,
    },
    /// Serve a hostname on an npanel server from a web server on this machine
    Tunnel {
        /// The npanel server, by its dashboard's hostname
        server: String,
        /// What to forward to: a port on this machine, or a host and port
        target: String,
        /// What the dashboard calls this tunnel [default: this machine's name]
        #[arg(long)]
        name: Option<String>,
        /// How many connections to hold open to the server
        #[arg(long, default_value_t = tunnel::agent::DEFAULT_CONNECTIONS, value_parser = clap::value_parser!(u16).range(1..=1000).map(usize::from))]
        connections: usize,
    },
}

fn main() {
    // A preview script's process, which reads no configuration: nothing of
    // the gateway's is any business of a site's script.
    if std::env::args().nth(1).as_deref() == Some(link_previews::script::WORKER_COMMAND) {
        link_previews::script::worker_main();
    }
    let cli = Cli::parse();
    // rustls needs one crypto provider chosen for the process; relays and
    // blob downloads both go through it.
    let _ = rustls::crypto::ring::default_provider().install_default();

    // The agent runs on another machine than the gateway, with nothing of
    // the gateway's to configure.
    if let Some(Command::Tunnel { server, target, name, connections }) = cli.command {
        if let Err(error) = tunnel::agent::main(tunnel::agent::Options { server, target, name, connections }) {
            // Much of what can go wrong is the server's to say.
            eprintln!("{}", tunnel::agent::printable(&format!("{error:#}")));
            std::process::exit(1);
        }
        return;
    }

    let config = match Config::load(cli.config.as_deref(), &config::Environment::current()) {
        Ok(config) => config,
        Err(error) => {
            eprintln!("{error:#}");
            std::process::exit(1);
        }
    };
    let log = Logger::new(config.log_level);

    let result = match cli.command.unwrap_or(Command::Serve) {
        Command::Serve => serve(config, log),
        Command::MakeAdmin { pubkey } => scripts::make_admin(&config, &log, &pubkey),
        Command::PruneBlobs { keep, dry_run } => scripts::prune_blobs(&config, &log, keep, dry_run),
        Command::CompressBlobs { dry_run } => scripts::compress_blobs(&config, &log, dry_run),
        Command::Tunnel { .. } => unreachable!("handled before the config is read"),
    };

    if let Err(error) = result {
        eprintln!("{error:#}");
        std::process::exit(1);
    }
}

fn serve(config: Config, log: Logger) -> Result<()> {
    #[cfg(target_os = "linux")]
    raise_open_file_limit(log);
    tokio::runtime::Builder::new_multi_thread().enable_all().thread_name("npanel").build()?.block_on(run(config, log))
}

/// Take the hard limit on open files as the soft one, as Go's runtime does.
/// Connections and the large blobs kept open for sendfile are all
/// descriptors, and the soft limit a service starts with — 1024 under
/// systemd — is a default for select(2), which nothing here calls.
#[cfg(target_os = "linux")]
fn raise_open_file_limit(log: Logger) {
    let mut limit = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
    // SAFETY: `limit` is a valid rlimit for the call to fill in.
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) } != 0 || limit.rlim_cur >= limit.rlim_max {
        return;
    }
    let raised = libc::rlimit { rlim_cur: limit.rlim_max, rlim_max: limit.rlim_max };
    // SAFETY: as above; raising the soft limit up to the hard one needs no privilege.
    if unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &raised) } != 0 {
        log.warn("open_file_limit_unchanged", crate::log::err_fields(std::io::Error::last_os_error()));
    }
}

async fn run(config: Config, log: Logger) -> Result<()> {
    let db = Db::open(&config.database_path(), Some(&log))?;
    let settings = Settings::load(&db, Some(log))?;
    let blobs = BlobStore::new(&config.blobs_dir());

    // One instance: serving reads it, and every writer that changes what
    // serving would answer — the API, the deploy runtime, Tor — clears it.
    let serve_cache = Arc::new(ServeCache::new(db.clone()));
    let last_prune = prune::LastPrune::default();

    // Relays, downloads, compression and builds, on threads of their own so
    // that a deploy never makes a visitor wait.
    let deployments: Arc<DeployService> = Arc::new(DeployService::start(DeployConfig {
        db: db.clone(),
        blobs: blobs.clone(),
        settings: settings.clone(),
        log,
        serving_changed: {
            let cache = serve_cache.clone();
            Arc::new(move || cache.clear())
        },
        last_prune: last_prune.clone(),
    }));

    // npanel's Plausible server, on a thread of its own.
    let analytics = Arc::new(Analytics::start(
        db.clone(),
        settings.clone(),
        log,
        AnalyticsConfig { data_dir: config.data_dir.clone(), download_geo: true },
    ));

    // npanel's error tracker, on a thread of its own.
    let errors = Arc::new(Errors::start(db.clone(), blobs.clone(), settings.clone(), log, ErrorsConfig { data_dir: config.data_dir.clone() }));

    // Link previews for crawlers, looked up on a thread of their own.
    let link_previews = Arc::new(link_previews::LinkPreviews::start(
        settings.clone(),
        log,
        link_previews::LinkPreviewsConfig { data_dir: config.data_dir.clone(), source: None },
    ));

    let scripts = Arc::new(crate::link_previews::script::Scripts::new(db.clone(), blobs.clone()));
    let site_csp = Arc::new(site_csp::SiteCsp::new(db.clone(), blobs.clone(), log));
    let previews = Arc::new(site_preview::SitePreviews::new(blobs.clone()));
    let site = Arc::new(SiteServer::new(SiteServerDeps {
        db: db.clone(),
        blobs: blobs.clone(),
        site_csp: site_csp.clone(),
        serve_cache: serve_cache.clone(),
        analytics: analytics.clone(),
        errors: errors.clone(),
        link_previews: link_previews.clone(),
        scripts: scripts.clone(),
        log,
    }));

    // Onion services, if an admin has turned them on, on a thread of their
    // own. Joining the Tor network takes a while, and no listener waits for it.
    let tor = Arc::new(tor::TorService::start(tor::TorDeps {
        db: db.clone(),
        settings: settings.clone(),
        log,
        data_dir: config.data_dir.clone(),
        site: site.clone(),
        serve_cache: serve_cache.clone(),
        deployments: deployments.clone() as Arc<dyn Deployments>,
    }));

    let shutdown = CancellationToken::new();

    // Certificates, when npanel gets them itself, on a thread of their own.
    // Started before the listeners so that what is already stored is served
    // from the first connection.
    let tls = match config.https_port {
        Some(_) => Some(Arc::new(
            tls::TlsService::start(tls::TlsDeps {
                db: db.clone(),
                settings: settings.clone(),
                log,
                dashboard_host: config.dashboard_host.clone(),
                acme_root: None,
            })
            .await?,
        )),
        None => None,
    };

    // Bound before anything is served, so a port in use fails startup.
    let listener = server::bind(SocketAddr::new(config.bind_address, config.port)).await?;
    let second_listener = match config.https_port {
        Some(https_port) => server::bind(SocketAddr::new(config.bind_address, https_port)).await?,
        // A second listener, on its own port, so the proxy can ask which
        // hostnames may have a certificate without that question being
        // answerable from the internet.
        None => server::bind(SocketAddr::new(config.bind_address, config.tls_check_port)).await?,
    };

    let tunnels = Arc::new(tunnel::gateway::Tunnels::new(db.clone(), log)?);

    let api = api::router(Arc::new(api::ApiDeps {
        db: db.clone(),
        settings: settings.clone(),
        deployments: deployments.clone(),
        analytics: analytics.clone(),
        errors: errors.clone(),
        dns: dns::DnsTargets::new(config.dashboard_host.clone(), settings.clone(), log),
        log,
        tor: tor.clone(),
        tls: tls.clone(),
        site_csp,
        previews,
        link_previews: link_previews.clone(),
        scripts,
        serve_cache,
        last_prune,
        tunnels: tunnels.clone(),
        dashboard_host: config.dashboard_host.clone(),
    }));
    let dashboard = dashboard::Dashboard::new(dashboard::source(), &config.dashboard_host, log);
    dashboard.warm();
    let app = Arc::new(App { dashboard_host: config.dashboard_host.clone(), site, tunnels, dashboard, api });

    let servers = match (&tls, config.https_port) {
        (Some(tls), Some(https_port)) => {
            // Plain HTTP answers the CA's HTTP-01 challenges and sends
            // everything else to HTTPS, which serves the lot.
            let redirect = Arc::new(tls.redirect(https_port));
            let plain = tokio::spawn(server::serve(
                listener,
                move |request, _| {
                    let redirect = redirect.clone();
                    async move { redirect.handle(&request) }
                },
                shutdown.clone(),
                log,
            ));
            let https = tokio::spawn(tls::serve(
                second_listener,
                tls.server_config(),
                move |request, remote| {
                    let app = app.clone();
                    async move { app.handle(request, Via::Tls(remote.ip())).await }
                },
                shutdown.clone(),
                log,
            ));
            [plain, https]
        }
        _ => {
            let tls_check = Arc::new(tls_check::TlsCheck::new(db.clone(), settings.clone(), log, config.dashboard_host.clone()));
            let tls_server = tokio::spawn(server::serve(
                second_listener,
                move |request, _| {
                    let tls_check = tls_check.clone();
                    async move { tls_check.handle(request).await }
                },
                shutdown.clone(),
                log,
            ));
            let main_server = tokio::spawn(server::serve(
                listener,
                move |request, remote| {
                    let app = app.clone();
                    async move { app.handle(request, Via::Proxy(remote.ip())).await }
                },
                shutdown.clone(),
                log,
            ));
            [tls_server, main_server]
        }
    };

    log.info(
        "started",
        fields! {
            "bind_address" => config.bind_address.to_string(),
            "port" => config.port,
            "https_port" => config.https_port,
            "tls_check_port" => config.https_port.is_none().then_some(config.tls_check_port),
            "dashboard_host" => config.dashboard_host.as_str(),
            "data_dir" => config.data_dir.display().to_string(),
            "config" => config.source.display().to_string(),
        },
    );

    let signal = wait_for_signal().await;
    log.info("shutting_down", fields! { "signal" => signal });

    shutdown.cancel();
    for server in servers {
        let _ = server.await;
    }
    if let Some(tls) = &tls {
        tls.stop().await;
    }
    tor.stop().await;
    deployments.stop().await;
    analytics.stop().await;
    errors.stop().await;
    link_previews.stop().await;

    Ok(())
}

async fn wait_for_signal() -> &'static str {
    use tokio::signal::unix::{signal, SignalKind};
    let mut interrupt = signal(SignalKind::interrupt()).expect("a SIGINT handler");
    let mut terminate = signal(SignalKind::terminate()).expect("a SIGTERM handler");
    tokio::select! {
        _ = interrupt.recv() => "SIGINT",
        _ = terminate.recv() => "SIGTERM",
    }
}
