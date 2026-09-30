//! Serving an nsite request.
//!
//! Everything is read from the local blob store — npanel never proxies a
//! Blossom server at request time — so a served site keeps working even if
//! every relay and Blossom server it came from is offline.
//!
//! A warm request touches neither the database nor the filesystem: which host
//! a hostname names and which build it serves are cached until a writer says
//! otherwise, which file a path resolves to within a build is cached forever
//! (a build is immutable), and the blob itself is a kept memory mapping.

use std::collections::HashMap;
use std::net::IpAddr;
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use bytes::Bytes;
use http::header::{self, HeaderMap, HeaderValue};
use http::request::Parts;
use http::{Method, Request, Response, StatusCode};
use parking_lot::{Mutex, RwLock};

use crate::analytics::tracker::EVENT_PATH;
use crate::analytics::Analytics;
use crate::blobs::{BlobStore, Encoding, Variants};
use crate::builds::current_build;
use crate::db::{BuildKind, Db};
use crate::fields;
use crate::hostname::{is_onion_address, www_apex};
use crate::identifiers::{parse_relay_list, NIP05_PATH};
use crate::log::{with_err, Logger};
use crate::mime::content_type;
use crate::nsite_address::encode_naddr;
use crate::seo::canonical_link;
use crate::site_csp::{SiteCsp, GATEWAY_CSP};
use crate::sites::{find_host, resolve_site_path, ResolvedFile, ServedHost};
use crate::util::{extname, http_date};

/// Where a request came from, as settled by the listener that accepted it.
///
/// Neither field can be trusted to the request itself: behind the proxy the
/// scheme comes from `X-Forwarded-Proto`, while a request over Tor has no
/// proxy in front of it and no header worth reading. Deciding both once, at
/// the boundary, keeps the question of which headers may be believed out of
/// everything below.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RequestOrigin {
    /// The hostname the client asked for, lowercased and without a port.
    pub hostname: String,
    /// The scheme the client used — `http` or `https`.
    pub scheme: &'static str,
}

/// Documents revalidate every time, which the `ETag` makes cheap; assets are
/// only ever fetched by a document that revalidated first, so they get an hour.
const CACHE_CONTROL: &str = "public, no-cache";
const ASSET_CACHE_CONTROL: &str = "public, max-age=3600, must-revalidate";

/// How long a browser may reuse a preflight result, in seconds.
const PREFLIGHT_MAX_AGE: &str = "86400";

/// How long a cached host lookup may be believed. A backstop: every writer in
/// the process clears the cache the moment serving's answer changes. This is
/// for writers the process can't see, like `prune-blobs`.
const HOST_TTL: Duration = Duration::from_secs(60);

/// Resolved paths to keep. Overflowing it costs a query, never correctness.
const MAX_CACHED_PATHS: usize = 5000;

/// The build a hostname serves, with the response fields derived from it.
#[derive(Clone, Debug)]
pub struct CachedVersion {
    /// Immutable, so safe to key caches on.
    pub build_id: i64,
    /// The build's `Last-Modified`, formatted once instead of per response.
    pub last_modified: String,
}

/// What serving needs to know about a hostname, resolved in one place.
#[derive(Clone, Debug)]
pub struct CachedSite {
    pub host: ServedHost,
    /// What the host serves under this name: `None` while the host is
    /// disabled or nothing has finished building for it.
    pub version: Option<CachedVersion>,
    /// The host's `X-Nsite-Addr`.
    pub naddr: HeaderValue,
}

/// The hostname → build lookups behind every request, cached.
///
/// Only hostnames that resolve to a host are kept, so a scanner spraying
/// random `Host` headers can't grow the map.
pub struct ServeCache {
    db: Db,
    entries: RwLock<HashMap<String, (Arc<CachedSite>, Instant)>>,
}

impl ServeCache {
    pub fn new(db: Db) -> Self {
        Self { db, entries: RwLock::new(HashMap::new()) }
    }

