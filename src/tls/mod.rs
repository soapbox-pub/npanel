//! HTTPS served by npanel itself, with certificates it gets over ACME.
//!
//! Optional: only with `https_port` set. Otherwise a reverse proxy terminates
//! TLS, and all npanel does about certificates is answer its `/tls-check`.
//!
//! Certificates are reconciled, not tracked, like builds and onions. Every
//! trigger — startup, a host being added, edited or removed, a setting
//! changing, a timer — asks the database which names may have a certificate
//! ([`names`]) and makes what is stored match: a name without one gets one,
//! one with a third of its lifetime left is renewed, and a name whose last
//! attempt failed waits out its backoff first. So a restart replays nothing,
//! and the runtime this runs on is cheap to restart.
//!
//! That runtime is its own, on a thread of its own (`src/supervise.rs`):
//! talking to the CA, generating keys and parsing certificates happen there,
//! never on the threads serving visitors. What crosses over is [`Shared`] —
//! the certificates served, and the challenges a CA is validating — which the
//! HTTPS listener reads on every handshake without waiting on anything.
//!
//! npanel keeps the keys, as it does the onion keys: certificates and the ACME
//! account live in the database (`store.rs`), so a database backup is a
//! complete one.

mod acme;
mod listener;
pub mod names;
mod redirect;
mod store;

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use arc_swap::ArcSwap;
use instant_acme::Account;
use parking_lot::Mutex;
use rusqlite::Connection;
use rustls::server::{ClientHello, ResolvesServerCert};
use rustls::sign::CertifiedKey;
use serde::Serialize;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use crate::db::{Db, Host, Tunnel};
use crate::fields;
use crate::log::{with_err, Logger};
use crate::rate_limit::{RateLimit, RateLimiter};
use crate::settings::Settings;
use crate::util::now;

pub use listener::serve;
pub use redirect::Redirect;
pub use store::CertificateStatus;
use store::{Certificate, Row};

/// The ALPN protocol a CA's TLS-ALPN-01 validation offers (RFC 8737).
const ACME_TLS_ALPN: &[u8] = b"acme-tls/1";

/// The longest between two looks at the certificates, so a clock that jumps
/// or a machine that sleeps can't carry one past its renewal.
const CHECK_INTERVAL: Duration = Duration::from_secs(12 * 60 * 60);

/// The wait after a name's first failure, doubling with each after it up to
/// an hour. Let's Encrypt allows five failed validations per name an hour,
/// and this makes five in the first: at 0, 2, 6, 14 and 30 minutes.
const FIRST_RETRY_SECS: i64 = 2 * 60;
const LAST_RETRY_SECS: i64 = 60 * 60;

/// How long to wait when the hourly budget has run out.
const THROTTLED_WAIT: Duration = Duration::from_secs(60);

/// The window both certificate limits are expressed over, in ms.
const HOUR_MS: f64 = 60.0 * 60.0 * 1000.0;

/// The hourly limits on certificates authorized, per hostname and in total
/// (`tls_check_limit_per_host`, `tls_check_limit_total`): metering Caddy's
/// asks behind a proxy, and npanel's own orders without one.
pub struct Budget {
    settings: Arc<Settings>,
    per_host: Mutex<RateLimiter>,
    total: Mutex<RateLimiter>,
}

impl Budget {
    pub fn new(settings: Arc<Settings>) -> Budget {
        Budget { settings, per_host: Mutex::new(RateLimiter::new(4096)), total: Mutex::new(RateLimiter::new(1)) }
    }

    /// Spend one authorization on `hostname`, or say which limit is spent.
    pub fn take(&self, hostname: &str) -> Result<(), &'static str> {
        let settings = self.settings.get();
        let within_host =
            self.per_host.lock().take(hostname, RateLimit { capacity: settings.tls_check_limit_per_host as f64, interval_ms: HOUR_MS });
        if !within_host {
            // Before the total, deliberately: a host already over its own
            // limit must not go on draining the budget the others share.
            return Err("host");
        }
        if !self.total.lock().take("", RateLimit { capacity: settings.tls_check_limit_total as f64, interval_ms: HOUR_MS }) {
            return Err("total");
        }
        Ok(())
    }
}

/// What the listeners and the ACME client share.
#[derive(Default)]
pub struct Shared {
    /// The certificates served, by hostname.
    certificates: ArcSwap<HashMap<String, Arc<CertifiedKey>>>,
    /// Every name that may have a certificate, whether or not it has one yet:
    /// the names plain HTTP redirects.
    names: ArcSwap<BTreeSet<String>>,
    /// The certificates pending TLS-ALPN-01 challenges are answered with, by
    /// hostname.
    alpn_challenges: Mutex<HashMap<String, Arc<CertifiedKey>>>,
    /// Pending HTTP-01 challenges: token to hostname and key authorization.
    http_challenges: Mutex<HashMap<String, (String, String)>>,
}

