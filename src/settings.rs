//! Runtime settings, DB-backed and live-reloading.
//!
//! Admins edit these on the dashboard; they take effect without a restart.
//! Consumers subscribe with [`Settings::subscribe`] and reconfigure themselves
//! — the relay pool resubscribes, the resync timer re-arms.
//!
//! Adding a setting: a field on [`SettingsValues`], its key in [`KEYS`], a
//! default, a branch in [`parse_setting`], and — if anything needs to react to
//! it — a subscriber. Then surface it on the dashboard's Settings page.

use std::net::IpAddr;
use std::sync::Arc;

use anyhow::Result;
use parking_lot::RwLock;
use rusqlite::params;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tokio::sync::broadcast;

use crate::db::Db;
use crate::fields;
use crate::hostname::{is_hostname, normalize_hostname};
use crate::identifiers::parse_relay_url;
use crate::log::Logger;
use crate::nostr::nip19::{self, Decoded};

/// Runtime configuration, editable by admins from the dashboard.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SettingsValues {
    /// Relays used for everything: watching for manifests, and looking up
    /// relay lists (kind 10002) and Blossom server lists (kind 10063).
    pub relays: Vec<String>,
    /// Blossom servers tried when a manifest has no `server` tags and its
    /// author publishes no kind 10063 list.
    pub blossom_fallback_servers: Vec<String>,
    /// Largest single blob npanel will download, in bytes.
    pub max_file_size: i64,
    /// Largest one version of a site may be, in bytes. A site's author, not
    /// its staff, decides what a deploy downloads, so this is what stops one of
    /// them filling the gateway's disk.
    pub max_site_size: i64,
    /// Most files one version of a site may list.
    pub max_site_files: i64,
    /// How many times a version is deployed before npanel stops for now.
    pub deploy_attempts: i64,
    /// Seconds between full manifest re-fetches from relays.
    pub resync_interval: i64,
    /// What staff are told to point hostnames at. Empty means detect it.
    pub dns_targets: Vec<String>,
    /// Domains hostnames may sit under, or empty to allow any hostname.
    pub allowed_host_suffixes: Vec<String>,
    /// Certificates `/tls-check` will authorize for one hostname per hour.
    pub tls_check_limit_per_host: i64,
    /// Certificates `/tls-check` will authorize across all hostnames per hour.
    pub tls_check_limit_total: i64,
    /// Whoever runs this instance, shown on the logged-out homepage.
    pub contact_npub: String,
    /// Whether the sites this gateway serves are listed publicly.
    pub public_site_list: bool,
    /// Whether every site also gets a Tor onion address.
    pub tor_enabled: bool,
    /// The Tor control endpoint, as `tcp://host:port` or `unix:/path`.
    pub tor_control_url: String,
    /// Password for the control port, when Tor uses a `HashedControlPassword`.
    pub tor_control_password: String,
    /// The IANA time zone analytics are reported in.
    pub analytics_timezone: String,
}

impl Default for SettingsValues {
    fn default() -> Self {
        Self {
            relays: vec!["wss://relay.ditto.pub".into(), "wss://relay.dreamith.to".into(), "wss://relay.primal.net".into()],
            blossom_fallback_servers: vec![
                "https://blossom.ditto.pub/".into(),
                "https://blossom.dreamith.to/".into(),
                "https://blossom.primal.net/".into(),
            ],
            max_file_size: 100 * 1024 * 1024,
            max_site_size: 1024 * 1024 * 1024,
            max_site_files: 50_000,
            deploy_attempts: 3,
            resync_interval: 600,
            dns_targets: vec![],
            allowed_host_suffixes: vec![],
            tls_check_limit_per_host: 60,
            tls_check_limit_total: 600,
            contact_npub: String::new(),
            public_site_list: false,
            tor_enabled: false,
            // Tor's own default control port, and what compose.yaml arranges.
            tor_control_url: "tcp://127.0.0.1:9051".into(),
            tor_control_password: String::new(),
            analytics_timezone: "UTC".into(),
        }
    }
}

