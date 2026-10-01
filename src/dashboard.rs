//! Serving the built dashboard SPA.
//!
//! Unknown paths fall back to `index.html` so client-side routes work on a
//! hard refresh. Hashed Vite assets are immutable; `index.html` never is.
//!
//! A release binary carries its dashboard (the `embedded-dashboard` feature,
//! see build.rs), so the two can't come from different versions. Without the
//! feature it is read from disk, which is what lets a developer rebuild it
//! while npanel runs.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::SystemTime;

use axum::body::Body;
use bytes::Bytes;
use http::{header, Method, Response, StatusCode};
use parking_lot::Mutex;

use crate::fields;
use crate::log::Logger;
use crate::mime::content_type;

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
    /// File contents by path, reused until the file on disk changes. HTML is
    /// held already rewritten.
    cache: Mutex<HashMap<PathBuf, (SystemTime, u64, Bytes)>>,
}

/// What a request found.
enum Found {
    File(DashboardFile, Bytes),
    NotBuilt,
    Unreadable,
}

impl Dashboard {
    pub fn new(source: Source, dashboard_host: &str, log: Logger) -> Dashboard {
        let inner = Inner { source, origin: origin_for(dashboard_host), cache: Mutex::new(HashMap::new()) };
        Dashboard { inner: Arc::new(inner), log, warned: AtomicBool::new(false) }
    }

    pub async fn serve(&self, request: &http::request::Parts) -> Response<Body> {
        // Statting and reading files is blocking work, so it is done off the
        // runtime threads even when the answer turns out to be cached.
        let (inner, path) = (self.inner.clone(), crate::util::decode_path(request.uri.path()));
        let found = tokio::task::spawn_blocking(move || inner.load(&path)).await.unwrap_or(Found::Unreadable);

        let (file, body) = match found {
            Found::File(file, body) => (file, body),
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

        let builder = Response::builder()
            .header(header::CONTENT_TYPE, content_type(&file.path.to_string_lossy()))
            .header(header::CONTENT_LENGTH, body.len())
            .header(header::CACHE_CONTROL, if file.immutable { "public, max-age=31536000, immutable" } else { "no-cache" });

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
            Ok(body) => Found::File(file, body),
            Err(_) => Found::Unreadable,
        }
    }

    /// A file's bytes, with HTML rewritten, cached until the file changes.
    fn contents(&self, file: &DashboardFile) -> std::io::Result<Bytes> {
        if let (Some(bytes), false) = (file.embedded, file.html) {
            return Ok(Bytes::from_static(bytes));
        }
        if let Some((modified, size, bytes)) = self.cache.lock().get(&file.path) {
            if *modified == file.modified && *size == file.size {
                return Ok(bytes.clone());
            }
        }

        let raw = match file.embedded {
            Some(bytes) => bytes.to_vec(),
            None => std::fs::read(&file.path)?,
        };
        let bytes = if file.html { Bytes::from(rewrite_preview_urls(&String::from_utf8_lossy(&raw), &self.origin)) } else { Bytes::from(raw) };

        self.cache.lock().insert(file.path.clone(), (file.modified, file.size, bytes.clone()));
        Ok(bytes)
    }

    /// The file at a normalized relative path, if the dashboard has one.
    fn find(&self, relative: &Path) -> Option<DashboardFile> {
        let html = relative.extension().is_some_and(|ext| ext == "html");
        let immutable = relative.starts_with("assets");
        match &self.source {
            Source::Disk { candidates, root } => {
                let root = match root.get() {
                    Some(root) => root,
                    None => {
                        let found = candidates.iter().find(|dir| dir.join("index.html").is_file())?;
                        root.get_or_init(|| found.clone())
                    }
                };
                let candidate = root.join(relative);
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

    #[cfg(feature = "embedded-dashboard")]
    #[tokio::test]
    async fn serves_the_embedded_dashboard() {
        let dashboard = Dashboard::new(Source::Embedded, "panel.example.com", Logger::new(crate::log::LogLevel::Error));
        let get = |path: &str| Request::get(path).body(()).unwrap().into_parts().0;

        let response = dashboard.serve(&get("/some/route")).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()["content-type"], "text/html; charset=utf-8");

        let (asset, _) = embedded::FILES.iter().find(|(path, _)| path.starts_with("assets/")).expect("a built asset");
        let response = dashboard.serve(&get(&format!("/{asset}"))).await;
        assert_eq!(response.headers()["cache-control"], "public, max-age=31536000, immutable");
    }
}