impl std::fmt::Debug for Shared {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Shared").field("certificates", &self.certificates.load().len()).finish_non_exhaustive()
    }
}

impl ResolvesServerCert for Shared {
    fn resolve(&self, hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        // No SNI, an IP address, or a name nothing is configured for: no
        // certificate, and the handshake fails. Anything else would be a
        // certificate for some other name, which a browser rejects anyway.
        let name = hello.server_name()?.trim_end_matches('.').to_ascii_lowercase();
        // Only a CA validating a challenge offers acme-tls/1, and it offers
        // nothing else (RFC 8737 §3).
        let validating = hello.alpn().is_some_and(|mut protocols| protocols.any(|protocol| protocol == ACME_TLS_ALPN));
        if validating {
            return self.alpn_challenges.lock().get(&name).cloned();
        }
        self.certificates.load().get(&name).cloned()
    }
}

pub struct TlsDeps {
    pub db: Db,
    pub settings: Arc<Settings>,
    pub log: Logger,
    /// Always has a certificate, whatever the hosts say.
    pub dashboard_host: String,
    /// A CA certificate to trust the directory by, besides the usual roots:
    /// for a test CA.
    pub acme_root: Option<reqwest::Certificate>,
}

/// What the Settings page shows.
#[derive(Clone, Debug, Serialize)]
pub struct TlsStatus {
    pub directory: String,
    /// Names that may have a certificate.
    pub names: usize,
    /// Names with one that hasn't expired.
    pub issued: i64,
    /// Names whose last attempt failed, and why.
    pub failing: Vec<CertificateStatus>,
}

