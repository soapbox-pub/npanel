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
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use bytes::Bytes;
use http::header::{self, HeaderMap, HeaderValue};
use http::request::Parts;
use http::{Method, Request, Response, StatusCode};
use parking_lot::{Mutex, RwLock};
use sha2::{Digest, Sha256};

use crate::analytics::tracker::EVENT_PATH;
use crate::analytics::{Analytics, Peer};
use crate::blobs::{BlobStore, Encoding, Variants};
use crate::builds::current_build;
use crate::db::{BuildKind, Db};
use crate::fields;
use crate::hash::BlobHash;
use crate::hostname::{is_onion_address, www_apex};
use crate::identifiers::{parse_relay_list, NIP05_PATH};
use crate::link_previews::card::{template_card, Card, ImageMeta};
use crate::link_previews::entity::Entity;
use crate::link_previews::image::{Painted, Painter, HEIGHT, WIDTH};
use crate::link_previews::splice::{splice, Page};
use crate::link_previews::templates::{Context, EntityContext, SiteTemplates, Templates};
use crate::link_previews::{kinds, Found, LinkPreviews, Outcome, IMAGE_WAIT, WAIT};
use crate::log::{with_err, Logger};
use crate::mime::content_type;
use crate::nsite_address::encode_naddr;
use crate::seo::canonical_link;
use crate::site_csp::{SiteCsp, GATEWAY_CSP};
use crate::site_env::ENV_PATH;
use crate::sites::{find_host, normalize_site_path, resolve_site_path, ResolvedFile, ServedHost};
use crate::util::{http_date, lowercase_extname};

/// Where a request came from, as settled by the listener that accepted it.
///
/// Neither field can be trusted to the request itself: behind the proxy the
/// scheme comes from `X-Forwarded-Proto`, while a request over Tor, or over
/// TLS npanel terminated itself, has no proxy in front of it and no header
/// worth reading. Deciding both once, at
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

/// The env script blocks the parser, and a request for it that hangs has no
/// timeout, so a browser holding a copy should never wait on the network for
/// it: it runs what it has, for up to a year, and refreshes in the background.
/// A page from the network names a new version when the values change, which
/// nothing has cached, so it still gets them at once; only an offline shell
/// naming the old version sees a change one load late.
const ENV_CACHE_CONTROL: &str = "public, max-age=0, stale-while-revalidate=31536000";

/// The largest page a link preview is spliced into. A shell is a few
/// kilobytes; anything near this is a page with no room left for a preview.
const MAX_PREVIEW_PAGE: u64 = 512 * 1024;

/// Where a site's preview images are, on every site: npanel's, like the
/// event endpoint, and never a file of the site's.
pub const PREVIEW_PATH: &str = "/_npanel/preview/";

/// A preview image at the version a page named never changes; one asked for
/// at any other is the current one, which may not last.
const IMAGE_CACHE_CONTROL: &str = "public, max-age=604800, immutable";
const STALE_IMAGE_CACHE_CONTROL: &str = "public, max-age=300";

/// The largest file of a site's an image template may draw.
const MAX_SITE_IMAGE: u64 = 8 << 20;

/// How long a browser may reuse a preflight result, in seconds.
const PREFLIGHT_MAX_AGE: &str = "86400";

/// How long a cached host lookup may be believed. A backstop: every writer in
/// the process clears the cache the moment serving's answer changes. This is
/// for writers the process can't see, like `prune-blobs`.
const HOST_TTL: Duration = Duration::from_secs(60);

/// Resolved paths to keep. Overflowing it costs a query, never correctness.
const MAX_CACHED_PATHS: usize = 5000;

/// The longest path whose resolution is kept. A request's path can be tens of
/// kilobytes, and the cache is keyed on it: a stranger sending long made-up
/// paths would otherwise fill it with hundreds of megabytes of them.
const MAX_CACHED_PATH_LEN: usize = 1024;

