//! End-to-end: a gateway assembled as `main` assembles it, with a fake Nostr
//! and a fake Blossom server standing in for the network. A manifest goes in
//! one end; a served site and the API describing it come out the other.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use futures_util::future::BoxFuture;
use http::{Request, StatusCode};
use http_body_util::BodyExt;
use parking_lot::Mutex;
use sha2::{Digest, Sha256};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::analytics::tracker::EVENT_PATH;
use crate::analytics::{Analytics, AnalyticsConfig};
use crate::app::App;
use crate::errors::{Errors, ErrorsConfig};
use crate::auth::sign_nip98;
use crate::blobs::BlobStore;
use crate::db::Db;
use crate::deploy::{Deployer, DeployerDeps, Deployments};
use crate::link_previews::relays::EventSource;
use crate::link_previews::{LinkPreviews, LinkPreviewsConfig};
use crate::log::{LogLevel, Logger};
use crate::nostr::pool::Filter;
use crate::nostr::service::NostrSource;
use crate::nostr::Event;
use crate::nsite_address::NsiteAddress;
use crate::serve::{ServeCache, SiteServer, SiteServerDeps};
use crate::settings::Settings;

const ADMIN: &str = "0000000000000000000000000000000000000000000000000000000000000007";
const AUTHOR: &str = "0000000000000000000000000000000000000000000000000000000000000009";

/// Relays that hold exactly the manifests a test gives them.
struct FakeNostr {
    manifests: Mutex<Vec<Event>>,
    server: String,
}

impl NostrSource for FakeNostr {
    fn relays(&self) -> Vec<String> {
        vec!["wss://relay.invalid".into()]
    }

    fn fetch_manifest<'a>(&'a self, address: &'a NsiteAddress, _: &'a CancellationToken) -> BoxFuture<'a, Option<Event>> {
        Box::pin(async move {
            self.manifests
                .lock()
                .iter()
                .filter(|e| crate::manifest::manifest_address(e) == *address)
                .max_by_key(|e| e.created_at)
                .cloned()
        })
    }

    fn authors_relays<'a>(&'a self, _: &'a [String], _: &'a CancellationToken) -> BoxFuture<'a, HashMap<String, Vec<String>>> {
        Box::pin(async { HashMap::new() })
    }

    fn blob_sources<'a>(&'a self, _: &'a Event, _: &'a [String], _: &'a CancellationToken) -> BoxFuture<'a, Vec<String>> {
        Box::pin(async move { vec![self.server.clone()] })
    }

    fn subscribe(&self, _: Vec<Filter>, _: &[String], cancel: CancellationToken) -> mpsc::Receiver<Event> {
        let (tx, rx) = mpsc::channel(1);
        tokio::spawn(async move {
            cancel.cancelled().await;
            drop(tx);
        });
        rx
    }

    fn clear_caches(&self) {}
}

/// Relays for link previews, holding exactly the events a test gives them —
/// or, while `hang` is set, never answering at all.
#[derive(Default)]
struct FakeEvents {
    events: Mutex<Vec<Event>>,
    hang: std::sync::atomic::AtomicBool,
}

impl EventSource for FakeEvents {
    fn query(&self, filters: Vec<Filter>, _: Vec<String>) -> BoxFuture<'_, Vec<Event>> {
        Box::pin(async move {
            if self.hang.load(std::sync::atomic::Ordering::SeqCst) {
                std::future::pending::<()>().await;
            }
            self.events.lock().iter().filter(|e| filters.iter().any(|f| f.matches(e))).cloned().collect()
        })
    }
}

/// A Blossom server on loopback, holding the given blobs.
async fn blossom(blobs: HashMap<String, Vec<u8>>) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let blobs = Arc::new(blobs);
    let router = axum::Router::new().route(
        "/{sha}",
        axum::routing::get(move |axum::extract::Path(sha): axum::extract::Path<String>| {
            let blobs = blobs.clone();
            async move {
                match blobs.get(&sha) {
                    Some(bytes) => (StatusCode::OK, bytes.clone()),
                    None => (StatusCode::NOT_FOUND, Vec::new()),
                }
            }
        }),
    );
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    format!("http://{address}/")
}

struct Gateway {
    app: App,
    db: Db,
    deployer: Deployer,
    events: Arc<FakeEvents>,
    _dir: tempfile::TempDir,
}

impl Gateway {
    async fn request(&self, method: &str, host: &str, path: &str, headers: &[(&str, &str)], body: Option<&str>) -> (StatusCode, http::HeaderMap, Vec<u8>) {
        let mut builder = Request::builder().method(method).uri(path).header("host", host);
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        let request = builder.body(body.map_or(Body::empty(), |b| Body::from(b.to_string()))).unwrap();
        let response = self.app.handle(request, crate::app::Via::Proxy([127, 0, 0, 1].into())).await;
        let (parts, body) = response.into_parts();
        (parts.status, parts.headers, body.collect().await.unwrap().to_bytes().to_vec())
    }

    /// A signed API call as the admin.
    async fn api(&self, method: &str, path: &str, body: Option<serde_json::Value>) -> (StatusCode, serde_json::Value) {
        let raw = body.map(|b| b.to_string());
        let auth = sign_nip98(ADMIN, method, &format!("https://panel.example.com{path}"), raw.as_deref().map(str::as_bytes), crate::util::now());
        let (status, _, bytes) = self.request(method, "panel.example.com", path, &[("authorization", &auth)], raw.as_deref()).await;
        (status, serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null))
    }
}

