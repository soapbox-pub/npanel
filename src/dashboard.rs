//! Serving the built dashboard SPA.
//!
//! Unknown paths fall back to `index.html` so client-side routes work on a
//! hard refresh. Hashed Vite assets are immutable; `index.html` never is.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::SystemTime;

use axum::body::Body;
use bytes::Bytes;
use http::{header, Method, Response, StatusCode};
use parking_lot::Mutex;

use crate::fields;
use crate::log::Logger;
use crate::mime::content_type;

/// A file resolved inside the dashboard directory.
struct DashboardFile {
    path: PathBuf,
    size: u64,
    modified: SystemTime,
    /// Fingerprinted by Vite, so it can be cached forever.
    immutable: bool,
    html: bool,
}

pub struct Dashboard {
    /// Where a built dashboard may be, best first.
    candidates: Vec<PathBuf>,
    /// The one found, once a request has found it.
    root: parking_lot::RwLock<Option<PathBuf>>,
    origin: String,
    log: Logger,
    /// File contents by path, reused until the file on disk changes. HTML is
    /// held already rewritten.
    cache: Mutex<HashMap<PathBuf, (SystemTime, u64, Bytes)>>,
    warned: AtomicBool,
}

impl Dashboard {
    pub fn new(candidates: Vec<PathBuf>, dashboard_host: &str, log: Logger) -> Dashboard {
        Dashboard {
            candidates,
            root: parking_lot::RwLock::new(None),
            origin: origin_for(dashboard_host),
            log,
            cache: Mutex::new(HashMap::new()),
            warned: AtomicBool::new(false),
        }
    }

    /// The built dashboard's directory. Looked for until found, so building
    /// the dashboard while npanel runs needs no restart.
    fn root(&self) -> Option<PathBuf> {
        if let Some(root) = self.root.read().as_ref() {
            return Some(root.clone());
        }
        let found = self.candidates.iter().find(|dir| dir.join("index.html").is_file())?.clone();
        *self.root.write() = Some(found.clone());
        Some(found)
    }

    pub async fn serve(&self, request: &http::request::Parts) -> Response<Body> {
        let path = crate::util::decode_path(request.uri.path());
        let file = self.root().and_then(|root| self.find(&root, &path).or_else(|| self.find(&root, "/index.html")));

        let Some(file) = file else {
            if !self.warned.swap(true, Ordering::Relaxed) {
                let dirs = self.candidates.iter().map(|d| d.display().to_string()).collect::<Vec<_>>().join(" ");
                self.log.warn("dashboard_not_built", fields! { "dirs" => dirs });
            }
            return Response::builder()
                .status(StatusCode::SERVICE_UNAVAILABLE)
                .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
                .body(Body::from("The dashboard has not been built. Run `bun run build` in dashboard/."))
                .expect("response");
        };

        let body = match self.contents(&file) {
            Ok(body) => body,
            Err(_) => return Response::builder().status(StatusCode::NOT_FOUND).body(Body::empty()).expect("response"),
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

    /// A file's bytes, with HTML rewritten, cached until the file changes.
    fn contents(&self, file: &DashboardFile) -> std::io::Result<Bytes> {
        if let Some((modified, size, bytes)) = self.cache.lock().get(&file.path) {
            if *modified == file.modified && *size == file.size {
                return Ok(bytes.clone());
            }
        }

        let raw = std::fs::read(&file.path)?;
        let bytes = if file.html { Bytes::from(rewrite_preview_urls(&String::from_utf8_lossy(&raw), &self.origin)) } else { Bytes::from(raw) };

        self.cache.lock().insert(file.path.clone(), (file.modified, file.size, bytes.clone()));
        Ok(bytes)
    }

    /// Resolve a URL path to a file inside the dashboard directory. `..` can't
    /// escape it: the path is normalized before it is joined.
    fn find(&self, root: &Path, path: &str) -> Option<DashboardFile> {
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

        let candidate = root.join(&relative);
        let metadata = std::fs::metadata(&candidate).ok().filter(|m| m.is_file())?;
        Some(DashboardFile {
            html: candidate.extension().is_some_and(|ext| ext == "html"),
            immutable: relative.starts_with("assets"),
            size: metadata.len(),
            modified: metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH),
            path: candidate,
        })
    }
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

/// Where a built dashboard may be: `dashboard/dist` under the working
/// directory, beside the binary, or in the source tree it was built from.
pub fn candidates() -> Vec<PathBuf> {
    let mut candidates = vec![std::path::absolute("dashboard/dist").unwrap_or_else(|_| PathBuf::from("dashboard/dist"))];
    if let Some(dir) = std::env::current_exe().ok().and_then(|exe| exe.parent().map(Path::to_path_buf)) {
        candidates.push(dir.join("dashboard/dist"));
    }
    candidates.push(Path::new(env!("CARGO_MANIFEST_DIR")).join("dashboard/dist"));
    candidates
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
        let dashboard = Dashboard::new(vec![dir.path().join("missing"), dir.path().to_path_buf()], "panel.example.com", Logger::new(crate::log::LogLevel::Error));

        let get = |path: &str| Request::get(path).body(()).unwrap().into_parts().0;
        let response = dashboard.serve(&get("/assets/app-1.js")).await;
        assert_eq!(response.headers()["cache-control"], "public, max-age=31536000, immutable");
        let response = dashboard.serve(&get("/some/route")).await;
        assert_eq!(response.headers()["content-type"], "text/html; charset=utf-8");
        assert_eq!(response.headers()["cache-control"], "no-cache");
        let response = dashboard.serve(&get("/../../etc/passwd")).await;
        assert_eq!(response.headers()["content-type"], "text/html; charset=utf-8");
    }
}
