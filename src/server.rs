//! Serving HTTP on a listener, with a handler per request.

use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
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
        connections.serve_tcp(stream, move |request| handler(request, remote));
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

    /// Serve HTTP/1.1 on a TCP connection, which sends a response carrying a
    /// [`hyper::ext::SendFile`] straight from the file.
    fn serve_tcp<H, F>(&self, stream: TcpStream, handler: H)
    where
        H: Fn(Request<Body>) -> F + Send + 'static,
        F: Future<Output = Response<Body>> + Send + 'static,
    {
        tokio::spawn(serve_http1(self.0.watcher(), TcpIo { stream: TokioIo::new(stream), corked: false }, handler));
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
    serve_http1(watcher, TokioIo::new(io), handler).await;
}

async fn serve_http1<I, H, F>(watcher: Watcher, io: I, handler: H)
where
    I: hyper::rt::Read + hyper::rt::Write + Unpin + Send + 'static,
    H: Fn(Request<Body>) -> F + Send + 'static,
    F: Future<Output = Response<Body>> + Send + 'static,
{
    let connection = hyper::server::conn::http1::Builder::new()
        .timer(TokioTimer::new())
        .header_read_timeout(HEADER_TIMEOUT)
        .keep_alive(true)
        .serve_connection(io, service(handler));
    // Clients hang up mid-response all the time; that is not news.
    let _ = watcher.watch(connection).await;
}

/// A TCP connection for hyper, which writes a file with sendfile(2): the
/// kernel moves the file's pages to the socket, and nothing is copied
/// through the process.
struct TcpIo {
    stream: TokioIo<TcpStream>,
    /// Whether `TCP_CORK` is on, which it is while a file is being sent.
    corked: bool,
}

impl hyper::rt::Read for TcpIo {
    fn poll_read(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: hyper::rt::ReadBufCursor<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().stream).poll_read(cx, buf)
    }
}

impl hyper::rt::Write for TcpIo {
    fn poll_write(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.get_mut().stream).poll_write(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().stream).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().stream).poll_shutdown(cx)
    }

    fn is_write_vectored(&self) -> bool {
        self.stream.is_write_vectored()
    }

    fn poll_write_vectored(self: Pin<&mut Self>, cx: &mut Context<'_>, bufs: &[io::IoSlice<'_>]) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.get_mut().stream).poll_write_vectored(cx, bufs)
    }

    fn supports_write_file(&self) -> bool {
        cfg!(target_os = "linux")
    }

    #[cfg(target_os = "linux")]
    fn poll_write_file(self: Pin<&mut Self>, cx: &mut Context<'_>, file: &std::fs::File, offset: u64, len: usize) -> Poll<io::Result<usize>> {
        use std::os::fd::AsRawFd;

        let this = self.get_mut();
        let stream = this.stream.inner();
        let mut offset = libc::off_t::try_from(offset).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
        // With `TCP_NODELAY` on, as every connection has it, the kernel sends
        // a file in whatever pieces sendfile hands it, a quarter more
        // segments than a corked socket sends for a large one, each a trip
        // through the network stack. Corked, it holds them back to fill whole
        // segments, which is nginx's `tcp_nopush`.
        if !this.corked {
            set_cork(stream, true)?;
            this.corked = true;
        }
        loop {
            std::task::ready!(stream.poll_write_ready(cx))?;
            // Given an offset to read from, sendfile leaves the file's own
            // position alone, so one file can be sent on any number of
            // connections at once.
            let sent = stream.try_io(tokio::io::Interest::WRITABLE, || {
                // SAFETY: both descriptors are open for the length of the call,
                // and `offset` is a valid `off_t` the call may write to.
                let n = unsafe { libc::sendfile(stream.as_raw_fd(), file.as_raw_fd(), &mut offset, len) };
                usize::try_from(n).map_err(|_| io::Error::last_os_error())
            });
            match sent {
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => continue,
                // hyper asks for exactly what is left of the body, so this
                // was its end: uncorking sends the last partial segment now
                // rather than after the kernel's 200 ms.
                Ok(n) if n == len => {
                    set_cork(stream, false)?;
                    this.corked = false;
                    return Poll::Ready(Ok(n));
                }
                sent => return Poll::Ready(sent),
            }
        }
    }
}

#[cfg(target_os = "linux")]
fn set_cork(stream: &TcpStream, on: bool) -> io::Result<()> {
    use std::os::fd::AsRawFd;

    let value = libc::c_int::from(on);
    // SAFETY: the descriptor is open, and `value` is the `int` TCP_CORK takes.
    let result = unsafe {
        libc::setsockopt(
            stream.as_raw_fd(),
            libc::IPPROTO_TCP,
            libc::TCP_CORK,
            (&raw const value).cast(),
            std::mem::size_of::<libc::c_int>() as libc::socklen_t,
        )
    };
    if result == 0 { Ok(()) } else { Err(io::Error::last_os_error()) }
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

#[cfg(test)]
mod tests {
    use std::io::Write;

    use hyper::ext::SendFile;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;
    use crate::log::LogLevel;

    /// A response whose file and body differ, so what arrives says which of
    /// the two was sent. They never differ outside this test.
    fn respond(file: &Arc<std::fs::File>, request: &Request<Body>) -> Response<Body> {
        let (offset, len) = match request.uri().path() {
            "/part" => (5, 10),
            _ => (0, 26),
        };
        let mut response = Response::builder().header(http::header::CONTENT_LENGTH, len).body(Body::from(vec![b'.'; len as usize])).unwrap();
        response.extensions_mut().insert(SendFile::new(file.clone(), offset, len));
        response
    }

    fn alphabet() -> Arc<std::fs::File> {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(b"abcdefghijklmnopqrstuvwxyz").unwrap();
        Arc::new(file)
    }

    async fn exchange(stream: &mut (impl AsyncRead + AsyncWrite + Unpin)) -> String {
        // Two on one connection: the second is only read right if the first
        // body ended exactly where its head said.
        stream.write_all(b"GET /part HTTP/1.1\r\nhost: x\r\n\r\nGET / HTTP/1.1\r\nhost: x\r\nconnection: close\r\n\r\n").await.unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).await.unwrap();
        response
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn a_tcp_connection_sends_the_file() {
        let file = alphabet();
        let listener = bind("127.0.0.1:0".parse().unwrap()).await.unwrap();
        let address = listener.local_addr().unwrap();
        let shutdown = CancellationToken::new();
        let server = tokio::spawn(serve(
            listener,
            move |request, _| {
                let response = respond(&file, &request);
                async move { response }
            },
            shutdown.clone(),
            Logger::new(LogLevel::Error),
        ));

        let response = exchange(&mut TcpStream::connect(address).await.unwrap()).await;
        let bodies: Vec<&str> = response.split("\r\n\r\n").skip(1).map(|rest| rest.split("HTTP/1.1").next().unwrap()).collect();
        assert_eq!(bodies, ["fghijklmno", "abcdefghijklmnopqrstuvwxyz"], "{response}");

        shutdown.cancel();
        server.await.unwrap();
    }

    #[tokio::test]
    async fn any_other_connection_sends_the_body() {
        let file = alphabet();
        let (mut client, stream) = tokio::io::duplex(1024);
        let connections = Connections::new();
        connections.serve(stream, move |request| {
            let response = respond(&file, &request);
            async move { response }
        });

        let response = exchange(&mut client).await;
        let bodies: Vec<&str> = response.split("\r\n\r\n").skip(1).map(|rest| rest.split("HTTP/1.1").next().unwrap()).collect();
        assert_eq!(bodies, [".".repeat(10), ".".repeat(26)], "{response}");
    }
}
