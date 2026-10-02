//! Serving the built dashboard SPA.
//!
//! Unknown paths fall back to `index.html` so client-side routes work on a
//! hard refresh. Hashed Vite assets are immutable; `index.html` never is.
//!
//! A release binary carries its dashboard (the `embedded-dashboard` feature,
//! see build.rs), so the two can't come from different versions. Without the
//! feature it is read from disk, which is what lets a developer rebuild it
//! while npanel runs.
//!
//! Text files go out compressed, with the same brotli and gzip settings as a
//! site's blobs, so the dashboard is small without a proxy's help. The copies
//! are made in memory when npanel starts, rather than at build time: the
//! built `dist` is also published as an nsite, which shouldn't carry them,
//! and `index.html` is rewritten at runtime anyway.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::SystemTime;

use axum::body::Body;
use bytes::Bytes;
use http::{header, Method, Response, StatusCode};
use parking_lot::Mutex;

use crate::blobs::{worth_compressing, worth_keeping, Encoding, Variants};
use crate::fields;
use crate::hash::BlobHash;
use crate::log::Logger;
use crate::mime::{content_type, is_compressible};
use crate::serve::{etag_for, matches_etag, negotiate_encoding};

#[cfg(feature = "embedded-dashboard")]
mod embedded {
    include!(concat!(env!("OUT_DIR"), "/dashboard_files.rs"));
}

/// A file resolved inside the dashboard.
struct DashboardFile {
    path: PathBuf,
    size: u64,
    modified: SystemTime,
    /// Fingerprinted by Vite, so it can be cached forever.
    immutable: bool,
    html: bool,
    /// The bytes, when they were compiled in rather than left on disk.
    embedded: Option<&'static [u8]>,
}

/// Where the dashboard's files come from.
pub enum Source {
    /// A directory on disk: the first of these with an `index.html`, best
    /// first. Looked for until found, so building the dashboard while npanel
    /// runs needs no restart. Only tests build one when the dashboard is
    /// embedded.
    #[cfg_attr(feature = "embedded-dashboard", allow(dead_code))]
    Disk { candidates: Vec<PathBuf>, root: OnceLock<PathBuf> },
    /// Compiled into the binary.
    #[cfg(feature = "embedded-dashboard")]
    Embedded,
}

impl Source {
    #[cfg_attr(feature = "embedded-dashboard", allow(dead_code))]
    pub fn disk(candidates: Vec<PathBuf>) -> Source {
        Source::Disk { candidates, root: OnceLock::new() }
    }
}

pub struct Dashboard {
    inner: Arc<Inner>,
    log: Logger,
    warned: AtomicBool,
}

struct Inner {
    source: Source,
    origin: String,
    /// Files by path, reused until the file on disk changes.
    cache: Mutex<HashMap<PathBuf, Arc<Loaded>>>,
}

/// A file as it is served: HTML already rewritten, and compressed copies
/// once they have been made.
struct Loaded {
    modified: SystemTime,
    size: u64,
    identity: Bytes,
    hash: BlobHash,
    /// Whether the response depends on `Accept-Encoding`.
    compressible: bool,
    /// Made off the request path, since brotli at its best takes seconds
    /// over a large bundle. Until it's done the file goes out as it is.
    compressed: OnceLock<Compressed>,
    /// Whether a request has already set the compressing going.
    queued: AtomicBool,
}

#[derive(Default)]
struct Compressed {
    br: Option<Bytes>,
    gzip: Option<Bytes>,
}

impl Compressed {
    fn get(&self, encoding: Encoding) -> Option<&Bytes> {
        match encoding {
            Encoding::Br => self.br.as_ref(),
            Encoding::Gzip => self.gzip.as_ref(),
        }
    }
}

impl Loaded {
    fn new(file: &DashboardFile, identity: Bytes) -> Loaded {
        let compressible = is_compressible(&file.path.to_string_lossy()) && worth_compressing(identity.len() as u64);
        let compressed = OnceLock::new();
        if !compressible {
            let _ = compressed.set(Compressed::default());
        }
        Loaded {
            modified: file.modified,
            size: file.size,
            hash: BlobHash::of(&identity),
            identity,
            compressible,
            compressed,
            queued: AtomicBool::new(false),
        }
    }

