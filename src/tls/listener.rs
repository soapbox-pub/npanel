//! Serving HTTPS: HTTP/2 or HTTP/1.1, as the client's ALPN asks.
//!
//! Each connection's handshake runs in a task of its own, under a timeout,
//! so a client that never finishes one holds up nobody else.

use std::future::Future;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use http::{Request, Response};
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;
use tokio_util::sync::CancellationToken;

use super::{Shared, ACME_TLS_ALPN};
use crate::log::Logger;
use crate::server::{accept, http1, http2, Connections};

/// How long a client has to finish its TLS handshake.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

pub fn server_config(shared: Arc<Shared>) -> Arc<rustls::ServerConfig> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut config = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .expect("ring supports the default protocol versions")
        .with_no_client_auth()
        .with_cert_resolver(shared);
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec(), ACME_TLS_ALPN.to_vec()];
    // Resumed sessions skip a round trip and the certificate on a visitor's
    // next connection. If tickets can't be made, connections go on without.
    if let Ok(ticketer) = rustls::crypto::ring::Ticketer::new() {
        config.ticketer = ticketer;
    }
    Arc::new(config)
}

/// Serve `handler` over TLS on `listener` until `shutdown` fires, then give
/// open connections a moment to finish.
pub async fn serve<H, F>(listener: TcpListener, config: Arc<rustls::ServerConfig>, handler: H, shutdown: CancellationToken, log: Logger)
where
    H: Fn(Request<Body>, SocketAddr) -> F + Send + Sync + 'static,
    F: Future<Output = Response<Body>> + Send + 'static,
{
    let acceptor = TlsAcceptor::from(config);
    let handler = Arc::new(handler);
    let connections = Connections::new();

    while let Some((stream, remote)) = accept(&listener, &shutdown, log).await {
        let (acceptor, handler, watcher) = (acceptor.clone(), handler.clone(), connections.watcher());
        tokio::spawn(async move {
            // A failed handshake is a scanner, a client that hung up, or a
            // name with no certificate yet; none of it is worth a log line.
            let Ok(Ok(stream)) = tokio::time::timeout(HANDSHAKE_TIMEOUT, acceptor.accept(stream)).await else {
                return;
            };
            let handler = move |request| handler(request, remote);
            match stream.get_ref().1.alpn_protocol() {
                // A CA validating a challenge only needed the handshake.
                Some(ACME_TLS_ALPN) => {}
                Some(b"h2") => http2(watcher, stream, handler).await,
                _ => http1(watcher, stream, handler).await,
            }
        });
    }

    drop(listener);
    connections.drain(log).await;
}
