//! The dashboard's JSON API, served only on `DASHBOARD_HOST`.
//!
//! Every route except `/api/instance` and `/api/sites` requires NIP-98 auth
//! from a pubkey that exists in the `users` table. The dashboard is a client
//! of this API and uses nothing private, so anything the UI can do, a script
//! can do.

use std::collections::HashMap;
use std::sync::Arc;

use axum::body::{Body, Bytes};
use axum::extract::{FromRequestParts, MatchedPath, Path, Query, Request, State};
use axum::middleware::Next;
use axum::response::IntoResponse;
use axum::routing::{get, patch, post, put};
use axum::{Extension, Router};
use http::request::Parts;
use http::{header, HeaderValue, Method, Response, StatusCode};
use http_body_util::BodyExt;
use rusqlite::{params, Connection, OptionalExtension};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer};
use serde_json::{json, Map, Value};

use crate::analytics::query::engine::QueryHost;
use crate::analytics::{Analytics, Answer};
use crate::auth::verify_nip98;
use crate::builds::current_build;
use crate::db::{BuildKind, DeployStatus, Db, Host, HostBuild, Role, SiteEvent, HOST_COLUMNS, SITE_EVENT_COLUMNS};
use crate::deploy::Deployments;
use crate::dns::DnsTargets;
use crate::fields;
use crate::hostname::parse_hostname;
use crate::identifiers::{parse_nip05_name, parse_pubkey, parse_relay_list, parse_relay_url};
use crate::log::{with_err, Logger};
use crate::nostr::nip19;
use crate::nsite_address::{encode_naddr, format_nsite_address, parse_nsite_address};
use crate::serve::ServeCache;
use crate::settings::{Settings, SettingsValues, KEYS};
use crate::site_csp::{SiteCsp, GATEWAY_CSP};
use crate::site_env::{parse_env_name, parse_env_value, MAX_VARIABLES};
use crate::site_preview::{SitePreview, SitePreviews};
use crate::sites::{host_by_id, load_host, newest_event, site_event, site_key};
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
type ApiResult = Result<Reply, ApiError>;

/// Why a request failed. Everything but `Internal` is a sentence for the
/// dashboard to show; `Internal` is logged, with the route, and answered
/// with nothing about it.
#[derive(Debug)]
pub enum ApiError {
    BadRequest(String),
    Forbidden(&'static str),
    NotFound(String),
    Conflict(String),
    Internal(anyhow::Error),
}

impl<E: Into<anyhow::Error>> From<E> for ApiError {
    fn from(error: E) -> Self {
        ApiError::Internal(error.into())
    }
}

fn bad(message: impl Into<String>) -> ApiError {
    ApiError::BadRequest(message.into())
}

/// An internal error, carried out of the handler for the middleware to log:
/// only it knows which route this was.
#[derive(Clone)]
struct Failure(Arc<anyhow::Error>);

impl IntoResponse for ApiError {
    fn into_response(self) -> Reply {
        let (status, message) = match &self {
            ApiError::BadRequest(message) | ApiError::NotFound(message) | ApiError::Conflict(message) => {
                let status = match self {
                    ApiError::BadRequest(_) => StatusCode::BAD_REQUEST,
                    ApiError::NotFound(_) => StatusCode::NOT_FOUND,
                    _ => StatusCode::CONFLICT,
                };
                (status, message.as_str())
            }
            ApiError::Forbidden(message) => (StatusCode::FORBIDDEN, *message),
            ApiError::Internal(_) => (StatusCode::INTERNAL_SERVER_ERROR, "Internal server error."),
        };
        let mut response = json_reply(status, json!({ "error": message }));
        if let ApiError::Internal(error) = self {
            response.extensions_mut().insert(Failure(Arc::new(error)));
        }
        response
    }
}

fn json_reply(status: StatusCode, value: Value) -> Reply {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(value.to_string()))
        .expect("response")
}

fn ok(value: Value) -> ApiResult {
    Ok(json_reply(StatusCode::OK, value))
}

/// Whoever signed the request, with a row in `users`.
#[derive(Clone, Debug)]
pub struct AuthUser {
    pub pubkey: String,
    pub role: Role,
}

impl AuthUser {
    fn is_admin(&self) -> bool {
        self.role == Role::Admin
    }
}

impl<S: Send + Sync> FromRequestParts<S> for AuthUser {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, ApiError> {
        // Only reachable on the private routes, which `authenticate` guards.
        parts.extensions.get::<AuthUser>().cloned().ok_or_else(|| ApiError::Internal(anyhow::anyhow!("no authenticated user")))
    }
}

/// An [`AuthUser`] who is an admin, or a `403`.
struct Admin(AuthUser);

impl<S: Send + Sync> FromRequestParts<S> for Admin {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, ApiError> {
        let user = AuthUser::from_request_parts(parts, state).await?;
        if !user.is_admin() {
            return Err(ApiError::Forbidden("Admins only."));
        }
        Ok(Admin(user))
    }
}

/// The request body, read once by the middleware that had to hash it.
#[derive(Clone)]
struct RawBody(Bytes);

impl RawBody {
    /// The body as a `T`, or a `400` saying what was expected. Read from the
    /// bytes rather than with axum's `Json`, which would also insist on a
    /// `Content-Type` this API has never required.
    fn json<T: DeserializeOwned>(&self, expected: &str) -> Result<T, ApiError> {
        serde_json::from_slice(&self.0).map_err(|_| bad(expected))
    }
}

/// Everything the API answers, with its middleware.
pub fn router(deps: Deps) -> Router {
    let public = Router::new().route("/api/instance", get(instance)).route("/api/sites", get(sites));

    let private = Router::new()
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
        .route("/api/hosts/{hostname}/env", get(list_env))
        .route("/api/hosts/{hostname}/env/{name}", put(put_env).delete(delete_env))
        .route("/api/hosts/{hostname}/stats/query", post(stats_query))
        .route("/api/hosts/{hostname}/stats/current-visitors", get(stats_current_visitors))
        .route("/api/hosts/{hostname}/stats/suggestions/{filter}", get(stats_suggestions))
        .route("/api/stats/sparklines", get(sparklines))
        .route("/api/users", get(list_users).post(create_user))
        .route("/api/users/{pubkey}", patch(update_user).delete(delete_user))
        .route("/api/settings", get(get_settings).put(put_settings))
        .route("/api/audit-log", get(audit_log))
        .route("/api/status", get(status_route))
        .route_layer(axum::middleware::from_fn_with_state(deps.clone(), authenticate));

    public
        .merge(private)
        // Unmatched API paths must not fall through to the SPA: an API client
        // asking for a route that doesn't exist should get JSON.
        .fallback(|| async { ApiError::NotFound("No such API route.".into()) })
        .layer(axum::middleware::from_fn_with_state(deps.clone(), gate))
        .with_state(deps)
}

