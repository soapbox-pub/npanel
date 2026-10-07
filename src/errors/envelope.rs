//! Sentry's envelope format, as much of it as error reports need, and the
//! report npanel keeps from each event in one.
//!
//! An envelope is a JSON header line followed by items, each a JSON header
//! line and a payload. Only `event` items are read; sessions, transactions,
//! client reports and the rest are what an SDK sends besides, and are
//! accepted and dropped.
//!
//! Nothing from an event is kept as it came. Each field is read leniently —
//! Sentry's SDKs write some of them more than one way — capped, and scrubbed
//! of Nostr secret keys and secret URL parameters, and whatever npanel has no use for is left behind:
//! no user, no request headers or cookies, no `extra`. What's kept is a
//! [`Report`], which is also what the dashboard is shown.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::sync::OnceLock;

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

const MAX_ITEMS: usize = 16;
const MAX_EXCEPTIONS: usize = 6;
/// Frames kept per exception: the outermost and innermost half each, as
/// Sentry trims them, since a stack overflow's middle says nothing new.
const MAX_FRAMES: usize = 100;
const MAX_BREADCRUMBS: usize = 100;
const MAX_TAGS: usize = 50;
const MAX_MESSAGE: usize = 8192;
const MAX_FIELD: usize = 1024;
const MAX_SHORT: usize = 200;

