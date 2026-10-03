//! Type-safe configuration backed by an injectable environment provider.
//!
//! npanel keeps file-based configuration deliberately tiny: only values that
//! are system-level, or that could lock an admin out of the dashboard if set
//! wrong, live here. Which ports are bound is the clearest case: whether npanel
//! terminates TLS itself, `HTTPS_PORT`, needs a restart however it is set. Relays, Blossom fallbacks, file size limits and the resync
//! interval are stored in the database and edited from the dashboard — see
//! `settings.rs`.

use std::net::IpAddr;
use std::path::{Path, PathBuf};

use anyhow::{bail, Result};

use crate::log::LogLevel;

/// The seven variables, resolved and validated.
#[derive(Clone, Debug)]
pub struct Config {
    /// Port the HTTP server listens on. With `https_port` set, plain HTTP is
    /// only for ACME challenges and redirects to HTTPS.
    pub port: u16,
    /// Address the HTTP server listens on.
    ///
    /// Loopback by default, because npanel is meant to be reachable only by the
    /// reverse proxy: it speaks plain HTTP, and certificate authorization on
    /// `tls_check_port` answers without authentication. With `https_port` set
    /// there is no proxy, and every address is the default.
    pub bind_address: IpAddr,
    /// Port serving certificate authorization for the reverse proxy. Not
    /// bound when npanel terminates TLS itself.
    pub tls_check_port: u16,
    /// Port npanel serves HTTPS on itself, getting certificates over ACME.
    /// Unset, a reverse proxy terminates TLS.
    pub https_port: Option<u16>,
    /// Hostname serving the dashboard and JSON API. Requests with any other
    /// `Host` header are served as nsites.
    pub dashboard_host: String,
    /// Minimum severity of emitted log entries.
    pub log_level: LogLevel,
    /// Directory holding the database and the blob store.
    pub data_dir: PathBuf,
}

impl Config {
    /// Read the configuration from `env`, failing with a sentence that says
    /// which variable is wrong and how.
    pub fn from_env(env: impl Fn(&str) -> Option<String>) -> Result<Self> {
        let get = |key: &str| env(key).filter(|value| !value.is_empty());

        let port = match get("PORT") {
            None => 3000,
            Some(value) => parse_port(&value).ok_or_else(|| anyhow::anyhow!("PORT must be a valid port number (1-65535)."))?,
        };

        let https_port = match get("HTTPS_PORT") {
            None => None,
            Some(value) => {
                Some(parse_port(&value).ok_or_else(|| anyhow::anyhow!("HTTPS_PORT must be a valid port number (1-65535)."))?)
            }
        };

        let bind_address = match get("BIND_ADDRESS") {
            // Facing the internet, every address: `::` takes IPv4 as well on
            // Linux unless the host has turned that off.
            None if https_port.is_some() => IpAddr::from([0u16; 8]),
            None => IpAddr::from([127, 0, 0, 1]),
            Some(value) => match value.trim().parse() {
                Ok(address) => address,
                Err(_) => bail!("BIND_ADDRESS must be an IP address, eg 127.0.0.1 or 0.0.0.0."),
            },
        };

        let tls_check_port = match get("TLS_CHECK_PORT") {
            None => 3001,
            Some(value) => parse_port(&value)
                .ok_or_else(|| anyhow::anyhow!("TLS_CHECK_PORT must be a valid port number (1-65535)."))?,
        };
        match https_port {
            Some(https_port) if https_port == port => bail!("HTTPS_PORT must be different from PORT."),
            None if tls_check_port == port => bail!("TLS_CHECK_PORT must be different from PORT."),
            _ => {}
        }

        let Some(dashboard_host) = get("DASHBOARD_HOST") else {
            bail!("DASHBOARD_HOST must be set, eg panel.example.com.");
        };
        let dashboard_host = dashboard_host.trim().to_lowercase();
        if dashboard_host.is_empty() || dashboard_host.contains(|c: char| c == '/' || c.is_whitespace()) {
            bail!("DASHBOARD_HOST must be a bare hostname, without a scheme or path.");
        }
        let dashboard_host = strip_port(&dashboard_host).to_string();

        let log_level = match get("LOG_LEVEL") {
            None => LogLevel::Info,
            Some(value) => match LogLevel::parse(&value.to_lowercase()) {
                Some(level) => level,
                None => bail!("LOG_LEVEL must be one of: debug, info, warn, error."),
            },
        };

        let data_dir = absolute(Path::new(&get("DATA_DIR").unwrap_or_else(|| "./data".into())));

        Ok(Self { port, bind_address, tls_check_port, https_port, dashboard_host, log_level, data_dir })
    }

