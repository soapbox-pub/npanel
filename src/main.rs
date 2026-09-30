//! npanel: a self-hostable gateway for nsites, with a team dashboard.
//!
//! `npanel` (or `npanel serve`) runs the gateway; `make-admin`,
//! `prune-blobs` and `compress-blobs` are the maintenance commands.

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
#[cfg(test)]
mod e2e;
mod hostname;
mod html;
mod identifiers;
mod log;
mod manifest;
mod mime;
mod net;
mod nostr;
mod nsite_address;
mod psl;
mod rate_limit;
mod scripts;
mod seo;
mod serve;
mod server;
mod settings;
mod site_csp;
mod site_preview;
mod sites;
mod sync;
mod tls_check;
mod tor;
mod tor_control;
mod util;

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Result;
use tokio_util::sync::CancellationToken;

use crate::analytics::{Analytics, AnalyticsConfig};
use crate::app::App;
use crate::blobs::BlobStore;
use crate::config::Config;
use crate::db::Db;
use crate::deploy::{DeployConfig, DeployService, Deployments};
use crate::log::Logger;
use crate::serve::{ServeCache, SiteServer, SiteServerDeps};
use crate::settings::Settings;

fn main() {
    config::load_dotenv();
    // rustls needs one crypto provider chosen for the process; relays and
    // blob downloads both go through it.
    let _ = rustls::crypto::ring::default_provider().install_default();

    let args: Vec<String> = std::env::args().skip(1).collect();
    let config = match Config::from_env(|key| std::env::var(key).ok()) {
        Ok(config) => config,
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    };
    let log = Logger::new(config.log_level);

    let result = match args.first().map(String::as_str) {
        None | Some("serve") => serve(config, log),
        Some("make-admin") => scripts::make_admin(&config, &log, args.get(1).map(String::as_str)),
        Some("prune-blobs") => scripts::prune_blobs(&config, &log, &args[1..]),
        Some("compress-blobs") => scripts::compress_blobs(&config, &log, &args[1..]),
        Some(other) => Err(anyhow::anyhow!(
            "Unknown command {other}. Use serve, make-admin <npub>, prune-blobs [--keep N] [--dry-run], or compress-blobs [--dry-run]."
        )),
    };

    if let Err(error) = result {
        eprintln!("{error:#}");
        std::process::exit(1);
    }
}

fn serve(config: Config, log: Logger) -> Result<()> {
    tokio::runtime::Builder::new_multi_thread().enable_all().thread_name("npanel").build()?.block_on(run(config, log))
}

async fn run(config: Config, log: Logger) -> Result<()> {
    let db = Db::open(&config.database_path(), Some(&log))?;
    let settings = Settings::load(&db, Some(log))?;
    let blobs = BlobStore::new(&config.blobs_dir());

    // One instance: serving reads it, and every writer that changes what
    // serving would answer — the API, the deploy runtime, Tor — clears it.
    let serve_cache = Arc::new(ServeCache::new(db.clone()));

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
    }));

    // npanel's Plausible server, on a thread and a port of its own.
    let analytics = Arc::new(Analytics::start(
        db.clone(),
        settings.clone(),
        log,
        AnalyticsConfig { port: config.analytics_port, address: config.bind_address, data_dir: config.data_dir.clone(), download_geo: true },
    ));

    let site_csp = Arc::new(site_csp::SiteCsp::new(db.clone(), blobs.clone(), log));
    let previews = Arc::new(site_preview::SitePreviews::new(blobs.clone()));
    let site = Arc::new(SiteServer::new(SiteServerDeps {
        db: db.clone(),
        blobs: blobs.clone(),
        site_csp: site_csp.clone(),
        serve_cache: serve_cache.clone(),
        analytics: analytics.clone(),
        log,
    }));

    // Onion services, if an admin has turned them on. An unreachable Tor
    // retries on its own, and no listener waits for it.
    let tor = Arc::new(tor::TorService::start(tor::TorDeps {
        db: db.clone(),
        settings: settings.clone(),
        log,
        site: site.clone(),
        serve_cache: serve_cache.clone(),
        deployments: deployments.clone() as Arc<dyn Deployments>,
    }));

    let shutdown = CancellationToken::new();

    // A second listener, on its own port, so the proxy can ask which hostnames
    // may have a certificate without that question being answerable from the
    // internet. Bound first, so a port in use fails startup.
    let tls_listener = server::bind(SocketAddr::new(config.bind_address, config.tls_check_port)).await?;
    let listener = server::bind(SocketAddr::new(config.bind_address, config.port)).await?;

    let tls_check = Arc::new(tls_check::TlsCheck::new(db.clone(), settings.clone(), log, config.dashboard_host.clone()));
    let tls_server = tokio::spawn(server::serve(
        tls_listener,
        move |request, _| {
            let tls_check = tls_check.clone();
            async move { tls_check.handle(request).await }
        },
        shutdown.clone(),
        log,
    ));

    let api = api::router(Arc::new(api::ApiDeps {
        db: db.clone(),
        settings: settings.clone(),
        deployments: deployments.clone(),
        analytics: analytics.clone(),
        dns: dns::DnsTargets::new(config.dashboard_host.clone(), settings.clone(), log),
        log,
        tor: tor.clone(),
        site_csp,
        previews,
        serve_cache,
        dashboard_host: config.dashboard_host.clone(),
    }));
    let app = Arc::new(App {
        dashboard_host: config.dashboard_host.clone(),
        site,
        dashboard: dashboard::Dashboard::new(dashboard::candidates(), &config.dashboard_host, log),
        api,
    });
    let main_server = tokio::spawn(server::serve(
        listener,
        move |request, _| {
            let app = app.clone();
            async move { app.handle(request).await }
        },
        shutdown.clone(),
        log,
    ));

    log.info(
        "started",
        fields! {
            "bind_address" => config.bind_address.to_string(),
            "port" => config.port,
            "tls_check_port" => config.tls_check_port,
            "analytics_port" => config.analytics_port,
            "dashboard_host" => config.dashboard_host.as_str(),
            "data_dir" => config.data_dir.display().to_string(),
        },
    );

    let signal = wait_for_signal().await;
    log.info("shutting_down", fields! { "signal" => signal });

    shutdown.cancel();
    let _ = tls_server.await;
    let _ = main_server.await;
    tor.stop().await;
    deployments.stop().await;
    analytics.stop().await;

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
