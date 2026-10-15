//! The configuration file: TOML, from the path `--config` names, or else the
//! first of `$XDG_CONFIG_HOME/npanel/npanel.toml` (`~/.config/...`) and
//! [`SYSTEM_PATH`] that exists. A file `--config` names has to exist, since
//! naming it meant something.
//!
//! npanel keeps it deliberately tiny: only values that are system-level, or
//! that could lock an admin out of the dashboard if set wrong, live here.
//! Which ports are bound is the clearest case: whether npanel terminates TLS
//! itself, `https_port`, needs a restart however it is set. Relays, Blossom
//! fallbacks, file size limits and the resync interval are stored in the
//! database and edited from the dashboard — see `settings.rs`.

use std::net::IpAddr;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;

use crate::log::LogLevel;

/// Where a packaged npanel's configuration is.
pub const SYSTEM_PATH: &str = "/etc/npanel/npanel.toml";
/// `data_dir` when the file doesn't say, for root.
const SYSTEM_DATA_DIR: &str = "/var/lib/npanel";

/// The file, resolved and validated.
#[derive(Clone, Debug)]
pub struct Config {
    /// The file it was read from.
    pub source: PathBuf,
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

/// The file as written. Every key is optional but `dashboard_host`, and a key
/// npanel doesn't know is refused rather than ignored, since it is a typo or
/// a setting that belongs on the dashboard.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    port: Option<u16>,
    bind_address: Option<String>,
    tls_check_port: Option<u16>,
    https_port: Option<u16>,
    dashboard_host: Option<String>,
    log_level: Option<String>,
    data_dir: Option<PathBuf>,
}

/// What the defaults depend on, read once so that tests can supply their own.
#[derive(Clone, Debug, Default)]
pub struct Environment {
    pub root: bool,
    pub home: Option<PathBuf>,
    pub xdg_config_home: Option<PathBuf>,
    pub xdg_data_home: Option<PathBuf>,
}

impl Environment {
    pub fn current() -> Environment {
        // The XDG spec has relative values ignored, and an empty one is unset.
        let absolute = |name: &str| std::env::var_os(name).map(PathBuf::from).filter(|path| path.is_absolute());
        Environment {
            // SAFETY: geteuid has no preconditions and cannot fail.
            root: unsafe { libc::geteuid() } == 0,
            home: absolute("HOME"),
            xdg_config_home: absolute("XDG_CONFIG_HOME"),
            xdg_data_home: absolute("XDG_DATA_HOME"),
        }
    }

    /// The files looked for when `--config` names none, in the order tried.
    pub fn config_candidates(&self) -> Vec<PathBuf> {
        self.user_config_dir().map(|dir| dir.join("npanel.toml")).into_iter().chain([PathBuf::from(SYSTEM_PATH)]).collect()
    }

    /// `$XDG_CONFIG_HOME/npanel`, or `~/.config/npanel`.
    pub fn user_config_dir(&self) -> Option<PathBuf> {
        let user = self.xdg_config_home.clone().or_else(|| self.home.as_ref().map(|home| home.join(".config")));
        user.map(|dir| dir.join("npanel"))
    }

    fn default_data_dir(&self) -> Result<PathBuf> {
        if self.root {
            return Ok(PathBuf::from(SYSTEM_DATA_DIR));
        }
        match self.xdg_data_home.clone().or_else(|| self.home.as_ref().map(|home| home.join(".local/share"))) {
            Some(dir) => Ok(dir.join("npanel")),
            None => bail!("Neither HOME nor XDG_DATA_HOME is set, so there is no default place to keep data. Set data_dir."),
        }
    }
}

