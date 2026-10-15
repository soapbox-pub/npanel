//! The whole public HTTP surface, routed by `Host`: the dashboard hostname
//! serves the API and the dashboard SPA, a tunnel's hostname is passed to its
//! agent, and every other hostname serves an nsite.
//!
//! Nothing here is exempt from that rule. Certificate authorization for the
//! proxy needs to answer requests that carry no hostname to route on, so it
//! listens on its own port instead — see `tls_check.rs`.

use std::net::IpAddr;
use std::sync::Arc;

use axum::body::Body;
use http::{header, HeaderMap, HeaderValue, Request, Response};
use tower::ServiceExt;

use crate::analytics::Peer;
use crate::dashboard::Dashboard;
use crate::hostname::request_hostname;
use crate::serve::{RequestOrigin, SiteServer};
use crate::tunnel::gateway::Tunnels;

pub struct App {
    pub dashboard_host: String,
    pub site: Arc<SiteServer>,
    pub tunnels: Arc<Tunnels>,
    pub dashboard: Dashboard,
    pub api: axum::Router,
}

/// Who an API request came from, as far as npanel can tell: the address that
/// connected, or behind a proxy, the first one its `X-Forwarded-For` names.
/// Shown to admins about agents, and believed for nothing.
#[derive(Clone, Copy, Debug)]
pub struct ClientAddress(pub Option<IpAddr>);

/// How a request reached npanel, which decides which of its headers may be
/// believed.
#[derive(Clone, Copy, Debug)]
pub enum Via {
    /// Through the reverse proxy, from the address given: its
    /// `X-Forwarded-*` headers are its word about the visitor.
    Proxy(IpAddr),
    /// Over TLS npanel terminated itself, from the visitor at the address
    /// given. There is no proxy, so those headers are the visitor's own.
    Tls(IpAddr),
}

impl App {
    pub async fn handle(&self, mut request: Request<Body>, via: Via) -> Response<Body> {
        let hostname = request_host(&request);

        if hostname != self.dashboard_host {
            let (scheme, peer) = match via {
                Via::Proxy(proxy) => (request_scheme(request.headers()), Peer::Proxy(proxy)),
                Via::Tls(visitor) => ("https", Peer::Visitor(visitor)),
            };
            let origin = RequestOrigin { hostname, scheme };
            if let Some(tunnel) = self.tunnels.route(&origin.hostname) {
                let visitor = match via {
                    Via::Proxy(_) => None,
                    Via::Tls(visitor) => Some(visitor),
                };
                return self.tunnels.proxy(tunnel, request, &origin, visitor).await;
            }
            return self.site.serve(request, origin, peer).await;
        }

        let client = match via {
            Via::Proxy(_) => request
                .headers()
                .get("x-forwarded-for")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.split(',').next())
                .and_then(|v| v.trim().parse().ok()),
            Via::Tls(visitor) => Some(visitor),
        };
        request.extensions_mut().insert(ClientAddress(client));

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

/// The hostname a request names. HTTP/1.1 says it in `Host`; HTTP/2 says it
/// in `:authority`, which hyper hands over as the URI's authority and which
/// browsers send instead of `Host`, not beside it.
pub fn request_host<B>(request: &Request<B>) -> String {
    match request.headers().get(header::HOST) {
        Some(host) => request_hostname(host.to_str().ok()),
        None => request_hostname(request.uri().host()),
    }
}

/// The scheme the client used to reach the proxy.
///
/// Behind a proxy that terminates TLS, the connection this process sees is
/// plain HTTP even when the visitor's was not. `X-Forwarded-Proto` is the
/// proxy's word for it, and `https` is the assumption when the proxy didn't
/// say: the wrong guess there costs a redirect, not a downgrade.
///
/// This is believed where `X-Forwarded-Host` is not, because the worst a
/// forged scheme achieves is a redirect to the one the visitor already used.
/// A request over Tor, or over TLS npanel terminated, came through no proxy,
/// so those listeners state the scheme instead.
pub fn request_scheme(headers: &HeaderMap) -> &'static str {
    let forwarded = headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(',').next())
        .map(str::trim);
    match forwarded {
        Some(scheme) if scheme.eq_ignore_ascii_case("http") => "http",
        _ => "https",
    }
}