    /// The host this hostname names and the build it serves, if either exists.
    pub async fn lookup(&self, hostname: &str) -> anyhow::Result<Option<Arc<CachedSite>>> {
        if let Some((site, expires)) = self.entries.read().get(hostname) {
            if *expires > Instant::now() {
                return Ok(Some(site.clone()));
            }
        }

        let name = hostname.to_string();
        let found = self
            .db
            .call(move |conn| {
                let Some(host) = find_host(conn, &name)? else {
                    return Ok(None);
                };
                // An onion has a build of its own, because the files npanel
                // generates for it name the onion rather than the hostname.
                let kind = if is_onion_address(&name) { BuildKind::Onion } else { BuildKind::Clearnet };
                let build = if host.host.enabled == 1 { current_build(conn, host.host.id, kind)? } else { None };
                Ok(Some((host, build)))
            })
            .await?;

        let Some((host, build)) = found else {
            // An expired entry whose host is gone would otherwise sit here forever.
            self.entries.write().remove(hostname);
            return Ok(None);
        };

        let version = build.map(|build| CachedVersion {
            build_id: build.id,
            last_modified: http_date(build.last_modified),
        });
        let naddr = HeaderValue::from_str(&encode_naddr(&host.host.address())).unwrap_or(HeaderValue::from_static(""));
        let site = Arc::new(CachedSite { host, version, naddr });

        self.entries.write().insert(hostname.to_string(), (site.clone(), Instant::now() + HOST_TTL));
        Ok(Some(site))
    }

    /// Drop everything. Host writes are rare enough that no writer needs to
    /// work out which hostnames its change touched.
    pub fn clear(&self) {
        self.entries.write().clear();
    }
}

pub struct SiteServerDeps {
    pub db: Db,
    pub blobs: BlobStore,
    pub site_csp: Arc<SiteCsp>,
    pub serve_cache: Arc<ServeCache>,
    pub analytics: Arc<Analytics>,
    pub log: Logger,
}

pub struct SiteServer {
    deps: SiteServerDeps,
    /// Which file a request path resolves to within a build. Nothing
    /// invalidates it and nothing has to: a build's files are written once and
    /// a build id is never reused. A row still missing its blob sizes is left
    /// uncached, since `compress-blobs` may yet record them.
    paths: Mutex<lru::LruCache<(i64, bool, String), Option<ResolvedFile>>>,
}

type Reply = Response<Body>;

impl SiteServer {
    pub fn new(deps: SiteServerDeps) -> SiteServer {
        SiteServer { deps, paths: Mutex::new(lru::LruCache::new(NonZeroUsize::new(MAX_CACHED_PATHS).unwrap())) }
    }

    /// Serve one request for a site, as it reached the listener `origin`
    /// describes. `peer` is the connection's address where it may stand in
    /// for the visitor's; only analytics reads it.
    pub async fn serve(&self, request: Request<Body>, origin: RequestOrigin, peer: Option<IpAddr>) -> Reply {
        // The one path that isn't the site's, on any site: npanel's own event
        // endpoint, which the tracker in every counting site's build reports
        // to. Ahead of everything, preflights included.
        if request.uri().path() == EVENT_PATH {
            return self.deps.analytics.forward_event(request, peer).await;
        }

        if request.method() == Method::OPTIONS && request.headers().contains_key(header::ACCESS_CONTROL_REQUEST_METHOD) {
            return preflight(request.headers());
        }

        let (parts, _) = request.into_parts();
        let mut response = self.handle(&parts, &origin).await;
        allow_cross_origin(response.headers_mut());
        response
    }

    async fn handle(&self, request: &Parts, origin: &RequestOrigin) -> Reply {
        match self.try_handle(request, origin).await {
            Ok(response) => response,
            // A database that couldn't answer says nothing about the site, so
            // the visitor is told to come back rather than that it's missing —
            // and nothing about the failure is cached.
            Err(error) => {
                self.deps.log.error("site_unavailable", with_err(fields! { "hostname" => origin.hostname.as_str(), "path" => request.uri.path() }, &error));
                let mut response = text(StatusCode::SERVICE_UNAVAILABLE, "This site is temporarily unavailable. Try again in a moment.");
                response.headers_mut().insert(header::RETRY_AFTER, HeaderValue::from_static("5"));
                response
            }
        }
    }

