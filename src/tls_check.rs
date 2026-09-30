//! Certificate authorization for Caddy's `on_demand_tls ask`, on a port of its
//! own.
//!
//! This cannot live on the public server. Caddy asks with a plain GET to the
//! address it proxies to, so the request carries no hostname npanel could
//! route on and the endpoint would have to answer on every hostname it serves.
//! Anyone could then drive approvals for a configured host until the hourly
//! budget was spent, denying certificates to the sites that deserve them. A
//! second listener, reachable only by the proxy, is the whole fix.

use std::sync::Arc;

use axum::body::Body;
use http::{header, Method, Request, Response, StatusCode};
use parking_lot::Mutex;
use rusqlite::OptionalExtension;

use crate::db::Db;
use crate::fields;
use crate::hostname::www_apex;
use crate::log::Logger;
use crate::rate_limit::{RateLimit, RateLimiter};
use crate::settings::Settings;

/// The only path this listener answers.
const TLS_CHECK_PATH: &str = "/tls-check";

/// The window both approval limits are expressed over, in ms.
const HOUR_MS: f64 = 60.0 * 60.0 * 1000.0;

pub struct TlsCheck {
    db: Db,
    settings: Arc<Settings>,
    log: Logger,
    /// Never refused a certificate, whatever the budget looks like.
    dashboard_host: String,
    per_host: Mutex<RateLimiter>,
    total: Mutex<RateLimiter>,
}

impl TlsCheck {
    pub fn new(db: Db, settings: Arc<Settings>, log: Logger, dashboard_host: String) -> TlsCheck {
        TlsCheck {
            db,
            settings,
            log,
            dashboard_host,
            per_host: Mutex::new(RateLimiter::new(4096)),
            total: Mutex::new(RateLimiter::new(1)),
        }
    }

    pub async fn handle(&self, request: Request<Body>) -> Response<Body> {
        if request.method() != Method::GET && request.method() != Method::HEAD {
            return Response::builder()
                .status(StatusCode::METHOD_NOT_ALLOWED)
                .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
                .header(header::ALLOW, "GET, HEAD")
                .body(Body::from("method not allowed"))
                .expect("response");
        }

        if request.uri().path() != TLS_CHECK_PATH {
            return reply(StatusCode::NOT_FOUND, "not found");
        }

        let domain = request
            .uri()
            .query()
            .and_then(|q| url::form_urlencoded::parse(q.as_bytes()).find(|(k, _)| k == "domain").map(|(_, v)| v.into_owned()))
            .unwrap_or_default()
            .trim()
            .to_lowercase();

        self.authorize(&domain).await
    }

    /// Whether `domain` may have a certificate. Reveals only whether a
    /// hostname is configured here, which DNS already tells you.
    async fn authorize(&self, domain: &str) -> Response<Body> {
        // Exempt: locking admins out of the dashboard because something else
        // exhausted the budget would be worse than whatever caused it.
        if domain == self.dashboard_host {
            return reply(StatusCode::OK, "ok");
        }

        match self.served(domain).await {
            Ok(true) => {}
            Ok(false) => {
                self.log.debug("tls_check_denied", fields! { "domain" => domain });
                return reply(StatusCode::NOT_FOUND, "unknown host");
            }
            Err(error) => {
                // Caddy treats anything but a 2xx as a refusal, so a failure
                // costs a certificate. It has to be visible in the log.
                self.log.error("tls_check_failed", crate::log::err_fields(error));
                return reply(StatusCode::INTERNAL_SERVER_ERROR, "internal error");
            }
        }

        let settings = self.settings.get();
        let within_host = self
            .per_host
            .lock()
            .take(domain, RateLimit { capacity: settings.tls_check_limit_per_host as f64, interval_ms: HOUR_MS });
        // Short-circuit deliberately: a host already over its own limit must
        // not go on draining the budget the other hosts share.
        let within_total = within_host
            && self.total.lock().take("", RateLimit { capacity: settings.tls_check_limit_total as f64, interval_ms: HOUR_MS });

        if !within_total {
            self.log.warn("tls_check_throttled", fields! { "domain" => domain, "scope" => if within_host { "total" } else { "host" } });
            return Response::builder()
                .status(StatusCode::TOO_MANY_REQUESTS)
                .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
                .header(header::RETRY_AFTER, "60")
                .body(Body::from("rate limited"))
                .expect("response");
        }

        reply(StatusCode::OK, "ok")
    }

    /// Whether npanel answers for `domain` at all. A `www` name npanel only
    /// redirects still needs a certificate: the redirect is an HTTPS response.
    async fn served(&self, domain: &str) -> anyhow::Result<bool> {
        let domain = domain.to_string();
        self.db
            .call(move |conn| {
                let configured: Option<i64> = conn
                    .prepare_cached("SELECT id FROM hosts WHERE hostname = ? AND enabled = 1")?
                    .query_row([&domain], |r| r.get(0))
                    .optional()?;
                if configured.is_some() {
                    return Ok(true);
                }
                let Some(apex) = www_apex(&domain) else {
                    return Ok(false);
                };
                let redirects: Option<i64> = conn
                    .prepare_cached("SELECT id FROM hosts WHERE hostname = ? AND enabled = 1 AND redirect_www = 1")?
                    .query_row([apex], |r| r.get(0))
                    .optional()?;
                Ok(redirects.is_some())
            })
            .await
    }
}

fn reply(status: StatusCode, body: &'static str) -> Response<Body> {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .body(Body::from(body))
        .expect("response")
}