async fn gateway(nostr: Arc<FakeNostr>) -> Gateway {
    let dir = tempfile::tempdir().unwrap();
    let log = Logger::new(LogLevel::Error);
    let db = Db::open(&dir.path().join("npanel.sqlite3"), None).unwrap();
    let settings = Settings::load(&db, None).unwrap();
    let blobs = BlobStore::allowing_private(&dir.path().join("blobs"));
    let serve_cache = Arc::new(ServeCache::new(db.clone()));
    let last_prune = crate::prune::LastPrune::default();

    let cache = serve_cache.clone();
    let deployer = Deployer::new(DeployerDeps {
        db: db.clone(),
        blobs: blobs.clone(),
        nostr,
        settings: settings.clone(),
        log,
        serving_changed: Arc::new(move || cache.clear()),
        retry_backoff: Some(Duration::from_millis(10)),
        last_prune: last_prune.clone(),
    });

    let analytics = Arc::new(Analytics::start(
        db.clone(),
        settings.clone(),
        log,
        AnalyticsConfig { data_dir: dir.path().to_path_buf(), download_geo: false },
    ));
    let errors = Arc::new(Errors::start(db.clone(), blobs.clone(), settings.clone(), log, ErrorsConfig { data_dir: dir.path().to_path_buf() }));
    let events = Arc::new(FakeEvents::default());
    let link_previews = Arc::new(LinkPreviews::start(
        settings.clone(),
        log,
        LinkPreviewsConfig { data_dir: dir.path().to_path_buf(), source: Some(events.clone()) },
    ));
    let scripts = Arc::new(crate::link_previews::script::Scripts::new(db.clone(), blobs.clone()));
    let site_csp = Arc::new(crate::site_csp::SiteCsp::new(db.clone(), blobs.clone(), log));
    let site = Arc::new(SiteServer::new(SiteServerDeps {
        db: db.clone(),
        blobs: blobs.clone(),
        site_csp: site_csp.clone(),
        serve_cache: serve_cache.clone(),
        analytics: analytics.clone(),
        errors: errors.clone(),
        link_previews: link_previews.clone(),
        scripts: scripts.clone(),
        log,
    }));
    let deployments: Arc<dyn Deployments> = Arc::new(deployer.clone());
    let tor = Arc::new(crate::tor::TorService::start(crate::tor::TorDeps {
        db: db.clone(),
        settings: settings.clone(),
        log,
        data_dir: dir.path().to_path_buf(),
        site: site.clone(),
        serve_cache: serve_cache.clone(),
        deployments: deployments.clone(),
    }));

    db.with(|conn| {
        conn.execute("INSERT INTO users (pubkey, role, added_by, created_at) VALUES (?, 'admin', NULL, 1)", [crate::nostr::event::public_key(ADMIN)])?;
        Ok(())
    })
    .unwrap();

    let api = crate::api::router(Arc::new(crate::api::ApiDeps {
        db: db.clone(),
        settings: settings.clone(),
        deployments,
        analytics,
        errors,
        dns: crate::dns::DnsTargets::new("panel.example.com".into(), settings.clone(), log),
        log,
        tor,
        tls: None,
        site_csp,
        previews: Arc::new(crate::site_preview::SitePreviews::new(blobs.clone())),
        link_previews,
        scripts,
        serve_cache,
        last_prune,
        dashboard_host: "panel.example.com".into(),
    }));
    let app = App {
        dashboard_host: "panel.example.com".into(),
        site,
        dashboard: crate::dashboard::Dashboard::new(crate::dashboard::Source::disk(vec![dir.path().join("no-dashboard")]), "panel.example.com", log),
        api,
    };

    deployer.start().await;
    Gateway { app, db, deployer, events, _dir: dir }
}