    async fn try_handle(&self, request: &Parts, origin: &RequestOrigin) -> anyhow::Result<Reply> {
        // The build comes along because the policy below is the build's and
        // applies to every answer this host gives.
        let site = self.deps.serve_cache.lookup(&origin.hostname).await?;
        let mut response = self.respond(request, origin, site.as_deref()).await?;
        let headers = response.headers_mut();

        // On unless this one site turned it off. A hostname with no host row
        // has nobody to have turned it off, so it gets the gateway's.
        if site.as_ref().is_none_or(|site| site.host.host.csp != 0) {
            let declared = match site.as_ref().and_then(|site| site.version.as_ref()) {
                Some(version) => self.deps.site_csp.policy(version.build_id).await?,
                None => None,
            };
            let policy = declared.as_deref().unwrap_or(GATEWAY_CSP);
            if let Ok(value) = HeaderValue::from_str(policy) {
                headers.insert(header::CONTENT_SECURITY_POLICY, value);
            }
        }

        // Set on everything rather than only on what varied, because a cache
        // has to be told before it stores the first response.
        headers.insert(header::VARY, HeaderValue::from_static("accept-encoding"));

        if let Some(site) = &site {
            advertise_onion(request, origin, &site.host, &mut response);
        }

        Ok(response)
    }