    /// The compressed copies, made now if they haven't been. CPU-bound: call
    /// it on the blocking pool.
    fn compress(&self) -> &Compressed {
        self.compressed.get_or_init(|| {
            let size = self.identity.len() as u64;
            let mut compressed = Compressed::default();
            for encoding in Encoding::ALL {
                let mut bytes = Vec::new();
                // Reading from a slice and writing to a Vec can't fail.
                if encoding.compress(&mut &self.identity[..], &mut bytes, size).is_ok() && worth_keeping(bytes.len() as u64, size) {
                    let bytes = Some(Bytes::from(bytes));
                    match encoding {
                        Encoding::Br => compressed.br = bytes,
                        Encoding::Gzip => compressed.gzip = bytes,
                    }
                }
            }
            compressed
        })
    }
}

/// What a request found.
enum Found {
    File(DashboardFile, Arc<Loaded>),
    NotBuilt,
    Unreadable,
}

impl Dashboard {
    pub fn new(source: Source, dashboard_host: &str, log: Logger) -> Dashboard {
        let inner = Inner { source, origin: origin_for(dashboard_host), cache: Mutex::new(HashMap::new()) };
        Dashboard { inner: Arc::new(inner), log, warned: AtomicBool::new(false) }
    }

    /// Load and compress every file the dashboard has, so that the visitors
    /// who arrive first after a start get compressed copies too. A dashboard
    /// that hasn't been built yet is left for requests to find.
    pub fn warm(&self) -> tokio::task::JoinHandle<()> {
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || {
            for relative in inner.list() {
                if let Some(loaded) = inner.find(&relative).and_then(|file| inner.contents(&file).ok()) {
                    loaded.compress();
                }
            }
        })
    }

    pub async fn serve(&self, request: &http::request::Parts) -> Response<Body> {
        // Statting and reading files is blocking work, so it is done off the
        // runtime threads even when the answer turns out to be cached.
        let (inner, path) = (self.inner.clone(), crate::util::decode_path(request.uri.path()));
        let found = tokio::task::spawn_blocking(move || inner.load(&path)).await.unwrap_or(Found::Unreadable);

        let (file, loaded) = match found {
            Found::File(file, loaded) => (file, loaded),
            Found::Unreadable => return Response::builder().status(StatusCode::NOT_FOUND).body(Body::empty()).expect("response"),
            Found::NotBuilt => {
                if !self.warned.swap(true, Ordering::Relaxed) {
                    // An embedded dashboard always has its index.html: build.rs
                    // refuses to embed one without.
                    let dirs = match &self.inner.source {
                        Source::Disk { candidates, .. } => candidates.iter().map(|d| d.display().to_string()).collect::<Vec<_>>().join(" "),
                        #[cfg(feature = "embedded-dashboard")]
                        Source::Embedded => String::new(),
                    };
                    self.log.warn("dashboard_not_built", fields! { "dirs" => dirs });
                }
                return Response::builder()
                    .status(StatusCode::SERVICE_UNAVAILABLE)
                    .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
                    .body(Body::from("The dashboard has not been built. Run `bun run build` in dashboard/."))
                    .expect("response");
            }
        };

        let compressed = loaded.compressed.get();
        if compressed.is_none() && !loaded.queued.swap(true, Ordering::Relaxed) {
            // A file that changed on disk since `warm`, or one asked for
            // while it was still running.
            let loaded = loaded.clone();
            tokio::task::spawn_blocking(move || {
                loaded.compress();
            });
        }

        let variants = Variants {
            identity: loaded.identity.len() as u64,
            br: compressed.and_then(|c| c.br.as_ref()).map(|b| b.len() as u64),
            gzip: compressed.and_then(|c| c.gzip.as_ref()).map(|b| b.len() as u64),
        };
        let accept_encoding = request.headers.get(header::ACCEPT_ENCODING).and_then(|v| v.to_str().ok());
        let encoding = negotiate_encoding(accept_encoding, &variants);
        let body = encoding.and_then(|e| compressed?.get(e)).unwrap_or(&loaded.identity).clone();
        let etag = etag_for(loaded.hash, encoding);

        let mut builder = Response::builder()
            .header(header::CONTENT_TYPE, content_type(&file.path.to_string_lossy()))
            .header(header::CACHE_CONTROL, if file.immutable { "public, max-age=31536000, immutable" } else { "no-cache" })
            .header(header::ETAG, &etag);
        if loaded.compressible {
            builder = builder.header(header::VARY, "Accept-Encoding");
        }

        let if_none_match = request.headers.get(header::IF_NONE_MATCH).and_then(|v| v.to_str().ok());
        if matches_etag(if_none_match, &etag) {
            return builder.status(StatusCode::NOT_MODIFIED).body(Body::empty()).expect("response");
        }

        if let Some(encoding) = encoding {
            builder = builder.header(header::CONTENT_ENCODING, encoding.as_str());
        }
        builder = builder.header(header::CONTENT_LENGTH, body.len());
        if request.method == Method::HEAD {
            return builder.body(Body::empty()).expect("response");
        }
        builder.body(Body::from(body)).expect("response")
    }
}