    /// Path of the SQLite database file.
    pub fn database_path(&self) -> PathBuf {
        self.data_dir.join("npanel.sqlite3")
    }

    /// Root of the content-addressed blob store.
    pub fn blobs_dir(&self) -> PathBuf {
        self.data_dir.join("blobs")
    }
}

/// A port the way `Number.parseInt` reads one: leading digits, anything after
/// them ignored.
fn parse_port(value: &str) -> Option<u16> {
    let digits: String = value.trim_start().chars().take_while(|c| c.is_ascii_digit()).collect();
    let port: u32 = digits.parse().ok()?;
    (1..=65535).contains(&port).then_some(port as u16)
}

/// Drop a trailing `:port`.
fn strip_port(host: &str) -> &str {
    match host.rsplit_once(':') {
        Some((name, port)) if !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) => name,
        _ => host,
    }
}

fn absolute(path: &Path) -> PathBuf {
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_default().join(path)
    };

    // Lexically, like `path.resolve`: the directory need not exist yet.
    let mut out = PathBuf::new();
    for component in joined.components() {
        match component {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

/// Load `.env` from the working directory into the process environment,
/// without overriding anything already set.
pub fn load_dotenv() {
    let Ok(text) = std::fs::read_to_string(".env") else {
        return;
    };

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line);
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let mut value = value.trim();
        if value.len() >= 2
            && ((value.starts_with('"') && value.ends_with('"')) || (value.starts_with('\'') && value.ends_with('\'')))
        {
            value = &value[1..value.len() - 1];
        } else if let Some((before, _)) = value.split_once(" #") {
            value = before.trim_end();
        }
        if std::env::var_os(key).is_none() {
            // SAFETY: called from `main` before any other thread exists.
            unsafe { std::env::set_var(key, value) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn config(vars: &[(&str, &str)]) -> Result<Config> {
        let map: HashMap<String, String> = vars.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        Config::from_env(|key| map.get(key).cloned())
    }

    #[test]
    fn defaults() {
        let c = config(&[("DASHBOARD_HOST", "Panel.Example.com:443")]).unwrap();
        assert_eq!(c.port, 3000);
        assert_eq!(c.tls_check_port, 3001);
        assert_eq!(c.dashboard_host, "panel.example.com");
        assert_eq!(c.bind_address.to_string(), "127.0.0.1");
        assert_eq!(c.log_level, LogLevel::Info);
        assert_eq!(c.https_port, None);
    }

    #[test]
    fn https() {
        let c = config(&[("DASHBOARD_HOST", "x"), ("HTTPS_PORT", "443"), ("PORT", "80")]).unwrap();
        assert_eq!(c.https_port, Some(443));
        assert_eq!(c.bind_address.to_string(), "::");
        // The check port isn't bound, so it may be anything.
        assert!(config(&[("DASHBOARD_HOST", "x"), ("HTTPS_PORT", "443"), ("PORT", "3001")]).is_ok());
        assert!(config(&[("DASHBOARD_HOST", "x"), ("HTTPS_PORT", "80"), ("PORT", "80")]).is_err());
        assert!(config(&[("DASHBOARD_HOST", "x"), ("HTTPS_PORT", "0")]).is_err());
        let pinned = config(&[("DASHBOARD_HOST", "x"), ("HTTPS_PORT", "443"), ("BIND_ADDRESS", "0.0.0.0")]).unwrap();
        assert_eq!(pinned.bind_address.to_string(), "0.0.0.0");
    }

    #[test]
    fn refuses_bad_values() {
        assert!(config(&[]).is_err());
        assert!(config(&[("DASHBOARD_HOST", "https://x")]).is_err());
        assert!(config(&[("DASHBOARD_HOST", "x"), ("PORT", "0")]).is_err());
        assert!(config(&[("DASHBOARD_HOST", "x"), ("PORT", "3001")]).is_err());
        assert!(config(&[("DASHBOARD_HOST", "x"), ("BIND_ADDRESS", "localhost")]).is_err());
        assert!(config(&[("DASHBOARD_HOST", "x"), ("LOG_LEVEL", "loud")]).is_err());
    }
}
