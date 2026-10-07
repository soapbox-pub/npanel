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

    /// Serve HTTP/1.1 on a TCP connection, which sends a large blob's body
    /// straight from its file.
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

/// A TCP connection for hyper, which sends a large blob with sendfile(2):
/// the kernel moves the file's pages to the socket, and nothing is copied
/// through the process. See `send_file` for how it knows a blob when it sees
/// one.
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
        // hyper passes a body's bytes through uncopied only to a writer that
        // takes them vectored, which is what lets this one recognise a file.
        self.stream.is_write_vectored()
    }

    fn poll_write_vectored(self: Pin<&mut Self>, cx: &mut Context<'_>, bufs: &[io::IoSlice<'_>]) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        #[cfg(target_os = "linux")]
        {
            // A slice below the threshold can only be a file's if it is the
            // tail of one already being sent, which only a corked socket has.
            let file = bufs
                .iter()
                .enumerate()
                .filter(|(_, buf)| this.corked || buf.len() as u64 >= crate::send_file::MIN)
                .find_map(|(at, buf)| crate::send_file::find(buf).map(|(fd, offset)| (at, fd, offset)));
            if let Some((at, fd, offset)) = file {
                return this.poll_send_file(cx, &bufs[..at], fd, offset, bufs[at].len());
            }
        }
        Pin::new(&mut this.stream).poll_write_vectored(cx, bufs)
    }
}