pub struct TlsService {
    deps: Arc<TlsDeps>,
    shared: Arc<Shared>,
    changed: Arc<Notify>,
    stop: CancellationToken,
    thread: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl TlsService {
    /// Load the certificates already stored, so the HTTPS listener can serve
    /// them from its first connection, and start keeping them current.
    pub async fn start(deps: TlsDeps) -> Result<TlsService> {
        let deps = Arc::new(deps);
        let shared = Arc::new(Shared::default());
        refresh(&deps, &shared, &deps.settings.get().acme_directory).await?;

        let changed = Arc::new(Notify::new());
        let stop = CancellationToken::new();
        let budget = Arc::new(Budget::new(deps.settings.clone()));
        let thread = {
            let (deps, shared, changed, stop) = (deps.clone(), shared.clone(), changed.clone(), stop.clone());
            crate::supervise::supervise("tls", 1, deps.log, move |runtime| {
                let reconciler = Reconciler {
                    deps: deps.clone(),
                    shared: shared.clone(),
                    budget: budget.clone(),
                    changed: changed.clone(),
                    stop: stop.clone(),
                    account: None,
                };
                runtime.block_on(reconciler.run());
                Ok(())
            })
        };
        Ok(TlsService { deps, shared, changed, stop, thread: Mutex::new(Some(thread)) })
    }

    /// Re-check which names should have a certificate. Returns at once.
    pub fn hosts_changed(&self) {
        self.changed.notify_one();
    }

    /// What HTTPS connections are accepted with. Its certificates follow
    /// whatever is issued.
    pub fn server_config(&self) -> Arc<rustls::ServerConfig> {
        listener::server_config(self.shared.clone())
    }

    /// What answers plain HTTP: challenges, and redirects to `https_port`.
    pub fn redirect(&self, https_port: u16) -> Redirect {
        Redirect::new(self.shared.clone(), https_port)
    }

    pub async fn status(&self) -> Result<TlsStatus> {
        let directory = self.deps.settings.get().acme_directory.clone();
        let names = self.shared.names.load().len();
        let dir = directory.clone();
        let (issued, failing) = self
            .deps
            .db
            .call(move |conn| {
                let now = now();
                Ok((store::issued(conn, &dir, now)?, store::failing(conn, &dir, now)?))
            })
            .await?;
        // A failure for a name nothing wants any more is history, not news.
        let wanted = self.shared.names.load();
        let failing = failing.into_iter().filter(|status| wanted.contains(&status.hostname)).collect();
        Ok(TlsStatus { directory, names, issued, failing })
    }

    /// The certificates a host's names have, or are waiting for: its own,
    /// and its `www` if that is redirected.
    pub fn host_certificates(&self, conn: &Connection, host: &Host) -> rusqlite::Result<Vec<CertificateStatus>> {
        self.certificates(conn, names::of_host(&host.hostname, host.enabled, host.redirect_www))
    }

    /// A tunnel's certificate, or the wait for one. Nothing redirects to a
    /// tunnel, so it has only its own name.
    pub fn tunnel_certificates(&self, conn: &Connection, tunnel: &Tunnel) -> rusqlite::Result<Vec<CertificateStatus>> {
        self.certificates(conn, names::of_host(&tunnel.hostname, tunnel.enabled, false))
    }

    fn certificates(&self, conn: &Connection, names: Vec<String>) -> rusqlite::Result<Vec<CertificateStatus>> {
        let directory = self.deps.settings.get().acme_directory.clone();
        let now = now();
        names
            .into_iter()
            .map(|name| {
                let status = store::status(conn, &directory, &name, now)?;
                Ok(status.unwrap_or(CertificateStatus { hostname: name, expires_at: None, error: None, failures: 0, attempted_at: None }))
            })
            .collect()
    }

    pub async fn stop(&self) {
        self.stop.cancel();
        let thread = self.thread.lock().take();
        if let Some(thread) = thread {
            let _ = tokio::task::spawn_blocking(move || thread.join()).await;
        }
    }
}

/// Read which names want a certificate and which are stored under
/// `directory`, and serve exactly the stored ones that are wanted. Returns
/// the wanted names and their rows. A stored certificate that can't be read
/// comes back without its validity, so it is issued again.
async fn refresh(deps: &TlsDeps, shared: &Shared, directory: &str) -> Result<(BTreeSet<String>, HashMap<String, Row>)> {
    let (dir, dashboard_host) = (directory.to_string(), deps.dashboard_host.clone());
    let (names, loaded, rows) = deps
        .db
        .call(move |conn| {
            let mut names = names::all(conn)?;
            names.insert(dashboard_host);
            let loaded = store::load(conn, &dir, &names, now())?;
            let rows = store::rows(conn, &dir)?;
            Ok((names, loaded, rows))
        })
        .await?;

    let mut rows: HashMap<String, Row> = rows.into_iter().map(|row| (row.hostname.clone(), row)).collect();
    for (hostname, error) in loaded.unusable {
        deps.log.error("tls_certificate_unusable", with_err(fields! { "hostname" => hostname.as_str() }, error));
        if let Some(row) = rows.get_mut(&hostname) {
            row.validity = None;
        }
    }
    let certificates = loaded.certificates.into_iter().map(|(hostname, certificate)| (hostname, certificate.key)).collect();
    shared.certificates.store(Arc::new(certificates));
    shared.names.store(Arc::new(names.clone()));
    Ok((names, rows))
}

/// When `row`'s name next needs an order: now if it has no certificate, when
/// its certificate is due for renewal otherwise, and never before the backoff
/// its failures have earned.
fn due_at(row: Option<&Row>) -> i64 {
    let Some(row) = row else { return 0 };
    let renew = row.validity.map_or(0, |(not_before, not_after)| store::renew_at(not_before, not_after));
    let retry = match (row.failures, row.attempted_at) {
        (failures, Some(attempted_at)) if failures > 0 => attempted_at + backoff(failures),
        _ => 0,
    };
    renew.max(retry)
}

/// Seconds to wait after `failures` failed attempts in a row.
fn backoff(failures: i64) -> i64 {
    let doublings = (failures - 1).clamp(0, 16) as u32;
    (FIRST_RETRY_SECS << doublings).min(LAST_RETRY_SECS)
}

struct Reconciler {
    deps: Arc<TlsDeps>,
    shared: Arc<Shared>,
    budget: Arc<Budget>,
    changed: Arc<Notify>,
    stop: CancellationToken,
    /// The account last used: its directory, the email it was brought in
    /// line with, and the account.
    account: Option<(String, String, Account)>,
}

impl Reconciler {
    async fn run(mut self) {
        let mut settings = self.deps.settings.subscribe();
        loop {
            let wait = self.reconcile().await;
            tokio::select! {
                _ = tokio::time::sleep(wait) => {}
                _ = self.changed.notified() => {}
                changed = settings.changed() => if changed.is_err() { return },
                _ = self.stop.cancelled() => return,
            }
        }
    }

