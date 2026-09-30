//! The dashboard's JSON API, served only on `DASHBOARD_HOST`.
//!
//! Every route except `/api/instance` and `/api/sites` requires NIP-98 auth
//! from a pubkey that exists in the `users` table. The dashboard is a client
//! of this API and uses nothing private, so anything the UI can do, a script
//! can do.

use std::collections::HashMap;
use std::sync::Arc;

use axum::body::{Body, Bytes};
use axum::extract::{Path, Query, Request, State};
use axum::middleware::Next;
use axum::routing::{get, post, put};
use axum::{Extension, Router};
use http::{header, HeaderValue, Method, Response, StatusCode};
use http_body_util::BodyExt;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Map, Value};

use crate::analytics::query::engine::QueryHost;
use crate::analytics::{Analytics, WorkerResponse};
use crate::auth::verify_nip98;
use crate::builds::current_build;
use crate::db::{status, BuildKind, Db, Host, HostBuild, SiteEvent, HOST_COLUMNS, SITE_EVENT_COLUMNS};
use crate::deploy::Deployments;
use crate::dns::DnsTargets;
use crate::fields;
use crate::hostname::parse_hostname;
use crate::identifiers::{parse_nip05_name, parse_pubkey, parse_relay_list, parse_relay_url};
use crate::log::Logger;
use crate::nostr::nip19;
use crate::nsite_address::{encode_naddr, format_nsite_address, parse_nsite_address};
use crate::serve::ServeCache;
use crate::settings::{Settings, SettingsValues, KEYS};
use crate::site_csp::{SiteCsp, GATEWAY_CSP};
use crate::site_preview::{SitePreview, SitePreviews};
use crate::sites::{load_host, newest_event, site_event, site_key};
use crate::tor::TorService;

/// Largest request body the API accepts. Bodies are read before the caller is
/// known — NIP-98 hashes the payload — so anyone able to sign an event could
/// otherwise make npanel buffer an arbitrarily large request.
const MAX_BODY_SIZE: usize = 1024 * 1024;

/// How many relays one NIP-05 name may carry.
const MAX_NIP05_RELAYS: usize = 10;

pub struct ApiDeps {
    pub db: Db,
    pub settings: Arc<Settings>,
    pub deployments: Arc<dyn Deployments>,
    pub analytics: Arc<Analytics>,
    pub dns: DnsTargets,
    pub log: Logger,
    pub tor: Arc<TorService>,
    pub site_csp: Arc<SiteCsp>,
    pub previews: Arc<SitePreviews>,
    pub serve_cache: Arc<ServeCache>,
    pub dashboard_host: String,
}

type Deps = Arc<ApiDeps>;
type Reply = Response<Body>;

#[derive(Clone, Debug)]
pub struct AuthUser {
    pub pubkey: String,
    pub role: String,
}

impl AuthUser {
    fn is_admin(&self) -> bool {
        self.role == "admin"
    }
}

/// The request body, read once by the middleware that had to hash it.
#[derive(Clone)]
struct RawBody(Bytes);

fn json_reply(status: StatusCode, value: Value) -> Reply {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(value.to_string()))
        .expect("response")
}

fn ok(value: Value) -> Reply {
    json_reply(StatusCode::OK, value)
}

fn error(status: StatusCode, message: impl AsRef<str>) -> Reply {
    json_reply(status, json!({ "error": message.as_ref() }))
}

fn internal(log: &Logger, path: &str, err: impl std::fmt::Display) -> Reply {
    log.error("request_failed", fields! { "path" => path, "err_msg" => err.to_string() });
    error(StatusCode::INTERNAL_SERVER_ERROR, "Internal server error.")
}

/// Everything the API answers, with its middleware.
pub fn router(deps: Deps) -> Router {
    Router::new()
        .route("/api/instance", get(instance))
        .route("/api/sites", get(sites))
        .route("/api/me", get(me))
        .route("/api/dns", get(dns))
        .route("/api/hosts", get(list_hosts).post(create_host))
        .route("/api/hosts/{hostname}", get(get_host).patch(update_host).delete(delete_host))
        .route("/api/hosts/{hostname}/versions", get(versions))
        .route("/api/hosts/{hostname}/versions/{event_id}/log", get(version_log))
        .route("/api/hosts/{hostname}/files", get(files))
        .route("/api/hosts/{hostname}/sync", post(sync))
        .route("/api/hosts/{hostname}/grants", get(get_grants).put(put_grants))
        .route("/api/hosts/{hostname}/nip05", get(list_nip05))
        .route("/api/hosts/{hostname}/nip05/{name}", put(put_nip05).delete(delete_nip05))
        .route("/api/hosts/{hostname}/stats/query", post(stats_query))
        .route("/api/hosts/{hostname}/stats/current-visitors", get(stats_current_visitors))
        .route("/api/hosts/{hostname}/stats/suggestions/{filter}", get(stats_suggestions))
        .route("/api/stats/sparklines", get(sparklines))
        .route("/api/users", get(list_users).post(create_user))
        .route("/api/users/{pubkey}", axum::routing::patch(update_user).delete(delete_user))
        .route("/api/settings", get(get_settings).put(put_settings))
        .route("/api/audit-log", get(audit_log))
        .route("/api/status", get(status_route))
        // Unmatched API paths must not fall through to the SPA: an API client
        // asking for a route that doesn't exist should get JSON.
        .fallback(|| async { error(StatusCode::NOT_FOUND, "No such API route.") })
        .layer(axum::middleware::from_fn_with_state(deps.clone(), gate))
        .with_state(deps)
}

/// CORS, the body limit, and NIP-98, in front of every route.
async fn gate(State(deps): State<Deps>, request: Request, next: Next) -> Reply {
    let (mut parts, body) = request.into_parts();

    if parts.method == Method::OPTIONS && parts.headers.contains_key(header::ACCESS_CONTROL_REQUEST_METHOD) {
        let mut builder = Response::builder()
            .status(StatusCode::NO_CONTENT)
            .header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
            .header(header::ACCESS_CONTROL_ALLOW_METHODS, "GET,HEAD,PUT,POST,DELETE,PATCH");
        if let Some(requested) = parts.headers.get(header::ACCESS_CONTROL_REQUEST_HEADERS) {
            builder = builder.header(header::ACCESS_CONTROL_ALLOW_HEADERS, requested.clone()).header(header::VARY, "Access-Control-Request-Headers");
        }
        return builder.body(Body::empty()).expect("response");
    }

    let declared = parts.headers.get(header::CONTENT_LENGTH).and_then(|v| v.to_str().ok()).and_then(|v| v.parse::<usize>().ok());
    let too_large = || with_cors(error(StatusCode::PAYLOAD_TOO_LARGE, "Request body is too large."));
    if declared.is_some_and(|d| d > MAX_BODY_SIZE) {
        return too_large();
    }
    let Ok(collected) = http_body_util::Limited::new(body, MAX_BODY_SIZE).collect().await else {
        return too_large();
    };
    let bytes = collected.to_bytes();

    let path = parts.uri.path();
    if path != "/api/instance" && path != "/api/sites" {
        let path_and_query = parts.uri.path_and_query().map_or(path, |pq| pq.as_str());
        let event = match verify_nip98(
            parts.headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()),
            parts.method.as_str(),
            path_and_query,
            &bytes,
            &deps.dashboard_host,
            crate::util::now(),
        ) {
            Ok(event) => event,
            Err(e) => return with_cors(error(StatusCode::UNAUTHORIZED, e.0)),
        };

        let pubkey = event.pubkey.clone();
        let user = deps
            .db
            .call(move |conn| {
                Ok(conn
                    .prepare_cached("SELECT pubkey, role FROM users WHERE pubkey = ?")?
                    .query_row([pubkey], |r| Ok(AuthUser { pubkey: r.get(0)?, role: r.get(1)? }))
                    .optional()?)
            })
            .await;
        match user {
            Ok(Some(user)) => {
                parts.extensions.insert(user);
            }
            Ok(None) => return with_cors(error(StatusCode::FORBIDDEN, "This account has no access to npanel. Ask an admin to add you.")),
            Err(e) => return with_cors(internal(&deps.log, path, e)),
        }
    }

    parts.extensions.insert(RawBody(bytes.clone()));
    with_cors(next.run(Request::from_parts(parts, Body::from(bytes))).await)
}