/// One event, as npanel keeps it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Report {
    /// 32 lowercase hex digits.
    pub event_id: String,
    pub level: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// Innermost cause first; the one that was thrown last.
    pub exceptions: Vec<Exception>,
    pub breadcrumbs: Vec<Breadcrumb>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub release: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub environment: Option<String>,
    pub tags: BTreeMap<String, String>,
    /// What the SDK asked it be grouped by, if anything.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub fingerprint: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sdk: Option<String>,
    pub browser: String,
    pub browser_version: String,
    pub os: String,
    pub os_version: String,
    pub device: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Exception {
    #[serde(rename = "type")]
    pub kind: String,
    pub value: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mechanism: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub handled: Option<bool>,
    /// Oldest call first, the one that threw last.
    pub frames: Vec<Frame>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Frame {
    pub abs_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub function: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lineno: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub colno: Option<u32>,
    /// Whether it is the site's own code: served from the origin the report
    /// came from.
    pub in_app: bool,
    /// Where it is in the original source, through the site's source map.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mapped: Option<Mapped>,
    /// The lines around it: the original source's when it is mapped.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<Context>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Mapped {
    pub source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub function: Option<String>,
    pub lineno: u32,
    pub colno: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Context {
    pub pre: Vec<String>,
    pub line: String,
    pub post: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Breadcrumb {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub level: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Map::is_empty")]
    pub data: Map<String, Value>,
}

/// The events of an envelope, or a sentence saying why it isn't one.
pub fn parse_envelope(body: &[u8]) -> Result<Vec<Value>, String> {
    let (header, mut rest) = line(body);
    if serde_json::from_slice::<Map<String, Value>>(header).is_err() {
        return Err("The envelope's first line must be a JSON object.".into());
    }

    let mut events = Vec::new();
    for _ in 0..MAX_ITEMS {
        if rest.iter().all(u8::is_ascii_whitespace) {
            break;
        }
        let (item, after) = line(rest);
        let Ok(item) = serde_json::from_slice::<Map<String, Value>>(item) else {
            return Err("Each item in the envelope must start with a JSON header line.".into());
        };
        let payload;
        (payload, rest) = match item.get("length").and_then(Value::as_u64) {
            Some(length) => {
                let length = usize::try_from(length).unwrap_or(usize::MAX);
                if length > after.len() {
                    return Err("An item in the envelope is shorter than its header says.".into());
                }
                let (payload, after) = after.split_at(length);
                (payload, after.strip_prefix(b"\n").unwrap_or(after))
            }
            None => line(after),
        };
        if item.get("type").and_then(Value::as_str) == Some("event") {
            match serde_json::from_slice::<Value>(payload) {
                Ok(event @ Value::Object(_)) => events.push(event),
                _ => return Err("An event in the envelope isn't a JSON object.".into()),
            }
        }
    }
    Ok(events)
}

/// Up to the next newline, and what follows it.
fn line(bytes: &[u8]) -> (&[u8], &[u8]) {
    match bytes.iter().position(|&b| b == b'\n') {
        Some(at) => (&bytes[..at], &bytes[at + 1..]),
        None => (bytes, &[]),
    }
}

/// The report kept from one event.
pub fn read_event(event: &Value) -> Report {
    let text = |value: Option<&Value>, max: usize| value.and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(|s| clean(s, max));

    let event_id = event
        .get("event_id")
        .and_then(Value::as_str)
        .map(|id| id.replace('-', "").to_ascii_lowercase())
        .filter(|id| id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit()))
        .unwrap_or_else(|| crate::util::random_hex(16));

    let level = text(event.get("level"), 16)
        .filter(|l| matches!(l.as_str(), "fatal" | "error" | "warning" | "info" | "debug"))
        .unwrap_or_else(|| "error".into());

    // A string, or `{ formatted }` / `{ message }`, under either name.
    let message = ["logentry", "message"].iter().find_map(|key| match event.get(*key) {
        Some(Value::String(s)) => text(Some(&Value::String(s.clone())), MAX_MESSAGE),
        Some(Value::Object(o)) => text(o.get("formatted"), MAX_MESSAGE).or_else(|| text(o.get("message"), MAX_MESSAGE)),
        _ => None,
    });

    let exceptions = values(event.get("exception")).iter().rev().take(MAX_EXCEPTIONS).rev().map(read_exception).collect();
    let breadcrumbs: Vec<Value> = values(event.get("breadcrumbs"));
    let skip = breadcrumbs.len().saturating_sub(MAX_BREADCRUMBS);
    let breadcrumbs = breadcrumbs[skip..].iter().map(read_breadcrumb).collect();

    let tags: BTreeMap<String, String> = match event.get("tags") {
        Some(Value::Object(tags)) => tags.iter().map(|(k, v)| (k.clone(), v.clone())).collect::<Vec<_>>(),
        Some(Value::Array(pairs)) => pairs
            .iter()
            .filter_map(|pair| Some((pair.get(0)?.as_str()?.to_string(), pair.get(1)?.clone())))
            .collect(),
        _ => Vec::new(),
    }
    .into_iter()
    .filter_map(|(key, value)| {
        let value = match value {
            Value::String(s) => s,
            Value::Number(n) => n.to_string(),
            Value::Bool(b) => b.to_string(),
            _ => return None,
        };
        Some((clean(key.trim(), 32), clean(value.trim(), MAX_SHORT)))
    })
    .filter(|(key, _)| !key.is_empty())
    .take(MAX_TAGS)
    .collect();

    let fingerprint = match event.get("fingerprint") {
        Some(Value::Array(parts)) => parts.iter().take(10).filter_map(|p| p.as_str().map(|s| clean(s, MAX_SHORT))).collect(),
        _ => Vec::new(),
    };

    Report {
        event_id,
        level,
        message,
        exceptions,
        breadcrumbs,
        url: text(event.get("request").and_then(|r| r.get("url")), 2048),
        release: text(event.get("release"), MAX_SHORT),
        environment: text(event.get("environment"), 64),
        tags,
        fingerprint,
        sdk: text(event.get("sdk").and_then(|s| s.get("name")), 64),
        ..Report::default()
    }
}

/// A list Sentry writes either bare or as `{ values: [...] }`.
fn values(value: Option<&Value>) -> Vec<Value> {
    match value {
        Some(Value::Array(list)) => list.clone(),
        Some(Value::Object(o)) => o.get("values").and_then(Value::as_array).cloned().unwrap_or_default(),
        _ => Vec::new(),
    }
}

fn read_exception(value: &Value) -> Exception {
    let text = |v: Option<&Value>, max: usize| v.and_then(Value::as_str).map(|s| clean(s.trim(), max)).unwrap_or_default();
    let frames: Vec<&Value> = value
        .get("stacktrace")
        .and_then(|s| s.get("frames"))
        .and_then(Value::as_array)
        .map(|frames| frames.iter().collect())
        .unwrap_or_default();
    let frames = if frames.len() > MAX_FRAMES {
        frames[..MAX_FRAMES / 2].iter().chain(&frames[frames.len() - MAX_FRAMES / 2..]).copied().collect()
    } else {
        frames
    };
    let mechanism = value.get("mechanism");

    Exception {
        kind: Some(text(value.get("type"), MAX_SHORT)).filter(|t| !t.is_empty()).unwrap_or_else(|| "Error".into()),
        value: text(value.get("value"), MAX_MESSAGE),
        mechanism: mechanism.and_then(|m| m.get("type")).and_then(Value::as_str).map(|t| clean(t, 64)),
        handled: mechanism.and_then(|m| m.get("handled")).and_then(Value::as_bool),
        frames: frames.into_iter().filter_map(read_frame).collect(),
    }
}

fn read_frame(value: &Value) -> Option<Frame> {
    let path = ["abs_path", "filename"].iter().find_map(|key| value.get(*key).and_then(Value::as_str)).map(str::trim).filter(|p| !p.is_empty())?;
    let number = |key: &str| value.get(key).and_then(Value::as_u64).and_then(|n| u32::try_from(n).ok()).filter(|n| *n > 0);
    Some(Frame {
        abs_path: clean(path, MAX_FIELD),
        function: value.get("function").and_then(Value::as_str).map(str::trim).filter(|f| !f.is_empty() && *f != "?").map(|f| clean(f, 256)),
        lineno: number("lineno"),
        colno: number("colno"),
        // Decided once the report's origin is known, whatever the SDK said.
        in_app: false,
        mapped: None,
        context: None,
    })
}

fn read_breadcrumb(value: &Value) -> Breadcrumb {
    let text = |key: &str, max: usize| value.get(key).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(|s| clean(s, max));
    // Only flat values: a breadcrumb's data is a line on the dashboard, not a
    // place to keep whatever a page had in memory.
    let data = value
        .get("data")
        .and_then(Value::as_object)
        .map(|data| {
            data.iter()
                .take(20)
                .filter_map(|(key, value)| {
                    let value = match value {
                        Value::String(s) => Value::String(clean(s, MAX_FIELD)),
                        Value::Number(_) | Value::Bool(_) => value.clone(),
                        _ => return None,
                    };
                    Some((clean(key, 64), value))
                })
                .collect()
        })
        .unwrap_or_default();
    Breadcrumb {
        timestamp: match value.get("timestamp") {
            Some(Value::Number(n)) => n.as_f64().filter(|t| t.is_finite()),
            Some(Value::String(s)) => chrono::DateTime::parse_from_rfc3339(s).ok().map(|t| t.timestamp_millis() as f64 / 1000.0),
            _ => None,
        },
        category: text("category", 64),
        level: text("level", 16),
        message: text("message", MAX_FIELD),
        data,
    }
}

/// Text as it is kept: Nostr secret keys replaced, and cut to `max`
/// characters.
pub fn clean(text: &str, max: usize) -> String {
    let scrubbed = scrub(text);
    match scrubbed.char_indices().nth(max) {
        Some((at, _)) => format!("{}…", &scrubbed[..at]),
        None => scrubbed.into_owned(),
    }
}

/// `text` with every `nsec` and `ncryptsec` replaced, and the value of every
/// URL parameter whose name says it is a secret. A Nostr client's state can
/// hold a key, and a page's address a bunker's `secret=` or an OAuth
/// `#access_token=`; an error message, a URL or a line of console output
/// can carry any of them here, and a secret in an error tracker is one every
/// staff member with access to the site can read. npanel's reporter scrubs
/// the same way before it sends (`assets/errors/reporter.js`): change both
/// together. This one still matters for Sentry's own SDKs.
pub fn scrub(text: &str) -> Cow<'_, str> {
    static SECRET: OnceLock<Regex> = OnceLock::new();
    static PARAMETER: OnceLock<Regex> = OnceLock::new();
    // No word boundary before it: a key in a URL is as often
    // `nostr%3Ansec1…` as `nostr:nsec1…`.
    let secret = SECRET.get_or_init(|| Regex::new(r"(?i)(?:nsec|ncryptsec)1[02-9ac-hj-np-z]{20,}").expect("regex"));
    let parameter = PARAMETER.get_or_init(|| {
        Regex::new(r#"(?i)([?&#][a-z0-9_.\-]*(?:secret|token|passw|pwd|api[_\-]?key|private[_\-]?key|credential|session|signature)[a-z0-9_.\-]*=)[^&#\s"'<>]+"#)
            .expect("regex")
    });
    let text = secret.replace_all(text, "[nsec]");
    match parameter.replace_all(&text, "${1}[redacted]") {
        Cow::Owned(scrubbed) => Cow::Owned(scrubbed),
        Cow::Borrowed(_) => text,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reads_envelopes() {
        let event = r#"{"event_id":"a","exception":{"values":[]}}"#;
        let body = format!("{{\"event_id\":\"a\"}}\n{{\"type\":\"session\"}}\n{{\"x\":1}}\n{{\"type\":\"event\",\"length\":{}}}\n{event}\n", event.len());
        let events = parse_envelope(body.as_bytes()).unwrap();
        assert_eq!(events, [serde_json::from_str::<Value>(event).unwrap()]);

        // Without lengths, each payload is a line; the last needs no newline.
        let events = parse_envelope(b"{}\n{\"type\":\"event\"}\n{\"message\":\"hi\"}").unwrap();
        assert_eq!(events[0]["message"], "hi");
        // A length can cover newlines.
        let payload = "{\"message\":\n\"hi\"}";
        let body = format!("{{}}\n{{\"type\":\"event\",\"length\":{}}}\n{payload}", payload.len());
        assert_eq!(parse_envelope(body.as_bytes()).unwrap()[0]["message"], "hi");

        assert!(parse_envelope(b"").is_err());
        assert!(parse_envelope(b"nope").is_err());
        assert!(parse_envelope(b"{}\nnope\n").is_err());
        assert!(parse_envelope(b"{}\n{\"type\":\"event\",\"length\":99}\n{}").is_err());
        assert!(parse_envelope(b"{}\n{\"type\":\"event\"}\n[1]").is_err());
        assert_eq!(parse_envelope(b"{}\n").unwrap(), Vec::<Value>::new());
    }

    #[test]
    fn reads_events() {
        let report = read_event(&json!({
            "event_id": "1F2E3D4C-5B6A-7980-1F2E-3D4C5B6A7980",
            "level": "warning",
            "logentry": { "formatted": "key nsec1vl029mgpspedva04g90vltkh6fvh240zqtv9k0t9af8935ke9laqsnlfe5 leaked" },
            "release": "e1",
            "request": { "url": "https://docs.example.com/?k=nsec1vl029mgpspedva04g90vltkh6fvh240zqtv9k0t9af8935ke9laqsnlfe5", "headers": { "Cookie": "x" } },
            "user": { "ip_address": "1.2.3.4" },
            "tags": [["browser", "Chrome"], ["n", 1]],
            "breadcrumbs": [{ "category": "fetch", "timestamp": "2026-01-01T00:00:00Z", "data": { "url": "/x", "deep": { "a": 1 } } }],
            "exception": { "values": [{
                "type": "TypeError",
                "value": "x is undefined",
                "mechanism": { "type": "onerror", "handled": false },
                "stacktrace": { "frames": [
                    { "filename": "https://docs.example.com/a.js", "function": "?", "lineno": 1, "colno": 2, "in_app": true },
                    { "lineno": 3 },
                ] },
            }] },
        }));
        assert_eq!(report.event_id, "1f2e3d4c5b6a79801f2e3d4c5b6a7980");
        assert_eq!(report.level, "warning");
        assert_eq!(report.message.as_deref(), Some("key [nsec] leaked"));
        assert_eq!(report.url.as_deref(), Some("https://docs.example.com/?k=[nsec]"));
        assert_eq!(report.tags, BTreeMap::from([("browser".into(), "Chrome".into()), ("n".into(), "1".into())]));
        assert_eq!(report.breadcrumbs[0].timestamp, Some(1767225600.0));
        assert_eq!(report.breadcrumbs[0].data, json!({ "url": "/x" }).as_object().unwrap().clone());
        let exception = &report.exceptions[0];
        assert_eq!((exception.kind.as_str(), exception.value.as_str(), exception.mechanism.as_deref(), exception.handled), ("TypeError", "x is undefined", Some("onerror"), Some(false)));
        assert_eq!(
            exception.frames,
            [Frame { abs_path: "https://docs.example.com/a.js".into(), function: None, lineno: Some(1), colno: Some(2), ..Frame::default() }]
        );
        let stored = serde_json::to_string(&report).unwrap();
        assert!(!stored.contains("1.2.3.4") && !stored.contains("Cookie"));

        // Whatever is missing is filled in.
        let report = read_event(&json!({ "message": "  hi  ", "level": "loud" }));
        assert_eq!((report.event_id.len(), report.level.as_str(), report.message.as_deref()), (32, "error", Some("hi")));
    }

    #[test]
    fn scrubs_secrets() {
        let key = "nsec1vl029mgpspedva04g90vltkh6fvh240zqtv9k0t9af8935ke9laqsnlfe5";
        assert_eq!(scrub(&format!("/login?uri=nostr%3A{key}&x=1")), "/login?uri=nostr%3A[nsec]&x=1");
        assert_eq!(scrub(&key.to_uppercase()), "[nsec]");
        assert_eq!(
            scrub("bunker://abc?relay=wss%3A%2F%2Fr.example&secret=hunter2&x=1"),
            "bunker://abc?relay=wss%3A%2F%2Fr.example&secret=[redacted]&x=1"
        );
        assert_eq!(scrub("https://s/cb#access_token=abc.def&state=1"), "https://s/cb#access_token=[redacted]&state=1");
        assert_eq!(scrub("/a?API_KEY=k&sessionId=s&q=token"), "/a?API_KEY=[redacted]&sessionId=[redacted]&q=token");
        // Not every `token=` is a URL's.
        assert_eq!(scrub("if (a.token=b) return;"), "if (a.token=b) return;");
        assert!(matches!(scrub("nothing here"), Cow::Borrowed(_)));
    }

    #[test]
    fn caps_what_it_keeps() {
        let frames: Vec<Value> = (1..=150).map(|i| json!({ "abs_path": "/a.js", "lineno": i })).collect();
        let report = read_event(&json!({
            "message": "x".repeat(10_000),
            "exception": [{ "type": "Error", "stacktrace": { "frames": frames } }],
            "breadcrumbs": { "values": (0..150).map(|i| json!({ "message": i.to_string() })).collect::<Vec<_>>() },
        }));
        assert_eq!(report.message.unwrap().chars().count(), MAX_MESSAGE + 1);
        let lines: Vec<u32> = report.exceptions[0].frames.iter().map(|f| f.lineno.unwrap()).collect();
        assert_eq!(lines.len(), 100);
        assert_eq!((lines[49], lines[50]), (50, 101));
        assert_eq!(report.breadcrumbs.len(), 100);
        assert_eq!(report.breadcrumbs[0].message.as_deref(), Some("50"));
    }
}