    /// Make the certificates match the names, ordering any that are due one
    /// at a time. Returns how long until something is next due.
    async fn reconcile(&mut self) -> Duration {
        let settings = self.deps.settings.get();
        let directory = settings.acme_directory.clone();
        let (names, rows) = match refresh(&self.deps, &self.shared, &directory).await {
            Ok(state) => state,
            Err(error) => {
                self.deps.log.error("tls_reconcile_failed", crate::log::err_fields(format!("{error:#}")));
                return Duration::from_secs(FIRST_RETRY_SECS as u64);
            }
        };

        let mut wait = CHECK_INTERVAL;
        for hostname in &names {
            let row = rows.get(hostname);
            let (due, now) = (due_at(row), now());
            if due > now {
                wait = wait.min(Duration::from_secs((due - now) as u64));
                continue;
            }
            // The dashboard is never refused, as `/tls-check` never refuses
            // it: locking admins out because something else spent the budget
            // would be worse than whatever spent it.
            if *hostname != self.deps.dashboard_host {
                if let Err(scope) = self.budget.take(hostname) {
                    self.deps.log.warn("tls_order_throttled", fields! { "hostname" => hostname.as_str(), "scope" => scope });
                    wait = wait.min(THROTTLED_WAIT);
                    continue;
                }
            }

            let failures = row.map_or(0, |row| row.failures);
            let stop = self.stop.clone();
            let issued = tokio::select! {
                issued = self.issue(&directory, &settings.acme_email, hostname, failures) => issued,
                _ = stop.cancelled() => return Duration::ZERO,
            };
            match issued {
                Ok(certificate) => {
                    self.deps.log.info(
                        "tls_certificate_issued",
                        fields! { "hostname" => hostname.as_str(), "expires" => crate::util::http_date(certificate.not_after) },
                    );
                    let key = certificate.key;
                    self.shared.certificates.rcu(|served| {
                        let mut served = HashMap::clone(served);
                        served.insert(hostname.clone(), key.clone());
                        served
                    });
                    wait = wait.min(Duration::from_secs((store::renew_at(certificate.not_before, certificate.not_after) - now).max(0) as u64));
                }
                Err(error) => {
                    let message = format!("{error:#}");
                    let retry_in = backoff(failures + 1);
                    self.deps.log.warn(
                        "tls_certificate_failed",
                        with_err(fields! { "hostname" => hostname.as_str(), "failures" => failures + 1, "retry_in" => retry_in }, &message),
                    );
                    let (dir, name) = (directory.clone(), hostname.clone());
                    let recorded = self.deps.db.call(move |conn| Ok(store::record_failure(conn, &dir, &name, &message, now)?)).await;
                    if let Err(error) = recorded {
                        self.deps.log.error("tls_reconcile_failed", crate::log::err_fields(format!("{error:#}")));
                    }
                    wait = wait.min(Duration::from_secs(retry_in as u64));
                }
            }
        }

        let forgotten = self.deps.db.call(move |conn| Ok(store::forget(conn, &names, now(), now() - LAST_RETRY_SECS)?)).await;
        if let Err(error) = forgotten {
            self.deps.log.error("tls_reconcile_failed", crate::log::err_fields(format!("{error:#}")));
        }
        wait
    }

    /// Get `hostname` a certificate from the CA and keep it.
    async fn issue(&mut self, directory: &str, email: &str, hostname: &str, attempt: i64) -> Result<Certificate> {
        let account = self.account(directory, email).await?;
        let (chain, key) = acme::order(&account, &self.shared, hostname, acme::challenge_for(attempt)).await?;
        let certificate = Certificate::from_pem(&chain, &key).context("The CA issued an unusable certificate")?;
        let (dir, name, now) = (directory.to_string(), hostname.to_string(), now());
        let validity = (certificate.not_before, certificate.not_after);
        self.deps.db.call(move |conn| Ok(store::save(conn, &dir, &name, &chain, &key, validity, now)?)).await?;
        Ok(certificate)
    }

    /// The ACME account for `directory`, made or loaded once and reused,
    /// with `email` as its contact.
    async fn account(&mut self, directory: &str, email: &str) -> Result<Account> {
        if let Some((dir, registered, account)) = &self.account {
            if dir == directory && registered == email {
                return Ok(account.clone());
            }
        }
        let dir = directory.to_string();
        let stored = self.deps.db.call(move |conn| Ok(store::account(conn, &dir)?)).await?;
        let root = self.deps.acme_root.clone();
        let (account, created) = acme::account(|| acme::http_client(root.clone()), directory, email, stored).await?;

        let created = created.map(|credentials| serde_json::to_string(&credentials)).transpose()?;
        let is_new = created.is_some();
        let (dir, mail, now) = (directory.to_string(), email.to_string(), now());
        self.deps
            .db
            .call(move |conn| {
                match created {
                    Some(credentials) => store::save_account(conn, &dir, &credentials, &mail, now)?,
                    None => store::set_account_email(conn, &dir, &mail)?,
                }
                Ok(())
            })
            .await?;
        if is_new {
            self.deps.log.info("acme_account_created", fields! { "directory" => directory });
        }
        self.account = Some((directory.to_string(), email.to_string(), account.clone()));
        Ok(account)
    }
}

#[cfg(test)]
mod tests;