fn with_cors(mut response: Reply) -> Reply {
    response.headers_mut().insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, HeaderValue::from_static("*"));
    response
}

/// The body as JSON, or `None` when it isn't — as `c.req.json().catch(() => null)`.
fn body_json(body: &RawBody) -> Option<Value> {
    serde_json::from_slice(&body.0).ok()
}

fn require_admin(user: &AuthUser) -> Option<Reply> {
    (!user.is_admin()).then(|| error(StatusCode::FORBIDDEN, "Admins only."))
}

/// Run a database closure, turning a failure into a 500.
async fn db<T: Send + 'static>(deps: &Deps, path: &str, f: impl FnOnce(&mut Connection) -> anyhow::Result<T> + Send + 'static) -> Result<T, Reply> {
    deps.db.call(f).await.map_err(|e| internal(&deps.log, path, e))
}

/// Who runs this instance, for the logged-out homepage. Nothing here is
/// private: the contact npub is published by an admin to be shown.
async fn instance(State(deps): State<Deps>) -> Reply {
    let contact = deps.settings.get().contact_npub.clone();
    ok(json!({
        "contact_npub": if contact.is_empty() { Value::Null } else { Value::String(contact) },
        "dashboard_host": deps.dashboard_host,
    }))
}

/// The sites this gateway serves, for the logged-out homepage. Empty unless
/// an admin turned `public_site_list` on.
async fn sites(State(deps): State<Deps>) -> Reply {
    let public = deps.settings.get().public_site_list;
    let previews = deps.previews.clone();
    let result = db(&deps, "/api/sites", move |conn| {
        let hosts = listed_hosts(conn, public)?;
        let mut cards = Vec::with_capacity(hosts.len());
        for host in hosts {
            let build = current_build(conn, host.id, BuildKind::Clearnet)?;
            let preview = previews.preview(conn, &host, build.as_ref())?;
            let mut card = Map::new();
            card.insert("hostname".into(), json!(host.hostname));
            if let Value::Object(fields) = json!(preview) {
                card.extend(fields);
            }
            cards.push(Value::Object(card));
        }
        Ok(cards)
    })
    .await;
    match result {
        Ok(cards) => ok(json!({ "sites": cards })),
        Err(reply) => reply,
    }
}

async fn me(Extension(user): Extension<AuthUser>) -> Reply {
    ok(json!({ "pubkey": user.pubkey, "npub": nip19::npub_encode(&user.pubkey), "role": user.role }))
}

/// Where staff should point a hostname's DNS, shown when they add one.
async fn dns(State(deps): State<Deps>) -> Reply {
    let (records, source) = deps.dns.target().await;
    ok(json!({ "records": records, "source": source, "dashboard_host": deps.dashboard_host }))
}

/// The hosts npanel will name to someone who can't manage them: none unless
/// the list is public, and never a disabled or hidden one.
fn listed_hosts(conn: &Connection, public: bool) -> rusqlite::Result<Vec<Host>> {
    if !public {
        return Ok(Vec::new());
    }
    conn.prepare_cached(&format!("SELECT {HOST_COLUMNS} FROM hosts WHERE enabled = 1 AND hidden = 0 ORDER BY hostname"))?
        .query_map([], Host::from_row)?
        .collect()
}

/// Whether a user may see and manage a host.
fn can_access(conn: &Connection, user: &AuthUser, host_id: i64) -> rusqlite::Result<bool> {
    if user.is_admin() {
        return Ok(true);
    }
    Ok(conn
        .prepare_cached("SELECT host_id FROM host_grants WHERE user_pubkey = ? AND host_id = ?")?
        .query_row(params![user.pubkey, host_id], |r| r.get::<_, i64>(0))
        .optional()?
        .is_some())
}

/// Load a host the user may manage, or the reply saying why not.
async fn accessible_host(deps: &Deps, user: &AuthUser, hostname: &str, path: &str) -> Result<Host, Reply> {
    let (name, u) = (hostname.to_string(), user.clone());
    let found = db(deps, path, move |conn| {
        let Some(host) = load_host(conn, &name)? else { return Ok(Err(false)) };
        Ok(if can_access(conn, &u, host.id)? { Ok(host) } else { Err(true) })
    })
    .await?;
    match found {
        Ok(host) => Ok(host),
        Err(false) => Err(error(StatusCode::NOT_FOUND, "Host not found.")),
        Err(true) => Err(error(StatusCode::FORBIDDEN, "You don't have access to this host.")),
    }
}

/// Record an administrative change.
fn audit(conn: &Connection, actor: &str, action: &str, target: Option<&str>, old: Option<&Value>, new: Option<&Value>) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO audit_log (actor_pubkey, action, target, old_value, new_value, created_at) VALUES (?, ?, ?, ?, ?, ?)",
        params![actor, action, target, old.map(Value::to_string), new.map(Value::to_string), crate::util::now()],
    )?;
    Ok(())
}

fn version_json(row: &SiteEvent) -> Value {
    json!({
        "event_id": row.event_id,
        "created_at": row.created_at,
        "status": row.status,
        "files_total": row.files_total,
        "files_ready": row.files_ready,
        "error": row.error,
        "fetched_at": row.fetched_at,
        "ready_at": row.ready_at,
    })
}

/// A host as the API shows it, including its deploy state: the version its
/// build serves (which the dashboard calls active), the newest version, and
/// the policy it is actually served under.
async fn serialize_host(deps: &Deps, host: Host) -> anyhow::Result<Value> {
    let previews = deps.previews.clone();
    let h = host.clone();
    let (build, latest, onion, active, preview): (Option<HostBuild>, Option<SiteEvent>, Option<String>, Option<SiteEvent>, SitePreview) = deps
        .db
        .call(move |conn| {
            let build = current_build(conn, h.id, BuildKind::Clearnet)?;
            let latest = newest_event(conn, &h.address())?;
            let onion = conn
                .prepare_cached("SELECT address FROM onion_services WHERE host_id = ?")?
                .query_row([h.id], |r| r.get(0))
                .optional()?;
            let active = match &build {
                Some(build) => site_event(conn, &build.event_id)?,
                None => None,
            };
            let preview = previews.preview(conn, &h, build.as_ref())?;
            Ok((build, latest, onion, active, preview))
        })
        .await?;

    // The same question the serving path asks, answered the same way.
    let declared = match (&build, host.csp == 1) {
        (Some(build), true) => deps.site_csp.policy(build.id).await,
        _ => None,
    };

    let address = host.address();
    Ok(json!({
        "hostname": host.hostname,
        "pubkey": host.pubkey,
        "kind": host.kind,
        "identifier": host.identifier,
        "address": format_nsite_address(&address),
        "naddr": encode_naddr(&address),
        "enabled": host.enabled == 1,
        "hidden": host.hidden == 1,
        "redirect_www": host.redirect_www == 1,
        "spa": host.spa == 1,
        "csp": host.csp == 1,
        "csp_policy": if host.csp == 1 { Value::String(declared.clone().unwrap_or_else(|| GATEWAY_CSP.to_string())) } else { Value::Null },
        "csp_source": if host.csp != 1 { "off" } else if declared.is_some() { "site" } else { "gateway" },
        "seo": host.seo == 1,
        "merge_deployments": host.merge_deployments == 1,
        "analytics": host.analytics == 1,
        "onion": onion,
        "pinned_event_id": host.pinned_event_id,
        "created_by": host.created_by,
        "created_at": host.created_at,
        "updated_at": host.updated_at,
        "active": active.as_ref().map(version_json),
        "latest": latest.as_ref().map(version_json),
        "preview": preview,
    }))
}

