//! The whole public HTTP surface, routed by `Host`: the dashboard hostname
//! serves the API and the dashboard SPA, every other hostname serves an nsite.
//!
//! Nothing here is exempt from that rule. Certificate authorization for the
//! proxy needs to answer requests that carry no hostname to route on, so it
//! listens on its own port instead — see `tls_check.rs`.

use std::sync::Arc;

use axum::body::Body;
use http::{header, HeaderMap, HeaderValue, Request, Response};
use tower::ServiceExt;

use crate::dashboard::Dashboard;
use crate::hostname::request_hostname;
use crate::serve::{RequestOrigin, SiteServer};

pub struct App {
    pub dashboard_host: String,
    pub site: Arc<SiteServer>,
    pub dashboard: Dashboard,
    pub api: axum::Router,
}

impl App {
    pub async fn handle(&self, request: Request<Body>) -> Response<Body> {
        let hostname = request_hostname(request.headers().get(header::HOST).and_then(|v| v.to_str().ok()));

        if hostname != self.dashboard_host {
            let scheme = request_scheme(request.headers());
            return self.site.serve(request, RequestOrigin { hostname, scheme }).await;
        }

        let path = request.uri().path();
        let mut response = if path == "/api" || path.starts_with("/api/") {
            match self.api.clone().oneshot(request).await {
                Ok(response) => response,
                Err(never) => match never {},
            }
        } else {
            self.dashboard.serve(&request.into_parts().0).await
        };

        // The dashboard and API are never meant to be framed, and the SPA's
        // own CSP is a <meta> tag, which cannot express frame-ancestors.
        let headers = response.headers_mut();
        headers.insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static("frame-ancestors 'none'"));
        headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
        headers.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
        headers.insert(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
        response
    }
}

/// The scheme the client used to reach the proxy.
///
/// npanel is always behind a proxy that terminates TLS, so the connection this
/// process sees is plain HTTP even when the visitor's was not. `X-Forwarded-
/// Proto` is the proxy's word for it, and `https` is the assumption when the
/// proxy didn't say: the wrong guess there costs a redirect, not a downgrade.
///
/// This is believed where `X-Forwarded-Host` is not, because the worst a
/// forged scheme achieves is a redirect to the one the visitor already used.
/// A request over Tor came through no proxy, so the Tor listener states its
/// scheme instead.
pub fn request_scheme(headers: &HeaderMap) -> &'static str {
    let forwarded = headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(',').next())
        .map(|v| v.trim().to_lowercase());
    match forwarded.as_deref() {
        Some("http") => "http",
        _ => "https",
    }
}
