//! The body `POST /api/event` receives, validated the way Plausible's
//! `Ingestion.Request` validates it: the same short and long names (`n` or
//! `name`, `u` or `url`, …), the same limits, and the same error messages, so
//! anything written against Plausible's endpoint behaves the same here.
//!
//! Props and revenue are accepted and ignored — npanel reports neither — and
//! so is anything else a newer tracker sends.

use indexmap::IndexMap;
use serde_json::Value;

/// URLs and referrers longer than this are refused or cut, as Plausible does.
const MAX_URL: usize = 2000;

/// Longest event name accepted.
const MAX_NAME: usize = 120;

/// An engagement event reporting no scroll depth stores this, which no real
/// depth can be.
pub const MISSING_SCROLL_DEPTH: i64 = 255;

/// Engagement time longer than a month is a broken clock, not a visitor.
const MAX_ENGAGEMENT_TIME: i64 = 30 * 24 * 60 * 60 * 1000;

/// An event, as the tracker described it and after Plausible's validation.
#[derive(Debug, Clone)]
pub struct EventRequest {
    pub name: String,
    pub url: url::Url,
    /// The page, percent-decoded, with its fragment in hash-routing mode.
    pub pathname: String,
    /// Every site the event was reported to, `www.` removed.
    pub domains: Vec<String>,
    pub referrer: Option<String>,
    pub interactive: bool,
    /// Milliseconds; 0 unless this is an engagement event.
    pub engagement_time: i64,
    /// Percent; [`MISSING_SCROLL_DEPTH`] unless reported.
    pub scroll_depth: i64,
}

/// Field errors, in the shape Plausible's `{"errors": …}` body takes.
pub type RequestErrors = IndexMap<String, Vec<String>>;

pub fn parse_event_request(body: &str) -> Result<EventRequest, RequestErrors> {
    let fields = match serde_json::from_str::<Value>(body) {
        Ok(Value::Object(fields)) => fields,
        _ => {
            let mut errors = RequestErrors::new();
            errors.insert("request".into(), vec!["Unable to parse request body as json".into()]);
            return Err(errors);
        }
    };

    let mut errors = RequestErrors::new();
    let mut error = |field: &str, message: String| errors.entry(field.to_string()).or_default().push(message);
    let get = |short: &str, long: &str| fields.get(short).filter(|v| !v.is_null()).or_else(|| fields.get(long)).filter(|v| !v.is_null());

    let name = match get("n", "name") {
        // A whole number is a name too, written without a fraction: a
        // float that is whole displays as one.
        Some(Value::Number(n)) if n.as_f64().is_some_and(|f| f.fract() == 0.0) => {
            Some(n.as_i64().map_or_else(|| n.as_f64().unwrap_or_default().to_string(), |i| i.to_string()))
        }
        Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
        _ => None,
    };
    match &name {
        None => error("event_name", "can't be blank".into()),
        Some(name) if name.chars().count() > MAX_NAME => {
            error("event_name", format!("should be at most {MAX_NAME} character(s)"))
        }
        _ => {}
    }

    let url = parse_url(get("u", "url"), &mut error);
    let hash_mode = get("h", "hashMode").and_then(Value::as_f64) == Some(1.0);

    let referrer = match get("r", "referrer") {
        Some(Value::String(r)) => Some(r.chars().take(MAX_URL).collect()),
        _ => None,
    };

    let mut engagement_time = 0;
    let mut scroll_depth = MISSING_SCROLL_DEPTH;
    if name.as_deref() == Some("engagement") {
        scroll_depth = parse_scroll_depth(fields.get("sd"));
        engagement_time = parse_engagement_time(fields.get("e"));
        if scroll_depth == MISSING_SCROLL_DEPTH && engagement_time == 0 {
            error(
                "event_name",
                "engagement event requires a valid integer value for at least one of 'sd' or 'e' fields".into(),
            );
        }
    }

    let domains = parse_domains(get("d", "domain"), url.as_ref(), &mut error);
    let interactive = get("i", "interactive") != Some(&Value::Bool(false));

    let (Some(name), Some(url)) = (name, url) else {
        return Err(errors);
    };
    if !errors.is_empty() {
        return Err(errors);
    }

    Ok(EventRequest {
        pathname: pathname(&url, hash_mode),
        name,
        url,
        domains,
        referrer,
        interactive,
        engagement_time,
        scroll_depth,
    })
}

fn parse_url(value: Option<&Value>, error: &mut impl FnMut(&str, String)) -> Option<url::Url> {
    let Some(value) = value else {
        error("url", "is required".into());
        return None;
    };
    let Some(text) = value.as_str().filter(|s| s.len() <= MAX_URL) else {
        error("url", "must be a valid url".into());
        return None;
    };
    let Ok(url) = url::Url::parse(text) else {
        error("url", "must be a valid url".into());
        return None;
    };
    if url.scheme() == "data" {
        error("url", "scheme is not allowed".into());
        return None;
    }
    Some(url)
}