fn sha(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// Wait for a condition the deploy runtime will bring about. Long enough for
/// a preview script's first process, which in a debug build on a system with
/// thousands of fonts takes seconds to read them.
async fn eventually(what: &str, mut check: impl AsyncFnMut() -> bool) {
    for _ in 0..1200 {
        if check().await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("{what} never happened");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_published_site_is_deployed_built_and_served() {
    let page = format!(
        "<!doctype html><html><head><title>Docs</title><meta property=\"og:description\" content=\"About things.\"></head><body>{}</body></html>",
        "hello ".repeat(200)
    );
    let script = "console.log('hi');\n".repeat(100);
    let files: HashMap<String, Vec<u8>> =
        [(sha(page.as_bytes()), page.clone().into_bytes()), (sha(script.as_bytes()), script.clone().into_bytes())].into();
    let server = blossom(files).await;

    let manifest = Event::sign(
        AUTHOR,
        crate::util::now() - 60,
        15128,
        vec![
            vec!["path".into(), "/index.html".into(), sha(page.as_bytes())],
            vec!["path".into(), "/app.js".into(), sha(script.as_bytes())],
        ],
        "",
    );
    let nostr = Arc::new(FakeNostr { manifests: Mutex::new(vec![manifest.clone()]), server });
    let gw = gateway(nostr).await;
    let npub = crate::nostr::nip19::npub_encode(&manifest.pubkey);

    // Before the host exists, the name is unknown.
    let (status, ..) = gw.request("GET", "docs.example.com", "/", &[], None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Unauthenticated API calls are refused; the admin's are not.
    let (status, ..) = gw.request("GET", "panel.example.com", "/api/me", &[], None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, body) = gw.api("POST", "/api/hosts", Some(serde_json::json!({ "hostname": "Docs.Example.com", "address": npub }))).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["host"]["hostname"], "docs.example.com");

    // A name that's taken is a conflict, whether added or renamed onto, and
    // a malformed body is the caller's mistake.
    let (status, body) = gw.api("POST", "/api/hosts", Some(serde_json::json!({ "hostname": "docs.example.com", "address": npub }))).await;
    assert_eq!((status, body["error"].as_str()), (StatusCode::CONFLICT, Some("docs.example.com is already configured.")));
    let (status, _) = gw.api("POST", "/api/hosts", Some(serde_json::json!({ "hostname": "other.example.com", "address": npub }))).await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, body) = gw.api("PATCH", "/api/hosts/other.example.com", Some(serde_json::json!({ "hostname": "DOCS.example.com" }))).await;
    assert_eq!((status, body["error"].as_str()), (StatusCode::CONFLICT, Some("docs.example.com is already configured.")));
    let (status, _) = gw.api("PATCH", "/api/hosts/other.example.com", Some(serde_json::json!({ "spa": "yes" }))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = gw.api("DELETE", "/api/hosts/other.example.com", None).await;
    assert_eq!(status, StatusCode::OK);

    // The new host fetches the manifest, downloads, and builds.
    eventually("the site going live", async || gw.request("GET", "docs.example.com", "/", &[], None).await.0 == StatusCode::OK).await;

    let (status, headers, body) = gw.request("GET", "docs.example.com", "/", &[("accept-encoding", "gzip, br")], None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["content-encoding"], "br");
    assert_eq!(headers["etag"], format!("\"{}-br\"", sha(page.as_bytes())));
    assert_eq!(headers["content-type"], "text/html; charset=utf-8");
    assert_eq!(headers["cache-control"], "public, no-cache");
    assert_eq!(headers["vary"], "accept-encoding");
    assert_eq!(headers["access-control-allow-origin"], "*");
    assert_eq!(headers["link"], "<https://docs.example.com/>; rel=\"canonical\"");
    assert_eq!(headers["x-nsite-addr"], crate::nsite_address::encode_naddr(&NsiteAddress { pubkey: manifest.pubkey.clone(), kind: 15128, identifier: String::new() }));
    assert_eq!(headers["content-security-policy"], crate::site_csp::GATEWAY_CSP);
    let mut decoded = Vec::new();
    brotli::BrotliDecompress(&mut body.as_slice(), &mut decoded).unwrap();
    assert_eq!(decoded, page.as_bytes());

    let etag = headers["etag"].to_str().unwrap().to_string();
    let (status, ..) = gw.request("GET", "docs.example.com", "/", &[("accept-encoding", "br"), ("if-none-match", &etag)], None).await;
    assert_eq!(status, StatusCode::NOT_MODIFIED);

    let (status, headers, body) = gw.request("GET", "docs.example.com", "/app.js", &[("range", "bytes=0-6")], None).await;
    assert_eq!(status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(headers["content-range"], format!("bytes 0-6/{}", script.len()));
    assert_eq!(body, b"console");

    // Generated for the host, since the site published neither.
    let (_, _, robots) = gw.request("GET", "docs.example.com", "/robots.txt", &[], None).await;
    assert_eq!(String::from_utf8(robots).unwrap(), "User-agent: *\nAllow: /\n\nSitemap: https://docs.example.com/sitemap.xml\n");
    let (_, _, sitemap) = gw.request("GET", "docs.example.com", "/sitemap.xml", &[], None).await;
    assert!(String::from_utf8(sitemap).unwrap().contains("<loc>https://docs.example.com/</loc>"));

    let (status, ..) = gw.request("POST", "docs.example.com", "/", &[], Some("x")).await;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
    let (status, headers, _) = gw.request("GET", "www.example.com", "/x?y=1", &[], None).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "www of a subdomain is not redirected");
    assert!(headers.get("location").is_none());

    // The API describes what is served, preview included.
    let (status, body) = gw.api("GET", "/api/hosts/docs.example.com", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["host"]["active"]["event_id"], manifest.id);
    assert_eq!(body["host"]["active"]["status"], "ready");
    assert_eq!(body["host"]["preview"]["title"], "Docs");
    assert_eq!(body["host"]["preview"]["description"], "About things.");
    assert_eq!(body["host"]["csp_source"], "gateway");

    let (_, files) = gw.api("GET", "/api/hosts/docs.example.com/files", None).await;
    let sources: Vec<(String, String)> = files["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| (f["path"].as_str().unwrap().to_string(), f["source"].as_str().unwrap().to_string()))
        .collect();
    assert_eq!(
        sources,
        [("/app.js".into(), "site".into()), ("/index.html".into(), "site".into()), ("/robots.txt".into(), "generated".into()), ("/sitemap.xml".into(), "generated".into())]
    );

    // A version's size is its files'; what the site stores counts the
    // generated files and compressed copies too.
    let (_, versions) = gw.api("GET", "/api/hosts/docs.example.com/versions", None).await;
    assert_eq!(versions["versions"][0]["size"], page.len() + script.len());
    assert_eq!(versions["storage"]["versions"], 1);
    assert_eq!(versions["storage"]["blobs"], 4);
    assert!(versions["storage"]["bytes"].as_u64().unwrap() > (page.len() + script.len()) as u64);
    let (status, storage) = gw.api("GET", "/api/storage", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(storage, serde_json::json!({ "unreferenced": { "blobs": 0, "bytes": 0 }, "last_prune": null }));

    // NIP-05 names answer at once, and are the gateway's rather than the site's.
    let (status, _) = gw.api("PUT", "/api/hosts/docs.example.com/nip05/Bob", Some(serde_json::json!({ "pubkey": npub, "relays": ["wss://R.example.com/"] }))).await;
    assert_eq!(status, StatusCode::CREATED);
    let (_, headers, body) = gw.request("GET", "docs.example.com", "/.well-known/nostr.json?name=bob", &[], None).await;
    assert_eq!(headers["cache-control"], "public, max-age=60");
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&body).unwrap(),
        serde_json::json!({ "names": { "bob": manifest.pubkey }, "relays": { manifest.pubkey.clone(): ["wss://r.example.com"] } })
    );

    // Variables reach every page as window.ENV, from a script loaded before
    // anything the site wrote.
    let (status, body) = gw.api("PUT", "/api/hosts/docs.example.com/env/API_URL", Some(serde_json::json!({ "value": "https://api.example.com" }))).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["variables"][0]["value"], "https://api.example.com");
    let (status, body) = gw.api("PUT", "/api/hosts/docs.example.com/env/2FAST", Some(serde_json::json!({ "value": "" }))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    eventually("the variables going live", async || gw.request("GET", "docs.example.com", "/_npanel/env.js", &[], None).await.0 == StatusCode::OK).await;
    let (_, headers, script) = gw.request("GET", "docs.example.com", "/_npanel/env.js?v=anything", &[], None).await;
    assert_eq!(headers["content-type"], "text/javascript; charset=utf-8");
    assert_eq!(headers["cache-control"], "public, max-age=0, stale-while-revalidate=31536000");
    let script = String::from_utf8(script).unwrap();
    assert!(script.contains("\nself.ENV = {\"API_URL\":\"https://api.example.com\"};\n"), "{script}");
    let (_, _, served) = gw.request("GET", "docs.example.com", "/", &[], None).await;
    let tag = format!(r#"<script src="{}"></script>"#, crate::inject::versioned_src(crate::site_env::ENV_PATH, script.as_bytes()));
    assert_eq!(String::from_utf8(served).unwrap(), page.replacen("<head>", &format!("<head>{tag}"), 1));

    // Removing the last one puts the pages back as published.
    let (status, body) = gw.api("DELETE", "/api/hosts/docs.example.com/env/API_URL", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["variables"], serde_json::json!([]));
    eventually("the variables going away", async || gw.request("GET", "docs.example.com", "/_npanel/env.js", &[], None).await.0 == StatusCode::NOT_FOUND).await;
    let (_, _, served) = gw.request("GET", "docs.example.com", "/", &[], None).await;
    assert_eq!(served, page.as_bytes());

    // A counting site's events arrive on its own origin, on the same listener
    // that serves it, and reach the analytics runtime.
    let (status, _) = gw.api("PATCH", "/api/hosts/docs.example.com", Some(serde_json::json!({ "analytics": true }))).await;
    assert_eq!(status, StatusCode::OK);
    let event = r#"{"n":"pageview","u":"https://docs.example.com/","d":"docs.example.com"}"#;
    eventually("the analytics runtime answering", async || {
        gw.request("POST", "docs.example.com", EVENT_PATH, &[], Some(event)).await.0 == StatusCode::ACCEPTED
    })
    .await;
    let (status, headers, _) = gw.request("OPTIONS", "docs.example.com", EVENT_PATH, &[("access-control-request-method", "POST")], None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(headers["access-control-allow-headers"], "content-type");
    let (status, visitors) = gw.api("GET", "/api/hosts/docs.example.com/stats/current-visitors", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(visitors, 1);

    // Disabling the host takes it off the air.
    let (status, _) = gw.api("PATCH", "/api/hosts/docs.example.com", Some(serde_json::json!({ "enabled": false }))).await;
    assert_eq!(status, StatusCode::OK);
    let (status, ..) = gw.request("GET", "docs.example.com", "/", &[], None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Every change was audited.
    let (_, audit) = gw.api("GET", "/api/audit-log", None).await;
    let actions: Vec<&str> = audit["entries"].as_array().unwrap().iter().map(|e| e["action"].as_str().unwrap()).collect();
    assert_eq!(actions, ["host.update", "host.update", "env.delete", "env.set", "nip05.set", "host.delete", "host.create", "host.create"]);

    gw.deployer.stop().await;
    drop(gw.db);
}

/// A large file goes out with the file it came from attached, for a
/// connection that can send it straight from disk; a small one doesn't.
#[tokio::test(flavor = "multi_thread")]
async fn a_large_file_is_offered_from_disk() {
    use hyper::ext::SendFile;

    let large: Vec<u8> = (0..100_000u32).map(|i| (i * 7 % 251) as u8).collect();
    let small = b"small".to_vec();
    let files: HashMap<String, Vec<u8>> = [(sha(&large), large.clone()), (sha(&small), small.clone())].into();
    let server = blossom(files).await;
    let manifest = Event::sign(
        AUTHOR,
        crate::util::now() - 60,
        15128,
        vec![vec!["path".into(), "/large.bin".into(), sha(&large)], vec!["path".into(), "/small.bin".into(), sha(&small)]],
        "",
    );
    let nostr = Arc::new(FakeNostr { manifests: Mutex::new(vec![manifest.clone()]), server });
    let gw = gateway(nostr).await;
    let npub = crate::nostr::nip19::npub_encode(&manifest.pubkey);
    let (status, _) = gw.api("POST", "/api/hosts", Some(serde_json::json!({ "hostname": "files.example.com", "address": npub }))).await;
    assert_eq!(status, StatusCode::CREATED);
    eventually("the site going live", async || gw.request("GET", "files.example.com", "/small.bin", &[], None).await.0 == StatusCode::OK).await;

    let send = async |method: &str, path: &str, headers: &[(&str, &str)]| {
        let mut builder = Request::builder().method(method).uri(path).header("host", "files.example.com");
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        let response = gw.app.handle(builder.body(Body::empty()).unwrap(), crate::app::Via::Proxy([127, 0, 0, 1].into())).await;
        let file = response.extensions().get::<SendFile>().map(|file| (file.offset(), file.len()));
        (file, response.into_body().collect().await.unwrap().to_bytes())
    };

    // The body is still the file's bytes, for a connection that can't.
    let (file, body) = send("GET", "/large.bin", &[]).await;
    assert_eq!(file, cfg!(target_os = "linux").then_some((0, 100_000)));
    assert_eq!(body, large);
    let (file, body) = send("GET", "/large.bin", &[("range", "bytes=1000-1999")]).await;
    assert_eq!(file, cfg!(target_os = "linux").then_some((1000, 1000)));
    assert_eq!(body, large[1000..2000]);

    assert_eq!(send("HEAD", "/large.bin", &[]).await.0, None);
    assert_eq!(send("GET", "/small.bin", &[]).await, (None, small.into()));

    gw.deployer.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_missing_blob_fails_the_deploy_and_says_why() {
    let manifest = Event::sign(AUTHOR, crate::util::now() - 60, 15128, vec![vec!["path".into(), "/index.html".into(), "e".repeat(64)]], "");
    let server = blossom(HashMap::new()).await;
    let nostr = Arc::new(FakeNostr { manifests: Mutex::new(vec![manifest.clone()]), server });
    let gw = gateway(nostr).await;
    let (status, _) = gw.api("PUT", "/api/settings", Some(serde_json::json!({ "deploy_attempts": 1 }))).await;
    assert_eq!(status, StatusCode::OK);

    let npub = crate::nostr::nip19::npub_encode(&manifest.pubkey);
    let (status, _) = gw.api("POST", "/api/hosts", Some(serde_json::json!({ "hostname": "broken.example.com", "address": npub }))).await;
    assert_eq!(status, StatusCode::CREATED);

    eventually("the deploy failing", async || {
        gw.api("GET", "/api/hosts/broken.example.com", None).await.1["host"]["latest"]["status"] == "failed"
    })
    .await;

    // Nothing ready means nothing to serve, and a 503 rather than a broken page.
    let (status, headers, _) = gw.request("GET", "broken.example.com", "/", &[], None).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(headers["retry-after"], "30");

    let (_, log) = gw.api("GET", &format!("/api/hosts/broken.example.com/versions/{}/log", manifest.id), None).await;
    let messages: Vec<&str> = log["entries"].as_array().unwrap().iter().map(|e| e["msg"].as_str().unwrap()).collect();
    assert!(messages.contains(&"blob_download_failed"), "{messages:?}");
    assert!(messages.contains(&"blob_unavailable"), "{messages:?}");
    assert_eq!(messages.last(), Some(&"deploy_failed"));
    assert_eq!(log["entries"][messages.iter().position(|m| *m == "blob_download_failed").unwrap()]["fields"]["status"], 404);

    gw.deployer.stop().await;
}

/// What the dashboard published as an nsite does: talks to the API from an
/// origin that isn't the dashboard's, signing for the API's own URL.
#[tokio::test(flavor = "multi_thread")]
async fn a_dashboard_on_another_origin_can_use_the_api() {
    let nostr = Arc::new(FakeNostr { manifests: Mutex::new(vec![]), server: blossom(HashMap::new()).await });
    let gw = gateway(nostr).await;
    let origin = ("origin", "https://npanel.nsite.example");

    let (status, headers, body) = gw.request("GET", "panel.example.com", "/api/instance", &[origin], None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["access-control-allow-origin"], "*");
    assert_eq!(serde_json::from_slice::<serde_json::Value>(&body).unwrap()["version"], env!("CARGO_PKG_VERSION"));

    let preflight = [origin, ("access-control-request-method", "GET"), ("access-control-request-headers", "authorization")];
    let (status, headers, _) = gw.request("OPTIONS", "panel.example.com", "/api/me", &preflight, None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(headers["access-control-allow-origin"], "*");
    assert_eq!(headers["access-control-allow-headers"], "authorization");

    let auth = sign_nip98(ADMIN, "GET", "https://panel.example.com/api/me", None, crate::util::now());
    let (status, headers, _) = gw.request("GET", "panel.example.com", "/api/me", &[origin, ("authorization", &auth)], None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["access-control-allow-origin"], "*");

    gw.deployer.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_deploy_is_retried_with_backoff_then_left_to_the_next_sync() {
    let manifest = Event::sign(AUTHOR, crate::util::now() - 60, 15128, vec![vec!["path".into(), "/index.html".into(), "f".repeat(64)]], "");
    let server = blossom(HashMap::new()).await;
    let nostr = Arc::new(FakeNostr { manifests: Mutex::new(vec![manifest.clone()]), server });
    let gw = gateway(nostr).await;

    let npub = crate::nostr::nip19::npub_encode(&manifest.pubkey);
    let (status, _) = gw.api("POST", "/api/hosts", Some(serde_json::json!({ "hostname": "flaky.example.com", "address": npub }))).await;
    assert_eq!(status, StatusCode::CREATED);

    // Three attempts by default, the last of which gives up for now.
    eventually("the attempts running out", async || {
        gw.api("GET", "/api/hosts/flaky.example.com", None).await.1["host"]["latest"]["error"]
            .as_str()
            .is_some_and(|error| error.contains("Attempt 3 of 3. The next sync will try again."))
    })
    .await;

    // A resync that finds the version unfinished starts a fresh run of
    // attempts, and the one at startup can overlap the one adding the host
    // asks for, so how many runs there were is a matter of timing. Each run is
    // retried with backoff, and the last one gives up.
    let (_, log) = gw.api("GET", &format!("/api/hosts/flaky.example.com/versions/{}/log", manifest.id), None).await;
    let messages: Vec<&str> = log["entries"].as_array().unwrap().iter().map(|e| e["msg"].as_str().unwrap()).collect();
    assert!(messages.iter().filter(|m| **m == "deploy_retrying").count() >= 2, "{messages:?}");
    assert_eq!(messages.last(), Some(&"deploy_failed"), "{messages:?}");

    gw.deployer.stop().await;
}

/// Onion traffic never touches a socket: Tor hands npanel a stream, which is
/// served as the onion it arrived at and as nothing else.
#[tokio::test(flavor = "multi_thread")]
async fn a_site_is_served_at_its_onion_and_only_there() {
    let page = "<!doctype html><html><head><title>Onion</title></head><body>hi</body></html>";
    let server = blossom([(sha(page.as_bytes()), page.as_bytes().to_vec())].into()).await;
    let manifest = Event::sign(AUTHOR, crate::util::now() - 60, 15128, vec![vec!["path".into(), "/index.html".into(), sha(page.as_bytes())]], "");
    let nostr = Arc::new(FakeNostr { manifests: Mutex::new(vec![manifest.clone()]), server });
    let gw = gateway(nostr).await;
    let npub = crate::nostr::nip19::npub_encode(&manifest.pubkey);
    let (status, _) = gw.api("POST", "/api/hosts", Some(serde_json::json!({ "hostname": "docs.example.com", "address": npub }))).await;
    assert_eq!(status, StatusCode::CREATED);
    eventually("the site going live", async || gw.request("GET", "docs.example.com", "/", &[], None).await.0 == StatusCode::OK).await;

    // What the Tor runtime does when it mints an address.
    let key = crate::tor::OnionKey::generate();
    let (address, secret) = (key.address.clone(), key.secret_key.clone());
    let id: i64 = gw
        .db
        .with(move |conn| {
            let id = conn.query_row("SELECT id FROM hosts WHERE hostname = 'docs.example.com'", [], |r| r.get(0))?;
            conn.execute("INSERT INTO onion_services (host_id, address, secret_key, created_at) VALUES (?, ?, ?, 1)", rusqlite::params![id, address, secret])?;
            Ok(id)
        })
        .unwrap();
    gw.deployer.rebuild(vec![id]);

    let over_tor = async |arrived_at: &str, host: &str| -> String {
        let (mut client, stream) = tokio::io::duplex(64 * 1024);
        let connections = crate::server::Connections::new();
        connections.serve(stream, crate::tor::onion_handler(gw.app.site.clone(), arrived_at.to_string(), Logger::new(LogLevel::Error)));
        let request = format!("GET / HTTP/1.1\r\nhost: {host}\r\nconnection: close\r\n\r\n");
        tokio::io::AsyncWriteExt::write_all(&mut client, request.as_bytes()).await.unwrap();
        let mut response = String::new();
        tokio::io::AsyncReadExt::read_to_string(&mut client, &mut response).await.unwrap();
        response
    };

    eventually("the onion being served", async || over_tor(&key.address, &key.address).await.starts_with("HTTP/1.1 200")).await;
    let response = over_tor(&key.address, &format!("{}:80", key.address)).await;
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert!(response.ends_with(page), "{response}");
    assert!(response.to_lowercase().contains("content-type: text/html"), "{response}");

    // A visitor writes the Host header, but can't make it name anything else.
    for host in ["panel.example.com", "docs.example.com", &crate::tor::OnionKey::generate().address] {
        let response = over_tor(&key.address, host).await;
        assert!(response.starts_with("HTTP/1.1 404"), "{host}: {response}");
    }

    gw.deployer.stop().await;
}

/// A site with link previews on that publishes a preview script gets
/// previews of its own design, with images it draws, from exactly the events
/// it asks for; everyone else, and every page it has nothing for, gets the
/// page as published.
#[tokio::test(flavor = "multi_thread")]
async fn a_sites_script_draws_its_previews() {
    let page = r#"<!doctype html><html><head><title>Ditto — Social</title><meta property="og:title" content="Ditto"></head><body><div id="root"></div></body></html>"#;
    let author = crate::nostr::event::public_key(AUTHOR);
    let npub = crate::nostr::nip19::npub_encode(&author);
    let routes = r#"["/:id(n(?:pub|ote)1[02-9ac-hj-np-z]+){/}?"]"#;
    let script = format!(
        r##"const background = (theme) => theme?.tags.find(([n, , role]) => n === "c" && role === "background")?.[1] ?? "#14161f";

export default {{
  routes: {routes},
  async preview(request, {{ nostr }}) {{
    if (new URL(request.url).pathname !== "/{npub}") return null;
    const [profile] = await nostr.query([{{ kinds: [0], authors: ["{author}"], limit: 1 }}]);
    if (!profile) return null;
    const [theme] = await nostr.query([{{ kinds: [16767], authors: ["{author}"], limit: 1 }}]);
    const name = JSON.parse(profile.content).name;
    const title = theme?.tags.find(([n]) => n === "title")?.[1] ?? "none";
    return {{
      title: `${{name}} on Ditto`,
      description: `Theme: ${{title}}`,
      body: `<article><p onclick="steal()">${{name}}</p></article>`,
      type: "profile",
      jsonLd: {{ "@type": "ProfilePage", name: `</script>${{name}}` }},
      image: draw(theme),
    }};
  }},
}};

async function draw(theme) {{
  const canvas = new OffscreenCanvas(1200, 630);
  const ctx = canvas.getContext("2d");
  ctx.fillStyle = background(theme);
  ctx.fillRect(0, 0, 1200, 630);
  return canvas.convertToBlob({{ type: "image/jpeg" }});
}}
"##
    );
    let files = [("/index.html", page.as_bytes().to_vec()), ("/.well-known/npanel/preview.js", script.clone().into_bytes())];
    let server = blossom(files.iter().map(|(_, bytes)| (sha(bytes), bytes.clone())).collect()).await;
    let tags = files.iter().map(|(path, bytes)| vec!["path".into(), path.to_string(), sha(bytes)]).collect();
    let manifest = Event::sign(AUTHOR, crate::util::now() - 60, 15128, tags, "");
    let nostr = Arc::new(FakeNostr { manifests: Mutex::new(vec![manifest.clone()]), server });
    let gw = gateway(nostr).await;
    let host = serde_json::json!({ "hostname": "social.example.com", "address": npub, "spa": true, "link_previews": true });
    let (status, body) = gw.api("POST", "/api/hosts", Some(host)).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["host"]["link_previews"], true);
    eventually("the site going live", async || gw.request("GET", "social.example.com", "/", &[], None).await.0 == StatusCode::OK).await;

    // The script is a file like any other, for anyone to read.
    let (status, _, body) = gw.request("GET", "social.example.com", "/.well-known/npanel/preview.js", &[], None).await;
    assert_eq!((status, body), (StatusCode::OK, script.into_bytes()));

    // The deploy log says the script loads.
    let log_path = format!("/api/hosts/social.example.com/versions/{}/log", manifest.id);
    eventually("the script being checked", async || {
        let (_, log) = gw.api("GET", &log_path, None).await;
        log["entries"].as_array().is_some_and(|entries| entries.iter().any(|e| e["msg"] == "preview_script_found"))
    })
    .await;

    let theme = Event::sign(
        AUTHOR,
        crate::util::now() - 30,
        16767,
        vec![vec!["title".into(), "Midnight".into()], vec!["c".into(), "#000000".into(), "background".into()]],
        "",
    );
    let profile = Event::sign(AUTHOR, crate::util::now() - 30, 0, vec![], r#"{"name":"Author"}"#);
    gw.events.events.lock().extend([theme, profile]);
    let bot = ("user-agent", "Mozilla/5.0 (compatible; Googlebot/2.1; +http://www.google.com/bot.html)");
    let browser = ("user-agent", "Mozilla/5.0 (X11; Linux x86_64; rv:128.0) Gecko/20100101 Firefox/128.0");
    let varies = |headers: &http::HeaderMap| headers.get_all("vary").iter().any(|v| v == "user-agent");
    let path = format!("/{npub}");

    // A browser gets the app, told that crawlers get something else.
    let (status, headers, body) = gw.request("GET", "social.example.com", &path, &[browser], None).await;
    assert_eq!((status, body.as_slice()), (StatusCode::OK, page.as_bytes()));
    assert!(varies(&headers));

    let (status, headers, body) = gw.request("GET", "social.example.com", &path, &[bot], None).await;
    let body = String::from_utf8(body).unwrap();
    assert_eq!(status, StatusCode::OK);
    assert!(varies(&headers));
    assert_eq!(headers["content-type"], "text/html; charset=utf-8");
    assert_eq!(headers["content-security-policy"], crate::site_csp::GATEWAY_CSP);
    if !body.contains("<title>Author on Ditto</title>") {
        // Which way the page came to be sent as published.
        let (_, previews) = gw.api("GET", "/api/link-previews", None).await;
        panic!("No preview: {previews}\n{body}");
    }
    assert!(body.contains(r#"<meta property="og:type" content="profile">"#), "{body}");
    assert!(body.contains(r#"<meta property="og:description" content="Theme: Midnight">"#), "{body}");
    assert!(body.contains(r#"<meta name="twitter:card" content="summary_large_image">"#), "{body}");
    assert!(body.contains(&format!(r#"<link rel="canonical" href="https://social.example.com{path}">"#)), "{body}");
    assert!(!body.contains(r#"<meta property="og:title" content="Ditto">"#), "{body}");
    // What the script writes is cleaned of anything that runs, and can't
    // close the element it's in.
    assert!(body.contains("<article><p>Author</p></article>") && !body.contains("steal()"), "{body}");
    assert!(body.contains("\\u003c/script\\u003eAuthor"), "{body}");
    let image = body.split(r#"<meta property="og:image" content=""#).nth(1).unwrap().split('"').next().unwrap();
    let image_path = image.strip_prefix("https://social.example.com").unwrap().to_string();
    assert!(image_path.starts_with(&format!("/_npanel/preview/{npub}?v=")), "{image}");

    let etag = headers["etag"].to_str().unwrap().to_string();
    let (status, ..) = gw.request("GET", "social.example.com", &path, &[bot, ("if-none-match", &etag)], None).await;
    assert_eq!(status, StatusCode::NOT_MODIFIED);

    // The image is for anyone, and lasts as long as its version does.
    let (status, headers, jpeg) = gw.request("GET", "social.example.com", &image_path, &[], None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["content-type"], "image/jpeg");
    assert_eq!(headers["cache-control"], "public, max-age=604800, immutable");
    assert_eq!(imagesize::blob_size(&jpeg).unwrap(), imagesize::ImageSize { width: 1200, height: 630 });
    let etag = headers["etag"].to_str().unwrap().to_string();
    let (status, ..) = gw.request("GET", "social.example.com", &image_path, &[("if-none-match", &etag)], None).await;
    assert_eq!(status, StatusCode::NOT_MODIFIED);
    // Asked for at no version, it's the current one, for now.
    let (status, headers, _) = gw.request("GET", "social.example.com", &format!("/_npanel/preview/{npub}"), &[bot], None).await;
    assert_eq!((status, headers["cache-control"].to_str().unwrap()), (StatusCode::OK, "public, max-age=300"));

    // A page the script's routes don't name is the page as published, for
    // anyone, and has no image.
    let (_, headers, body) = gw.request("GET", "social.example.com", "/about", &[bot], None).await;
    assert_eq!(body, page.as_bytes());
    assert!(!varies(&headers));
    assert_eq!(gw.request("GET", "social.example.com", "/_npanel/preview/about", &[], None).await.0, StatusCode::NOT_FOUND);

    // So is a page the script has nothing for, and it has no image.
    let note = format!("/{}", crate::nostr::nip19::note_encode(&"c".repeat(64)));
    let (status, headers, body) = gw.request("GET", "social.example.com", &note, &[bot], None).await;
    assert_eq!((status, body.as_slice()), (StatusCode::OK, page.as_bytes()));
    assert!(varies(&headers));
    assert_eq!(gw.request("GET", "social.example.com", &format!("/_npanel/preview{note}"), &[], None).await.0, StatusCode::NOT_FOUND);

    let (status, previews) = gw.api("GET", "/api/link-previews", None).await;
    assert_eq!(status, StatusCode::OK, "{previews}");
    assert_eq!((previews["found"].as_i64(), previews["empty"].as_i64()), (Some(1), Some(1)), "{previews}");
    assert!(previews["bytes"].as_u64().unwrap() > 0);
    assert!(previews["stats"]["hits"].as_u64().unwrap() >= 1, "{previews}");

    // An admin can run a page's script again, by its URL.
    let renamed = Event::sign(AUTHOR, crate::util::now() - 20, 0, vec![], r#"{"name":"Renamed"}"#);
    gw.events.events.lock().push(renamed);
    let refresh = |url: &str| serde_json::json!({ "url": url });
    let (status, refreshed) = gw.api("POST", "/api/link-previews/refresh", Some(refresh(&format!("https://social.example.com{path}/")))).await;
    assert_eq!(status, StatusCode::OK, "{refreshed}");
    assert_eq!(refreshed["url"], format!("https://social.example.com{path}"));
    assert_eq!(refreshed["preview"]["title"], "Renamed on Ditto");
    let (_, _, body) = gw.request("GET", "social.example.com", &path, &[bot], None).await;
    assert!(String::from_utf8(body).unwrap().contains("<title>Renamed on Ditto</title>"));
    let (status, refreshed) = gw.api("POST", "/api/link-previews/refresh", Some(refresh(&format!("https://social.example.com{note}")))).await;
    assert_eq!((status, refreshed["preview"].is_null()), (StatusCode::OK, true), "{refreshed}");
    for (url, error) in [
        ("nonsense", "Paste the URL"),
        ("https://social.example.com/about", "routes"),
        (&format!("https://elsewhere.example.com{path}"), "No site"),
    ] {
        let (status, refused) = gw.api("POST", "/api/link-previews/refresh", Some(refresh(url))).await;
        assert!(status.is_client_error() && refused["error"].as_str().unwrap().contains(error), "{url}: {refused}");
    }

    let (status, cleared) = gw.api("DELETE", "/api/link-previews", None).await;
    assert_eq!((status, cleared["cleared"].as_u64()), (StatusCode::OK, Some(2)), "{cleared}");
    let (_, previews) = gw.api("GET", "/api/link-previews", None).await;
    assert_eq!((previews["found"].as_i64(), previews["empty"].as_i64()), (Some(0), Some(0)), "{previews}");

    // Relays that never answer cost a crawler a short wait and the page as
    // published, and a run past the limit costs it no wait at all.
    let (status, _) = gw.api("PUT", "/api/settings", Some(serde_json::json!({ "link_preview_concurrency": 1 }))).await;
    assert_eq!(status, StatusCode::OK);
    gw.events.hang.store(true, std::sync::atomic::Ordering::SeqCst);
    let (status, _, body) = gw.request("GET", "social.example.com", &path, &[bot], None).await;
    assert_eq!((status, body.as_slice()), (StatusCode::OK, page.as_bytes()));
    let started = std::time::Instant::now();
    let other = format!("/{}", crate::nostr::nip19::note_encode(&"d".repeat(64)));
    let (status, _, body) = gw.request("GET", "social.example.com", &other, &[bot], None).await;
    assert_eq!((status, body.as_slice()), (StatusCode::OK, page.as_bytes()));
    assert!(started.elapsed() < Duration::from_secs(1));
    let (_, previews) = gw.api("GET", "/api/link-previews", None).await;
    assert_eq!((previews["stats"]["timeouts"].as_u64(), previews["stats"]["busy"].as_u64()), (Some(1), Some(1)), "{previews}");
    gw.events.hang.store(false, std::sync::atomic::Ordering::SeqCst);

    // A site with previews off is the page as published, crawler or not, and
    // has no images.
    let (status, _) = gw.api("PATCH", "/api/hosts/social.example.com", Some(serde_json::json!({ "link_previews": false }))).await;
    assert_eq!(status, StatusCode::OK);
    let (_, headers, body) = gw.request("GET", "social.example.com", &path, &[bot], None).await;
    assert_eq!(body, page.as_bytes());
    assert!(!varies(&headers));
    assert_eq!(gw.request("GET", "social.example.com", &image_path, &[], None).await.0, StatusCode::NOT_FOUND);

    let (_, audit) = gw.api("GET", "/api/audit-log", None).await;
    let actions: Vec<&str> = audit["entries"].as_array().unwrap().iter().map(|e| e["action"].as_str().unwrap()).collect();
    assert!(actions.contains(&"link_preview.refresh") && actions.contains(&"link_preview.clear"), "{actions:?}");

    gw.deployer.stop().await;
}

/// A site's script isn't run, not even to check it, until staff turn link
/// previews on: it is the author's code.
#[tokio::test(flavor = "multi_thread")]
async fn a_script_is_left_alone_with_previews_off() {
    let page = r#"<!doctype html><html><head><title>Docs</title></head><body></body></html>"#;
    let script = "export default { preview() { return null; } };";
    let files = [("/index.html", page.as_bytes().to_vec()), ("/.well-known/npanel/preview.js", script.as_bytes().to_vec())];
    let server = blossom(files.iter().map(|(_, bytes)| (sha(bytes), bytes.clone())).collect()).await;
    let tags = files.iter().map(|(path, bytes)| vec!["path".into(), path.to_string(), sha(bytes)]).collect();
    let manifest = Event::sign(AUTHOR, crate::util::now() - 60, 15128, tags, "");
    let nostr = Arc::new(FakeNostr { manifests: Mutex::new(vec![manifest.clone()]), server });
    let gw = gateway(nostr).await;
    let npub = crate::nostr::nip19::npub_encode(&manifest.pubkey);
    let host = serde_json::json!({ "hostname": "docs.example.com", "address": npub });
    assert_eq!(gw.api("POST", "/api/hosts", Some(host)).await.0, StatusCode::CREATED);

    let log_path = format!("/api/hosts/docs.example.com/versions/{}/log", manifest.id);
    let said = async |msg: &str| {
        let (_, log) = gw.api("GET", &log_path, None).await;
        log["entries"].as_array().is_some_and(|entries| entries.iter().any(|e| e["msg"] == msg))
    };
    eventually("the script being noticed", async || said("preview_script_unused").await).await;
    assert!(!said("preview_script_found").await);

    gw.deployer.stop().await;
}

/// A site without a script gets no previews, link previews on or not.
#[tokio::test(flavor = "multi_thread")]
async fn a_site_without_a_script_has_no_previews() {
    let page = r#"<!doctype html><html><head><title>Docs</title></head><body></body></html>"#;
    let server = blossom([(sha(page.as_bytes()), page.as_bytes().to_vec())].into()).await;
    let manifest = Event::sign(AUTHOR, crate::util::now() - 60, 15128, vec![vec!["path".into(), "/index.html".into(), sha(page.as_bytes())]], "");
    let nostr = Arc::new(FakeNostr { manifests: Mutex::new(vec![manifest.clone()]), server });
    let gw = gateway(nostr).await;
    let npub = crate::nostr::nip19::npub_encode(&manifest.pubkey);
    let host = serde_json::json!({ "hostname": "docs.example.com", "address": npub, "spa": true, "link_previews": true });
    assert_eq!(gw.api("POST", "/api/hosts", Some(host)).await.0, StatusCode::CREATED);
    eventually("the site going live", async || gw.request("GET", "docs.example.com", "/", &[], None).await.0 == StatusCode::OK).await;

    let bot = ("user-agent", "Twitterbot/1.0");
    let (status, headers, body) = gw.request("GET", "docs.example.com", &format!("/{npub}"), &[bot], None).await;
    assert_eq!((status, body.as_slice()), (StatusCode::OK, page.as_bytes()));
    assert!(!headers.get_all("vary").iter().any(|v| v == "user-agent"));
    eventually("link previews starting", async || gw.api("GET", "/api/link-previews", None).await.0 == StatusCode::OK).await;
    let (_, previews) = gw.api("GET", "/api/link-previews", None).await;
    assert_eq!(previews["stats"]["lookups"], 0, "{previews}");
    let (status, refused) = gw.api("POST", "/api/link-previews/refresh", Some(serde_json::json!({ "url": format!("https://docs.example.com/{npub}") }))).await;
    assert!(status.is_client_error() && refused["error"].as_str().unwrap().contains("preview.js"), "{refused}");

    gw.deployer.stop().await;
}

/// A reporting site's pages load the reporter, and what they throw reaches
/// the dashboard as an issue, mapped through the site's own source map.
#[tokio::test(flavor = "multi_thread")]
async fn a_reporting_site_reports_its_errors() {
    use crate::errors::reporter::{reporter_script, REPORTER_PATH, REPORT_PATH};
    use crate::errors::symbolicate::tests::{map, MINIFIED};

    let page = r#"<!doctype html><html><head><title>Docs</title><script src="/assets/app.js"></script></head><body></body></html>"#;
    let map = map();
    let blobs: HashMap<String, Vec<u8>> = [page.as_bytes(), MINIFIED.as_bytes(), map.as_bytes()].iter().map(|b| (sha(b), b.to_vec())).collect();
    let server = blossom(blobs).await;
    let manifest = Event::sign(
        AUTHOR,
        crate::util::now() - 60,
        15128,
        vec![
            vec!["path".into(), "/index.html".into(), sha(page.as_bytes())],
            vec!["path".into(), "/assets/app.js".into(), sha(MINIFIED.as_bytes())],
            vec!["path".into(), "/assets/app.js.map".into(), sha(map.as_bytes())],
        ],
        "",
    );
    let nostr = Arc::new(FakeNostr { manifests: Mutex::new(vec![manifest.clone()]), server });
    let gw = gateway(nostr).await;
    let npub = crate::nostr::nip19::npub_encode(&manifest.pubkey);
    let host = serde_json::json!({ "hostname": "docs.example.com", "address": npub, "errors": true });
    let (status, body) = gw.api("POST", "/api/hosts", Some(host)).await;
    assert_eq!((status, body["host"]["errors"].as_bool()), (StatusCode::CREATED, Some(true)));

    // The reporter loads first thing, before the site's own script.
    eventually("the reporter going live", async || gw.request("GET", "docs.example.com", REPORTER_PATH, &[], None).await.0 == StatusCode::OK).await;
    let (_, _, served) = gw.request("GET", "docs.example.com", "/", &[], None).await;
    let src = crate::inject::versioned_src(REPORTER_PATH, reporter_script(&manifest.id).as_bytes());
    assert_eq!(String::from_utf8(served).unwrap(), page.replacen("<head>", &format!(r#"<head><script src="{src}"></script>"#), 1));

    // What the reporter sends for `r.user.name` throwing in the minified file.
    let event = serde_json::json!({
        "event_id": "a".repeat(32),
        "release": manifest.id,
        "request": { "url": "https://docs.example.com/" },
        "exception": { "values": [{
            "type": "TypeError",
            "value": "Cannot read properties of undefined (reading 'name')",
            "stacktrace": { "frames": [
                { "abs_path": "https://docs.example.com/assets/app.js", "lineno": 1, "colno": 34 },
                { "abs_path": "https://docs.example.com/assets/app.js", "function": "n", "lineno": 1, "colno": 22 },
            ] },
        }] },
    });
    let envelope = format!("{{}}\n{{\"type\":\"event\"}}\n{event}\n");
    eventually("the errors runtime answering", async || {
        gw.request("POST", "docs.example.com", REPORT_PATH, &[("user-agent", "Mozilla/5.0 (X11; Linux x86_64; rv:130.0) Gecko/20100101 Firefox/130.0")], Some(&envelope)).await.0
            == StatusCode::OK
    })
    .await;
    // A site that keeps no reports reads none, and says it took them.
    let (status, _, body) = gw.request("POST", "nobody.example.com", REPORT_PATH, &[], Some("not an envelope")).await;
    assert_eq!((status, body.as_slice()), (StatusCode::OK, &b"{}"[..]));
    // A Sentry SDK's DSN path reaches the same place.
    let (status, ..) = gw.request("POST", "docs.example.com", "/_npanel/api/1/envelope/", &[], Some(&envelope)).await;
    assert_eq!(status, StatusCode::OK);

    let (status, list) = gw.api("GET", "/api/hosts/docs.example.com/issues", None).await;
    assert_eq!(status, StatusCode::OK, "{list}");
    let issue = &list["issues"][0];
    assert_eq!(issue["title"], "TypeError: Cannot read properties of undefined (reading 'name')");
    assert_eq!(issue["culprit"], "render (src/App.js)");
    assert_eq!((issue["events"].as_i64(), issue["visitors"].as_i64()), (Some(2), Some(2)));
    let id = issue["id"].as_i64().unwrap();

    let (_, one) = gw.api("GET", &format!("/api/hosts/docs.example.com/issues/{id}"), None).await;
    let latest = one["issue"]["latest_event"].as_i64().unwrap();
    let (status, event) = gw.api("GET", &format!("/api/hosts/docs.example.com/issues/{id}/events/{latest}"), None).await;
    assert_eq!(status, StatusCode::OK, "{event}");
    assert_eq!(event["event"]["report"]["exceptions"][0]["frames"][1]["mapped"]["lineno"], 2);

    let (status, resolved) = gw.api("PATCH", &format!("/api/hosts/docs.example.com/issues/{id}"), Some(serde_json::json!({ "status": "resolved" }))).await;
    assert_eq!((status, resolved["issue"]["status"].as_str()), (StatusCode::OK, Some("resolved")));
    let (_, summary) = gw.api("GET", "/api/errors/summary", None).await;
    assert_eq!(summary["docs.example.com"], serde_json::json!({ "unresolved": 0, "events_24h": 2 }));

    gw.deployer.stop().await;
}