impl Inner {
    /// The file a URL path names, falling back to `index.html` for the SPA's
    /// own routes.
    fn load(&self, path: &str) -> Found {
        let Some(file) = self.find(&normalize(path)).or_else(|| self.find(Path::new("index.html"))) else { return Found::NotBuilt };
        match self.contents(&file) {
            Ok(loaded) => Found::File(file, loaded),
            Err(_) => Found::Unreadable,
        }
    }

    /// A file's bytes, with HTML rewritten, cached until the file changes.
    fn contents(&self, file: &DashboardFile) -> std::io::Result<Arc<Loaded>> {
        if let Some(loaded) = self.cache.lock().get(&file.path) {
            if loaded.modified == file.modified && loaded.size == file.size {
                return Ok(loaded.clone());
            }
        }

        let raw = match file.embedded {
            Some(bytes) => Bytes::from_static(bytes),
            None => Bytes::from(std::fs::read(&file.path)?),
        };
        let identity = if file.html { Bytes::from(rewrite_preview_urls(&String::from_utf8_lossy(&raw), &self.origin)) } else { raw };

        let loaded = Arc::new(Loaded::new(file, identity));
        self.cache.lock().insert(file.path.clone(), loaded.clone());
        Ok(loaded)
    }

    /// Every file the dashboard has, as paths relative to its root.
    fn list(&self) -> Vec<PathBuf> {
        match &self.source {
            Source::Disk { .. } => {
                let mut files = Vec::new();
                if let Some(root) = self.disk_root() {
                    walk(root, Path::new(""), &mut files);
                }
                files
            }
            #[cfg(feature = "embedded-dashboard")]
            Source::Embedded => embedded::FILES.iter().map(|(path, _)| PathBuf::from(path)).collect(),
        }
    }

    /// The directory a disk dashboard is served from, once one has been built.
    fn disk_root(&self) -> Option<&PathBuf> {
        let (candidates, root) = match &self.source {
            Source::Disk { candidates, root } => (candidates, root),
            #[cfg(feature = "embedded-dashboard")]
            Source::Embedded => return None,
        };
        if let Some(root) = root.get() {
            return Some(root);
        }
        let found = candidates.iter().find(|dir| dir.join("index.html").is_file())?;
        Some(root.get_or_init(|| found.clone()))
    }

    /// The file at a normalized relative path, if the dashboard has one.
    fn find(&self, relative: &Path) -> Option<DashboardFile> {
        let html = relative.extension().is_some_and(|ext| ext == "html");
        let immutable = relative.starts_with("assets");
        match &self.source {
            Source::Disk { .. } => {
                let candidate = self.disk_root()?.join(relative);
                let metadata = std::fs::metadata(&candidate).ok().filter(|m| m.is_file())?;
                Some(DashboardFile {
                    html,
                    immutable,
                    size: metadata.len(),
                    modified: metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                    path: candidate,
                    embedded: None,
                })
            }
            #[cfg(feature = "embedded-dashboard")]
            Source::Embedded => {
                let name = relative.to_str()?;
                let (_, bytes) = embedded::FILES.iter().find(|(path, _)| *path == name)?;
                Some(DashboardFile {
                    html,
                    immutable,
                    size: bytes.len() as u64,
                    modified: SystemTime::UNIX_EPOCH,
                    path: relative.to_path_buf(),
                    embedded: Some(bytes),
                })
            }
        }
    }
}

/// Every file under `dir`, as paths relative to the root it started from.
fn walk(dir: &Path, relative: &Path, files: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let (path, relative) = (entry.path(), relative.join(entry.file_name()));
        match entry.file_type() {
            Ok(kind) if kind.is_dir() => walk(&path, &relative, files),
            Ok(kind) if kind.is_file() => files.push(relative),
            _ => {}
        }
    }
}