/// The fields of a host the audit log records, as the table stores them.
fn host_summary(host: &Host) -> Map<String, Value> {
    let mut summary = Map::new();
    for (key, value) in [
        ("hostname", json!(host.hostname)),
        ("pubkey", json!(host.pubkey)),
        ("kind", json!(host.kind)),
        ("identifier", json!(host.identifier)),
        ("enabled", json!(host.enabled)),
        ("hidden", json!(host.hidden)),
        ("redirect_www", json!(host.redirect_www)),
        ("spa", json!(host.spa)),
        ("csp", json!(host.csp)),
        ("seo", json!(host.seo)),
        ("merge_deployments", json!(host.merge_deployments)),
        ("analytics", json!(host.analytics)),
        ("pinned_event_id", json!(host.pinned_event_id)),
    ] {
        summary.insert(key.into(), value);
    }
    summary
}

async fn list_hosts(State(deps): State<Deps>, Extension(user): Extension<AuthUser>) -> Reply {
    let public = deps.settings.get().public_site_list;
    let u = user.clone();
    let result = db(&deps, "/api/hosts", move |conn| {
        let hosts: Vec<Host> = if u.is_admin() {
            conn.prepare_cached(&format!("SELECT {HOST_COLUMNS} FROM hosts ORDER BY hostname"))?.query_map([], Host::from_row)?.collect::<rusqlite::Result<_>>()?
        } else {
            conn.prepare_cached(&format!(
                "SELECT {HOST_COLUMNS} FROM hosts WHERE id IN (SELECT host_id FROM host_grants WHERE user_pubkey = ?) ORDER BY hostname"
            ))?
            .query_map([&u.pubkey], Host::from_row)?
            .collect::<rusqlite::Result<_>>()?
        };
        // Staff are told what else this gateway serves on the same terms as
        // the public. Admins have every host already.
        let others: Vec<String> = if u.is_admin() {
            Vec::new()
        } else {
            let mine: std::collections::HashSet<&str> = hosts.iter().map(|h| h.hostname.as_str()).collect();
            listed_hosts(conn, public)?.into_iter().map(|h| h.hostname).filter(|h| !mine.contains(h.as_str())).collect()
        };
        Ok((hosts, others))
    })
    .await;
    let (hosts, others) = match result {
        Ok(found) => found,
        Err(reply) => return reply,
    };

    let serialized = futures_util::future::join_all(hosts.into_iter().map(|host| serialize_host(&deps, host))).await;
    let mut list = Vec::with_capacity(serialized.len());
    for host in serialized {
        match host {
            Ok(host) => list.push(host),
            Err(e) => return internal(&deps.log, "/api/hosts", e),
        }
    }
    ok(json!({ "hosts": list, "other_sites": others.into_iter().map(|h| json!({ "hostname": h })).collect::<Vec<_>>() }))
}

/// A boolean field that may be absent, but must be a boolean if present.
fn optional_bool(body: &Map<String, Value>, key: &str) -> Result<Option<bool>, ()> {
    match body.get(key) {
        None => Ok(None),
        Some(Value::Bool(b)) => Ok(Some(*b)),
        Some(_) => Err(()),
    }
}

fn optional_string<'a>(body: &'a Map<String, Value>, key: &str) -> Result<Option<&'a str>, ()> {
    match body.get(key) {
        None => Ok(None),
        Some(Value::String(s)) => Ok(Some(s)),
        Some(_) => Err(()),
    }
}

const FLAGS: [&str; 8] = ["enabled", "hidden", "redirect_www", "spa", "csp", "seo", "merge_deployments", "analytics"];

async fn create_host(State(deps): State<Deps>, Extension(user): Extension<AuthUser>, Extension(body): Extension<RawBody>) -> Reply {
    let bad = || error(StatusCode::BAD_REQUEST, "Expected a JSON body with hostname and address.");
    let Some(Value::Object(body)) = body_json(&body) else { return bad() };
    let (Some(Value::String(hostname)), Some(Value::String(address))) = (body.get("hostname"), body.get("address")) else {
        return bad();
    };
    let mut flags = HashMap::new();
    for flag in &FLAGS[1..] {
        match optional_bool(&body, flag) {
            Ok(value) => {
                flags.insert(*flag, value);
            }
            Err(()) => return bad(),
        }
    }

    let settings = deps.settings.get();
    let hostname = match parse_hostname(hostname, &settings.allowed_host_suffixes) {
        Ok(h) => h,
        Err(e) => return error(StatusCode::BAD_REQUEST, e),
    };
    let address = match parse_nsite_address(address) {
        Ok(a) => a,
        Err(e) => return error(StatusCode::BAD_REQUEST, e),
    };
    if hostname == deps.dashboard_host {
        return error(StatusCode::CONFLICT, "That domain serves the dashboard itself.");
    }

    // Off unless asked for, or on unless asked otherwise, flag by flag.
    let on = |flag: &str, default: bool| i64::from(flags.get(flag).copied().flatten().unwrap_or(default));
    let values = [on("hidden", false), on("redirect_www", true), on("spa", false), on("csp", true), on("seo", true), on("merge_deployments", false), on("analytics", false)];

    let (u, name, addr) = (user.clone(), hostname.clone(), address.clone());
    let created = db(&deps, "/api/hosts", move |conn| {
        if load_host(conn, &name)?.is_some() {
            return Ok(None);
        }
        let now = crate::util::now();
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO hosts (hostname, pubkey, kind, identifier, pinned_event_id, enabled, hidden, redirect_www, spa, csp, seo,
               merge_deployments, analytics, created_by, created_at, updated_at)
             VALUES (?, ?, ?, ?, NULL, 1, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            params![name, addr.pubkey, addr.kind, addr.identifier, values[0], values[1], values[2], values[3], values[4], values[5], values[6], u.pubkey, now, now],
        )?;
        let id = tx.last_insert_rowid();
        // Whoever adds a host can manage it, so staff aren't locked out of
        // their own work.
        if !u.is_admin() {
            tx.execute("INSERT INTO host_grants (user_pubkey, host_id, created_at) VALUES (?, ?, ?)", params![u.pubkey, id, now])?;
        }
        audit(&tx, &u.pubkey, "host.create", Some(&name), None, Some(&json!({ "hostname": name, "address": site_key(&addr) })))?;
        tx.commit()?;
        Ok(crate::sites::host_by_id(conn, id)?)
    })
    .await;

    let host = match created {
        Ok(Some(host)) => host,
        Ok(None) => return error(StatusCode::CONFLICT, format!("{hostname} is already configured.")),
        Err(reply) => return reply,
    };

    deps.log.info("host_created", fields! { "hostname" => hostname.as_str(), "pubkey" => address.pubkey.as_str(), "actor" => user.pubkey.as_str() });

    // Before the response, so the row staff were just told about is served.
    deps.serve_cache.clear();
    deps.deployments.hosts_changed();
    deps.tor.hosts_changed();
    // A site another host already serves has a ready version to build from
    // straight away, and no deploy is coming along to say so.
    deps.deployments.rebuild(vec![host.id]);
    deps.analytics.hosts_changed().await;

    match serialize_host(&deps, host).await {
        Ok(host) => json_reply(StatusCode::CREATED, json!({ "host": host })),
        Err(e) => internal(&deps.log, "/api/hosts", e),
    }
}

