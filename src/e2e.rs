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
use crate::auth::sign_nip98;
use crate::blobs::BlobStore;
use crate::db::Db;
use crate::deploy::{Deployer, DeployerDeps, Deployments};
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
    let site_csp = Arc::new(crate::site_csp::SiteCsp::new(db.clone(), blobs.clone(), log));
    let site = Arc::new(SiteServer::new(SiteServerDeps {
        db: db.clone(),
        blobs: blobs.clone(),
        site_csp: site_csp.clone(),
        serve_cache: serve_cache.clone(),
        analytics: analytics.clone(),
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
        dns: crate::dns::DnsTargets::new("panel.example.com".into(), settings.clone(), log),
        log,
        tor,
        tls: None,
        site_csp,
        previews: Arc::new(crate::site_preview::SitePreviews::new(blobs.clone())),
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
    Gateway { app, db, deployer, _dir: dir }
}

fn sha(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// Wait for a condition the deploy runtime will bring about.
async fn eventually(what: &str, mut check: impl AsyncFnMut() -> bool) {
    for _ in 0..200 {
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