impl SettingsValues {
    pub fn to_map(&self) -> Map<String, Value> {
        match serde_json::to_value(self) {
            Ok(Value::Object(map)) => map,
            _ => Map::new(),
        }
    }

    fn get(&self, key: &str) -> Value {
        self.to_map().remove(key).unwrap_or(Value::Null)
    }

    fn with(&self, key: &str, value: Value) -> SettingsValues {
        let mut map = self.to_map();
        map.insert(key.to_string(), value);
        serde_json::from_value(Value::Object(map)).unwrap_or_else(|_| self.clone())
    }
}

/// Every setting, in the order they are listed and seeded.
pub const KEYS: [&str; 17] = [
    "relays",
    "blossom_fallback_servers",
    "max_file_size",
    "max_site_size",
    "max_site_files",
    "deploy_attempts",
    "resync_interval",
    "dns_targets",
    "allowed_host_suffixes",
    "tls_check_limit_per_host",
    "tls_check_limit_total",
    "contact_npub",
    "public_site_list",
    "tor_enabled",
    "tor_control_url",
    "tor_control_password",
    "analytics_timezone",
];

/// Settings that are credentials, which the audit log must not repeat.
const SECRET_SETTINGS: [&str; 1] = ["tor_control_password"];

/// The keys that changed in one write.
pub type Changed = Arc<Vec<&'static str>>;

/// Reads and writes runtime settings, caching them in memory.
pub struct Settings {
    db: Db,
    log: Option<Logger>,
    cache: RwLock<Arc<SettingsValues>>,
    changes: broadcast::Sender<Changed>,
}

impl Settings {
    /// Load settings from the database, seeding any that are missing.
    pub fn load(db: &Db, log: Option<Logger>) -> Result<Arc<Settings>> {
        let values = db.with(|conn| {
            let stored: std::collections::HashMap<String, String> = conn
                .prepare("SELECT key, value FROM settings")?
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
                .collect::<rusqlite::Result<_>>()?;

            let mut values = SettingsValues::default();
            let mut missing = Vec::new();

            for key in KEYS {
                let Some(raw) = stored.get(key) else {
                    missing.push(key);
                    continue;
                };
                let parsed = serde_json::from_str::<Value>(raw)
                    .map_err(|e| e.to_string())
                    .and_then(|value| parse_setting(key, &value));
                match parsed {
                    Ok(value) => values = values.with(key, value),
                    // A corrupt or outdated row must not stop the server from booting.
                    Err(error) => {
                        if let Some(log) = &log {
                            log.warn("setting_invalid", fields! { "key" => key, "err_msg" => error });
                        }
                    }
                }
            }

            if !missing.is_empty() {
                let now = crate::util::now();
                let tx = conn.transaction()?;
                for key in &missing {
                    tx.execute(
                        "INSERT INTO settings (key, value, updated_at) VALUES (?, ?, ?)",
                        params![key, values.get(key).to_string(), now],
                    )?;
                }
                tx.commit()?;
                if let Some(log) = &log {
                    log.info("settings_seeded", fields! { "keys" => missing.join(",") });
                }
            }

            Ok(values)
        })?;

        let (changes, _) = broadcast::channel(64);
        Ok(Arc::new(Settings { db: db.clone(), log, cache: RwLock::new(Arc::new(values)), changes }))
    }

    /// A snapshot of every setting.
    pub fn get(&self) -> Arc<SettingsValues> {
        self.cache.read().clone()
    }

    /// Subscribe to settings changes: each message is the keys one write changed.
    pub fn subscribe(&self) -> broadcast::Receiver<Changed> {
        self.changes.subscribe()
    }

