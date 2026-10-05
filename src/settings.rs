//! Runtime settings, DB-backed and live-reloading.
//!
//! Admins edit these on the dashboard; they take effect without a restart.
//! Consumers subscribe with [`Settings::subscribe`], compare the fields they
//! care about with the last values they saw, and reconfigure themselves — the
//! relay pool resubscribes, the resync timer re-arms.
//!
//! Adding a setting: a field in the [`settings!`] list, a default, a branch in
//! [`parse_setting`], and — if anything needs to react to it — a subscriber.
//! Then surface it on the dashboard's Settings page.

use std::net::IpAddr;
use std::sync::Arc;

use anyhow::Result;
use rusqlite::params;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tokio::sync::watch;

use crate::db::Db;
use crate::fields;
use crate::hostname::{is_hostname, normalize_hostname};
use crate::identifiers::parse_relay_url;
use crate::log::Logger;
use crate::nostr::nip19::{self, Decoded};

/// Let's Encrypt's production directory, where certificates come from unless
/// an admin says otherwise.
pub const LETS_ENCRYPT: &str = "https://acme-v02.api.letsencrypt.org/directory";

/// Declares [`SettingsValues`] and everything that has to name each of its
/// fields: [`KEYS`], and reading or replacing one field by its key. One list,
/// so a setting can't be added to one of them and forgotten in another.
macro_rules! settings {
    ($($(#[$doc:meta])* $name:ident: $type:ty,)*) => {
        /// Runtime configuration, editable by admins from the dashboard.
        #[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
        pub struct SettingsValues {
            $($(#[$doc])* pub $name: $type,)*
        }

        /// Every setting, in the order they are listed and seeded.
        pub const KEYS: &[&str] = &[$(stringify!($name)),*];

        impl SettingsValues {
            /// One setting, as JSON.
            fn get(&self, key: &str) -> Value {
                match key {
                    $(stringify!($name) => serde_json::to_value(&self.$name).unwrap_or(Value::Null),)*
                    _ => Value::Null,
                }
            }

            /// Replace one setting with a value [`parse_setting`] returned.
            fn set(&mut self, key: &str, value: Value) -> Result<(), String> {
                match key {
                    $(stringify!($name) => self.$name = serde_json::from_value(value).map_err(|e| e.to_string())?,)*
                    _ => return Err("Unknown setting.".into()),
                }
                Ok(())
            }
        }
    };
}

settings! {
    /// Relays used for everything: watching for manifests, and looking up
    /// relay lists (kind 10002) and Blossom server lists (kind 10063).
    relays: Vec<String>,
    /// Blossom servers tried when a manifest has no `server` tags and its
    /// author publishes no kind 10063 list.
    blossom_fallback_servers: Vec<String>,
    /// Largest single blob npanel will download, in bytes.
    max_file_size: i64,
    /// Largest one version of a site may be, in bytes. A site's author, not
    /// its staff, decides what a deploy downloads, so this is what stops one of
    /// them filling the gateway's disk.
    max_site_size: i64,
    /// Most files one version of a site may list.
    max_site_files: i64,
    /// How many times a version is deployed before npanel stops for now.
    deploy_attempts: i64,
    /// Seconds between full manifest re-fetches from relays.
    resync_interval: i64,
    /// Ready versions kept per site when the daily prune runs, or 0 to keep
    /// every one.
    keep_versions: i64,
    /// What staff are told to point hostnames at. Empty means detect it.
    dns_targets: Vec<String>,
    /// Domains hostnames may sit under, or empty to allow any hostname.
    allowed_host_suffixes: Vec<String>,
    /// Certificates authorized for one hostname per hour: by `/tls-check`
    /// behind a proxy, or as ACME orders with `HTTPS_PORT`.
    tls_check_limit_per_host: i64,
    /// Certificates authorized across all hostnames per hour, the same way.
    tls_check_limit_total: i64,
    /// The ACME directory certificates come from, with `HTTPS_PORT`.
    acme_directory: String,
    /// Who the CA writes to about certificates, or empty for nobody.
    acme_email: String,
    /// Whoever runs this instance, shown on the logged-out homepage.
    contact_npub: String,
    /// Whether the sites this gateway serves are listed publicly.
    public_site_list: bool,
    /// Whether every site also gets a Tor onion address.
    tor_enabled: bool,
    /// The IANA time zone analytics are reported in.
    analytics_timezone: String,
    /// Which User-Agents get a link preview, as a case-insensitive pattern.
    link_preview_crawler_pattern: String,
    /// Seconds a link preview is served before its site's script runs again.
    link_preview_ttl: i64,
    /// Link preview lookups that may run at once. Past it, a crawler gets
    /// the page as published.
    link_preview_concurrency: i64,
    /// Sites' preview scripts that may run in a minute. Cached previews don't
    /// count, so this is what a stranger making up event ids can make them
    /// do.
    link_preview_misses_per_minute: i64,
    /// Link previews kept on disk. The least recently made go first.
    link_preview_max_entries: i64,
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
            keep_versions: 20,
            dns_targets: vec![],
            allowed_host_suffixes: vec![],
            tls_check_limit_per_host: 60,
            tls_check_limit_total: 600,
            acme_directory: LETS_ENCRYPT.into(),
            acme_email: String::new(),
            contact_npub: String::new(),
            public_site_list: false,
            tor_enabled: false,
            analytics_timezone: "UTC".into(),
            link_preview_crawler_pattern: crate::link_previews::DEFAULT_CRAWLER_PATTERN.into(),
            link_preview_ttl: 3600,
            link_preview_concurrency: 256,
            link_preview_misses_per_minute: 600,
            link_preview_max_entries: 100_000,
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
}

/// Reads and writes runtime settings, caching them in memory.
pub struct Settings {
    db: Db,
    log: Option<Logger>,
    /// The current values, and the channel subscribers watch them on. A
    /// subscriber that falls behind skips to the newest values rather than
    /// missing a change.
    values: watch::Sender<Arc<SettingsValues>>,
    /// Held across a whole update, so two saves at once can't each start
    /// from the same snapshot and have the later one drop the other's change.
    writing: tokio::sync::Mutex<()>,
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

            for &key in KEYS {
                let Some(raw) = stored.get(key) else {
                    missing.push(key);
                    continue;
                };
                let parsed = serde_json::from_str::<Value>(raw)
                    .map_err(|e| e.to_string())
                    .and_then(|value| parse_setting(key, &value));
                match parsed.and_then(|value| values.set(key, value)) {
                    Ok(()) => {}
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

        Ok(Arc::new(Settings { db: db.clone(), log, values: watch::Sender::new(Arc::new(values)), writing: tokio::sync::Mutex::new(()) }))
    }

    /// A snapshot of every setting.
    pub fn get(&self) -> Arc<SettingsValues> {
        self.values.borrow().clone()
    }

    /// Watch the settings: the receiver is marked changed after every write
    /// that changed anything.
    pub fn subscribe(&self) -> watch::Receiver<Arc<SettingsValues>> {
        self.values.subscribe()
    }

    /// Validate and persist a partial update, writing one audit entry per
    /// changed key. Returns the keys that actually changed.
    ///
    /// Every key is validated before anything is written, so a bad value
    /// leaves the settings exactly as they were.
    pub async fn update(&self, patch: &Map<String, Value>, actor: &str) -> Result<Vec<&'static str>, String> {
        let _writing = self.writing.lock().await;
        let current = self.get();
        let mut next = (*current).clone();
        let mut changed: Vec<(&'static str, Value, Value)> = Vec::new();

        for &key in KEYS {
            let Some(raw) = patch.get(key) else {
                continue;
            };
            let value = parse_setting(key, raw)?;
            let previous = current.get(key);
            if previous == value {
                continue;
            }
            next.set(key, value.clone())?;
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
                        params![writer, key, previous.to_string(), value.to_string(), now],
                    )?;
                }
                tx.commit()?;
                Ok(())
            })
            .await
            .map_err(|e| e.to_string())?;

        self.values.send_replace(Arc::new(next));

        let keys: Vec<&'static str> = changed.iter().map(|(key, ..)| *key).collect();
        if let Some(log) = &self.log {
            log.info("settings_updated", fields! { "keys" => keys.join(","), "actor" => actor });
        }

        Ok(keys)
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
        "keep_versions" => integer(value, 0, 1000),
        "dns_targets" => {
            let targets = strings(value, |s| dns_target(s.trim()))?;
            if targets.len() > 8 {
                return Err("List at most 8 targets.".into());
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
        "acme_directory" => Ok(acme_directory(string(value)?.trim())?.into()),
        "acme_email" => Ok(email(string(value)?.trim())?.into()),
        "contact_npub" => Ok(contact_npub(string(value)?.trim())?.into()),
        "public_site_list" | "tor_enabled" => boolean(value),
        "analytics_timezone" => Ok(time_zone(string(value)?.trim())?.into()),
        "link_preview_crawler_pattern" => Ok(crawler_pattern(string(value)?.trim())?.into()),
        "link_preview_ttl" => integer(value, 60, 30 * 86_400),
        "link_preview_concurrency" => integer(value, 1, 4096),
        "link_preview_misses_per_minute" => integer(value, 1, 100_000),
        "link_preview_max_entries" => integer(value, 100, 10_000_000),
        _ => Err("Unknown setting.".into()),
    }
}

/// What a JSON value is, as the end of "Expected …, got …".
fn described(value: &Value) -> &'static str {
    match value {
        Value::Null => "nothing",
        Value::Bool(_) => "true or false",
        Value::Number(_) => "a number",
        Value::String(_) => "text",
        Value::Array(_) => "a list",
        Value::Object(_) => "an object",
    }
}

fn string(value: &Value) -> Result<&str, String> {
    value.as_str().ok_or_else(|| format!("Expected text, got {}.", described(value)))
}

fn boolean(value: &Value) -> Result<Value, String> {
    match value {
        Value::Bool(_) => Ok(value.clone()),
        other => Err(format!("Expected true or false, got {}.", described(other))),
    }
}

fn integer(value: &Value, min: i64, max: i64) -> Result<Value, String> {
    // Through f64, because `60.0` is as good a way to write sixty in JSON.
    let number = value.as_f64().filter(|n| n.is_finite() && n.fract() == 0.0);
    let Some(number) = number.map(|n| n as i64) else {
        return Err(format!("Expected a whole number, got {value}."));
    };
    if number < min {
        return Err(format!("Use a number of at least {min}."));
    }
    if number > max {
        return Err(format!("Use a number of at most {max}."));
    }
    Ok(number.into())
}

fn strings(value: &Value, each: impl Fn(&str) -> Result<String, String>) -> Result<Vec<String>, String> {
    let Value::Array(items) = value else {
        return Err(format!("Expected a list, got {}.", described(value)));
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

/// An ACME directory, which RFC 8555 requires be served over HTTPS.
fn acme_directory(value: &str) -> Result<String, String> {
    match url::Url::parse(value) {
        Ok(url) if url.scheme() == "https" && url.host_str().is_some() => Ok(url.to_string()),
        _ => Err(format!("Use the https:// URL of an ACME directory, eg {LETS_ENCRYPT}")),
    }
}

/// An email address, loosely: the CA is the one that decides.
fn email(value: &str) -> Result<String, String> {
    if value.is_empty() {
        return Ok(String::new());
    }
    match value.split_once('@') {
        Some((local, domain)) if !local.is_empty() && domain.contains('.') && !value.contains(char::is_whitespace) => {
            Ok(value.to_string())
        }
        _ => Err(format!("Not an email address: {value}")),
    }
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

/// A pattern User-Agents are matched against, which must compile within the
/// size every pattern is held to.
fn crawler_pattern(value: &str) -> Result<String, String> {
    if value.is_empty() {
        return Err("Enter a pattern. One that matches nothing would turn link previews off; turn them off per site instead.".into());
    }
    if value.len() > 4096 {
        return Err("Use a pattern of at most 4096 characters.".into());
    }
    crate::link_previews::crawler_pattern(value).map_err(|error| match error {
        regex::Error::CompiledTooBig(_) => "That pattern is too large. Use a shorter one.".to_string(),
        _ => format!("Not a valid pattern: {value}"),
    })?;
    Ok(value.to_string())
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
        assert_eq!(parse_setting("resync_interval", &json!(59)).unwrap_err(), "Invalid value for resync_interval: Use a number of at least 60.");
        assert_eq!(parse_setting("resync_interval", &json!(60.5)).unwrap_err(), "Invalid value for resync_interval: Expected a whole number, got 60.5.");
        assert_eq!(parse_setting("tor_enabled", &json!("yes")).unwrap_err(), "Invalid value for tor_enabled: Expected true or false, got text.");
        assert_eq!(parse_setting("resync_interval", &json!(60.0)).unwrap(), json!(60));
        assert_eq!(parse_setting("keep_versions", &json!(0)).unwrap(), json!(0));
        assert!(parse_setting("keep_versions", &json!(-1)).is_err());
        assert!(parse_setting("resync_interval", &json!("60")).is_err());
        assert!(parse_setting("dns_targets", &json!(["1.2.3.4", "gw.example.com"])).is_err());
        assert_eq!(parse_setting("dns_targets", &json!(["GW.example.com."])).unwrap(), json!(["gw.example.com"]));
        assert_eq!(parse_setting("analytics_timezone", &json!("europe/berlin")).unwrap(), json!("Europe/Berlin"));
        assert!(parse_setting("analytics_timezone", &json!("Mars/Olympus")).is_err());
        assert_eq!(parse_setting("acme_directory", &json!(" https://ca.example/dir ")).unwrap(), json!("https://ca.example/dir"));
        assert!(parse_setting("acme_directory", &json!("http://ca.example/dir")).is_err());
        assert_eq!(parse_setting("acme_email", &json!("")).unwrap(), json!(""));
        assert_eq!(parse_setting("acme_email", &json!("ops@example.com")).unwrap(), json!("ops@example.com"));
        assert!(parse_setting("acme_email", &json!("ops")).is_err());
        let hex = "3bf0c63fcb93463407af97a5e5ee64fa883d107ef9e558472c4eb9aaaefa459d";
        assert_eq!(parse_setting("contact_npub", &json!(hex)).unwrap(), json!(nip19::npub_encode(hex)));
        assert!(parse_setting("contact_npub", &json!(nip19::nsec_encode(hex))).unwrap_err().contains("private key"));
        assert_eq!(parse_setting("link_preview_crawler_pattern", &json!(" googlebot|bingbot ")).unwrap(), json!("googlebot|bingbot"));
        assert!(parse_setting("link_preview_crawler_pattern", &json!("(unclosed")).unwrap_err().contains("Not a valid pattern"));
        assert!(parse_setting("link_preview_crawler_pattern", &json!("")).is_err());
        assert!(parse_setting("link_preview_crawler_pattern", &json!("a{1000}{1000}")).unwrap_err().contains("too large"));
        assert!(parse_setting("link_preview_concurrency", &json!(0)).is_err());
    }

    #[tokio::test]
    async fn updates_and_audits() {
        let t = crate::db::testing::temp_db();
        let settings = Settings::load(&t.db, None).unwrap();
        let mut changes = settings.subscribe();

        let patch = json!({ "resync_interval": 120, "tor_enabled": true, "max_file_size": 104857600 });
        let changed = settings.update(patch.as_object().unwrap(), "actor").await.unwrap();
        assert_eq!(changed, ["resync_interval", "tor_enabled"]);
        assert!(changes.has_changed().unwrap());
        assert_eq!(changes.borrow_and_update().resync_interval, 120);
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
        assert_eq!(audit, [("resync_interval".into(), "120".into()), ("tor_enabled".into(), "true".into())]);

        // A bad value changes nothing.
        let bad = json!({ "resync_interval": 300, "relays": [] });
        assert!(settings.update(bad.as_object().unwrap(), "actor").await.is_err());
        assert_eq!(settings.get().resync_interval, 120);
        assert!(!changes.has_changed().unwrap());

        // Reloads from the database.
        assert_eq!(Settings::load(&t.db, None).unwrap().get().resync_interval, 120);
    }
}