async fn get_host(State(deps): State<Deps>, Extension(user): Extension<AuthUser>, Path(hostname): Path<String>) -> Reply {
    let host = match accessible_host(&deps, &user, &hostname, "/api/hosts/:hostname").await {
        Ok(host) => host,
        Err(reply) => return reply,
    };
    match serialize_host(&deps, host).await {
        Ok(host) => ok(json!({ "host": host })),
        Err(e) => internal(&deps.log, "/api/hosts/:hostname", e),
    }
}

async fn update_host(
    State(deps): State<Deps>,
    Extension(user): Extension<AuthUser>,
    Extension(body): Extension<RawBody>,
    Path(hostname): Path<String>,
) -> Reply {
    let host = match accessible_host(&deps, &user, &hostname, "/api/hosts/:hostname").await {
        Ok(host) => host,
        Err(reply) => return reply,
    };

    let bad = || error(StatusCode::BAD_REQUEST, "Expected a JSON body.");
    let Some(Value::Object(body)) = body_json(&body) else { return bad() };
    let (Ok(new_hostname), Ok(new_address)) = (optional_string(&body, "hostname"), optional_string(&body, "address")) else {
        return bad();
    };
    let pinned = match body.get("pinned_event_id") {
        None => None,
        Some(Value::Null) => Some(None),
        Some(Value::String(s)) => Some(Some(s.clone())),
        Some(_) => return bad(),
    };
    let mut flags = Vec::new();
    for flag in FLAGS {
        match optional_bool(&body, flag) {
            Ok(Some(value)) => flags.push((flag, i64::from(value))),
            Ok(None) => {}
            Err(()) => return bad(),
        }
    }

    // Changed columns, in the order the audit log's summary lists them.
    let mut patch: Vec<(&'static str, Value)> = Vec::new();

    if let Some(new_hostname) = new_hostname {
        let parsed = match parse_hostname(new_hostname, &deps.settings.get().allowed_host_suffixes) {
            Ok(h) => h,
            Err(e) => return error(StatusCode::BAD_REQUEST, e),
        };
        if parsed == deps.dashboard_host {
            return error(StatusCode::CONFLICT, "That domain serves the dashboard itself.");
        }
        if parsed != host.hostname {
            let name = parsed.clone();
            match db(&deps, "/api/hosts/:hostname", move |conn| Ok(load_host(conn, &name)?.is_some())).await {
                Ok(true) => return error(StatusCode::CONFLICT, format!("{parsed} is already configured.")),
                Ok(false) => patch.push(("hostname", json!(parsed))),
                Err(reply) => return reply,
            }
        }
    }

    if let Some(new_address) = new_address {
        match parse_nsite_address(new_address) {
            Ok(address) => {
                patch.push(("pubkey", json!(address.pubkey)));
                patch.push(("kind", json!(address.kind)));
                patch.push(("identifier", json!(address.identifier)));
                // A retarget invalidates a pin that belongs to the old site.
                patch.push(("pinned_event_id", Value::Null));
            }
            Err(e) => return error(StatusCode::BAD_REQUEST, e),
        }
    }

    for (flag, value) in flags {
        patch.push((flag, json!(value)));
    }

    if let (Some(pinned), None) = (pinned, new_address) {
        match pinned {
            None => patch.push(("pinned_event_id", Value::Null)),
            Some(event_id) => {
                let found = db(&deps, "/api/hosts/:hostname", move |conn| Ok(site_event(conn, &event_id)?)).await;
                let version = match found {
                    Ok(version) => version,
                    Err(reply) => return reply,
                };
                let Some(version) = version.filter(|v| v.address() == host.address()) else {
                    return error(StatusCode::BAD_REQUEST, "That version belongs to a different site.");
                };
                if version.status != status::READY {
                    return error(StatusCode::CONFLICT, "That version isn't fully downloaded yet.");
                }
                patch.push(("pinned_event_id", json!(version.event_id)));
            }
        }
    }

    if patch.is_empty() {
        return match serialize_host(&deps, host).await {
            Ok(host) => ok(json!({ "host": host })),
            Err(e) => internal(&deps.log, "/api/hosts/:hostname", e),
        };
    }

    let summary = host_summary(&host);
    let mut after = summary.clone();
    for (key, value) in &patch {
        after.insert(key.to_string(), value.clone());
    }

    let (h, p, actor) = (host.clone(), patch.clone(), user.pubkey.clone());
    let updated = db(&deps, "/api/hosts/:hostname", move |conn| {
        let tx = conn.transaction()?;
        for (column, value) in &p {
            let value: rusqlite::types::Value = match value {
                Value::Null => rusqlite::types::Value::Null,
                Value::String(s) => rusqlite::types::Value::Text(s.clone()),
                Value::Number(n) => rusqlite::types::Value::Integer(n.as_i64().unwrap_or(0)),
                _ => rusqlite::types::Value::Null,
            };
            tx.execute(&format!("UPDATE hosts SET {column} = ? WHERE id = ?"), params![value, h.id])?;
        }
        tx.execute("UPDATE hosts SET updated_at = ? WHERE id = ?", params![crate::util::now(), h.id])?;
        audit(&tx, &actor, "host.update", Some(&h.hostname), Some(&Value::Object(summary)), Some(&Value::Object(after)))?;
        tx.commit()?;
        Ok(crate::sites::host_by_id(conn, h.id)?)
    })
    .await;
    let updated = match updated {
        Ok(Some(host)) => host,
        Ok(None) => return error(StatusCode::NOT_FOUND, "Host not found."),
        Err(reply) => return reply,
    };

    deps.log.info("host_updated", fields! { "hostname" => host.hostname.as_str(), "actor" => user.pubkey.as_str() });

    deps.serve_cache.clear();
    let changed = |key: &str| patch.iter().any(|(k, _)| *k == key);
    if changed("pubkey") || changed("enabled") {
        deps.deployments.hosts_changed();
    }
    // Every edit, rather than the ones that feed a build: the builder hashes
    // what the build is made of and does nothing if that didn't change.
    deps.deployments.rebuild(vec![host.id]);
    deps.analytics.hosts_changed().await;
    // Tor publishes a name, and does not care which nsite is behind it.
    if changed("enabled") {
        deps.tor.hosts_changed();
    }

    match serialize_host(&deps, updated).await {
        Ok(host) => ok(json!({ "host": host })),
        Err(e) => internal(&deps.log, "/api/hosts/:hostname", e),
    }
}

async fn delete_host(State(deps): State<Deps>, Extension(user): Extension<AuthUser>, Path(hostname): Path<String>) -> Reply {
    let host = match accessible_host(&deps, &user, &hostname, "/api/hosts/:hostname").await {
        Ok(host) => host,
        Err(reply) => return reply,
    };
    let (h, actor) = (host.clone(), user.pubkey.clone());
    if let Err(reply) = db(&deps, "/api/hosts/:hostname", move |conn| {
        conn.execute("DELETE FROM hosts WHERE id = ?", [h.id])?;
        audit(conn, &actor, "host.delete", Some(&h.hostname), Some(&Value::Object(host_summary(&h))), None)?;
        Ok(())
    })
    .await
    {
        return reply;
    }

    deps.log.info("host_deleted", fields! { "hostname" => host.hostname.as_str(), "actor" => user.pubkey.as_str() });
    deps.serve_cache.clear();
    deps.deployments.hosts_changed();
    deps.tor.hosts_changed();
    deps.analytics.hosts_changed().await;
    ok(json!({ "ok": true }))
}

fn clamp(value: Option<&String>, default: i64, min: i64, max: i64) -> i64 {
    let number = value.map_or(Some(default as f64), |v| v.trim().parse::<f64>().ok().or(if v.trim().is_empty() { Some(0.0) } else { None }));
    match number.filter(|n| n.is_finite()) {
        Some(n) => (n.trunc() as i64).clamp(min, max),
        None => min,
    }
}

async fn versions(
    State(deps): State<Deps>,
    Extension(user): Extension<AuthUser>,
    Path(hostname): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Reply {
    let host = match accessible_host(&deps, &user, &hostname, "/api/hosts/:hostname/versions").await {
        Ok(host) => host,
        Err(reply) => return reply,
    };
    let limit = clamp(query.get("limit"), 50, 1, 200);
    let result = db(&deps, "/api/hosts/:hostname/versions", move |conn| {
        let versions: Vec<SiteEvent> = conn
            .prepare_cached(&format!(
                "SELECT {SITE_EVENT_COLUMNS} FROM site_events WHERE pubkey = ? AND kind = ? AND identifier = ? ORDER BY created_at DESC LIMIT ?"
            ))?
            .query_map(params![host.pubkey, host.kind, host.identifier, limit], SiteEvent::from_row)?
            .collect::<rusqlite::Result<_>>()?;
        let build = current_build(conn, host.id, BuildKind::Clearnet)?;
        Ok(json!({
            "active_event_id": build.map(|b| b.event_id),
            "pinned_event_id": host.pinned_event_id,
            "versions": versions.iter().map(version_json).collect::<Vec<_>>(),
        }))
    })
    .await;
    result.map_or_else(|reply| reply, ok)
}

async fn version_log(State(deps): State<Deps>, Extension(user): Extension<AuthUser>, Path((hostname, event_id)): Path<(String, String)>) -> Reply {
    let host = match accessible_host(&deps, &user, &hostname, "/api/hosts/:hostname/versions/:eventId/log").await {
        Ok(host) => host,
        Err(reply) => return reply,
    };
    let result = db(&deps, "/api/hosts/:hostname/versions/:eventId/log", move |conn| {
        let Some(version) = site_event(conn, &event_id)?.filter(|v| v.address() == host.address()) else {
            return Ok(None);
        };
        let entries: Vec<Value> = conn
            .prepare_cached("SELECT id, level, msg, fields, created_at FROM deploy_log WHERE event_id = ? ORDER BY id ASC")?
            .query_map([&event_id], |r| {
                let fields: Option<String> = r.get(3)?;
                Ok(json!({
                    "id": r.get::<_, i64>(0)?,
                    "level": r.get::<_, String>(1)?,
                    "msg": r.get::<_, String>(2)?,
                    "fields": fields.and_then(|f| serde_json::from_str::<Value>(&f).ok()),
                    "created_at": r.get::<_, i64>(4)?,
                }))
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(Some(json!({ "version": version_json(&version), "entries": entries })))
    })
    .await;
    match result {
        Ok(Some(body)) => ok(body),
        Ok(None) => error(StatusCode::NOT_FOUND, "That version belongs to a different site."),
        Err(reply) => reply,
    }
}

async fn files(
    State(deps): State<Deps>,
    Extension(user): Extension<AuthUser>,
    Path(hostname): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Reply {
    let host = match accessible_host(&deps, &user, &hostname, "/api/hosts/:hostname/files").await {
        Ok(host) => host,
        Err(reply) => return reply,
    };
    let requested = query.get("event_id").filter(|e| !e.is_empty()).cloned();

    let result = db(&deps, "/api/hosts/:hostname/files", move |conn| {
        let build = current_build(conn, host.id, BuildKind::Clearnet)?;
        let version = match &requested {
            Some(event_id) => site_event(conn, event_id)?,
            None => match build.as_ref().map(|b| site_event(conn, &b.event_id)).transpose()?.flatten() {
                Some(served) => Some(served),
                None => newest_event(conn, &host.address())?,
            },
        };
        let Some(version) = version.filter(|v| v.address() == host.address()) else {
            return Ok(None);
        };

        // What the site actually serves is its build — for a merged host more
        // than one version's manifest, and for any host the files npanel made.
        // Only for the version being served; one named outright is inspected
        // for itself, so that listing stays the manifest it is.
        if let Some(build) = build.filter(|b| requested.is_none() && b.event_id == version.event_id) {
            let files: Vec<Value> = conn
                .prepare_cached(
                    "SELECT f.path, f.sha256, f.event_id, f.source_sha256, blobs.size FROM host_build_files f
                     LEFT JOIN blobs ON blobs.sha256 = f.sha256 WHERE f.build_id = ? ORDER BY f.path",
                )?
                .query_map([build.id], |r| {
                    let event_id: Option<String> = r.get(2)?;
                    let source: Option<String> = r.get(3)?;
                    let size: Option<i64> = r.get(4)?;
                    Ok(json!({
                        "path": r.get::<_, String>(0)?,
                        "sha256": r.get::<_, String>(1)?,
                        "size": size,
                        "stored": size.is_some(),
                        // Served from an earlier deploy, because this one dropped the path.
                        "inherited": event_id.as_ref().is_some_and(|e| *e != version.event_id),
                        "source": if event_id.is_none() { "generated" } else if source.is_some() { "modified" } else { "site" },
                    }))
                })?
                .collect::<rusqlite::Result<_>>()?;
            return Ok(Some(json!({ "event_id": version.event_id, "files": files })));
        }

        let files: Vec<Value> = conn
            .prepare_cached(
                "SELECT f.path, f.sha256, blobs.size FROM site_files f
                 LEFT JOIN blobs ON blobs.sha256 = f.sha256 WHERE f.event_id = ? ORDER BY f.path",
            )?
            .query_map([&version.event_id], |r| {
                let size: Option<i64> = r.get(2)?;
                Ok(json!({
                    "path": r.get::<_, String>(0)?,
                    "sha256": r.get::<_, String>(1)?,
                    "size": size,
                    "stored": size.is_some(),
                    "inherited": false,
                    "source": "site",
                }))
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(Some(json!({ "event_id": version.event_id, "files": files })))
    })
    .await;

    match result {
        Ok(Some(body)) => ok(body),
        Ok(None) => error(StatusCode::NOT_FOUND, "No version to list."),
        Err(reply) => reply,
    }
}

async fn sync(State(deps): State<Deps>, Extension(user): Extension<AuthUser>, Path(hostname): Path<String>) -> Reply {
    let host = match accessible_host(&deps, &user, &hostname, "/api/hosts/:hostname/sync").await {
        Ok(host) => host,
        Err(reply) => return reply,
    };
    // Relay round trips take seconds; the dashboard polls the host for status.
    deps.deployments.sync_site(host.address());
    json_reply(StatusCode::ACCEPTED, json!({ "ok": true }))
}

async fn get_grants(State(deps): State<Deps>, Extension(user): Extension<AuthUser>, Path(hostname): Path<String>) -> Reply {
    if let Some(reply) = require_admin(&user) {
        return reply;
    }
    let result = db(&deps, "/api/hosts/:hostname/grants", move |conn| {
        let Some(host) = load_host(conn, &hostname)? else { return Ok(None) };
        let grants: Vec<Value> = conn
            .prepare_cached("SELECT user_pubkey, created_at FROM host_grants WHERE host_id = ?")?
            .query_map([host.id], |r| Ok(json!({ "user_pubkey": r.get::<_, String>(0)?, "created_at": r.get::<_, i64>(1)? })))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(Some(grants))
    })
    .await;
    match result {
        Ok(Some(grants)) => ok(json!({ "grants": grants })),
        Ok(None) => error(StatusCode::NOT_FOUND, "Host not found."),
        Err(reply) => reply,
    }
}

async fn put_grants(
    State(deps): State<Deps>,
    Extension(user): Extension<AuthUser>,
    Extension(body): Extension<RawBody>,
    Path(hostname): Path<String>,
) -> Reply {
    if let Some(reply) = require_admin(&user) {
        return reply;
    }
    let bad = || error(StatusCode::BAD_REQUEST, "Expected a JSON body with a pubkeys array.");
    let Some(Value::Object(body)) = body_json(&body) else { return bad() };
    let Some(Value::Array(list)) = body.get("pubkeys") else { return bad() };
    let Some(list) = list.iter().map(|p| p.as_str().map(str::to_lowercase)).collect::<Option<Vec<_>>>() else {
        return bad();
    };
    let pubkeys = crate::nostr::service::dedupe(list);
    if pubkeys.iter().any(|p| !crate::nostr::is_hex64(p)) {
        return error(StatusCode::BAD_REQUEST, "Grants must be hex pubkeys.");
    }

    let (actor, keys) = (user.pubkey.clone(), pubkeys.clone());
    let result = db(&deps, "/api/hosts/:hostname/grants", move |conn| {
        let Some(host) = load_host(conn, &hostname)? else { return Ok(Err(StatusCode::NOT_FOUND)) };
        let mut missing = Vec::new();
        for pubkey in &keys {
            let known: Option<String> = conn.query_row("SELECT pubkey FROM users WHERE pubkey = ?", [pubkey], |r| r.get(0)).optional()?;
            if known.is_none() {
                missing.push(pubkey.clone());
            }
        }
        if !missing.is_empty() {
            return Ok(Ok(Err(missing)));
        }

        let previous: Vec<String> =
            conn.prepare("SELECT user_pubkey FROM host_grants WHERE host_id = ?")?.query_map([host.id], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
        let now = crate::util::now();
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM host_grants WHERE host_id = ?", [host.id])?;
        for pubkey in &keys {
            tx.execute("INSERT INTO host_grants (user_pubkey, host_id, created_at) VALUES (?, ?, ?)", params![pubkey, host.id, now])?;
        }
        audit(&tx, &actor, "host.grants", Some(&host.hostname), Some(&json!(previous)), Some(&json!(keys)))?;
        tx.commit()?;
        Ok(Ok(Ok(())))
    })
    .await;

    match result {
        Ok(Ok(Ok(()))) => ok(json!({ "grants": pubkeys })),
        Ok(Ok(Err(missing))) => error(StatusCode::BAD_REQUEST, format!("Not npanel users: {}", missing.join(", "))),
        Ok(Err(_)) => error(StatusCode::NOT_FOUND, "Host not found."),
        Err(reply) => reply,
    }
}

/// A host's NIP-05 names, in the shape every route here returns them.
fn nip05_list(conn: &Connection, host_id: i64) -> rusqlite::Result<Vec<Value>> {
    conn.prepare_cached("SELECT name, pubkey, relays, created_at, updated_at FROM nip05_names WHERE host_id = ? ORDER BY name")?
        .query_map([host_id], |r| {
            Ok(json!({
                "name": r.get::<_, String>(0)?,
                "pubkey": r.get::<_, String>(1)?,
                "relays": parse_relay_list(&r.get::<_, String>(2)?),
                "created_at": r.get::<_, i64>(3)?,
                "updated_at": r.get::<_, i64>(4)?,
            }))
        })?
        .collect()
}

async fn list_nip05(State(deps): State<Deps>, Extension(user): Extension<AuthUser>, Path(hostname): Path<String>) -> Reply {
    let host = match accessible_host(&deps, &user, &hostname, "/api/hosts/:hostname/nip05").await {
        Ok(host) => host,
        Err(reply) => return reply,
    };
    db(&deps, "/api/hosts/:hostname/nip05", move |conn| Ok(nip05_list(conn, host.id)?))
        .await
        .map_or_else(|reply| reply, |names| ok(json!({ "names": names })))
}

/// Add a name, or replace whatever was stored under it. A name is a setting
/// of the site, so anyone with a grant on the site manages it.
async fn put_nip05(
    State(deps): State<Deps>,
    Extension(user): Extension<AuthUser>,
    Extension(body): Extension<RawBody>,
    Path((hostname, name)): Path<(String, String)>,
) -> Reply {
    let host = match accessible_host(&deps, &user, &hostname, "/api/hosts/:hostname/nip05/:name").await {
        Ok(host) => host,
        Err(reply) => return reply,
    };

    let bad = || error(StatusCode::BAD_REQUEST, "Expected a JSON body with a pubkey.");
    let Some(Value::Object(body)) = body_json(&body) else { return bad() };
    let Some(Value::String(pubkey)) = body.get("pubkey") else { return bad() };
    let relays = match body.get("relays") {
        None => Vec::new(),
        Some(Value::Array(list)) => match list.iter().map(|r| r.as_str().map(str::to_string)).collect::<Option<Vec<_>>>() {
            Some(relays) => relays,
            None => return bad(),
        },
        Some(_) => return bad(),
    };
    if relays.len() > MAX_NIP05_RELAYS {
        return error(StatusCode::BAD_REQUEST, format!("A name can list at most {MAX_NIP05_RELAYS} relays."));
    }

    let name = match parse_nip05_name(&name) {
        Ok(n) => n,
        Err(e) => return error(StatusCode::BAD_REQUEST, e),
    };
    let pubkey = match parse_pubkey(pubkey) {
        Ok(p) => p,
        Err(e) => return error(StatusCode::BAD_REQUEST, e),
    };
    // Deduplicated after normalizing, so the same relay written two ways is
    // stored once.
    let relays = match relays.iter().map(|r| parse_relay_url(r)).collect::<Result<Vec<_>, _>>() {
        Ok(relays) => crate::nostr::service::dedupe(relays),
        Err(e) => return error(StatusCode::BAD_REQUEST, e),
    };

    let (actor, n, p, r, h) = (user.pubkey.clone(), name.clone(), pubkey.clone(), relays.clone(), host.clone());
    let result = db(&deps, "/api/hosts/:hostname/nip05/:name", move |conn| {
        let previous: Option<(String, String)> = conn
            .query_row("SELECT pubkey, relays FROM nip05_names WHERE host_id = ? AND name = ?", params![h.id, n], |row| Ok((row.get(0)?, row.get(1)?)))
            .optional()?;
        let now = crate::util::now();
        let relays_json = json!(r).to_string();
        if previous.is_some() {
            conn.execute(
                "UPDATE nip05_names SET pubkey = ?, relays = ?, updated_at = ? WHERE host_id = ? AND name = ?",
                params![p, relays_json, now, h.id, n],
            )?;
        } else {
            conn.execute(
                "INSERT INTO nip05_names (host_id, name, pubkey, relays, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?)",
                params![h.id, n, p, relays_json, now, now],
            )?;
        }
        let old = previous.as_ref().map(|(pk, relays)| json!({ "pubkey": pk, "relays": parse_relay_list(relays) }));
        audit(conn, &actor, "nip05.set", Some(&format!("{n}@{}", h.hostname)), old.as_ref(), Some(&json!({ "pubkey": p, "relays": r })))?;
        Ok((previous.is_some(), nip05_list(conn, h.id)?))
    })
    .await;

    match result {
        Ok((replaced, names)) => {
            deps.log.info(
                "nip05_name_set",
                fields! { "hostname" => host.hostname.as_str(), "name" => name.as_str(), "pubkey" => pubkey.as_str(), "actor" => user.pubkey.as_str() },
            );
            json_reply(if replaced { StatusCode::OK } else { StatusCode::CREATED }, json!({ "names": names }))
        }
        Err(reply) => reply,
    }
}

async fn delete_nip05(State(deps): State<Deps>, Extension(user): Extension<AuthUser>, Path((hostname, name)): Path<(String, String)>) -> Reply {
    let host = match accessible_host(&deps, &user, &hostname, "/api/hosts/:hostname/nip05/:name").await {
        Ok(host) => host,
        Err(reply) => return reply,
    };
    let name = match parse_nip05_name(&name) {
        Ok(n) => n,
        Err(e) => return error(StatusCode::BAD_REQUEST, e),
    };

    let (actor, n, h) = (user.pubkey.clone(), name.clone(), host.clone());
    let result = db(&deps, "/api/hosts/:hostname/nip05/:name", move |conn| {
        let previous: Option<(String, String)> = conn
            .query_row("SELECT pubkey, relays FROM nip05_names WHERE host_id = ? AND name = ?", params![h.id, n], |row| Ok((row.get(0)?, row.get(1)?)))
            .optional()?;
        let Some((pubkey, relays)) = previous else { return Ok(None) };
        conn.execute("DELETE FROM nip05_names WHERE host_id = ? AND name = ?", params![h.id, n])?;
        audit(conn, &actor, "nip05.delete", Some(&format!("{n}@{}", h.hostname)), Some(&json!({ "pubkey": pubkey, "relays": parse_relay_list(&relays) })), None)?;
        Ok(Some(nip05_list(conn, h.id)?))
    })
    .await;

    match result {
        Ok(Some(names)) => {
            deps.log.info("nip05_name_deleted", fields! { "hostname" => host.hostname.as_str(), "name" => name.as_str(), "actor" => user.pubkey.as_str() });
            ok(json!({ "names": names }))
        }
        Ok(None) => error(StatusCode::NOT_FOUND, format!("{} has no name {name}.", host.hostname)),
        Err(reply) => reply,
    }
}

fn relay(response: WorkerResponse) -> Reply {
    response.into_response(&[])
}

async fn stats_host(deps: &Deps, user: &AuthUser, hostname: &str) -> Result<QueryHost, Reply> {
    let host = accessible_host(deps, user, hostname, "/api/hosts/:hostname/stats").await?;
    Ok(QueryHost { id: host.id, hostname: host.hostname })
}

/// Plausible's dashboard query, answered on the analytics runtime.
async fn stats_query(
    State(deps): State<Deps>,
    Extension(user): Extension<AuthUser>,
    Extension(body): Extension<RawBody>,
    Path(hostname): Path<String>,
) -> Reply {
    let host = match stats_host(&deps, &user, &hostname).await {
        Ok(host) => host,
        Err(reply) => return reply,
    };
    let Some(body) = body_json(&body) else {
        return error(StatusCode::BAD_REQUEST, "Expected a JSON body describing the query.");
    };
    relay(deps.analytics.query(host, body).await)
}

async fn stats_current_visitors(State(deps): State<Deps>, Extension(user): Extension<AuthUser>, Path(hostname): Path<String>) -> Reply {
    match stats_host(&deps, &user, &hostname).await {
        Ok(host) => relay(deps.analytics.current_visitors(host).await),
        Err(reply) => reply,
    }
}

async fn stats_suggestions(
    State(deps): State<Deps>,
    Extension(user): Extension<AuthUser>,
    Path((hostname, filter)): Path<(String, String)>,
    Query(params): Query<HashMap<String, String>>,
) -> Reply {
    match stats_host(&deps, &user, &hostname).await {
        Ok(host) => relay(deps.analytics.suggestions(host, filter, params).await),
        Err(reply) => reply,
    }
}

/// The last 24 hours of every site the user may manage, in one request.
async fn sparklines(State(deps): State<Deps>, Extension(user): Extension<AuthUser>) -> Reply {
    let u = user.clone();
    let hosts = db(&deps, "/api/stats/sparklines", move |conn| {
        let (sql, args): (&str, Vec<String>) = if u.is_admin() {
            ("SELECT id, hostname FROM hosts", vec![])
        } else {
            ("SELECT id, hostname FROM hosts WHERE id IN (SELECT host_id FROM host_grants WHERE user_pubkey = ?)", vec![u.pubkey.clone()])
        };
        Ok(conn
            .prepare(sql)?
            .query_map(rusqlite::params_from_iter(args), |r| Ok(QueryHost { id: r.get(0)?, hostname: r.get(1)? }))?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    })
    .await;
    match hosts {
        Ok(hosts) => relay(deps.analytics.sparklines(hosts).await),
        Err(reply) => reply,
    }
}

fn user_json(pubkey: &str, role: &str) -> Value {
    json!({ "pubkey": pubkey, "npub": nip19::npub_encode(pubkey), "role": role })
}

async fn list_users(State(deps): State<Deps>, Extension(user): Extension<AuthUser>) -> Reply {
    if let Some(reply) = require_admin(&user) {
        return reply;
    }
    let result = db(&deps, "/api/users", |conn| {
        let grants: Vec<(String, String)> = conn
            .prepare("SELECT host_grants.user_pubkey, hosts.hostname FROM host_grants INNER JOIN hosts ON hosts.id = host_grants.host_id")?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        let users: Vec<Value> = conn
            .prepare("SELECT pubkey, role, added_by, created_at FROM users ORDER BY created_at")?
            .query_map([], |r| {
                let pubkey: String = r.get(0)?;
                let hostnames: Vec<&String> = grants.iter().filter(|(p, _)| *p == pubkey).map(|(_, h)| h).collect();
                Ok(json!({
                    "pubkey": pubkey,
                    "npub": nip19::npub_encode(&pubkey),
                    "role": r.get::<_, String>(1)?,
                    "added_by": r.get::<_, Option<String>>(2)?,
                    "created_at": r.get::<_, i64>(3)?,
                    "hostnames": hostnames,
                }))
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(users)
    })
    .await;
    result.map_or_else(|reply| reply, |users| ok(json!({ "users": users })))
}

fn role_of(body: &Map<String, Value>, default: Option<&'static str>) -> Option<String> {
    match body.get("role") {
        None => default.map(str::to_string),
        Some(Value::String(role)) if role == "admin" || role == "staff" => Some(role.clone()),
        Some(_) => None,
    }
}

async fn create_user(State(deps): State<Deps>, Extension(user): Extension<AuthUser>, Extension(body): Extension<RawBody>) -> Reply {
    if let Some(reply) = require_admin(&user) {
        return reply;
    }
    let bad = || error(StatusCode::BAD_REQUEST, "Expected a JSON body with a pubkey.");
    let Some(Value::Object(body)) = body_json(&body) else { return bad() };
    let (Some(Value::String(raw)), Some(role)) = (body.get("pubkey"), role_of(&body, Some("staff"))) else { return bad() };
    let pubkey = match parse_pubkey(raw) {
        Ok(p) => p,
        Err(e) => return error(StatusCode::BAD_REQUEST, e),
    };

    let (actor, p, r) = (user.pubkey.clone(), pubkey.clone(), role.clone());
    let result = db(&deps, "/api/users", move |conn| {
        let existing: Option<String> = conn.query_row("SELECT pubkey FROM users WHERE pubkey = ?", [&p], |row| row.get(0)).optional()?;
        if existing.is_some() {
            return Ok(false);
        }
        conn.execute("INSERT INTO users (pubkey, role, added_by, created_at) VALUES (?, ?, ?, ?)", params![p, r, actor, crate::util::now()])?;
        audit(conn, &actor, "user.create", Some(&p), None, Some(&json!({ "role": r })))?;
        Ok(true)
    })
    .await;

    match result {
        Ok(true) => {
            deps.log.info("user_created", fields! { "pubkey" => pubkey.as_str(), "role" => role.as_str(), "actor" => user.pubkey.as_str() });
            json_reply(StatusCode::CREATED, json!({ "user": user_json(&pubkey, &role) }))
        }
        Ok(false) => error(StatusCode::CONFLICT, "That user already has access."),
        Err(reply) => reply,
    }
}

fn admin_count(conn: &Connection) -> rusqlite::Result<i64> {
    conn.query_row("SELECT COUNT(*) FROM users WHERE role = 'admin'", [], |r| r.get(0))
}

async fn update_user(
    State(deps): State<Deps>,
    Extension(user): Extension<AuthUser>,
    Extension(body): Extension<RawBody>,
    Path(raw): Path<String>,
) -> Reply {
    if let Some(reply) = require_admin(&user) {
        return reply;
    }
    let pubkey = match parse_pubkey(&raw) {
        Ok(p) => p,
        Err(e) => return error(StatusCode::BAD_REQUEST, e),
    };
    let role = match body_json(&body) {
        Some(Value::Object(body)) => role_of(&body, None),
        _ => None,
    };
    let Some(role) = role else {
        return error(StatusCode::BAD_REQUEST, "Expected a JSON body with a role of admin or staff.");
    };

    let (actor, p, r) = (user.pubkey.clone(), pubkey.clone(), role.clone());
    let result = db(&deps, "/api/users/:pubkey", move |conn| {
        let current: Option<String> = conn.query_row("SELECT role FROM users WHERE pubkey = ?", [&p], |row| row.get(0)).optional()?;
        let Some(current) = current else { return Ok(Err(StatusCode::NOT_FOUND)) };
        if current == "admin" && r != "admin" && admin_count(conn)? == 1 {
            return Ok(Err(StatusCode::CONFLICT));
        }
        conn.execute("UPDATE users SET role = ? WHERE pubkey = ?", params![r, p])?;
        audit(conn, &actor, "user.update", Some(&p), Some(&json!({ "role": current })), Some(&json!({ "role": r })))?;
        Ok(Ok(()))
    })
    .await;

    match result {
        Ok(Ok(())) => {
            deps.log.info("user_updated", fields! { "pubkey" => pubkey.as_str(), "role" => role.as_str(), "actor" => user.pubkey.as_str() });
            ok(json!({ "user": user_json(&pubkey, &role) }))
        }
        Ok(Err(StatusCode::NOT_FOUND)) => error(StatusCode::NOT_FOUND, "User not found."),
        Ok(Err(_)) => error(StatusCode::CONFLICT, "That's the last admin. Promote someone else first."),
        Err(reply) => reply,
    }
}

async fn delete_user(State(deps): State<Deps>, Extension(user): Extension<AuthUser>, Path(raw): Path<String>) -> Reply {
    if let Some(reply) = require_admin(&user) {
        return reply;
    }
    let pubkey = match parse_pubkey(&raw) {
        Ok(p) => p,
        Err(e) => return error(StatusCode::BAD_REQUEST, e),
    };

    let (actor, p) = (user.pubkey.clone(), pubkey.clone());
    let result = db(&deps, "/api/users/:pubkey", move |conn| {
        let current: Option<String> = conn.query_row("SELECT role FROM users WHERE pubkey = ?", [&p], |row| row.get(0)).optional()?;
        let Some(current) = current else { return Ok(Err(StatusCode::NOT_FOUND)) };
        if current == "admin" && admin_count(conn)? == 1 {
            return Ok(Err(StatusCode::CONFLICT));
        }
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM host_grants WHERE user_pubkey = ?", [&p])?;
        tx.execute("DELETE FROM users WHERE pubkey = ?", [&p])?;
        audit(&tx, &actor, "user.delete", Some(&p), Some(&json!({ "role": current })), None)?;
        tx.commit()?;
        Ok(Ok(()))
    })
    .await;

    match result {
        Ok(Ok(())) => {
            deps.log.info("user_deleted", fields! { "pubkey" => pubkey.as_str(), "actor" => user.pubkey.as_str() });
            ok(json!({ "ok": true }))
        }
        Ok(Err(StatusCode::NOT_FOUND)) => error(StatusCode::NOT_FOUND, "User not found."),
        Ok(Err(_)) => error(StatusCode::CONFLICT, "That's the last admin. Promote someone else first."),
        Err(reply) => reply,
    }
}

async fn get_settings(State(deps): State<Deps>, Extension(user): Extension<AuthUser>) -> Reply {
    if let Some(reply) = require_admin(&user) {
        return reply;
    }
    ok(json!({ "settings": deps.settings.get().to_map(), "defaults": SettingsValues::default().to_map() }))
}

async fn put_settings(State(deps): State<Deps>, Extension(user): Extension<AuthUser>, Extension(body): Extension<RawBody>) -> Reply {
    if let Some(reply) = require_admin(&user) {
        return reply;
    }
    let Some(Value::Object(body)) = body_json(&body) else {
        return error(StatusCode::BAD_REQUEST, "Expected a JSON body.");
    };
    let unknown: Vec<&String> = body.keys().filter(|key| !KEYS.contains(&key.as_str())).collect();
    if !unknown.is_empty() {
        return error(StatusCode::BAD_REQUEST, format!("Unknown settings: {}", unknown.iter().map(|k| k.as_str()).collect::<Vec<_>>().join(", ")));
    }
    match deps.settings.update(&body, &user.pubkey).await {
        Ok(changed) => ok(json!({ "settings": deps.settings.get().to_map(), "changed": changed })),
        Err(message) => error(StatusCode::BAD_REQUEST, message),
    }
}

async fn audit_log(State(deps): State<Deps>, Extension(user): Extension<AuthUser>, Query(query): Query<HashMap<String, String>>) -> Reply {
    if let Some(reply) = require_admin(&user) {
        return reply;
    }
    let limit = clamp(query.get("limit"), 100, 1, 500);
    let offset = clamp(query.get("offset"), 0, 0, 1_000_000);
    let result = db(&deps, "/api/audit-log", move |conn| {
        let parse = |value: Option<String>| value.filter(|v| !v.is_empty()).and_then(|v| serde_json::from_str::<Value>(&v).ok());
        Ok(conn
            .prepare("SELECT id, actor_pubkey, action, target, old_value, new_value, created_at FROM audit_log ORDER BY id DESC LIMIT ? OFFSET ?")?
            .query_map(params![limit, offset], |r| {
                Ok(json!({
                    "id": r.get::<_, i64>(0)?,
                    "actor_pubkey": r.get::<_, String>(1)?,
                    "action": r.get::<_, String>(2)?,
                    "target": r.get::<_, Option<String>>(3)?,
                    "old_value": parse(r.get(4)?),
                    "new_value": parse(r.get(5)?),
                    "created_at": r.get::<_, i64>(6)?,
                }))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    })
    .await;
    result.map_or_else(|reply| reply, |entries| ok(json!({ "entries": entries })))
}

/// Server-side status: aggregates, not row fetches, since the footer polls it.
async fn status_route(State(deps): State<Deps>) -> Reply {
    let result = db(&deps, "/api/status", |conn| {
        let hosts: i64 = conn.query_row("SELECT COUNT(*) FROM hosts", [], |r| r.get(0))?;
        let ready: i64 = conn.query_row("SELECT COUNT(*) FROM site_events WHERE status = 'ready'", [], |r| r.get(0))?;
        let (blobs, bytes): (i64, Option<i64>) = conn.query_row("SELECT COUNT(*), SUM(size) FROM blobs", [], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok((hosts, ready, blobs, bytes.unwrap_or(0)))
    })
    .await;
    match result {
        Ok((hosts, ready, blobs, bytes)) => ok(json!({
            "hosts": hosts,
            "ready_versions": ready,
            "blobs": blobs,
            "blob_bytes": bytes,
            "relays": deps.settings.get().relays,
            "tor": deps.tor.status(),
        })),
        Err(reply) => reply,
    }
}