    /// Validate and persist a partial update, writing one audit entry per
    /// changed key. Returns the keys that actually changed.
    ///
    /// Every key is validated before anything is written, so a bad value
    /// leaves the settings exactly as they were.
    pub async fn update(&self, patch: &Map<String, Value>, actor: &str) -> Result<Vec<&'static str>, String> {
        let current = self.get();
        let mut next = (*current).clone();
        let mut changed: Vec<(&'static str, Value, Value)> = Vec::new();

        for key in KEYS {
            let Some(raw) = patch.get(key) else {
                continue;
            };
            let value = parse_setting(key, raw)?;
            let previous = current.get(key);
            if previous == value {
                continue;
            }
            next = next.with(key, value.clone());
            changed.push((key, previous, value));
        }

        if changed.is_empty() {
            return Ok(Vec::new());
        }

        let now = crate::util::now();
        let actor = actor.to_string();
        let writer = actor.clone();
        let rows = changed.clone();
        self.db
            .call(move |conn| {
                let tx = conn.transaction()?;
                for (key, previous, value) in &rows {
                    tx.execute(
                        "INSERT INTO settings (key, value, updated_at) VALUES (?1, ?2, ?3)
                         ON CONFLICT (key) DO UPDATE SET value = ?2, updated_at = ?3",
                        params![key, value.to_string(), now],
                    )?;
                    tx.execute(
                        "INSERT INTO audit_log (actor_pubkey, action, target, old_value, new_value, created_at)
                         VALUES (?, 'settings.update', ?, ?, ?, ?)",
                        params![writer, key, audit_value(key, previous).to_string(), audit_value(key, value).to_string(), now],
                    )?;
                }
                tx.commit()?;
                Ok(())
            })
            .await
            .map_err(|e| e.to_string())?;

        *self.cache.write() = Arc::new(next);

        let keys: Vec<&'static str> = changed.iter().map(|(key, ..)| *key).collect();
        if let Some(log) = &self.log {
            log.info("settings_updated", fields! { "keys" => keys.join(","), "actor" => actor });
        }
        let _ = self.changes.send(Arc::new(keys.clone()));

        Ok(keys)
    }
}

/// A setting's value as the audit log records it: whether a secret is set,
/// rather than the secret. The log is kept forever and read by every admin.
fn audit_value(key: &str, value: &Value) -> Value {
    if SECRET_SETTINGS.contains(&key) {
        Value::Bool(value.as_str().is_some_and(|s| !s.is_empty()))
    } else {
        value.clone()
    }
}

/// Validate one setting value, returning it normalized, or a readable error
/// naming the key.
pub fn parse_setting(key: &str, value: &Value) -> Result<Value, String> {
    validate(key, value).map_err(|message| format!("Invalid value for {key}: {message}"))
}

fn validate(key: &str, value: &Value) -> Result<Value, String> {
    match key {
        "relays" => {
            let relays = strings(value, |s| parse_relay_url(s.trim()))?;
            if relays.is_empty() {
                return Err("At least one relay is required.".into());
            }
            Ok(relays.into())
        }
        "blossom_fallback_servers" => Ok(strings(value, |s| blossom_url(s.trim()))?.into()),
        "max_file_size" => integer(value, 1, 8 * 1024_i64.pow(3)),
        "max_site_size" => integer(value, 1024_i64.pow(2), 1024_i64.pow(4)),
        "max_site_files" => integer(value, 1, 1_000_000),
        "deploy_attempts" => integer(value, 1, 10),
        "resync_interval" => integer(value, 60, 86_400),
        "dns_targets" => {
            let targets = strings(value, |s| dns_target(s.trim()))?;
            if targets.len() > 8 {
                return Err("Too big: expected array to have <=8 items".into());
            }
            // A CNAME may not coexist with any other record for the same name,
            // so a hostname target has to stand alone.
            if targets.len() >= 2 && !targets.iter().all(|t| t.parse::<IpAddr>().is_ok()) {
                return Err(
                    "A domain target becomes a CNAME, which must be the only record. Use IP addresses to list several."
                        .into(),
                );
            }
            Ok(targets.into())
        }
        "allowed_host_suffixes" => Ok(strings(value, |s| host_suffix(s.trim()))?.into()),
        "tls_check_limit_per_host" => integer(value, 1, 10_000),
        "tls_check_limit_total" => integer(value, 1, 100_000),
        "contact_npub" => Ok(contact_npub(string(value)?.trim())?.into()),
        "public_site_list" | "tor_enabled" => boolean(value),
        "tor_control_url" => Ok(tor_control_url(string(value)?.trim())?.into()),
        "tor_control_password" => Ok(string(value)?.into()),
        "analytics_timezone" => Ok(time_zone(string(value)?.trim())?.into()),
        _ => Err("Unknown setting.".into()),
    }
}