/// The build a hostname serves, with the response fields derived from it.
#[derive(Clone, Debug)]
pub struct CachedVersion {
    /// Immutable, so safe to key caches on.
    pub build_id: i64,
    /// The build's `Last-Modified`, formatted once instead of per response.
    pub last_modified: HeaderValue,
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
                let build = if host.host.enabled { current_build(conn, host.host.id, kind)? } else { None };
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
            last_modified: HeaderValue::from_str(&http_date(build.last_modified)).expect("an HTTP date is a valid header"),
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
    pub link_previews: Arc<LinkPreviews>,
    pub templates: Arc<Templates>,
    pub painter: Arc<Painter>,
    pub log: Logger,
}

pub struct SiteServer {
    deps: SiteServerDeps,
    /// Which file a request path resolves to within a build. Nothing
    /// invalidates it and nothing has to: a build's files are written once and
    /// a build id is never reused. A row still missing its blob sizes is left
    /// uncached, since `compress-blobs` may yet record them.
    paths: Mutex<lru::LruCache<(i64, bool, String), Option<ResolvedFile>>>,
    /// Preview images drawn already.
    painted: Mutex<Painted>,
}

type Reply = Response<Body>;

impl SiteServer {
    pub fn new(deps: SiteServerDeps) -> SiteServer {
        SiteServer {
            deps,
            paths: Mutex::new(lru::LruCache::new(NonZeroUsize::new(MAX_CACHED_PATHS).unwrap())),
            painted: Mutex::new(Painted::default()),
        }
    }