    async fn respond(&self, request: &Parts, origin: &RequestOrigin, site: Option<&CachedSite>) -> anyhow::Result<Reply> {
        let log = &self.deps.log;
        let hostname = origin.hostname.as_str();

        if request.method != Method::GET && request.method != Method::HEAD {
            // An nsite is a pile of files. There is nothing to POST to.
            return Ok(Response::builder()
                .status(StatusCode::METHOD_NOT_ALLOWED)
                .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
                .header(header::ALLOW, "GET, HEAD")
                .body(Body::from("This site only serves GET and HEAD requests."))
                .expect("response"));
        }

        if site.is_none() {
            if let Some(redirect) = self.www_redirect(request, origin).await? {
                return Ok(redirect);
            }
        }

        let Some(site) = site.filter(|site| site.host.host.enabled == 1) else {
            log.debug("site_unknown_host", fields! { "hostname" => hostname });
            return Ok(text(StatusCode::NOT_FOUND, "This domain is not configured."));
        };

        let path = request.uri.path();

        // Ahead of the deploy check, because these names are npanel's and not
        // the site's: one added here resolves straight away.
        if path == NIP05_PATH {
            if let Some(names) = self.nip05(request, &site.host).await? {
                return Ok(names);
            }
        }

        let Some(version) = &site.version else {
            log.debug("site_not_deployed", fields! { "hostname" => hostname });
            return Ok(Response::builder()
                .status(StatusCode::SERVICE_UNAVAILABLE)
                .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
                .header(header::RETRY_AFTER, "30")
                .body(Body::from("This site has not finished deploying yet."))
                .expect("response"));
        };

        let Some(file) = self.resolve(version.build_id, path, site.host.host.spa == 1).await? else {
            log.debug("site_path_missing", fields! { "hostname" => hostname, "path" => path });
            return Ok(text(StatusCode::NOT_FOUND, "Not found."));
        };

        // Sizes come with the path lookup when a deploy recorded them; the
        // stat is only for rows that predate that.
        let variants = match file.variants {
            Some(variants) => Some(variants),
            None => {
                let (blobs, sha) = (self.deps.blobs.clone(), file.sha256.clone());
                tokio::task::spawn_blocking(move || blobs.variants(&sha)).await.ok().flatten()
            }
        };
        let Some(variants) = variants else {
            // The build is ready, so this means the blob was removed underneath us.
            log.error("blob_missing", fields! { "hostname" => hostname, "path" => file.path.as_str(), "sha256" => file.sha256.as_str() });
            return Ok(text(StatusCode::INTERNAL_SERVER_ERROR, "This file is missing from local storage."));
        };

        // Everything below is about the representation this picks, not the
        // blob: its size, its `ETag`, and the bytes a `Range` counts from.
        let encoding = negotiate_encoding(header_str(&request.headers, header::ACCEPT_ENCODING), &variants);
        let size = variants.size(encoding);
        let etag = etag_for(&file.sha256, encoding);

        let mut builder = Response::builder()
            .header(header::CONTENT_TYPE, content_type(&file.path))
            .header(header::CACHE_CONTROL, cache_control(&file.path))
            .header(header::ETAG, &etag)
            .header(header::LAST_MODIFIED, &version.last_modified)
            .header(header::ACCEPT_RANGES, "bytes")
            // Which nsite this hostname is serving, in the one form a client
            // can paste anywhere.
            .header("x-nsite-addr", site.naddr.clone());

        if let Some(encoding) = encoding {
            builder = builder.header(header::CONTENT_ENCODING, encoding.as_str());
        }

        // Names the one spelling of this page, so `/foo`, `/foo/` and
        // `/foo/index.html` score as one page instead of three.
        if site.host.host.seo != 0 {
            if let Some(canonical) = canonical_link(origin, path, &file.path, file.status) {
                builder = builder.header(header::LINK, canonical);
            }
        }

        if matches_etag(header_str(&request.headers, header::IF_NONE_MATCH), &etag) {
            return Ok(builder.status(StatusCode::NOT_MODIFIED).body(Body::empty()).expect("response"));
        }

        let range = match parse_range(header_str(&request.headers, header::RANGE), size) {
            Range::Unsatisfiable => {
                return Ok(builder
                    .status(StatusCode::RANGE_NOT_SATISFIABLE)
                    .header(header::CONTENT_RANGE, format!("bytes */{size}"))
                    .body(Body::empty())
                    .expect("response"));
            }
            Range::Whole => None,
            Range::Part(start, end) => Some((start, end)),
        };

        let status = if file.status == 404 {
            StatusCode::NOT_FOUND
        } else if range.is_some() {
            StatusCode::PARTIAL_CONTENT
        } else {
            StatusCode::OK
        };

        builder = match range {
            Some((start, end)) => builder
                .header(header::CONTENT_RANGE, format!("bytes {start}-{end}/{size}"))
                .header(header::CONTENT_LENGTH, end - start + 1),
            None => builder.header(header::CONTENT_LENGTH, size),
        };

        if request.method == Method::HEAD {
            return Ok(builder.status(status).body(Body::empty()).expect("response"));
        }

        let bytes = match self.open(&file.sha256, encoding).await {
            Ok(bytes) => bytes,
            Err(error) => {
                log.error(
                    "blob_missing",
                    fields! { "hostname" => hostname, "path" => file.path.as_str(), "sha256" => file.sha256.as_str(), "err_msg" => error.to_string() },
                );
                return Ok(text(StatusCode::INTERNAL_SERVER_ERROR, "This file is missing from local storage."));
            }
        };
        let body = match range {
            Some((start, end)) => bytes.slice(start as usize..(end as usize + 1).min(bytes.len())),
            None => bytes,
        };

        if log.enabled(crate::log::LogLevel::Debug) {
            log.debug(
                "site_request",
                fields! {
                    "hostname" => hostname,
                    "path" => path,
                    "status" => status.as_u16(),
                    "sha256" => file.sha256.as_str(),
                    "bytes" => size,
                    "encoding" => encoding.map(Encoding::as_str),
                },
            );
        }

        Ok(builder.status(status).body(Body::from(body)).expect("response"))
    }