fn type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn string(value: &Value) -> Result<&str, String> {
    value.as_str().ok_or_else(|| format!("Invalid input: expected string, received {}", type_name(value)))
}

fn boolean(value: &Value) -> Result<Value, String> {
    match value {
        Value::Bool(_) => Ok(value.clone()),
        other => Err(format!("Invalid input: expected boolean, received {}", type_name(other))),
    }
}

fn integer(value: &Value, min: i64, max: i64) -> Result<Value, String> {
    let Some(number) = value.as_f64() else {
        return Err(format!("Invalid input: expected number, received {}", type_name(value)));
    };
    if number.fract() != 0.0 || !number.is_finite() {
        return Err("Invalid input: expected int, received number".into());
    }
    let number = number as i64;
    if number < min {
        return Err(format!("Too small: expected number to be >={min}"));
    }
    if number > max {
        return Err(format!("Too big: expected number to be <={max}"));
    }
    Ok(number.into())
}

fn strings(value: &Value, each: impl Fn(&str) -> Result<String, String>) -> Result<Vec<String>, String> {
    let Value::Array(items) = value else {
        return Err(format!("Invalid input: expected array, received {}", type_name(value)));
    };
    items.iter().map(|item| each(string(item)?)).collect()
}

/// An `http(s)://` URL, with a trailing slash so a path prefix is kept when
/// the blob's hash is appended.
fn blossom_url(value: &str) -> Result<String, String> {
    let mut url = url::Url::parse(value).map_err(|_| format!("Invalid Blossom server URL: {value}"))?;
    if url.scheme() != "https" && url.scheme() != "http" {
        return Err(format!("Blossom server URL must be http(s): {value}"));
    }
    if !url.path().ends_with('/') {
        let path = format!("{}/", url.path());
        url.set_path(&path);
    }
    Ok(url.to_string())
}

fn host_suffix(value: &str) -> Result<String, String> {
    let suffix = normalize_hostname(value);
    if !is_hostname(&suffix) {
        return Err(format!("Not a valid domain: {value}"));
    }
    Ok(suffix)
}

fn dns_target(value: &str) -> Result<String, String> {
    if value.parse::<IpAddr>().is_ok() {
        return Ok(value.to_string());
    }
    let hostname = normalize_hostname(value);
    if !is_hostname(&hostname) {
        return Err(format!("Not an IP address or domain: {value}"));
    }
    Ok(hostname)
}

/// A person, normalized to an npub. Anything that names one is accepted, but
/// only the npub is kept.
fn contact_npub(value: &str) -> Result<String, String> {
    if value.is_empty() {
        return Ok(String::new());
    }

    let identifier = value.strip_prefix("web+nostr:").or_else(|| value.strip_prefix("nostr:")).unwrap_or(value);

    if identifier.len() == 64 && identifier.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Ok(nip19::npub_encode(&identifier.to_lowercase()));
    }

    match nip19::decode(identifier) {
        Ok(Decoded::Npub(pubkey)) | Ok(Decoded::Nprofile { pubkey, .. }) => Ok(nip19::npub_encode(&pubkey)),
        Ok(Decoded::Nsec(_)) => Err("That's a private key. Never paste an nsec anywhere.".into()),
        Ok(other) => Err(format!("A {} does not name a person. Use an npub.", other.type_name())),
        Err(_) => Err(format!("Not an npub: {value}")),
    }
}

/// Where Tor listens for control connections: `unix:/path` or `tcp://host:port`.
fn tor_control_url(value: &str) -> Result<String, String> {
    if let Some(path) = value.strip_prefix("unix:") {
        if !path.starts_with('/') {
            return Err(format!("A unix socket needs an absolute path: {value}"));
        }
        return Ok(format!("unix:{path}"));
    }

    let url = url::Url::parse(value).map_err(|_| format!("Not a Tor control address: {value}"))?;
    if url.scheme() != "tcp" {
        return Err(format!("Tor control address must be tcp:// or unix:: {value}"));
    }
    match (url.host_str(), url.port()) {
        (Some(host), Some(port)) if !host.is_empty() => Ok(format!("tcp://{host}:{port}")),
        _ => Err(format!("Tor control address needs a host and a port: {value}")),
    }
}

