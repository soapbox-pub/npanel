//! Structured JSON logging — one line per entry, queryable in Loki.
//!
//! Format (slog-style, flat snake_case keys):
//!
//! ```text
//! {"level":"info","msg":"started","port":13131}
//! ```
//!
//! - `level` is a string so Grafana's log panel auto-detects it for coloring.
//! - There is no app-emitted `time` field: journald stamps every entry with a
//!   receive timestamp, so an extra timestamp per line is pure overhead.
//! - `msg` is a short stable event name — put variable data in fields, never
//!   interpolated into `msg`, so LogQL can aggregate.
//! - Fields must be flat scalars. Serialize structured data to a compact JSON
//!   *string* field so `| json` output stays flat.
//!
//! There is no global logger: `main` constructs a [`Logger`] from
//! `Config::log_level` and passes it down to whatever logs.

use std::fmt::Display;
use std::io::Write;

use serde_json::{Map, Value};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum LogLevel {
    Debug = 10,
    Info = 20,
    Warn = 30,
    Error = 40,
}

impl LogLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            LogLevel::Debug => "debug",
            LogLevel::Info => "info",
            LogLevel::Warn => "warn",
            LogLevel::Error => "error",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "debug" => Some(LogLevel::Debug),
            "info" => Some(LogLevel::Info),
            "warn" => Some(LogLevel::Warn),
            "error" => Some(LogLevel::Error),
            _ => None,
        }
    }
}

/// Flat log fields, in the order they are written.
pub type Fields = Vec<(&'static str, Value)>;

/// Build [`Fields`] from `key => value` pairs. `None` values are left out, the
/// way `undefined` is in a JSON object.
#[macro_export]
macro_rules! fields {
    () => { $crate::log::Fields::new() };
    ($($key:literal => $value:expr),+ $(,)?) => {{
        let mut fields = $crate::log::Fields::new();
        $( $crate::log::push(&mut fields, $key, $value); )+
        fields
    }};
}

/// Append one field, skipping it when the value is null.
pub fn push(fields: &mut Fields, key: &'static str, value: impl Into<Value>) {
    let value = value.into();
    if !value.is_null() {
        fields.push((key, value));
    }
}

/// An error, flattened into log fields.
pub fn err_fields(error: impl Display) -> Fields {
    vec![("err_msg", Value::String(error.to_string()))]
}

/// `fields` with an error's fields appended.
pub fn with_err(mut fields: Fields, error: impl Display) -> Fields {
    fields.extend(err_fields(error));
    fields
}

#[derive(Clone, Copy, Debug)]
pub struct Logger {
    level: LogLevel,
}

impl Logger {
    pub fn new(level: LogLevel) -> Self {
        Self { level }
    }

    /// Whether entries at `level` are emitted. Hot paths can use this to skip
    /// building fields entirely when the level is disabled.
    pub fn enabled(&self, level: LogLevel) -> bool {
        level >= self.level
    }

    pub fn debug(&self, msg: &str, fields: Fields) {
        self.write(LogLevel::Debug, msg, fields);
    }

    pub fn info(&self, msg: &str, fields: Fields) {
        self.write(LogLevel::Info, msg, fields);
    }

    pub fn warn(&self, msg: &str, fields: Fields) {
        self.write(LogLevel::Warn, msg, fields);
    }

    pub fn error(&self, msg: &str, fields: Fields) {
        self.write(LogLevel::Error, msg, fields);
    }

    fn write(&self, level: LogLevel, msg: &str, fields: Fields) {
        if !self.enabled(level) {
            return;
        }

        let mut entry = Map::with_capacity(fields.len() + 2);
        entry.insert("level".into(), Value::String(level.as_str().into()));
        entry.insert("msg".into(), Value::String(msg.into()));
        for (key, value) in fields {
            entry.insert(key.into(), value);
        }

        let mut line = serde_json::to_string(&entry).unwrap_or_default();
        line.push('\n');

        // warn/error go to stderr so journald records them at a higher priority.
        if level >= LogLevel::Warn {
            let _ = std::io::stderr().lock().write_all(line.as_bytes());
        } else {
            let _ = std::io::stdout().lock().write_all(line.as_bytes());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drops_null_fields() {
        let fields = fields! { "a" => 1, "b" => Value::Null, "c" => Some("x"), "d" => None::<&str> };
        let keys: Vec<_> = fields.iter().map(|(k, _)| *k).collect();
        assert_eq!(keys, ["a", "c"]);
    }
}