/// A URL path as a path relative to the dashboard's root. `..` can't escape
/// it: the path is normalized before anything joins it.
fn normalize(path: &str) -> PathBuf {
    let mut relative = PathBuf::new();
    for component in Path::new(path).components() {
        match component {
            Component::Normal(part) => relative.push(part),
            Component::ParentDir => {
                relative.pop();
            }
            _ => {}
        }
    }
    relative
}

/// Rewrite link-preview meta tags whose content has to be an absolute URL.
/// The built `index.html` can't know its own origin, and crawlers are
/// inconsistent about resolving relative `og:image` values.
fn rewrite_preview_urls(html: &str, origin: &str) -> String {
    static PREVIEW: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let preview = PREVIEW.get_or_init(|| {
        regex::Regex::new(r#"(<meta\s+(?:property|name)="(?:og:image|og:url|twitter:image)"\s+content=")(/[^"]*)(")"#)
            .expect("regex")
    });
    preview.replace_all(html, |caps: &regex::Captures| format!("{}{origin}{}{}", &caps[1], &caps[2], &caps[3])).into_owned()
}

/// The dashboard's public origin. Any real hostname is assumed to be reached
/// over https, since npanel expects a TLS-terminating proxy.
fn origin_for(hostname: &str) -> String {
    let loopback = hostname == "localhost"
        || hostname.ends_with(".localhost")
        || hostname.trim_matches(['[', ']']) == "::1"
        || (hostname.starts_with("127.") && hostname.parse::<std::net::Ipv4Addr>().is_ok());
    format!("{}://{hostname}", if loopback { "http" } else { "https" })
}

/// Where this build's dashboard comes from: compiled in, or else
/// `dashboard/dist` under the working directory, beside the binary, or in the
/// source tree it was built from.
pub fn source() -> Source {
    #[cfg(feature = "embedded-dashboard")]
    return Source::Embedded;

    #[cfg(not(feature = "embedded-dashboard"))]
    {
        let mut candidates = vec![std::path::absolute("dashboard/dist").unwrap_or_else(|_| PathBuf::from("dashboard/dist"))];
        if let Some(dir) = std::env::current_exe().ok().and_then(|exe| exe.parent().map(Path::to_path_buf)) {
            candidates.push(dir.join("dashboard/dist"));
        }
        candidates.push(Path::new(env!("CARGO_MANIFEST_DIR")).join("dashboard/dist"));
        Source::disk(candidates)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http::Request;

    #[test]
    fn rewrites_preview_urls() {
        let html = r#"<meta property="og:image" content="/og.png"><meta name="twitter:image" content="https://x/y.png">"#;
        assert_eq!(
            rewrite_preview_urls(html, "https://panel.example.com"),
            r#"<meta property="og:image" content="https://panel.example.com/og.png"><meta name="twitter:image" content="https://x/y.png">"#
        );
        assert_eq!(origin_for("localhost"), "http://localhost");
        assert_eq!(origin_for("panel.example.com"), "https://panel.example.com");
    }

    #[tokio::test]
    async fn serves_files_and_falls_back() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("assets")).unwrap();
        std::fs::write(dir.path().join("index.html"), "<p>app</p>").unwrap();
        std::fs::write(dir.path().join("assets/app-1.js"), "js").unwrap();
        let dashboard = Dashboard::new(Source::disk(vec![dir.path().join("missing"), dir.path().to_path_buf()]), "panel.example.com", Logger::new(crate::log::LogLevel::Error));

        let get = |path: &str| Request::get(path).body(()).unwrap().into_parts().0;
        let response = dashboard.serve(&get("/assets/app-1.js")).await;
        assert_eq!(response.headers()["cache-control"], "public, max-age=31536000, immutable");
        let response = dashboard.serve(&get("/some/route")).await;
        assert_eq!(response.headers()["content-type"], "text/html; charset=utf-8");
        assert_eq!(response.headers()["cache-control"], "no-cache");
        let response = dashboard.serve(&get("/../../etc/passwd")).await;
        assert_eq!(response.headers()["content-type"], "text/html; charset=utf-8");
    }

    #[tokio::test]
    async fn compresses_and_revalidates() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("assets")).unwrap();
        let script = "console.log('hello, world');\n".repeat(200);
        std::fs::write(dir.path().join("index.html"), "<p>app</p>").unwrap();
        std::fs::write(dir.path().join("assets/app-1.js"), &script).unwrap();
        std::fs::write(dir.path().join("assets/logo.png"), "png ".repeat(200)).unwrap();
        let dashboard = Dashboard::new(Source::disk(vec![dir.path().to_path_buf()]), "panel.example.com", Logger::new(crate::log::LogLevel::Error));
        dashboard.warm().await.unwrap();

        let request = |method: Method, path: &str, headers: &[(&str, &str)]| {
            let mut builder = Request::builder().method(method).uri(path);
            for (name, value) in headers {
                builder = builder.header(*name, *value);
            }
            builder.body(()).unwrap().into_parts().0
        };
        let body = |response: Response<Body>| async { http_body_util::BodyExt::collect(response.into_body()).await.unwrap().to_bytes() };

        let response = dashboard.serve(&request(Method::GET, "/assets/app-1.js", &[("accept-encoding", "gzip, br")])).await;
        assert_eq!(response.headers()["content-encoding"], "br");
        assert_eq!(response.headers()["vary"], "Accept-Encoding");
        let etag = response.headers()["etag"].to_str().unwrap().to_string();
        assert!(etag.ends_with("-br\""));
        let length: usize = response.headers()["content-length"].to_str().unwrap().parse().unwrap();
        let compressed = body(response).await;
        assert_eq!(compressed.len(), length);
        let mut decompressed = Vec::new();
        brotli::BrotliDecompress(&mut &compressed[..], &mut decompressed).unwrap();
        assert_eq!(decompressed, script.as_bytes());

        let response = dashboard.serve(&request(Method::GET, "/assets/app-1.js", &[("accept-encoding", "gzip")])).await;
        assert_eq!(response.headers()["content-encoding"], "gzip");
        let response = dashboard.serve(&request(Method::HEAD, "/assets/app-1.js", &[])).await;
        assert!(!response.headers().contains_key("content-encoding"));
        assert_eq!(response.headers()["content-length"], script.len().to_string().as_str());
        assert!(body(response).await.is_empty());

        // A tag only matches the representation it was given for.
        let response = dashboard.serve(&request(Method::GET, "/assets/app-1.js", &[("accept-encoding", "br"), ("if-none-match", &etag)])).await;
        assert_eq!(response.status(), StatusCode::NOT_MODIFIED);
        assert_eq!(response.headers()["etag"], etag.as_str());
        assert!(body(response).await.is_empty());
        let response = dashboard.serve(&request(Method::GET, "/assets/app-1.js", &[("accept-encoding", "gzip"), ("if-none-match", &etag)])).await;
        assert_eq!(response.status(), StatusCode::OK);

        // index.html is revalidated on every load, so it needs the tag most.
        let response = dashboard.serve(&request(Method::GET, "/hosts", &[])).await;
        let etag = response.headers()["etag"].to_str().unwrap().to_string();
        let response = dashboard.serve(&request(Method::GET, "/hosts", &[("if-none-match", &etag)])).await;
        assert_eq!(response.status(), StatusCode::NOT_MODIFIED);

        let response = dashboard.serve(&request(Method::GET, "/assets/logo.png", &[("accept-encoding", "gzip, br")])).await;
        assert!(!response.headers().contains_key("content-encoding"));
        assert!(!response.headers().contains_key("vary"));
    }

    #[cfg(feature = "embedded-dashboard")]
    #[tokio::test]
    async fn serves_the_embedded_dashboard() {
        let dashboard = Dashboard::new(Source::Embedded, "panel.example.com", Logger::new(crate::log::LogLevel::Error));
        let get = |path: &str| Request::get(path).body(()).unwrap().into_parts().0;

        let response = dashboard.serve(&get("/some/route")).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()["content-type"], "text/html; charset=utf-8");

        let (asset, _) = embedded::FILES.iter().filter(|(path, _)| path.ends_with(".js")).max_by_key(|(_, bytes)| bytes.len()).expect("a built script");
        let response = dashboard.serve(&get(&format!("/{asset}"))).await;
        assert_eq!(response.headers()["cache-control"], "public, max-age=31536000, immutable");
        assert_eq!(response.headers()["vary"], "Accept-Encoding");

        // What `warm` goes through.
        assert!(dashboard.inner.list().iter().any(|path| path == Path::new("index.html")));
    }
}