/// An IANA time zone, in its canonical spelling.
fn time_zone(value: &str) -> Result<String, String> {
    let zone = chrono_tz::Tz::from_str_insensitive(value)
        .map_err(|_| format!("Not a time zone: {value}. Use an IANA name like Europe/Berlin."))?;
    Ok(zone.name().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn validates() {
        assert_eq!(parse_setting("relays", &json!([" wss://r.example.com/ "])).unwrap(), json!(["wss://r.example.com"]));
        assert!(parse_setting("relays", &json!([])).unwrap_err().contains("At least one relay"));
        assert!(parse_setting("relays", &json!(["ws://r.example.com"])).is_err());
        assert_eq!(
            parse_setting("blossom_fallback_servers", &json!(["https://b.example.com/x"])).unwrap(),
            json!(["https://b.example.com/x/"])
        );
        assert!(parse_setting("resync_interval", &json!(59)).is_err());
        assert_eq!(parse_setting("resync_interval", &json!(60.0)).unwrap(), json!(60));
        assert!(parse_setting("resync_interval", &json!("60")).is_err());
        assert!(parse_setting("dns_targets", &json!(["1.2.3.4", "gw.example.com"])).is_err());
        assert_eq!(parse_setting("dns_targets", &json!(["GW.example.com."])).unwrap(), json!(["gw.example.com"]));
        assert_eq!(parse_setting("tor_control_url", &json!("tcp://127.0.0.1:9051")).unwrap(), json!("tcp://127.0.0.1:9051"));
        assert!(parse_setting("tor_control_url", &json!("tcp://127.0.0.1")).is_err());
        assert!(parse_setting("tor_control_url", &json!("unix:run/tor")).is_err());
        assert_eq!(parse_setting("analytics_timezone", &json!("europe/berlin")).unwrap(), json!("Europe/Berlin"));
        assert!(parse_setting("analytics_timezone", &json!("Mars/Olympus")).is_err());
        let hex = "3bf0c63fcb93463407af97a5e5ee64fa883d107ef9e558472c4eb9aaaefa459d";
        assert_eq!(parse_setting("contact_npub", &json!(hex)).unwrap(), json!(nip19::npub_encode(hex)));
        assert!(parse_setting("contact_npub", &json!(nip19::nsec_encode(hex))).unwrap_err().contains("private key"));
    }

    #[tokio::test]
    async fn updates_and_audits() {
        let t = crate::db::testing::temp_db();
        let settings = Settings::load(&t.db, None).unwrap();
        let mut changes = settings.subscribe();

        let patch = json!({ "resync_interval": 120, "tor_control_password": "hunter2", "max_file_size": 104857600 });
        let changed = settings.update(patch.as_object().unwrap(), "actor").await.unwrap();
        assert_eq!(changed, ["resync_interval", "tor_control_password"]);
        assert_eq!(*changes.recv().await.unwrap(), ["resync_interval", "tor_control_password"]);
        assert_eq!(settings.get().resync_interval, 120);

        let audit: Vec<(String, String)> = t
            .db
            .with(|conn| {
                Ok(conn
                    .prepare("SELECT target, new_value FROM audit_log ORDER BY id")?
                    .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                    .collect::<rusqlite::Result<_>>()?)
            })
            .unwrap();
        assert_eq!(audit, [("resync_interval".into(), "120".into()), ("tor_control_password".into(), "true".into())]);

        // A bad value changes nothing.
        let bad = json!({ "resync_interval": 300, "relays": [] });
        assert!(settings.update(bad.as_object().unwrap(), "actor").await.is_err());
        assert_eq!(settings.get().resync_interval, 120);

        // Reloads from the database.
        assert_eq!(Settings::load(&t.db, None).unwrap().get().resync_interval, 120);
    }
}