/// The sites an event is for: `d`, split on commas, each without `www.`, or
/// the page's own host when the tracker named none.
fn parse_domains(value: Option<&Value>, url: Option<&url::Url>, error: &mut impl FnMut(&str, String)) -> Vec<String> {
    let raw = value.and_then(Value::as_str).map(str::trim);
    let host = url.and_then(|u| u.host_str()).unwrap_or("");

    if raw == Some("") || (raw.is_none() && host.is_empty()) {
        error("domain", "can't be blank".into());
        return Vec::new();
    }

    match raw {
        None => vec![sanitize_hostname(host)],
        Some(raw) => raw.split(',').map(sanitize_hostname).filter(|d| !d.is_empty()).collect(),
    }
}

/// A page's path as Plausible records it: decoded, and with its route in hash
/// mode.
fn pathname(url: &url::Url, hash_mode: bool) -> String {
    let path = if url.path().is_empty() { "/" } else { url.path() };
    let path = decode(path).trim_end().to_string();
    match url.fragment().filter(|f| hash_mode && !f.is_empty()) {
        Some(fragment) => format!("{path}#{}", decode(fragment)),
        None => path,
    }
}

fn parse_scroll_depth(value: Option<&Value>) -> i64 {
    match integer(value) {
        Some(depth) if depth >= 0 => depth.min(100),
        _ => MISSING_SCROLL_DEPTH,
    }
}

fn parse_engagement_time(value: Option<&Value>) -> i64 {
    match integer(value) {
        Some(time) if (0..MAX_ENGAGEMENT_TIME).contains(&time) => time,
        _ => 0,
    }
}

/// An integer, or a string that is exactly one.
fn integer(value: Option<&Value>) -> Option<i64> {
    match value? {
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().filter(|f| f.fract() == 0.0).map(|f| f as i64)),
        Value::String(s) => {
            let digits = s.strip_prefix('-').unwrap_or(s);
            (!digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())).then(|| s.parse().ok()).flatten()
        }
        _ => None,
    }
}

pub fn sanitize_hostname(hostname: &str) -> String {
    let lower = hostname.trim().to_lowercase();
    lower.strip_prefix("www.").map(str::to_string).unwrap_or(lower)
}

fn decode(value: &str) -> String {
    crate::util::decode_uri_component(value).unwrap_or_else(|| value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_pageviews() {
        let event = parse_event_request(r#"{"n":"pageview","u":"https://www.docs.example.com/caf%C3%A9?x=1#top","d":"docs.example.com,www.Other.com","r":"https://google.com/"}"#).unwrap();
        assert_eq!(event.name, "pageview");
        assert_eq!(event.pathname, "/café");
        assert_eq!(event.domains, ["docs.example.com", "other.com"]);
        assert_eq!(event.referrer.as_deref(), Some("https://google.com/"));
        assert!(event.interactive);
        assert_eq!(event.scroll_depth, MISSING_SCROLL_DEPTH);

        let hashed = parse_event_request(r#"{"name":"pageview","url":"https://a.com/app#/route","h":1}"#).unwrap();
        assert_eq!(hashed.pathname, "/app#/route");
        assert_eq!(hashed.domains, ["a.com"]);
    }

    #[test]
    fn engagement() {
        let event = parse_event_request(r#"{"n":"engagement","u":"https://a.com/","d":"a.com","sd":"150","e":2000,"i":false}"#).unwrap();
        assert_eq!(event.scroll_depth, 100);
        assert_eq!(event.engagement_time, 2000);
        assert!(!event.interactive);

        let errors = parse_event_request(r#"{"n":"engagement","u":"https://a.com/","d":"a.com"}"#).unwrap_err();
        assert!(errors["event_name"][0].contains("engagement event requires"));
    }

    #[test]
    fn errors() {
        let errors = parse_event_request("nope").unwrap_err();
        assert_eq!(errors["request"], ["Unable to parse request body as json"]);
        let errors = parse_event_request(r#"{"d":""}"#).unwrap_err();
        assert_eq!(errors["event_name"], ["can't be blank"]);
        assert_eq!(errors["url"], ["is required"]);
        assert_eq!(errors["domain"], ["can't be blank"]);
        let errors = parse_event_request(r#"{"n":"x","u":"data:text/plain,hi","d":"a.com"}"#).unwrap_err();
        assert_eq!(errors["url"], ["scheme is not allowed"]);
    }
}