    /// A blob's bytes for a response body. Kept mappings answer without a
    /// system call; a new one is opened off the runtime threads.
    async fn open(&self, sha256: &str, encoding: Option<Encoding>) -> std::io::Result<Bytes> {
        let blobs = self.deps.blobs.clone();
        let sha = sha256.to_string();
        tokio::task::spawn_blocking(move || blobs.open(&sha, encoding))
            .await
            .map_err(std::io::Error::other)?
            .map_err(|e| std::io::Error::other(e.to_string()))
    }

    async fn resolve(&self, build_id: i64, path: &str, spa: bool) -> anyhow::Result<Option<ResolvedFile>> {
        // The SPA flag is part of the key because it is read per request rather
        // than built in: switching it must not have to wait for a rebuild.
        let key = (build_id, spa, path.to_string());
        if let Some(cached) = self.paths.lock().get(&key) {
            return Ok(cached.clone());
        }

        let request_path = path.to_string();
        let file = self
            .deps
            .db
            .call(move |conn| Ok(resolve_site_path(conn, build_id, &request_path, spa)?))
            .await?;

        // A path the build has no file for is cached too — the file list that
        // settled that is as immutable as the one that resolves a hit.
        if file.as_ref().is_none_or(|file| file.variants.is_some()) {
            self.paths.lock().put(key, file.clone());
        }
        Ok(file)
    }

    /// The redirect `www.example.com` gets when `example.com` asked for one.
    /// Only reached when no host of that name is configured, so a `www` host
    /// someone added deliberately is served rather than redirected away.
    async fn www_redirect(&self, request: &Parts, origin: &RequestOrigin) -> anyhow::Result<Option<Reply>> {
        let Some(apex) = www_apex(&origin.hostname) else { return Ok(None) };
        let Some(site) = self.deps.serve_cache.lookup(apex).await? else { return Ok(None) };
        if site.host.host.enabled != 1 || site.host.host.redirect_www != 1 {
            return Ok(None);
        }

        let path_and_query = request.uri.path_and_query().map(|pq| pq.as_str()).unwrap_or("/");
        let location = format!("{}://{apex}{path_and_query}", origin.scheme);

        self.deps.log.debug("site_www_redirect", fields! { "hostname" => origin.hostname.as_str(), "location" => location.as_str() });

        Ok(Some(
            Response::builder()
                .status(StatusCode::MOVED_PERMANENTLY)
                .header(header::LOCATION, location)
                // A 301 with no freshness of its own is cached until the browser
                // restarts, which would outlive an admin switching it back off.
                .header(header::CACHE_CONTROL, "public, max-age=3600")
                .body(Body::empty())
                .expect("response"),
        ))
    }