impl Config {
    /// Read the file `path` names, or else the first of the candidates that
    /// exists, failing with a sentence that says what to do.
    pub fn load(path: Option<&Path>, env: &Environment) -> Result<Self> {
        let source = match path {
            Some(path) => path.to_path_buf(),
            // One this user can't look into, like /etc/npanel/npanel.toml to
            // anyone but root and npanel, is there all the same: reading it
            // says so, where skipping it would quietly find another.
            None => env
                .config_candidates()
                .into_iter()
                .find(|candidate| match candidate.metadata() {
                    Ok(metadata) => metadata.is_file(),
                    Err(error) => error.kind() == std::io::ErrorKind::PermissionDenied,
                })
                .ok_or_else(|| not_found(env))?,
        };
        let text = std::fs::read_to_string(&source).map_err(|error| {
            let hint = if error.kind() == std::io::ErrorKind::PermissionDenied { "; run this as root or as npanel, with sudo" } else { "" };
            anyhow!("Could not read the config file {}: {error}{hint}.", source.display())
        })?;
        let file: File = toml::from_str(&text).with_context(|| format!("{} is not a valid config file", source.display()))?;
        Self::from_file(file, source.clone(), env).with_context(|| format!("In {}", source.display()))
    }

    fn from_file(file: File, source: PathBuf, env: &Environment) -> Result<Self> {
        let nonzero = |key: &str, port: Option<u16>| match port {
            Some(0) => Err(anyhow!("{key} must be a port number from 1 to 65535.")),
            _ => Ok(port),
        };
        let port = nonzero("port", file.port)?.unwrap_or(3000);
        let https_port = nonzero("https_port", file.https_port)?;
        let tls_check_port = nonzero("tls_check_port", file.tls_check_port)?.unwrap_or(3001);
        match https_port {
            Some(https_port) if https_port == port => bail!("https_port must be different from port."),
            None if tls_check_port == port => bail!("tls_check_port must be different from port."),
            _ => {}
        }

        let bind_address = match file.bind_address {
            // Facing the internet, every address: `::` takes IPv4 as well on
            // Linux unless the host has turned that off.
            None if https_port.is_some() => IpAddr::from([0u16; 8]),
            None => IpAddr::from([127, 0, 0, 1]),
            Some(value) => match value.trim().parse() {
                Ok(address) => address,
                Err(_) => bail!("bind_address must be an IP address, eg \"127.0.0.1\" or \"0.0.0.0\"."),
            },
        };

        let dashboard_host = file.dashboard_host.unwrap_or_default().trim().to_lowercase();
        if dashboard_host.is_empty() {
            bail!("dashboard_host must be set, eg dashboard_host = \"panel.example.com\".");
        }
        if dashboard_host.contains(|c: char| c == '/' || c.is_whitespace()) {
            bail!("dashboard_host must be a bare hostname, without a scheme or path.");
        }
        let dashboard_host = strip_port(&dashboard_host).to_string();

        let log_level = match file.log_level {
            None => LogLevel::Info,
            Some(value) => match LogLevel::parse(&value.to_lowercase()) {
                Some(level) => level,
                None => bail!("log_level must be one of: debug, info, warn, error."),
            },
        };

        let data_dir = match file.data_dir {
            // Against the file, so the setting means one place however npanel
            // was started.
            Some(dir) => absolute(&source.parent().unwrap_or(Path::new("")).join(dir)),
            None => env.default_data_dir()?,
        };

        Ok(Self { source, port, bind_address, tls_check_port, https_port, dashboard_host, log_level, data_dir })
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

fn not_found(env: &Environment) -> anyhow::Error {
    let tried: Vec<String> = env.config_candidates().iter().map(|path| path.display().to_string()).collect();
    // Up to 0.5, configuration was a .env in the working directory, which the
    // old unit set to /etc/npanel.
    let old = if Path::new(".env").is_file() {
        " npanel no longer reads .env: move its settings to npanel.toml, as lowercase keys (DASHBOARD_HOST=\"x\" is dashboard_host = \"x\")."
    } else {
        ""
    };
    anyhow!(
        "No config file at {}. Write one with dashboard_host set (npanel.example.toml lists every setting), or name one with --config.{old}",
        tried.join(" or ")
    )
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

#[cfg(test)]
mod tests {
    use super::*;

    fn env() -> Environment {
        Environment { root: false, home: Some("/home/someone".into()), ..Default::default() }
    }

    fn config(text: &str) -> Result<Config> {
        let file: File = toml::from_str(text)?;
        Config::from_file(file, "/etc/npanel/npanel.toml".into(), &env())
    }

    #[test]
    fn defaults() {
        let c = config(r#"dashboard_host = "Panel.Example.com:443""#).unwrap();
        assert_eq!(c.port, 3000);
        assert_eq!(c.tls_check_port, 3001);
        assert_eq!(c.dashboard_host, "panel.example.com");
        assert_eq!(c.bind_address.to_string(), "127.0.0.1");
        assert_eq!(c.log_level, LogLevel::Info);
        assert_eq!(c.https_port, None);
        assert_eq!(c.data_dir, Path::new("/home/someone/.local/share/npanel"));
    }

    #[test]
    fn https() {
        let c = config("dashboard_host = \"x\"\nhttps_port = 443\nport = 80").unwrap();
        assert_eq!(c.https_port, Some(443));
        assert_eq!(c.bind_address.to_string(), "::");
        // The check port isn't bound, so it may be anything.
        assert!(config("dashboard_host = \"x\"\nhttps_port = 443\nport = 3001").is_ok());
        assert!(config("dashboard_host = \"x\"\nhttps_port = 80\nport = 80").is_err());
        assert!(config("dashboard_host = \"x\"\nhttps_port = 0").is_err());
        let pinned = config("dashboard_host = \"x\"\nhttps_port = 443\nbind_address = \"0.0.0.0\"").unwrap();
        assert_eq!(pinned.bind_address.to_string(), "0.0.0.0");
    }

    #[test]
    fn data_dir_is_relative_to_the_file() {
        assert_eq!(config("dashboard_host = \"x\"\ndata_dir = \"data\"").unwrap().data_dir, Path::new("/etc/npanel/data"));
        assert_eq!(config("dashboard_host = \"x\"\ndata_dir = \"/srv/npanel\"").unwrap().data_dir, Path::new("/srv/npanel"));
        let root = Environment { root: true, ..env() };
        let file: File = toml::from_str("dashboard_host = \"x\"").unwrap();
        assert_eq!(Config::from_file(file, "/x/npanel.toml".into(), &root).unwrap().data_dir, Path::new("/var/lib/npanel"));
    }

    #[test]
    fn refuses_mistakes() {
        assert!(config("").is_err());
        assert!(config("dashboard_host = \"\"").is_err());
        assert!(config("dashboard_host = \"https://x\"").is_err());
        assert!(config("dashboard_host = \"x\"\nport = 0").is_err());
        assert!(config("dashboard_host = \"x\"\nport = 70000").is_err());
        assert!(config("dashboard_host = \"x\"\nport = \"3000\"").is_err());
        assert!(config("dashboard_host = \"x\"\nport = 3001").is_err());
        assert!(config("dashboard_host = \"x\"\nbind_address = \"localhost\"").is_err());
        assert!(config("dashboard_host = \"x\"\nlog_level = \"loud\"").is_err());
        // Unknown keys, and the old environment variables' names.
        assert!(config("dashboard_host = \"x\"\nrelays = []").is_err());
        assert!(config("DASHBOARD_HOST = \"x\"").is_err());
    }

    #[test]
    fn the_example_is_valid() {
        let c = config(include_str!("../npanel.example.toml")).unwrap();
        assert_eq!(c.dashboard_host, "panel.example.com");
        assert_eq!(c.port, 3000);
    }

    #[test]
    fn finds_the_user_config_first() {
        let dir = tempfile::tempdir().unwrap();
        let env = Environment { xdg_config_home: Some(dir.path().into()), ..env() };
        assert_eq!(env.config_candidates(), [dir.path().join("npanel/npanel.toml"), PathBuf::from(SYSTEM_PATH)]);

        std::fs::create_dir(dir.path().join("npanel")).unwrap();
        std::fs::write(dir.path().join("npanel/npanel.toml"), "dashboard_host = \"mine\"\ndata_dir = \"d\"").unwrap();
        let c = Config::load(None, &env).unwrap();
        assert_eq!(c.dashboard_host, "mine");
        assert_eq!(c.data_dir, dir.path().join("npanel/d"));
    }

    #[test]
    fn a_named_file_has_to_exist() {
        let dir = tempfile::tempdir().unwrap();
        let error = Config::load(Some(&dir.path().join("missing.toml")), &env()).unwrap_err();
        assert!(error.to_string().starts_with("Could not read the config file"), "{error}");
    }
}
