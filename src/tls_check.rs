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
use crate::db::Db;
use crate::fields;
use crate::log::Logger;
use crate::settings::Settings;
use crate::tls::names;
use crate::tls::Budget;

/// The only path this listener answers.
const TLS_CHECK_PATH: &str = "/tls-check";

pub struct TlsCheck {
    db: Db,
    log: Logger,
    /// Never refused a certificate, whatever the budget looks like.
    dashboard_host: String,
    budget: Budget,
}

impl TlsCheck {
    pub fn new(db: Db, settings: Arc<Settings>, log: Logger, dashboard_host: String) -> TlsCheck {
        TlsCheck { db, log, dashboard_host, budget: Budget::new(settings) }
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

        if let Err(scope) = self.budget.take(domain) {
            self.log.warn("tls_check_throttled", fields! { "domain" => domain, "scope" => scope });
            return Response::builder()
                .status(StatusCode::TOO_MANY_REQUESTS)
                .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
                .header(header::RETRY_AFTER, "60")
                .body(Body::from("rate limited"))
                .expect("response");
        }

        reply(StatusCode::OK, "ok")
    }

    /// Whether npanel answers for `domain` at all, by the same rule its own
    /// ACME client follows.
    async fn served(&self, domain: &str) -> anyhow::Result<bool> {
        let domain = domain.to_string();
        self.db.call(move |conn| Ok(names::qualifies(conn, &domain)?)).await
    }
}

fn reply(status: StatusCode, body: &'static str) -> Response<Body> {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .body(Body::from(body))
        .expect("response")
}