    /// A host's NIP-05 names, as the document at `/.well-known/nostr.json`.
    ///
    /// `None` when the host has none, which is what lets a site that published
    /// its own `nostr.json` go on serving it. A request naming one gets that
    /// one; a request naming nothing gets the lot. A name nobody here answers
    /// for is an empty document rather than a 404: the question was answered.
    async fn nip05(&self, request: &Parts, host: &ServedHost) -> anyhow::Result<Option<Reply>> {
        let host_id = host.host.id;
        let entries: Vec<(String, String, String)> = self
            .deps
            .db
            .call(move |conn| {
                Ok(conn
                    .prepare_cached("SELECT name, pubkey, relays FROM nip05_names WHERE host_id = ? ORDER BY name")?
                    .query_map([host_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                    .collect::<rusqlite::Result<_>>()?)
            })
            .await?;
        if entries.is_empty() {
            return Ok(None);
        }

        let requested = request
            .uri
            .query()
            .and_then(|q| url::form_urlencoded::parse(q.as_bytes()).find(|(k, _)| k == "name").map(|(_, v)| v.into_owned()));
        let wanted: Vec<(String, String, Vec<String>)> = entries
            .into_iter()
            .filter(|(name, ..)| requested.as_ref().is_none_or(|want| *name == want.trim().to_lowercase()))
            .map(|(name, pubkey, relays)| (name, pubkey, parse_relay_list(&relays)))
            .collect();

        let body = nip05_document(&wanted).to_string();

        self.deps.log.debug(
            "site_nip05",
            fields! { "hostname" => host.host.hostname.as_str(), "name" => requested.as_deref(), "names" => wanted.len() },
        );

        let builder = Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "application/json; charset=utf-8")
            // Generated per request, so there is no blob hash for an `ETag`. A
            // minute bounds how long a name survives being changed.
            .header(header::CACHE_CONTROL, "public, max-age=60")
            .header(header::CONTENT_LENGTH, body.len());

        Ok(Some(if request.method == Method::HEAD { builder.body(Body::empty()) } else { builder.body(Body::from(body)) }.expect("response")))
    }
}

/// Build the NIP-05 document for a set of entries. Two names may point at one
/// pubkey, so relay lists are unioned; `relays` is left out when no entry
/// named one.
pub fn nip05_document(entries: &[(String, String, Vec<String>)]) -> serde_json::Value {
    let mut names = serde_json::Map::new();
    let mut relays: indexmap::IndexMap<String, Vec<String>> = indexmap::IndexMap::new();

    for (name, pubkey, entry_relays) in entries {
        names.insert(name.clone(), pubkey.clone().into());
        if entry_relays.is_empty() {
            continue;
        }
        let list = relays.entry(pubkey.clone()).or_default();
        for relay in entry_relays {
            if !list.contains(relay) {
                list.push(relay.clone());
            }
        }
    }

    if relays.is_empty() {
        serde_json::json!({ "names": names })
    } else {
        serde_json::json!({ "names": names, "relays": relays })
    }
}

/// Tell Tor Browser that this site has an onion address of its own. Only over
/// HTTPS, only on a document, and never on a request that arrived over Tor.
fn advertise_onion(request: &Parts, origin: &RequestOrigin, host: &ServedHost, response: &mut Reply) {
    let Some(onion) = &host.onion else { return };
    if is_onion_address(&origin.hostname) || response.status() != StatusCode::OK || origin.scheme != "https" {
        return;
    }
    let html = response.headers().get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).is_some_and(|v| v.starts_with("text/html"));
    if !html {
        return;
    }

    let path_and_query = request.uri.path_and_query().map(|pq| pq.as_str()).unwrap_or("/");
    if let Ok(value) = HeaderValue::from_str(&format!("http://{onion}{path_and_query}")) {
        response.headers_mut().insert("onion-location", value);
    }
}

/// Let any origin read any site, headers included. A site is public files with
/// no credential attached, so the same-origin policy protects nothing here.
fn allow_cross_origin(headers: &mut HeaderMap) {
    headers.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, HeaderValue::from_static("*"));
    headers.insert(header::ACCESS_CONTROL_EXPOSE_HEADERS, HeaderValue::from_static("*"));
}

/// Answer a CORS preflight. The requested headers are echoed back because a
/// static file server has no request header it needs to refuse.
fn preflight(request: &HeaderMap) -> Reply {
    let mut builder = Response::builder()
        .status(StatusCode::NO_CONTENT)
        .header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
        .header(header::ACCESS_CONTROL_ALLOW_METHODS, "GET, HEAD, OPTIONS")
        .header(header::ACCESS_CONTROL_MAX_AGE, PREFLIGHT_MAX_AGE);
    if let Some(requested) = request.get(header::ACCESS_CONTROL_REQUEST_HEADERS) {
        builder = builder.header(header::ACCESS_CONTROL_ALLOW_HEADERS, requested.clone());
    }
    builder.body(Body::empty()).expect("response")
}

fn header_str(headers: &HeaderMap, name: header::HeaderName) -> Option<&str> {
    headers.get(name).and_then(|v| v.to_str().ok())
}

fn text(status: StatusCode, body: &'static str) -> Reply {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .body(Body::from(body))
        .expect("response")
}