/// CORS and the body limit, in front of every route, and the log line for
/// any request that failed inside.
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
    let too_large = || with_cors(json_reply(StatusCode::PAYLOAD_TOO_LARGE, json!({ "error": "Request body is too large." })));
    if declared.is_some_and(|d| d > MAX_BODY_SIZE) {
        return too_large();
    }
    let Ok(collected) = http_body_util::Limited::new(body, MAX_BODY_SIZE).collect().await else {
        return too_large();
    };
    let bytes = collected.to_bytes();

    let route = parts.extensions.get::<MatchedPath>().map_or_else(|| parts.uri.path().to_string(), |p| p.as_str().to_string());
    parts.extensions.insert(RawBody(bytes.clone()));
    let response = next.run(Request::from_parts(parts, Body::from(bytes))).await;

    if let Some(Failure(error)) = response.extensions().get::<Failure>() {
        deps.log.error("request_failed", with_err(fields! { "path" => route }, error));
    }
    with_cors(response)
}

/// NIP-98, from a pubkey with a row in `users`, on every private route.
async fn authenticate(State(deps): State<Deps>, Extension(body): Extension<RawBody>, request: Request, next: Next) -> Reply {
    let (mut parts, body_stream) = request.into_parts();
    let path_and_query = parts.uri.path_and_query().map_or(parts.uri.path(), |pq| pq.as_str());
    let event = match verify_nip98(
        parts.headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()),
        parts.method.as_str(),
        path_and_query,
        &body.0,
        &deps.dashboard_host,
        crate::util::now(),
    ) {
        Ok(event) => event,
        Err(e) => return json_reply(StatusCode::UNAUTHORIZED, json!({ "error": e.0 })),
    };

    let pubkey = event.pubkey;
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
            next.run(Request::from_parts(parts, body_stream)).await
        }
        Ok(None) => ApiError::Forbidden("This account has no access to npanel. Ask an admin to add you.").into_response(),
        Err(e) => ApiError::Internal(e).into_response(),
    }
}

fn with_cors(mut response: Reply) -> Reply {
    response.headers_mut().insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, HeaderValue::from_static("*"));
    response
}

/// Who runs this instance, for the logged-out homepage. Nothing here is
/// private: the contact npub is published by an admin to be shown.
async fn instance(State(deps): State<Deps>) -> ApiResult {
    let contact = deps.settings.get().contact_npub.clone();
    ok(json!({
        "contact_npub": if contact.is_empty() { Value::Null } else { Value::String(contact) },
        "dashboard_host": deps.dashboard_host,
    }))
}

/// The sites this gateway serves, for the logged-out homepage. Empty unless
/// an admin turned `public_site_list` on.
async fn sites(State(deps): State<Deps>) -> ApiResult {
    #[derive(serde::Serialize)]
    struct Card {
        hostname: String,
        #[serde(flatten)]
        preview: SitePreview,
    }

    let public = deps.settings.get().public_site_list;
    let previews = deps.previews.clone();
    let cards = deps
        .db
        .call(move |conn| {
            let mut cards = Vec::new();
            for host in listed_hosts(conn, public)? {
                let build = current_build(conn, host.id, BuildKind::Clearnet)?;
                let preview = previews.preview(conn, &host, build.as_ref())?;
                cards.push(Card { hostname: host.hostname, preview });
            }
            Ok(cards)
        })
        .await?;
    ok(json!({ "sites": cards }))
}

fn user_json(pubkey: &str, role: Role) -> Value {
    json!({ "pubkey": pubkey, "npub": nip19::npub_encode(pubkey), "role": role })
}

async fn me(user: AuthUser) -> ApiResult {
    ok(user_json(&user.pubkey, user.role))
}