    /// Serve one request for a site, as it reached the listener `origin`
    /// describes. `peer` is who the connection came from; only analytics
    /// reads it.
    pub async fn serve(&self, request: Request<Body>, origin: RequestOrigin, peer: Peer) -> Reply {
        // The one path that isn't the site's, on any site: npanel's own event
        // endpoint, which the tracker in every counting site's build reports
        // to. Ahead of everything, preflights included.
        if request.uri().path() == EVENT_PATH {
            let site = match self.deps.serve_cache.lookup(&origin.hostname).await {
                Ok(site) => site.map(|site| site.host.host.id),
                Err(error) => {
                    self.deps.log.error("site_unavailable", with_err(fields! { "hostname" => origin.hostname.as_str(), "path" => EVENT_PATH }, &error));
                    return text(StatusCode::SERVICE_UNAVAILABLE, "This site is temporarily unavailable. Try again in a moment.");
                }
            };
            return self.deps.analytics.forward_event(request, site, peer).await;
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
        if site.as_ref().is_none_or(|site| site.host.host.csp) {
            let declared = match site.as_ref().and_then(|site| site.version.as_ref()) {
                Some(version) => self.deps.site_csp.policy(version.build_id).await?,
                None => None,
            };
            headers.insert(header::CONTENT_SECURITY_POLICY, declared.unwrap_or(HeaderValue::from_static(GATEWAY_CSP)));
        }

        // Set on everything rather than only on what varied, because a cache
        // has to be told before it stores the first response.
        headers.append(header::VARY, HeaderValue::from_static("accept-encoding"));

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

        let Some(site) = site.filter(|site| site.host.host.enabled) else {
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

        if let Some(name) = path.strip_prefix(PREVIEW_PATH) {
            return self.preview_image(request, origin, site, version.build_id, name).await;
        }

        let Some(file) = self.resolve(version.build_id, path, site.host.host.spa).await? else {
            log.debug("site_path_missing", fields! { "hostname" => hostname, "path" => path });
            return Ok(text(StatusCode::NOT_FOUND, "Not found."));
        };

        // Sizes come with the path lookup when a deploy recorded them; the
        // stat is only for rows that predate that.
        let variants = match file.variants {
            Some(variants) => Some(variants),
            None => {
                let (blobs, sha) = (self.deps.blobs.clone(), file.sha256);
                tokio::task::spawn_blocking(move || blobs.variants(sha)).await.ok().flatten()
            }
        };
        let Some(variants) = variants else {
            // The build is ready, so this means the blob was removed underneath us.
            log.error("blob_missing", fields! { "hostname" => hostname, "path" => file.path.as_str(), "sha256" => file.sha256.to_string() });
            return Ok(text(StatusCode::INTERNAL_SERVER_ERROR, "This file is missing from local storage."));
        };

        // A page whose path names a Nostr event answers crawlers with that
        // event's preview, and everyone else as published — so a cache in
        // between has to keep the two apart.
        let previewed = site.host.host.link_previews && file.status == 200 && content_type(&file.path).starts_with("text/html");
        let entity = if previewed { Entity::from_path(path) } else { None };
        let templates = match &entity {
            Some(_) => self.deps.templates.for_build(version.build_id).await?,
            None => None,
        };
        // A path naming a kind nothing previews is no different from any
        // other, and costs no lookup.
        let entity = entity.filter(|entity| entity.kind().is_none_or(|kind| kinds::supported(kind) || templates.as_ref().is_some_and(|t| t.has(kind))));
        if let Some(entity) = &entity {
            let crawler = request.method == Method::GET && self.deps.link_previews.is_crawler(header_str(&request.headers, header::USER_AGENT));
            if crawler && variants.identity <= MAX_PREVIEW_PAGE {
                if let Some(reply) = self.link_preview(request, origin, site, version.build_id, &file, entity, templates).await {
                    return Ok(reply);
                }
            }
        }

        // Everything below is about the representation this picks, not the
        // blob: its size, its `ETag`, and the bytes a `Range` counts from.
        let encoding = negotiate_encoding(header_str(&request.headers, header::ACCEPT_ENCODING), &variants);
        let size = variants.size(encoding);
        let etag = etag_for(file.sha256, encoding);

        let mut builder = Response::builder()
            .header(header::CONTENT_TYPE, content_type(&file.path))
            .header(header::CACHE_CONTROL, cache_control(&file.path))
            .header(header::ETAG, &etag)
            .header(header::LAST_MODIFIED, version.last_modified.clone())
            .header(header::ACCEPT_RANGES, "bytes")
            // Which nsite this hostname is serving, in the one form a client
            // can paste anywhere.
            .header("x-nsite-addr", site.naddr.clone());

        if let Some(encoding) = encoding {
            builder = builder.header(header::CONTENT_ENCODING, encoding.as_str());
        }
        if entity.is_some() {
            builder = builder.header(header::VARY, "user-agent");
        }

        // Names the one spelling of this page, so `/foo`, `/foo/` and
        // `/foo/index.html` score as one page instead of three.
        if site.host.host.seo {
            if let Some(canonical) = canonical_link(origin, path, &file.path, file.status) {
                builder = builder.header(header::LINK, canonical);
            }
        }

        if matches_etag(header_str(&request.headers, header::IF_NONE_MATCH), &etag) {
            return Ok(builder.status(StatusCode::NOT_MODIFIED).body(Body::empty()).expect("response"));
        }

        // A GET is measured by the bytes it sends. The recorded size describes
        // the same file, but if the two ever disagreed, a `Content-Length` the
        // body doesn't match is a response hyper aborts, and a range past the
        // body's end a slice that panics.
        let bytes = if request.method == Method::HEAD {
            None
        } else {
            match self.open(file.sha256, encoding).await {
                Ok(bytes) => Some(bytes),
                Err(error) => {
                    log.error(
                        "blob_missing",
                        fields! { "hostname" => hostname, "path" => file.path.as_str(), "sha256" => file.sha256.to_string(), "err_msg" => error.to_string() },
                    );
                    return Ok(text(StatusCode::INTERNAL_SERVER_ERROR, "This file is missing from local storage."));
                }
            }
        };
        let size = bytes.as_ref().map_or(size, |bytes| bytes.len() as u64);

        // A range is of this representation only: not of a not-found page
        // standing in for what was asked, and not of a version the client
        // didn't name in `If-Range`.
        let ranged = file.status != 404 && header_str(&request.headers, header::IF_RANGE).is_none_or(|tag| tag == etag);
        let requested = if ranged { header_str(&request.headers, header::RANGE) } else { None };
        let range = match parse_range(requested, size) {
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

        let Some(bytes) = bytes else {
            return Ok(builder.status(status).body(Body::empty()).expect("response"));
        };
        // `parse_range` keeps both ends within `size`, which is this length.
        let body = match range {
            Some((start, end)) => bytes.slice(start as usize..=end as usize),
            None => bytes,
        };

        if log.enabled(crate::log::LogLevel::Debug) {
            log.debug(
                "site_request",
                fields! {
                    "hostname" => hostname,
                    "path" => path,
                    "status" => status.as_u16(),
                    "sha256" => file.sha256.to_string(),
                    "bytes" => size,
                    "encoding" => encoding.map(Encoding::as_str),
                },
            );
        }

        Ok(builder.status(status).body(Body::from(body)).expect("response"))
    }

    /// The page about the Nostr event `entity` names, for a crawler: the
    /// site's own document, with the event's preview in its head and the
    /// event itself at the top of its body. `None` sends the page as
    /// published, whatever the reason — this is never worth an error.
    #[allow(clippy::too_many_arguments)]
    async fn link_preview(
        &self,
        request: &Parts,
        origin: &RequestOrigin,
        site: &CachedSite,
        build_id: i64,
        file: &ResolvedFile,
        entity: &Entity,
        templates: Option<Arc<SiteTemplates>>,
    ) -> Option<Reply> {
        let site_origin = format!("{}://{}", origin.scheme, origin.hostname);
        let path = request.uri.path();
        let url = format!("{site_origin}{}", path.strip_suffix('/').filter(|p| !p.is_empty()).unwrap_or(path));

        // The site's own templates, where it has one for this kind or the
        // kind is yet to be learned; npanel's card otherwise.
        let card = match templates.filter(|t| entity.kind().is_none_or(|kind| t.has(kind))) {
            Some(templates) => match self.deps.link_previews.templated(entity, templates.queries.clone(), WAIT).await? {
                (Outcome::Card(card), _) => card,
                (Outcome::Results(found), fetched_at) => {
                    Arc::new(self.template_card(origin, build_id, templates, found, fetched_at, url.clone(), site_origin.clone()).await?)
                }
                _ => return None,
            },
            None => self.deps.link_previews.card(entity).await?,
        };
        let document = self.open(file.sha256, None).await.ok()?;
        let body = splice(&document, &card, &Page { url: &url, origin: &site_origin })?;

        // Made per request, so its tag is its own hash: a card refreshed
        // behind a crawler's back is a different page.
        let etag = format!("\"{}\"", &hex::encode(Sha256::digest(&body))[..32]);
        let builder = Response::builder()
            .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
            .header(header::CACHE_CONTROL, CACHE_CONTROL)
            .header(header::ETAG, &etag)
            .header(header::VARY, "user-agent")
            .header("x-nsite-addr", site.naddr.clone());

        self.deps.log.debug("site_link_preview", fields! { "hostname" => origin.hostname.as_str(), "path" => path, "key" => entity.key() });

        if matches_etag(header_str(&request.headers, header::IF_NONE_MATCH), &etag) {
            return Some(builder.status(StatusCode::NOT_MODIFIED).body(Body::empty()).expect("response"));
        }
        Some(builder.status(StatusCode::OK).header(header::CONTENT_LENGTH, body.len()).body(Body::from(body)).expect("response"))
    }

    /// The card a site's template makes of what its queries found. `None`
    /// for a template that fails, which the log says why.
    #[allow(clippy::too_many_arguments)]
    async fn template_card(
        &self,
        origin: &RequestOrigin,
        build_id: i64,
        templates: Arc<SiteTemplates>,
        found: Arc<Found>,
        fetched_at: i64,
        url: String,
        site_origin: String,
    ) -> Option<Card> {
        let kind = found.subject.kind;
        let generated = match (templates.has_image(kind), found.subject.nip19(), templates.sha256(kind)) {
            (true, Some(nip19), Some(template)) => {
                let version = image_version(build_id, template, fetched_at, &nip19);
                Some(format!("{site_origin}{PREVIEW_PATH}{nip19}.jpg?v={}", &version[..16]))
            }
            _ => None,
        };

        let render = {
            let (templates, found, url, site_origin) = (templates.clone(), found.clone(), url.clone(), site_origin.clone());
            move || {
                let context = Context { entity: EntityContext::of(&found.subject), results: &found.results, url: &url, origin: &site_origin };
                templates.render_data(kind, &context)
            }
        };
        let fields = match tokio::task::spawn_blocking(render).await.ok()? {
            Ok(fields) => fields,
            Err(error) => {
                self.deps.log.warn(
                    "link_preview_template_failed",
                    fields! { "hostname" => origin.hostname.as_str(), "kind" => kind, "err_msg" => format!("{error:#}") },
                );
                return None;
            }
        };

        // An image the template names is taken relative to the page; one it
        // sets to `none` is no image of its own; one it leaves alone is drawn.
        let drawn = || Some(ImageMeta { mime: "image/jpeg".into(), width: WIDTH, height: HEIGHT });
        let (image, meta) = match &fields.image {
            None => (generated.clone(), generated.as_ref().and_then(|_| drawn())),
            Some(None) => (None, None),
            Some(Some(named)) => {
                let resolved = url::Url::parse(&url).ok().and_then(|base| base.join(named).ok()).filter(|u| matches!(u.scheme(), "https" | "http"));
                (resolved.map(String::from), None)
            }
        };
        Some(template_card(kind, fields, image, meta))
    }

    /// The image a site's template draws for an entity, at
    /// `/_npanel/preview/<nip19>.jpg`. Any client may fetch it, since the
    /// services that fetch a preview's image seldom say they are the crawler
    /// that read its page.
    async fn preview_image(&self, request: &Parts, origin: &RequestOrigin, site: &CachedSite, build_id: i64, name: &str) -> anyhow::Result<Reply> {
        let not_found = || Ok(text(StatusCode::NOT_FOUND, "Not found."));
        if !site.host.host.link_previews {
            return not_found();
        }
        let Some(entity) = name.strip_suffix(".jpg").and_then(|nip19| Entity::from_path(&format!("/{nip19}"))) else { return not_found() };
        let Some(templates) = self.deps.templates.for_build(build_id).await? else { return not_found() };
        if entity.kind().is_some_and(|kind| !templates.has_image(kind)) {
            return not_found();
        }

        let Some((outcome, fetched_at)) = self.deps.link_previews.templated(&entity, templates.queries.clone(), IMAGE_WAIT).await else {
            let mut busy = text(StatusCode::SERVICE_UNAVAILABLE, "This preview isn't ready yet. Try again in a moment.");
            busy.headers_mut().insert(header::RETRY_AFTER, HeaderValue::from_static("5"));
            return Ok(busy);
        };
        let Outcome::Results(found) = outcome else { return not_found() };
        let kind = found.subject.kind;
        let (Some(nip19), Some(template)) = (found.subject.nip19(), templates.sha256(kind)) else { return not_found() };
        if !templates.has_image(kind) {
            return not_found();
        }

        let version = image_version(build_id, template, fetched_at, &nip19);
        let etag = format!("\"{}\"", &version[..32]);
        let named = request.uri.query().and_then(|q| url::form_urlencoded::parse(q.as_bytes()).find(|(k, _)| k == "v").map(|(_, v)| v.into_owned()));
        let current = named.is_some_and(|v| v == version[..16]);
        let builder = Response::builder()
            .header(header::CONTENT_TYPE, "image/jpeg")
            .header(header::CACHE_CONTROL, if current { IMAGE_CACHE_CONTROL } else { STALE_IMAGE_CACHE_CONTROL })
            .header(header::ETAG, &etag)
            .header("x-nsite-addr", site.naddr.clone());
        if matches_etag(header_str(&request.headers, header::IF_NONE_MATCH), &etag) {
            return Ok(builder.status(StatusCode::NOT_MODIFIED).body(Body::empty()).expect("response"));
        }

        let cached = self.painted.lock().get(&version);
        let image = match cached {
            Some(image) => image,
            None => {
                let site_origin = format!("{}://{}", origin.scheme, origin.hostname);
                match self.paint(templates, found, build_id, format!("{site_origin}/{nip19}"), site_origin).await {
                    Ok(image) => {
                        self.painted.lock().put(version, image.clone());
                        image
                    }
                    Err(error) => {
                        self.deps.log.warn(
                            "link_preview_image_failed",
                            fields! { "hostname" => origin.hostname.as_str(), "kind" => kind, "err_msg" => format!("{error:#}") },
                        );
                        return not_found();
                    }
                }
            }
        };

        let builder = builder.status(StatusCode::OK).header(header::CONTENT_LENGTH, image.len());
        Ok(if request.method == Method::HEAD { builder.body(Body::empty()) } else { builder.body(Body::from(image)) }.expect("response"))
    }

    /// Draw a template's image: the SVG it writes, off the runtime threads,
    /// then the JPEG made of it.
    async fn paint(&self, templates: Arc<SiteTemplates>, found: Arc<Found>, build_id: i64, url: String, site_origin: String) -> anyhow::Result<Bytes> {
        let fonts = templates.fonts.clone();
        let svg = tokio::task::spawn_blocking(move || {
            let context = Context { entity: EntityContext::of(&found.subject), results: &found.results, url: &url, origin: &site_origin };
            templates.render_image(found.subject.kind, &context)
        })
        .await??;
        let jpeg = self.deps.painter.paint(svg, fonts, |path| self.site_file(build_id, path)).await?;
        Ok(Bytes::from(jpeg))
    }

    /// A file of the site's own, by the exact path an image names.
    async fn site_file(&self, build_id: i64, path: String) -> Option<Vec<u8>> {
        let path = crate::util::decode_uri_component(path.split(['?', '#']).next().unwrap_or_default())?;
        let blobs = self.deps.blobs.clone();
        self.deps
            .db
            .call(move |conn| {
                use rusqlite::OptionalExtension;
                let sha256: Option<BlobHash> = conn
                    .prepare_cached("SELECT sha256 FROM host_build_files WHERE build_id = ? AND path = ?")?
                    .query_row(rusqlite::params![build_id, path], |row| row.get(0))
                    .optional()?;
                Ok(sha256.filter(|sha| blobs.size(*sha).is_some_and(|size| size <= MAX_SITE_IMAGE)).map(|sha| blobs.read_bytes(sha, MAX_SITE_IMAGE)))
            })
            .await
            .ok()
            .flatten()
    }

    /// A blob's bytes for a response body. A kept mapping answers right
    /// here, with no system call; a new one is opened off the runtime threads.
    async fn open(&self, sha256: BlobHash, encoding: Option<Encoding>) -> anyhow::Result<Bytes> {
        if let Some(bytes) = self.deps.blobs.mapped(sha256, encoding) {
            return Ok(bytes);
        }
        let blobs = self.deps.blobs.clone();
        tokio::task::spawn_blocking(move || blobs.open(sha256, encoding)).await?
    }

    async fn resolve(&self, build_id: i64, path: &str, spa: bool) -> anyhow::Result<Option<ResolvedFile>> {
        // The SPA flag is part of the key because it is read per request rather
        // than built in: switching it must not have to wait for a rebuild.
        // Keyed on the path as resolution sees it, so spellings of one path
        // share an entry.
        let normalized = normalize_site_path(path);
        let key = (normalized.len() <= MAX_CACHED_PATH_LEN).then_some((build_id, spa, normalized));
        if let Some(cached) = key.as_ref().and_then(|key| self.paths.lock().get(key).cloned()) {
            return Ok(cached);
        }

        let request_path = path.to_string();
        let file = self
            .deps
            .db
            .call(move |conn| Ok(resolve_site_path(conn, build_id, &request_path, spa)?))
            .await?;

        // A path the build has no file for is cached too — the file list that
        // settled that is as immutable as the one that resolves a hit.
        if let Some(key) = key.filter(|_| file.as_ref().is_none_or(|file| file.variants.is_some())) {
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
        if !site.host.host.enabled || !site.host.host.redirect_www {
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
                // The scheme is the proxy's word, so a shared cache must not
                // hand one scheme's redirect to a request that came in on the other.
                .header(header::VARY, "x-forwarded-proto")
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

/// What names one drawing of a preview image: everything it is drawn from.
fn image_version(build_id: i64, template: BlobHash, fetched_at: i64, nip19: &str) -> String {
    hex::encode(Sha256::digest(format!("{build_id}:{template}:{fetched_at}:{nip19}").as_bytes()))
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
    let accepted = parse_accept_encoding(header);

    // The best quality, then the smallest; the first listed if both tie.
    let (encoding, size, _) = Encoding::ALL
        .into_iter()
        .filter_map(|encoding| {
            let size = variants.get(encoding)?;
            let q = accepted.quality(encoding).unwrap_or(0.0);
            (q > 0.0).then_some((encoding, size, q))
        })
        .min_by(|a, b| b.2.total_cmp(&a.2).then(a.1.cmp(&b.1)))?;

    // A copy is only stored when it beat the blob, but serving one that
    // doesn't would be worse than pointless.
    (size < variants.identity).then_some(encoding)
}

/// The quality an `Accept-Encoding` gives each encoding npanel stores, and
/// `*`, where it names them.
#[derive(Default)]
struct Accepted {
    br: Option<f64>,
    gzip: Option<f64>,
    any: Option<f64>,
}

impl Accepted {
    fn quality(&self, encoding: Encoding) -> Option<f64> {
        let named = match encoding {
            Encoding::Br => self.br,
            Encoding::Gzip => self.gzip,
        };
        named.or(self.any)
    }
}

fn parse_accept_encoding(header: &str) -> Accepted {
    let mut accepted = Accepted::default();

    for entry in header.split(',') {
        let mut parts = entry.split(';');
        let name = parts.next().unwrap_or("").trim();
        let q = parts.map(str::trim).find_map(|p| p.strip_prefix("q=").or_else(|| p.strip_prefix("Q=")));
        // A malformed q is treated as absent, as the header's grammar says.
        let value = match q {
            None => 1.0,
            Some(q) => q.trim().parse::<f64>().ok().filter(|n| n.is_finite()).unwrap_or(1.0).clamp(0.0, 1.0),
        };
        let slot = if name.eq_ignore_ascii_case("br") {
            &mut accepted.br
        } else if name.eq_ignore_ascii_case("gzip") {
            &mut accepted.gzip
        } else if name == "*" {
            &mut accepted.any
        } else {
            continue;
        };
        *slot = Some(value);
    }

    accepted
}

/// The `ETag` for one representation of a blob. Encodings need tags of their
/// own, or a shared cache holding the brotli copy would hand it to a client
/// whose `If-None-Match` matched, however little brotli that client speaks.
pub fn etag_for(sha256: BlobHash, encoding: Option<Encoding>) -> String {
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
    if path == ENV_PATH {
        return ENV_CACHE_CONTROL;
    }
    match &*lowercase_extname(path) {
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
        assert_eq!(negotiate_encoding(Some("GZIP;Q=1, BR;q=0.1"), &both), Some(Encoding::Gzip));
        assert_eq!(negotiate_encoding(Some("br, gzip"), &variants(Some(200), Some(200))), Some(Encoding::Br));
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
        let hash = BlobHash::of(b"abc");
        assert_eq!(etag_for(hash, Some(Encoding::Gzip)), format!("\"{hash}-gzip\""));
    }

    #[test]
    fn caches() {
        assert_eq!(cache_control("/index.html"), CACHE_CONTROL);
        assert_eq!(cache_control("/app.js"), ASSET_CACHE_CONTROL);
        assert_eq!(cache_control(ENV_PATH), ENV_CACHE_CONTROL);
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