/// Pick which stored copy of a blob to serve.
///
/// `q` is honored as the ranking the header defines — zero is a refusal — but
/// ties are broken by size rather than by any preference of npanel's own,
/// which is the case that actually happens: browsers send their encodings at
/// equal quality. `None` means the blob itself; a client that accepts nothing
/// stored gets it anyway rather than a 406.
pub fn negotiate_encoding(header: Option<&str>, variants: &Variants) -> Option<Encoding> {
    let header = header.filter(|h| !h.is_empty())?;
    let quality = parse_accept_encoding(header);

    let mut candidates: Vec<(Encoding, u64, f64)> = Encoding::ALL
        .into_iter()
        .filter_map(|encoding| {
            let size = variants.get(encoding)?;
            let q = quality.get(encoding.as_str()).or(quality.get("*")).copied().unwrap_or(0.0);
            (q > 0.0).then_some((encoding, size, q))
        })
        .collect();

    candidates.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal).then(a.1.cmp(&b.1)));
    let (encoding, size, _) = *candidates.first()?;

    // A copy is only stored when it beat the blob, but serving one that
    // doesn't would be worse than pointless.
    (size < variants.identity).then_some(encoding)
}

/// Quality values by encoding name, lowercased, from an `Accept-Encoding`.
fn parse_accept_encoding(header: &str) -> HashMap<String, f64> {
    let mut quality = HashMap::new();

    for entry in header.split(',') {
        let mut parts = entry.split(';');
        let name = parts.next().unwrap_or("").trim().to_lowercase();
        if name.is_empty() {
            continue;
        }
        let q = parts.map(|p| p.trim().to_lowercase()).find(|p| p.starts_with("q="));
        // A malformed q is treated as absent, as the header's grammar says.
        let value = match q {
            None => 1.0,
            Some(q) => js_number(&q[2..]).unwrap_or(1.0).clamp(0.0, 1.0),
        };
        quality.insert(name, value);
    }

    quality
}

/// `Number(text)` for the values a `q` parameter holds.
fn js_number(text: &str) -> Option<f64> {
    let text = text.trim();
    if text.is_empty() {
        return Some(0.0);
    }
    text.parse::<f64>().ok().filter(|n| n.is_finite())
}

