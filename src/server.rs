//! Serving HTTP on a listener, with a handler per request.

use std::future::Future;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use http::{Request, Response};
use hyper::body::Incoming;
use hyper_util::rt::{TokioExecutor, TokioIo, TokioTimer};
use hyper_util::server::graceful::{GracefulShutdown, Watcher};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::sync::CancellationToken;

use crate::fields;
use crate::log::Logger;

/// How long a client has to send its request headers. Without this, a
/// connection that never finishes its headers is held open forever.
const HEADER_TIMEOUT: Duration = Duration::from_secs(30);

/// How often an HTTP/2 connection is pinged, and how long the answer may
/// take before the connection is given up for dead.
const H2_KEEP_ALIVE: Duration = Duration::from_secs(30);

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
    let connections = Connections::new();

    while let Some((stream, remote)) = accept(&listener, &shutdown, log).await {
        let handler = handler.clone();
        connections.serve(stream, move |request| handler(request, remote));
    }

    drop(listener);
    connections.drain(log).await;
}

/// The next connection on `listener`, or `None` once `shutdown` fires.
pub async fn accept(listener: &TcpListener, shutdown: &CancellationToken, log: Logger) -> Option<(TcpStream, SocketAddr)> {
    loop {
        tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok((stream, remote)) => {
                    let _ = stream.set_nodelay(true);
                    return Some((stream, remote));
                }
                Err(error) => {
                    // Out of descriptors, or a connection reset mid-accept:
                    // neither is a reason to stop serving.
                    log.warn("accept_failed", crate::log::err_fields(error));
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            },
            _ = shutdown.cancelled() => return None,
        }
    }
}

/// Open HTTP connections, wherever they came from: a TCP listener, a TLS
/// handshake, or a Tor stream that never touches a socket.
pub struct Connections(GracefulShutdown);

impl Connections {
    pub fn new() -> Connections {
        Connections(GracefulShutdown::new())
    }

    /// Serve HTTP/1.1 on `io`, in a task of its own.
    pub fn serve<I, H, F>(&self, io: I, handler: H)
    where
        I: AsyncRead + AsyncWrite + Unpin + Send + 'static,
        H: Fn(Request<Body>) -> F + Send + 'static,
        F: Future<Output = Response<Body>> + Send + 'static,
    {
        tokio::spawn(http1(self.0.watcher(), io, handler));
    }

    /// A claim on a connection that is still being set up — a TLS handshake —
    /// so that draining waits for it too.
    pub fn watcher(&self) -> Watcher {
        self.0.watcher()
    }

    /// Ask every connection to finish, and give them a moment to.
    pub async fn drain(self, log: Logger) {
        if tokio::time::timeout(DRAIN_TIMEOUT, self.0.shutdown()).await.is_err() {
            log.warn("drain_timed_out", fields! {});
        }
    }
}

/// Serve HTTP/1.1 on `io` until it closes or `watcher` is told to finish.
pub async fn http1<I, H, F>(watcher: Watcher, io: I, handler: H)
where
    I: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    H: Fn(Request<Body>) -> F + Send + 'static,
    F: Future<Output = Response<Body>> + Send + 'static,
{
    let connection = hyper::server::conn::http1::Builder::new()
        .timer(TokioTimer::new())
        .header_read_timeout(HEADER_TIMEOUT)
        .keep_alive(true)
        .serve_connection(TokioIo::new(io), service(handler));
    // Clients hang up mid-response all the time; that is not news.
    let _ = watcher.watch(connection).await;
}

/// Serve HTTP/2 on `io` until it closes or `watcher` is told to finish.
pub async fn http2<I, H, F>(watcher: Watcher, io: I, handler: H)
where
    I: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    H: Fn(Request<Body>) -> F + Send + 'static,
    F: Future<Output = Response<Body>> + Send + 'static,
{
    let connection = hyper::server::conn::http2::Builder::new(TokioExecutor::new())
        .timer(TokioTimer::new())
        .keep_alive_interval(H2_KEEP_ALIVE)
        .keep_alive_timeout(H2_KEEP_ALIVE)
        .serve_connection(TokioIo::new(io), service(handler));
    let _ = watcher.watch(connection).await;
}

fn service<H, F>(
    handler: H,
) -> impl hyper::service::Service<Request<Incoming>, Response = Response<Body>, Error = std::convert::Infallible, Future: Send + 'static>
where
    H: Fn(Request<Body>) -> F + Send + 'static,
    F: Future<Output = Response<Body>> + Send + 'static,
{
    hyper::service::service_fn(move |request: Request<Incoming>| {
        let response = handler(request.map(Body::new));
        async move { Ok::<_, std::convert::Infallible>(response.await) }
    })
}