#[cfg(target_os = "linux")]
impl TcpIo {
    /// Write `head`, then `len` bytes of the file `fd` from `offset`, and
    /// report how much of the two went out, as a vectored write would.
    fn poll_send_file(&mut self, cx: &mut Context<'_>, head: &[io::IoSlice<'_>], fd: std::os::fd::RawFd, offset: u64, len: usize) -> Poll<io::Result<usize>> {
        // With `TCP_NODELAY` on, as every connection has it, the kernel sends
        // a file in whatever pieces sendfile hands it — a quarter more
        // segments than a corked socket for a large one, each a trip through
        // the network stack — and a head written before it in a packet of its
        // own. Corked, the head and the file fill whole segments together,
        // which is nginx's `tcp_nopush`.
        if !self.corked {
            set_cork(self.stream.inner(), true)?;
            self.corked = true;
        }

        let head_len: usize = head.iter().map(|buf| buf.len()).sum();
        let written = if head_len == 0 {
            0
        } else {
            let written = std::task::ready!(hyper::rt::Write::poll_write_vectored(Pin::new(&mut self.stream), cx, head))?;
            if written < head_len {
                return Poll::Ready(Ok(written));
            }
            written
        };

        let stream = self.stream.inner();
        let mut offset = libc::off_t::try_from(offset).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
        loop {
            // Given an offset to read from, sendfile leaves the file's own
            // position alone, so one file can be sent on any number of
            // connections at once.
            let sent = stream.try_io(tokio::io::Interest::WRITABLE, || {
                // SAFETY: both descriptors are open for the length of the call
                // (see `send_file::find`), and `offset` is a valid `off_t` the
                // call may write to.
                let n = unsafe { libc::sendfile(std::os::fd::AsRawFd::as_raw_fd(stream), fd, &mut offset, len) };
                usize::try_from(n).map_err(|_| io::Error::last_os_error())
            });
            match sent {
                // The head went out, so that is what this call reports; the
                // next one meets the full socket, or the error, again.
                Err(_) if written > 0 => return Poll::Ready(Ok(written)),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => std::task::ready!(stream.poll_write_ready(cx))?,
                Err(error) => return Poll::Ready(Err(error)),
                Ok(n) => {
                    #[cfg(test)]
                    crate::send_file::SENT.fetch_add(n as u64, std::sync::atomic::Ordering::Relaxed);
                    // That was all of this file there is to write: uncorking
                    // sends its last partial segment now, rather than after
                    // the kernel's 200 ms.
                    if n == len {
                        set_cork(stream, false)?;
                        self.corked = false;
                    }
                    return Poll::Ready(Ok(written + n));
                }
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

    use bytes::Bytes;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;
    use crate::log::LogLevel;

    /// A blob of a megabyte, mapped as `BlobStore::open` maps one.
    fn blob() -> Bytes {
        let contents: Vec<u8> = (0..1024 * 1024u32).map(|i| (i % 251) as u8).collect();
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(&contents).unwrap();
        // SAFETY: nothing else has the temporary file.
        let map = unsafe { memmap2::Mmap::map(&file).unwrap() };
        crate::send_file::bytes(map, file)
    }

    /// `/part` is a range from the middle of the blob, as `Range` asks for one;
    /// anything else is all of it.
    fn respond(blob: &Bytes, request: &Request<Body>) -> Response<Body> {
        let body = match request.uri().path() {
            "/part" => blob.slice(100_000..300_000),
            _ => blob.clone(),
        };
        Response::builder().header(http::header::CONTENT_LENGTH, body.len()).body(Body::from(body)).unwrap()
    }

    /// Two requests on one connection, and the bodies of their responses: the
    /// second is only read right if the first body ended exactly where its head
    /// said it would.
    async fn exchange(stream: &mut (impl AsyncRead + AsyncWrite + Unpin)) -> Vec<Vec<u8>> {
        stream.write_all(b"GET /part HTTP/1.1\r\nhost: x\r\n\r\nGET / HTTP/1.1\r\nhost: x\r\nconnection: close\r\n\r\n").await.unwrap();
        let mut response = Vec::new();
        stream.read_to_end(&mut response).await.unwrap();

        let mut bodies = Vec::new();
        let mut rest = &response[..];
        while !rest.is_empty() {
            let end = rest.windows(4).position(|w| w == b"\r\n\r\n").expect("a head") + 4;
            let head = String::from_utf8_lossy(&rest[..end]).to_lowercase();
            let len: usize = head.split("content-length: ").nth(1).and_then(|v| v.split("\r\n").next()).unwrap().parse().unwrap();
            bodies.push(rest[end..end + len].to_vec());
            rest = &rest[end + len..];
        }
        bodies
    }

    #[cfg(target_os = "linux")]
    #[tokio::test(flavor = "multi_thread")]
    async fn sends_files_over_tcp() {
        let blob = blob();
        let expected = vec![blob[100_000..300_000].to_vec(), blob.to_vec()];
        let listener = bind("127.0.0.1:0".parse().unwrap()).await.unwrap();
        let address = listener.local_addr().unwrap();
        let shutdown = CancellationToken::new();
        let served = blob.clone();
        let server = tokio::spawn(serve(
            listener,
            move |request, _| {
                let response = respond(&served, &request);
                async move { response }
            },
            shutdown.clone(),
            Logger::new(LogLevel::Error),
        ));

        let before = crate::send_file::SENT.load(std::sync::atomic::Ordering::Relaxed);
        assert_eq!(exchange(&mut TcpStream::connect(address).await.unwrap()).await, expected);
        // From the file, not through the process. If this fails after updating
        // hyper, hyper has started copying bodies before writing them.
        let sent = crate::send_file::SENT.load(std::sync::atomic::Ordering::Relaxed) - before;
        assert!(sent >= (200_000 + blob.len()) as u64, "only {sent} bytes went by sendfile");

        shutdown.cancel();
        server.await.unwrap();
    }

    #[tokio::test]
    async fn any_other_connection_sends_the_bytes() {
        let blob = blob();
        let expected = vec![blob[100_000..300_000].to_vec(), blob.to_vec()];
        let (mut client, stream) = tokio::io::duplex(64 * 1024);
        let connections = Connections::new();
        connections.serve(stream, move |request| {
            let response = respond(&blob, &request);
            async move { response }
        });
        assert_eq!(exchange(&mut client).await, expected);
    }
}
