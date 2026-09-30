//! Serving HTTP/1.1 on a listener, with a handler per request.

use std::future::Future;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use http::{Request, Response};
use hyper::body::Incoming;
use hyper_util::rt::{TokioIo, TokioTimer};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

use crate::fields;
use crate::log::Logger;

/// How long a client has to send its request headers. Without this, a
/// connection that never finishes its headers is held open forever.
const HEADER_TIMEOUT: Duration = Duration::from_secs(30);

/// How long open connections get to finish once shutdown begins.
const DRAIN_TIMEOUT: Duration = Duration::from_secs(10);

/// Bind a listener, failing with a sentence that names the address.
pub async fn bind(address: SocketAddr) -> anyhow::Result<TcpListener> {
    TcpListener::bind(address).await.map_err(|error| anyhow::anyhow!("Could not listen on {address}: {error}"))
}

/// Serve `handler` on `listener` until `shutdown` fires, then give open
/// connections a moment to finish.
pub async fn serve<H, F>(listener: TcpListener, handler: H, shutdown: CancellationToken, log: Logger)
where
    H: Fn(Request<Body>, SocketAddr) -> F + Send + Sync + 'static,
    F: Future<Output = Response<Body>> + Send + 'static,
{
    let handler = Arc::new(handler);
    let graceful = hyper_util::server::graceful::GracefulShutdown::new();

    loop {
        let (stream, remote) = tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok(accepted) => accepted,
                Err(error) => {
                    // Out of descriptors, or a connection reset mid-accept:
                    // neither is a reason to stop serving.
                    log.warn("accept_failed", crate::log::err_fields(error));
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    continue;
                }
            },
            _ = shutdown.cancelled() => break,
        };
        let _ = stream.set_nodelay(true);

        let handler = handler.clone();
        let service = hyper::service::service_fn(move |request: Request<Incoming>| {
            let response = handler(request.map(Body::new), remote);
            async move { Ok::<_, std::convert::Infallible>(response.await) }
        });

        let connection = hyper::server::conn::http1::Builder::new()
            .timer(TokioTimer::new())
            .header_read_timeout(HEADER_TIMEOUT)
            .keep_alive(true)
            .serve_connection(TokioIo::new(stream), service);
        let connection = graceful.watch(connection);

        tokio::spawn(async move {
            if let Err(error) = connection.await {
                // Clients hang up mid-response all the time; that is not news.
                let _ = error;
            }
        });
    }

    drop(listener);
    if tokio::time::timeout(DRAIN_TIMEOUT, graceful.shutdown()).await.is_err() {
        log.warn("drain_timed_out", fields! {});
    }
}