/// Where staff should point a hostname's DNS, shown when they add one.
async fn dns(State(deps): State<Deps>) -> ApiResult {
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

/// Load a host the user may manage, or the error saying why not.
async fn accessible_host(deps: &Deps, user: &AuthUser, hostname: &str) -> Result<Host, ApiError> {
    enum Access {
        Missing,
        Denied,
        Granted(Host),
    }

    let (name, u) = (hostname.to_string(), user.clone());
    let access = deps
        .db
        .call(move |conn| {
            let Some(host) = load_host(conn, &name)? else { return Ok(Access::Missing) };
            Ok(if can_access(conn, &u, host.id)? { Access::Granted(host) } else { Access::Denied })
        })
        .await?;
    match access {
        Access::Granted(host) => Ok(host),
        Access::Missing => Err(ApiError::NotFound("Host not found.".into())),
        Access::Denied => Err(ApiError::Forbidden("You don't have access to this host.")),
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

/// Whether a write failed on a `UNIQUE` constraint: two requests racing to
/// claim one name, where the loser should hear it is taken rather than that
/// something broke.
fn is_unique_violation(error: &rusqlite::Error) -> bool {
    matches!(error, rusqlite::Error::SqliteFailure(e, _) if e.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE)
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

/// What the API shows about a host beyond its own row.
struct HostDetails {
    build: Option<HostBuild>,
    latest: Option<SiteEvent>,
    onion: Option<String>,
    active: Option<SiteEvent>,
    preview: SitePreview,
}

fn host_details(conn: &Connection, previews: &SitePreviews, host: &Host) -> anyhow::Result<HostDetails> {
    let build = current_build(conn, host.id, BuildKind::Clearnet)?;
    let latest = newest_event(conn, &host.address())?;
    let onion = conn.prepare_cached("SELECT address FROM onion_services WHERE host_id = ?")?.query_row([host.id], |r| r.get(0)).optional()?;
    let active = match &build {
        Some(build) => site_event(conn, &build.event_id)?,
        None => None,
    };
    let preview = previews.preview(conn, host, build.as_ref())?;
    Ok(HostDetails { build, latest, onion, active, preview })
}

/// A host as the API shows it, including its deploy state: the version its
/// build serves (which the dashboard calls active), the newest version, and
/// the policy it is actually served under.
async fn host_json(deps: &Deps, host: &Host, details: HostDetails) -> anyhow::Result<Value> {
    // The same question the serving path asks, answered the same way.
    let declared = match (&details.build, host.csp) {
        (Some(build), true) => deps.site_csp.policy(build.id).await?,
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
        "enabled": host.enabled,
        "hidden": host.hidden,
        "redirect_www": host.redirect_www,
        "spa": host.spa,
        "csp": host.csp,
        "csp_policy": host.csp.then(|| declared.as_ref().and_then(|policy| policy.to_str().ok()).unwrap_or(GATEWAY_CSP)),
        "csp_source": if !host.csp { "off" } else if declared.is_some() { "site" } else { "gateway" },
        "seo": host.seo,
        "merge_deployments": host.merge_deployments,
        "analytics": host.analytics,
        "onion": details.onion,
        "pinned_event_id": host.pinned_event_id,
        "created_by": host.created_by,
        "created_at": host.created_at,
        "updated_at": host.updated_at,
        "active": details.active.as_ref().map(version_json),
        "latest": details.latest.as_ref().map(version_json),
        "preview": details.preview,
    }))
}

async fn serialize_host(deps: &Deps, host: Host) -> anyhow::Result<Value> {
    let previews = deps.previews.clone();
    let h = host.clone();
    let details = deps.db.call(move |conn| host_details(conn, &previews, &h)).await?;
    host_json(deps, &host, details).await
}

/// The fields of a host the audit log records, as the table stores them —
/// flags included, which it has always recorded as 0 and 1.
fn host_summary(host: &Host) -> Map<String, Value> {
    let mut summary = Map::new();
    for (key, value) in [
        ("hostname", json!(host.hostname)),
        ("pubkey", json!(host.pubkey)),
        ("kind", json!(host.kind)),
        ("identifier", json!(host.identifier)),
        ("enabled", json!(i64::from(host.enabled))),
        ("hidden", json!(i64::from(host.hidden))),
        ("redirect_www", json!(i64::from(host.redirect_www))),
        ("spa", json!(i64::from(host.spa))),
        ("csp", json!(i64::from(host.csp))),
        ("seo", json!(i64::from(host.seo))),
        ("merge_deployments", json!(i64::from(host.merge_deployments))),
        ("analytics", json!(i64::from(host.analytics))),
        ("pinned_event_id", json!(host.pinned_event_id)),
    ] {
        summary.insert(key.into(), value);
    }
    summary
}

async fn list_hosts(State(deps): State<Deps>, user: AuthUser) -> ApiResult {
    let public = deps.settings.get().public_site_list;
    let (u, previews) = (user.clone(), deps.previews.clone());
    let (hosts, others) = deps
        .db
        .call(move |conn| {
            let hosts: Vec<Host> = if u.is_admin() {
                conn.prepare_cached(&format!("SELECT {HOST_COLUMNS} FROM hosts ORDER BY hostname"))?
                    .query_map([], Host::from_row)?
                    .collect::<rusqlite::Result<_>>()?
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
            // Every host's details in this one trip to the database, rather
            // than a trip per host.
            let hosts = hosts
                .into_iter()
                .map(|host| {
                    let details = host_details(conn, &previews, &host)?;
                    Ok((host, details))
                })
                .collect::<anyhow::Result<Vec<_>>>()?;
            Ok((hosts, others))
        })
        .await?;

    let mut list = Vec::with_capacity(hosts.len());
    for (host, details) in hosts {
        list.push(host_json(&deps, &host, details).await?);
    }
    ok(json!({ "hosts": list, "other_sites": others.into_iter().map(|h| json!({ "hostname": h })).collect::<Vec<_>>() }))
}

/// `null` and absent are different answers for `pinned_event_id`: one
/// unpins, the other leaves the pin alone.
fn present<'de, D: Deserializer<'de>, T: Deserialize<'de>>(deserializer: D) -> Result<Option<Option<T>>, D::Error> {
    Option::<T>::deserialize(deserializer).map(Some)
}

/// The switches on a host, each left alone when absent.
#[derive(Deserialize, Default)]
#[serde(default)]
struct Flags {
    enabled: Option<bool>,
    hidden: Option<bool>,
    redirect_www: Option<bool>,
    spa: Option<bool>,
    csp: Option<bool>,
    seo: Option<bool>,
    merge_deployments: Option<bool>,
    analytics: Option<bool>,
}

impl Flags {
    /// Each flag that was given, by column.
    fn given(&self) -> impl Iterator<Item = (&'static str, bool)> {
        [
            ("enabled", self.enabled),
            ("hidden", self.hidden),
            ("redirect_www", self.redirect_www),
            ("spa", self.spa),
            ("csp", self.csp),
            ("seo", self.seo),
            ("merge_deployments", self.merge_deployments),
            ("analytics", self.analytics),
        ]
        .into_iter()
        .filter_map(|(column, value)| Some((column, value?)))
    }
}

async fn create_host(State(deps): State<Deps>, user: AuthUser, Extension(body): Extension<RawBody>) -> ApiResult {
    #[derive(Deserialize)]
    struct NewHost {
        hostname: String,
        address: String,
        #[serde(flatten)]
        flags: Flags,
    }
    let NewHost { hostname, address, flags } = body.json("Expected a JSON body with hostname and address.")?;

    let hostname = parse_hostname(&hostname, &deps.settings.get().allowed_host_suffixes).map_err(bad)?;
    let address = parse_nsite_address(&address).map_err(bad)?;
    if hostname == deps.dashboard_host {
        return Err(ApiError::Conflict("That domain serves the dashboard itself.".into()));
    }

    // Off unless asked for, or on unless asked otherwise, flag by flag. A new
    // host is always enabled.
    let hidden = flags.hidden.unwrap_or(false);
    let redirect_www = flags.redirect_www.unwrap_or(true);
    let spa = flags.spa.unwrap_or(false);
    let csp = flags.csp.unwrap_or(true);
    let seo = flags.seo.unwrap_or(true);
    let merge = flags.merge_deployments.unwrap_or(false);
    let analytics = flags.analytics.unwrap_or(false);

    let (u, name, addr) = (user.clone(), hostname.clone(), address.clone());
    let created = deps
        .db
        .call(move |conn| {
            let now = crate::util::now();
            let tx = conn.transaction()?;
            let inserted = tx.execute(
                "INSERT INTO hosts (hostname, pubkey, kind, identifier, pinned_event_id, enabled, hidden, redirect_www, spa, csp, seo,
                   merge_deployments, analytics, created_by, created_at, updated_at)
                 VALUES (?, ?, ?, ?, NULL, 1, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                params![name, addr.pubkey, addr.kind, addr.identifier, hidden, redirect_www, spa, csp, seo, merge, analytics, u.pubkey, now, now],
            );
            match inserted {
                Err(error) if is_unique_violation(&error) => return Ok(None),
                inserted => inserted?,
            };
            let id = tx.last_insert_rowid();
            // Whoever adds a host can manage it, so staff aren't locked out of
            // their own work.
            if !u.is_admin() {
                tx.execute("INSERT INTO host_grants (user_pubkey, host_id, created_at) VALUES (?, ?, ?)", params![u.pubkey, id, now])?;
            }
            audit(&tx, &u.pubkey, "host.create", Some(&name), None, Some(&json!({ "hostname": name, "address": site_key(&addr) })))?;
            tx.commit()?;
            Ok(host_by_id(conn, id)?)
        })
        .await?;
    let Some(host) = created else {
        return Err(ApiError::Conflict(format!("{hostname} is already configured.")));
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

    Ok(json_reply(StatusCode::CREATED, json!({ "host": serialize_host(&deps, host).await? })))
}

async fn get_host(State(deps): State<Deps>, user: AuthUser, Path(hostname): Path<String>) -> ApiResult {
    let host = accessible_host(&deps, &user, &hostname).await?;
    ok(json!({ "host": serialize_host(&deps, host).await? }))
}

/// One column of a host to change, as SQLite will store it.
type Change = (&'static str, rusqlite::types::Value);

async fn update_host(State(deps): State<Deps>, user: AuthUser, Extension(body): Extension<RawBody>, Path(hostname): Path<String>) -> ApiResult {
    use rusqlite::types::Value as Sql;

    #[derive(Deserialize)]
    struct HostPatch {
        #[serde(default)]
        hostname: Option<String>,
        #[serde(default)]
        address: Option<String>,
        #[serde(default, deserialize_with = "present")]
        pinned_event_id: Option<Option<String>>,
        #[serde(flatten)]
        flags: Flags,
    }

    let host = accessible_host(&deps, &user, &hostname).await?;
    let request: HostPatch = body.json("Expected a JSON body.")?;

    // Changed columns, in the order the audit log's summary lists them.
    let mut patch: Vec<Change> = Vec::new();
    let mut renamed = None;

    if let Some(new_hostname) = &request.hostname {
        let parsed = parse_hostname(new_hostname, &deps.settings.get().allowed_host_suffixes).map_err(bad)?;
        if parsed == deps.dashboard_host {
            return Err(ApiError::Conflict("That domain serves the dashboard itself.".into()));
        }
        if parsed != host.hostname {
            patch.push(("hostname", Sql::Text(parsed.clone())));
            renamed = Some(parsed);
        }
    }

    if let Some(new_address) = &request.address {
        let address = parse_nsite_address(new_address).map_err(bad)?;
        patch.push(("pubkey", Sql::Text(address.pubkey)));
        patch.push(("kind", Sql::Integer(address.kind)));
        patch.push(("identifier", Sql::Text(address.identifier)));
        // A retarget invalidates a pin that belongs to the old site.
        patch.push(("pinned_event_id", Sql::Null));
    }

    for (flag, value) in request.flags.given() {
        patch.push((flag, Sql::Integer(i64::from(value))));
    }

    if let (Some(pinned), None) = (request.pinned_event_id, &request.address) {
        match pinned {
            None => patch.push(("pinned_event_id", Sql::Null)),
            Some(event_id) => {
                let version = deps.db.call(move |conn| Ok(site_event(conn, &event_id)?)).await?;
                let Some(version) = version.filter(|v| v.address() == host.address()) else {
                    return Err(bad("That version belongs to a different site."));
                };
                if version.status != DeployStatus::Ready {
                    return Err(ApiError::Conflict("That version isn't fully downloaded yet.".into()));
                }
                patch.push(("pinned_event_id", Sql::Text(version.event_id)));
            }
        }
    }

    if patch.is_empty() {
        return ok(json!({ "host": serialize_host(&deps, host).await? }));
    }

    let summary = host_summary(&host);
    let mut after = summary.clone();
    for (column, value) in &patch {
        let value = match value {
            Sql::Text(text) => json!(text),
            Sql::Integer(number) => json!(number),
            _ => Value::Null,
        };
        after.insert(column.to_string(), value);
    }

    let (h, p, actor) = (host.clone(), patch.clone(), user.pubkey.clone());
    let updated = deps
        .db
        .call(move |conn| {
            let tx = conn.transaction()?;
            for (column, value) in &p {
                match tx.execute(&format!("UPDATE hosts SET {column} = ? WHERE id = ?"), params![value, h.id]) {
                    // Renamed onto a hostname another host has.
                    Err(error) if is_unique_violation(&error) => return Ok(Err(())),
                    result => result?,
                };
            }
            tx.execute("UPDATE hosts SET updated_at = ? WHERE id = ?", params![crate::util::now(), h.id])?;
            audit(&tx, &actor, "host.update", Some(&h.hostname), Some(&Value::Object(summary)), Some(&Value::Object(after)))?;
            tx.commit()?;
            Ok(Ok(host_by_id(conn, h.id)?))
        })
        .await?;
    let updated = match updated {
        Ok(Some(host)) => host,
        Ok(None) => return Err(ApiError::NotFound("Host not found.".into())),
        Err(()) => return Err(ApiError::Conflict(format!("{} is already configured.", renamed.unwrap_or_default()))),
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

    ok(json!({ "host": serialize_host(&deps, updated).await? }))
}

async fn delete_host(State(deps): State<Deps>, user: AuthUser, Path(hostname): Path<String>) -> ApiResult {
    let host = accessible_host(&deps, &user, &hostname).await?;
    let (h, actor) = (host.clone(), user.pubkey.clone());
    deps.db
        .call(move |conn| {
            conn.execute("DELETE FROM hosts WHERE id = ?", [h.id])?;
            audit(conn, &actor, "host.delete", Some(&h.hostname), Some(&Value::Object(host_summary(&h))), None)?;
            Ok(())
        })
        .await?;

    deps.log.info("host_deleted", fields! { "hostname" => host.hostname.as_str(), "actor" => user.pubkey.as_str() });
    deps.serve_cache.clear();
    deps.deployments.hosts_changed();
    deps.tor.hosts_changed();
    deps.analytics.hosts_changed().await;
    ok(json!({ "ok": true }))
}

/// A numeric query parameter, within bounds: `default` when it is absent or
/// isn't a number, and truncated when it has a fraction.
fn clamp(value: Option<&String>, default: i64, min: i64, max: i64) -> i64 {
    value
        .and_then(|v| v.trim().parse::<f64>().ok())
        .filter(|n| n.is_finite())
        .map_or(default, |n| n.trunc() as i64)
        .clamp(min, max)
}

async fn versions(State(deps): State<Deps>, user: AuthUser, Path(hostname): Path<String>, Query(query): Query<HashMap<String, String>>) -> ApiResult {
    let host = accessible_host(&deps, &user, &hostname).await?;
    let limit = clamp(query.get("limit"), 50, 1, 200);
    let body = deps
        .db
        .call(move |conn| {
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
        .await?;
    ok(body)
}

async fn version_log(State(deps): State<Deps>, user: AuthUser, Path((hostname, event_id)): Path<(String, String)>) -> ApiResult {
    let host = accessible_host(&deps, &user, &hostname).await?;
    let body = deps
        .db
        .call(move |conn| {
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
        .await?;
    body.map_or_else(|| Err(ApiError::NotFound("That version belongs to a different site.".into())), ok)
}

async fn files(State(deps): State<Deps>, user: AuthUser, Path(hostname): Path<String>, Query(query): Query<HashMap<String, String>>) -> ApiResult {
    let host = accessible_host(&deps, &user, &hostname).await?;
    let requested = query.get("event_id").filter(|e| !e.is_empty()).cloned();

    let body = deps
        .db
        .call(move |conn| {
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
        .await?;

    body.map_or_else(|| Err(ApiError::NotFound("No version to list.".into())), ok)
}

async fn sync(State(deps): State<Deps>, user: AuthUser, Path(hostname): Path<String>) -> ApiResult {
    let host = accessible_host(&deps, &user, &hostname).await?;
    // Relay round trips take seconds; the dashboard polls the host for status.
    deps.deployments.sync_site(host.address());
    Ok(json_reply(StatusCode::ACCEPTED, json!({ "ok": true })))
}

async fn get_grants(State(deps): State<Deps>, _: Admin, Path(hostname): Path<String>) -> ApiResult {
    let grants = deps
        .db
        .call(move |conn| {
            let Some(host) = load_host(conn, &hostname)? else { return Ok(None) };
            let grants: Vec<Value> = conn
                .prepare_cached("SELECT user_pubkey, created_at FROM host_grants WHERE host_id = ?")?
                .query_map([host.id], |r| Ok(json!({ "user_pubkey": r.get::<_, String>(0)?, "created_at": r.get::<_, i64>(1)? })))?
                .collect::<rusqlite::Result<_>>()?;
            Ok(Some(grants))
        })
        .await?;
    match grants {
        Some(grants) => ok(json!({ "grants": grants })),
        None => Err(ApiError::NotFound("Host not found.".into())),
    }
}

async fn put_grants(State(deps): State<Deps>, Admin(user): Admin, Extension(body): Extension<RawBody>, Path(hostname): Path<String>) -> ApiResult {
    #[derive(Deserialize)]
    struct Grants {
        pubkeys: Vec<String>,
    }
    enum Outcome {
        Saved,
        NoHost,
        NotUsers(Vec<String>),
    }

    let Grants { pubkeys } = body.json("Expected a JSON body with a pubkeys array.")?;
    let pubkeys = crate::nostr::service::dedupe(pubkeys.into_iter().map(|p| p.to_lowercase()));
    if pubkeys.iter().any(|p| !crate::nostr::is_hex64(p)) {
        return Err(bad("Grants must be hex pubkeys."));
    }

    let (actor, keys) = (user.pubkey.clone(), pubkeys.clone());
    let outcome = deps
        .db
        .call(move |conn| {
            let Some(host) = load_host(conn, &hostname)? else { return Ok(Outcome::NoHost) };
            let mut missing = Vec::new();
            for pubkey in &keys {
                let known: Option<String> = conn.query_row("SELECT pubkey FROM users WHERE pubkey = ?", [pubkey], |r| r.get(0)).optional()?;
                if known.is_none() {
                    missing.push(pubkey.clone());
                }
            }
            if !missing.is_empty() {
                return Ok(Outcome::NotUsers(missing));
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
            Ok(Outcome::Saved)
        })
        .await?;

    match outcome {
        Outcome::Saved => ok(json!({ "grants": pubkeys })),
        Outcome::NotUsers(missing) => Err(bad(format!("Not npanel users: {}", missing.join(", ")))),
        Outcome::NoHost => Err(ApiError::NotFound("Host not found.".into())),
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

/// What a host's NIP-05 name points at, as it is stored.
fn nip05_entry(conn: &Connection, host_id: i64, name: &str) -> rusqlite::Result<Option<(String, String)>> {
    conn.query_row("SELECT pubkey, relays FROM nip05_names WHERE host_id = ? AND name = ?", params![host_id, name], |row| Ok((row.get(0)?, row.get(1)?)))
        .optional()
}

async fn list_nip05(State(deps): State<Deps>, user: AuthUser, Path(hostname): Path<String>) -> ApiResult {
    let host = accessible_host(&deps, &user, &hostname).await?;
    let names = deps.db.call(move |conn| Ok(nip05_list(conn, host.id)?)).await?;
    ok(json!({ "names": names }))
}

/// Add a name, or replace whatever was stored under it. A name is a setting
/// of the site, so anyone with a grant on the site manages it.
async fn put_nip05(
    State(deps): State<Deps>,
    user: AuthUser,
    Extension(body): Extension<RawBody>,
    Path((hostname, name)): Path<(String, String)>,
) -> ApiResult {
    #[derive(Deserialize)]
    struct Entry {
        pubkey: String,
        #[serde(default)]
        relays: Vec<String>,
    }

    let host = accessible_host(&deps, &user, &hostname).await?;
    let Entry { pubkey, relays } = body.json("Expected a JSON body with a pubkey.")?;
    if relays.len() > MAX_NIP05_RELAYS {
        return Err(bad(format!("A name can list at most {MAX_NIP05_RELAYS} relays.")));
    }

    let name = parse_nip05_name(&name).map_err(bad)?;
    let pubkey = parse_pubkey(&pubkey).map_err(bad)?;
    // Deduplicated after normalizing, so the same relay written two ways is
    // stored once.
    let relays = crate::nostr::service::dedupe(relays.iter().map(|r| parse_relay_url(r)).collect::<Result<Vec<_>, _>>().map_err(bad)?);

    let (actor, n, p, r, h) = (user.pubkey.clone(), name.clone(), pubkey.clone(), relays.clone(), host.clone());
    let (replaced, names) = deps
        .db
        .call(move |conn| {
            let previous = nip05_entry(conn, h.id, &n)?;
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
        .await?;

    deps.log.info(
        "nip05_name_set",
        fields! { "hostname" => host.hostname.as_str(), "name" => name.as_str(), "pubkey" => pubkey.as_str(), "actor" => user.pubkey.as_str() },
    );
    Ok(json_reply(if replaced { StatusCode::OK } else { StatusCode::CREATED }, json!({ "names": names })))
}

async fn delete_nip05(State(deps): State<Deps>, user: AuthUser, Path((hostname, name)): Path<(String, String)>) -> ApiResult {
    let host = accessible_host(&deps, &user, &hostname).await?;
    let name = parse_nip05_name(&name).map_err(bad)?;

    let (actor, n, h) = (user.pubkey.clone(), name.clone(), host.clone());
    let names = deps
        .db
        .call(move |conn| {
            let Some((pubkey, relays)) = nip05_entry(conn, h.id, &n)? else { return Ok(None) };
            conn.execute("DELETE FROM nip05_names WHERE host_id = ? AND name = ?", params![h.id, n])?;
            audit(conn, &actor, "nip05.delete", Some(&format!("{n}@{}", h.hostname)), Some(&json!({ "pubkey": pubkey, "relays": parse_relay_list(&relays) })), None)?;
            Ok(Some(nip05_list(conn, h.id)?))
        })
        .await?;

    let Some(names) = names else {
        return Err(ApiError::NotFound(format!("{} has no name {name}.", host.hostname)));
    };
    deps.log.info("nip05_name_deleted", fields! { "hostname" => host.hostname.as_str(), "name" => name.as_str(), "actor" => user.pubkey.as_str() });
    ok(json!({ "names": names }))
}

/// A host's variables, in the shape every route here returns them.
fn env_list(conn: &Connection, host_id: i64) -> rusqlite::Result<Vec<Value>> {
    conn.prepare_cached("SELECT name, value, created_at, updated_at FROM host_env WHERE host_id = ? ORDER BY name")?
        .query_map([host_id], |r| {
            Ok(json!({
                "name": r.get::<_, String>(0)?,
                "value": r.get::<_, String>(1)?,
                "created_at": r.get::<_, i64>(2)?,
                "updated_at": r.get::<_, i64>(3)?,
            }))
        })?
        .collect()
}

/// The value a host's variable holds.
fn env_value(conn: &Connection, host_id: i64, name: &str) -> rusqlite::Result<Option<String>> {
    conn.query_row("SELECT value FROM host_env WHERE host_id = ? AND name = ?", params![host_id, name], |row| row.get(0)).optional()
}

async fn list_env(State(deps): State<Deps>, user: AuthUser, Path(hostname): Path<String>) -> ApiResult {
    let host = accessible_host(&deps, &user, &hostname).await?;
    let variables = deps.db.call(move |conn| Ok(env_list(conn, host.id)?)).await?;
    ok(json!({ "variables": variables }))
}

/// Add a variable, or replace the value stored under its name. Like a NIP-05
/// name, it is a setting of the site, so anyone with a grant on the site
/// manages it.
async fn put_env(
    State(deps): State<Deps>,
    user: AuthUser,
    Extension(body): Extension<RawBody>,
    Path((hostname, name)): Path<(String, String)>,
) -> ApiResult {
    #[derive(Deserialize)]
    struct Variable {
        value: String,
    }

    let host = accessible_host(&deps, &user, &hostname).await?;
    let Variable { value } = body.json("Expected a JSON body with a value.")?;
    let name = parse_env_name(&name).map_err(bad)?;
    let value = parse_env_value(&value).map_err(bad)?;

    let (actor, n, v, h) = (user.pubkey.clone(), name.clone(), value.clone(), host.clone());
    let result = deps
        .db
        .call(move |conn| {
            let tx = conn.transaction()?;
            let previous = env_value(&tx, h.id, &n)?;
            let now = crate::util::now();
            match &previous {
                Some(_) => {
                    tx.execute("UPDATE host_env SET value = ?, updated_at = ? WHERE host_id = ? AND name = ?", params![v, now, h.id, n])?;
                }
                None => {
                    let count: i64 = tx.query_row("SELECT COUNT(*) FROM host_env WHERE host_id = ?", [h.id], |row| row.get(0))?;
                    if count as usize >= MAX_VARIABLES {
                        return Ok(None);
                    }
                    tx.execute("INSERT INTO host_env (host_id, name, value, created_at, updated_at) VALUES (?, ?, ?, ?, ?)", params![h.id, n, v, now, now])?;
                }
            }
            let old = previous.as_ref().map(|old| json!({ "name": n, "value": old }));
            audit(&tx, &actor, "env.set", Some(&h.hostname), old.as_ref(), Some(&json!({ "name": n, "value": v })))?;
            tx.commit()?;
            Ok(Some((previous.is_some(), env_list(conn, h.id)?)))
        })
        .await?;

    let Some((replaced, variables)) = result else {
        return Err(bad(format!("A site can have at most {MAX_VARIABLES} variables. Remove one before adding another.")));
    };
    deps.log.info("env_set", fields! { "hostname" => host.hostname.as_str(), "name" => name.as_str(), "actor" => user.pubkey.as_str() });
    deps.deployments.rebuild(vec![host.id]);
    Ok(json_reply(if replaced { StatusCode::OK } else { StatusCode::CREATED }, json!({ "variables": variables })))
}

async fn delete_env(State(deps): State<Deps>, user: AuthUser, Path((hostname, name)): Path<(String, String)>) -> ApiResult {
    let host = accessible_host(&deps, &user, &hostname).await?;
    let name = parse_env_name(&name).map_err(bad)?;

    let (actor, n, h) = (user.pubkey.clone(), name.clone(), host.clone());
    let variables = deps
        .db
        .call(move |conn| {
            let Some(value) = env_value(conn, h.id, &n)? else { return Ok(None) };
            conn.execute("DELETE FROM host_env WHERE host_id = ? AND name = ?", params![h.id, n])?;
            audit(conn, &actor, "env.delete", Some(&h.hostname), Some(&json!({ "name": n, "value": value })), None)?;
            Ok(Some(env_list(conn, h.id)?))
        })
        .await?;

    let Some(variables) = variables else {
        return Err(ApiError::NotFound(format!("{} has no variable {name}.", host.hostname)));
    };
    deps.log.info("env_deleted", fields! { "hostname" => host.hostname.as_str(), "name" => name.as_str(), "actor" => user.pubkey.as_str() });
    deps.deployments.rebuild(vec![host.id]);
    ok(json!({ "variables": variables }))
}

fn relay(answer: Answer) -> ApiResult {
    Ok(answer.into_response())
}

async fn stats_host(deps: &Deps, user: &AuthUser, hostname: &str) -> Result<QueryHost, ApiError> {
    let host = accessible_host(deps, user, hostname).await?;
    Ok(QueryHost { id: host.id, hostname: host.hostname })
}

/// Plausible's dashboard query, answered on the analytics runtime.
async fn stats_query(State(deps): State<Deps>, user: AuthUser, Extension(body): Extension<RawBody>, Path(hostname): Path<String>) -> ApiResult {
    let host = stats_host(&deps, &user, &hostname).await?;
    let body: Value = body.json("Expected a JSON body describing the query.")?;
    relay(deps.analytics.query(host, body).await)
}

async fn stats_current_visitors(State(deps): State<Deps>, user: AuthUser, Path(hostname): Path<String>) -> ApiResult {
    let host = stats_host(&deps, &user, &hostname).await?;
    relay(deps.analytics.current_visitors(host).await)
}

async fn stats_suggestions(
    State(deps): State<Deps>,
    user: AuthUser,
    Path((hostname, filter)): Path<(String, String)>,
    Query(params): Query<HashMap<String, String>>,
) -> ApiResult {
    let host = stats_host(&deps, &user, &hostname).await?;
    relay(deps.analytics.suggestions(host, filter, params).await)
}

/// The last 24 hours of every site the user may manage, in one request.
async fn sparklines(State(deps): State<Deps>, user: AuthUser) -> ApiResult {
    let hosts = deps
        .db
        .call(move |conn| {
            let (sql, args): (&str, Vec<String>) = if user.is_admin() {
                ("SELECT id, hostname FROM hosts", vec![])
            } else {
                ("SELECT id, hostname FROM hosts WHERE id IN (SELECT host_id FROM host_grants WHERE user_pubkey = ?)", vec![user.pubkey.clone()])
            };
            Ok(conn
                .prepare(sql)?
                .query_map(rusqlite::params_from_iter(args), |r| Ok(QueryHost { id: r.get(0)?, hostname: r.get(1)? }))?
                .collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .await?;
    relay(deps.analytics.sparklines(hosts).await)
}

async fn list_users(State(deps): State<Deps>, _: Admin) -> ApiResult {
    let users = deps
        .db
        .call(|conn| {
            let grants: Vec<(String, String)> = conn
                .prepare("SELECT host_grants.user_pubkey, hosts.hostname FROM host_grants INNER JOIN hosts ON hosts.id = host_grants.host_id")?
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<_>>()?;
            let users: Vec<Value> = conn
                .prepare("SELECT pubkey, role, added_by, created_at FROM users ORDER BY created_at")?
                .query_map([], |r| {
                    let pubkey: String = r.get(0)?;
                    let hostnames: Vec<&String> = grants.iter().filter(|(p, _)| *p == pubkey).map(|(_, h)| h).collect();
                    let mut user = user_json(&pubkey, r.get(1)?);
                    user["added_by"] = json!(r.get::<_, Option<String>>(2)?);
                    user["created_at"] = json!(r.get::<_, i64>(3)?);
                    user["hostnames"] = json!(hostnames);
                    Ok(user)
                })?
                .collect::<rusqlite::Result<_>>()?;
            Ok(users)
        })
        .await?;
    ok(json!({ "users": users }))
}

async fn create_user(State(deps): State<Deps>, Admin(user): Admin, Extension(body): Extension<RawBody>) -> ApiResult {
    #[derive(Deserialize)]
    struct NewUser {
        pubkey: String,
        #[serde(default = "staff")]
        role: Role,
    }
    fn staff() -> Role {
        Role::Staff
    }

    let NewUser { pubkey, role } = body.json("Expected a JSON body with a pubkey.")?;
    let pubkey = parse_pubkey(&pubkey).map_err(bad)?;

    let (actor, p) = (user.pubkey.clone(), pubkey.clone());
    let created = deps
        .db
        .call(move |conn| {
            let inserted =
                conn.execute("INSERT INTO users (pubkey, role, added_by, created_at) VALUES (?, ?, ?, ?)", params![p, role, actor, crate::util::now()]);
            match inserted {
                Err(rusqlite::Error::SqliteFailure(e, _)) if e.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_PRIMARYKEY => return Ok(false),
                inserted => inserted?,
            };
            audit(conn, &actor, "user.create", Some(&p), None, Some(&json!({ "role": role })))?;
            Ok(true)
        })
        .await?;
    if !created {
        return Err(ApiError::Conflict("That user already has access.".into()));
    }

    deps.log.info("user_created", fields! { "pubkey" => pubkey.as_str(), "role" => role.as_str(), "actor" => user.pubkey.as_str() });
    Ok(json_reply(StatusCode::CREATED, json!({ "user": user_json(&pubkey, role) })))
}

fn admin_count(conn: &Connection) -> rusqlite::Result<i64> {
    conn.query_row("SELECT COUNT(*) FROM users WHERE role = 'admin'", [], |r| r.get(0))
}

/// What happened to a change to a user.
enum UserChange {
    Done,
    NoUser,
    LastAdmin,
}

impl UserChange {
    fn into_result(self) -> Result<(), ApiError> {
        match self {
            UserChange::Done => Ok(()),
            UserChange::NoUser => Err(ApiError::NotFound("User not found.".into())),
            UserChange::LastAdmin => Err(ApiError::Conflict("That's the last admin. Promote someone else first.".into())),
        }
    }
}

async fn update_user(State(deps): State<Deps>, Admin(user): Admin, Extension(body): Extension<RawBody>, Path(raw): Path<String>) -> ApiResult {
    #[derive(Deserialize)]
    struct Change {
        role: Role,
    }

    let pubkey = parse_pubkey(&raw).map_err(bad)?;
    let Change { role } = body.json("Expected a JSON body with a role of admin or staff.")?;

    let (actor, p) = (user.pubkey.clone(), pubkey.clone());
    deps.db
        .call(move |conn| {
            let current: Option<Role> = conn.query_row("SELECT role FROM users WHERE pubkey = ?", [&p], |row| row.get(0)).optional()?;
            let Some(current) = current else { return Ok(UserChange::NoUser) };
            if current == Role::Admin && role != Role::Admin && admin_count(conn)? == 1 {
                return Ok(UserChange::LastAdmin);
            }
            conn.execute("UPDATE users SET role = ? WHERE pubkey = ?", params![role, p])?;
            audit(conn, &actor, "user.update", Some(&p), Some(&json!({ "role": current })), Some(&json!({ "role": role })))?;
            Ok(UserChange::Done)
        })
        .await?
        .into_result()?;

    deps.log.info("user_updated", fields! { "pubkey" => pubkey.as_str(), "role" => role.as_str(), "actor" => user.pubkey.as_str() });
    ok(json!({ "user": user_json(&pubkey, role) }))
}

async fn delete_user(State(deps): State<Deps>, Admin(user): Admin, Path(raw): Path<String>) -> ApiResult {
    let pubkey = parse_pubkey(&raw).map_err(bad)?;

    let (actor, p) = (user.pubkey.clone(), pubkey.clone());
    deps.db
        .call(move |conn| {
            let current: Option<Role> = conn.query_row("SELECT role FROM users WHERE pubkey = ?", [&p], |row| row.get(0)).optional()?;
            let Some(current) = current else { return Ok(UserChange::NoUser) };
            if current == Role::Admin && admin_count(conn)? == 1 {
                return Ok(UserChange::LastAdmin);
            }
            let tx = conn.transaction()?;
            tx.execute("DELETE FROM host_grants WHERE user_pubkey = ?", [&p])?;
            tx.execute("DELETE FROM users WHERE pubkey = ?", [&p])?;
            audit(&tx, &actor, "user.delete", Some(&p), Some(&json!({ "role": current })), None)?;
            tx.commit()?;
            Ok(UserChange::Done)
        })
        .await?
        .into_result()?;

    deps.log.info("user_deleted", fields! { "pubkey" => pubkey.as_str(), "actor" => user.pubkey.as_str() });
    ok(json!({ "ok": true }))
}

async fn get_settings(State(deps): State<Deps>, _: Admin) -> ApiResult {
    ok(json!({ "settings": deps.settings.get().to_map(), "defaults": SettingsValues::default().to_map() }))
}

async fn put_settings(State(deps): State<Deps>, Admin(user): Admin, Extension(body): Extension<RawBody>) -> ApiResult {
    let body: Map<String, Value> = body.json("Expected a JSON body.")?;
    let unknown: Vec<&str> = body.keys().map(String::as_str).filter(|key| !KEYS.contains(key)).collect();
    if !unknown.is_empty() {
        return Err(bad(format!("Unknown settings: {}", unknown.join(", "))));
    }
    let changed = deps.settings.update(&body, &user.pubkey).await.map_err(bad)?;
    ok(json!({ "settings": deps.settings.get().to_map(), "changed": changed }))
}

async fn audit_log(State(deps): State<Deps>, _: Admin, Query(query): Query<HashMap<String, String>>) -> ApiResult {
    let limit = clamp(query.get("limit"), 100, 1, 500);
    let offset = clamp(query.get("offset"), 0, 0, 1_000_000);
    let entries = deps
        .db
        .call(move |conn| {
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
        .await?;
    ok(json!({ "entries": entries }))
}

/// Server-side status: aggregates, not row fetches, since the footer polls it.
async fn status_route(State(deps): State<Deps>) -> ApiResult {
    let (hosts, ready, blobs, bytes) = deps
        .db
        .call(|conn| {
            let hosts: i64 = conn.query_row("SELECT COUNT(*) FROM hosts", [], |r| r.get(0))?;
            let ready: i64 = conn.query_row("SELECT COUNT(*) FROM site_events WHERE status = ?", [DeployStatus::Ready], |r| r.get(0))?;
            let (blobs, bytes): (i64, Option<i64>) = conn.query_row("SELECT COUNT(*), SUM(size) FROM blobs", [], |r| Ok((r.get(0)?, r.get(1)?)))?;
            Ok((hosts, ready, blobs, bytes.unwrap_or(0)))
        })
        .await?;
    ok(json!({
        "hosts": hosts,
        "ready_versions": ready,
        "blobs": blobs,
        "blob_bytes": bytes,
        "relays": deps.settings.get().relays,
        "tor": deps.tor.status(),
    }))
}