/// The `ETag` for one representation of a blob. Encodings need tags of their
/// own, or a shared cache holding the brotli copy would hand it to a client
/// whose `If-None-Match` matched, however little brotli that client speaks.
pub fn etag_for(sha256: &str, encoding: Option<Encoding>) -> String {
    match encoding {
        Some(encoding) => format!("\"{sha256}-{}\"", encoding.as_str()),
        None => format!("\"{sha256}\""),
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Range {
    Whole,
    Part(u64, u64),
    Unsatisfiable,
}

/// Parse a single-range `Range` header. Multi-range requests are ignored.
pub fn parse_range(header: Option<&str>, size: u64) -> Range {
    let Some(spec) = header.and_then(|h| h.strip_prefix("bytes=")) else {
        return Range::Whole;
    };
    let Some((raw_start, raw_end)) = spec.split_once('-') else {
        return Range::Whole;
    };
    let digits = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
    if !digits(raw_start) || !digits(raw_end) || (raw_start.is_empty() && raw_end.is_empty()) {
        return Range::Whole;
    }

    let parse = |s: &str| s.parse::<u64>().unwrap_or(u64::MAX);

    let (start, end) = if raw_start.is_empty() {
        // Suffix range: the last N bytes.
        let suffix = parse(raw_end);
        if suffix == 0 {
            return Range::Unsatisfiable;
        }
        (size.saturating_sub(suffix), size.wrapping_sub(1))
    } else {
        let start = parse(raw_start);
        let end = if raw_end.is_empty() { size.wrapping_sub(1) } else { parse(raw_end).min(size.wrapping_sub(1)) };
        (start, end)
    };

    if size == 0 || start > end || start >= size {
        return Range::Unsatisfiable;
    }
    Range::Part(start, end)
}

/// How long a file may be cached, by what kind of file it is.
pub fn cache_control(path: &str) -> &'static str {
    match extname(path).to_lowercase().as_str() {
        ".html" | ".htm" | ".xhtml" | ".json" | ".webmanifest" | ".xml" | ".txt" | ".md" => CACHE_CONTROL,
        _ => ASSET_CACHE_CONTROL,
    }
}

/// Whether an `If-None-Match` header matches the current ETag.
pub fn matches_etag(header: Option<&str>, etag: &str) -> bool {
    let Some(header) = header else { return false };
    if header.trim() == "*" {
        return true;
    }
    header.split(',').map(|value| value.trim()).map(|value| value.strip_prefix("W/").unwrap_or(value)).any(|value| value == etag)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn variants(br: Option<u64>, gzip: Option<u64>) -> Variants {
        Variants { identity: 1000, br, gzip }
    }

    #[test]
    fn negotiates() {
        let both = variants(Some(200), Some(300));
        assert_eq!(negotiate_encoding(Some("gzip, deflate, br"), &both), Some(Encoding::Br));
        assert_eq!(negotiate_encoding(Some("gzip"), &both), Some(Encoding::Gzip));
        assert_eq!(negotiate_encoding(Some("br;q=0.5, gzip"), &both), Some(Encoding::Gzip));
        assert_eq!(negotiate_encoding(Some("br;q=0, gzip;q=0"), &both), None);
        assert_eq!(negotiate_encoding(Some("*"), &both), Some(Encoding::Br));
        assert_eq!(negotiate_encoding(Some("identity"), &both), None);
        assert_eq!(negotiate_encoding(None, &both), None);
        assert_eq!(negotiate_encoding(Some("br"), &variants(None, Some(10))), None);
        assert_eq!(negotiate_encoding(Some("br;q=abc"), &both), Some(Encoding::Br));
    }

    #[test]
    fn ranges() {
        assert_eq!(parse_range(None, 100), Range::Whole);
        assert_eq!(parse_range(Some("bytes=0-9"), 100), Range::Part(0, 9));
        assert_eq!(parse_range(Some("bytes=90-"), 100), Range::Part(90, 99));
        assert_eq!(parse_range(Some("bytes=-10"), 100), Range::Part(90, 99));
        assert_eq!(parse_range(Some("bytes=-1000"), 100), Range::Part(0, 99));
        assert_eq!(parse_range(Some("bytes=50-1000"), 100), Range::Part(50, 99));
        assert_eq!(parse_range(Some("bytes=100-"), 100), Range::Unsatisfiable);
        assert_eq!(parse_range(Some("bytes=-0"), 100), Range::Unsatisfiable);
        assert_eq!(parse_range(Some("bytes=5-1"), 100), Range::Unsatisfiable);
        assert_eq!(parse_range(Some("bytes=0-1,5-6"), 100), Range::Whole);
        assert_eq!(parse_range(Some("items=0-1"), 100), Range::Whole);
        assert_eq!(parse_range(Some("bytes=0-0"), 0), Range::Unsatisfiable);
    }

    #[test]
    fn etags() {
        assert!(matches_etag(Some("W/\"abc\", \"def\""), "\"abc\""));
        assert!(matches_etag(Some("*"), "\"x\""));
        assert!(!matches_etag(Some("\"abc-br\""), "\"abc\""));
        assert_eq!(etag_for("abc", Some(Encoding::Gzip)), "\"abc-gzip\"");
    }

    #[test]
    fn nip05_documents() {
        let doc = nip05_document(&[
            ("a".into(), "p1".into(), vec!["wss://r1".into()]),
            ("b".into(), "p1".into(), vec!["wss://r1".into(), "wss://r2".into()]),
            ("c".into(), "p2".into(), vec![]),
        ]);
        assert_eq!(doc, serde_json::json!({ "names": { "a": "p1", "b": "p1", "c": "p2" }, "relays": { "p1": ["wss://r1", "wss://r2"] } }));
        assert_eq!(nip05_document(&[("c".into(), "p2".into(), vec![])]), serde_json::json!({ "names": { "c": "p2" } }));
    }
}
