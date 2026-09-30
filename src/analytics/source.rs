//! Where a visit came from: which source, and the referring page itself.
//!
//! A port of Plausible's `Ingestion.Source`, over the same data:
//!
//! - `referers.json` is snowplow's referer-parser database, which Plausible
//!   reads through RefInspector, flattened to one host (or host and path) per
//!   key. It is what turns `google.co.uk` into `Google`.
//! - `custom-sources.json` is Plausible's own list layered on top.
//! - `spammers.txt` is Matomo's referrer spam list, which is public domain.
//!
//! The first two are taken from Plausible at 8be0427 (AGPL-3.0, as npanel
//! is); the spam list from matomo-org/referrer-spam-list at eaaa5ea.

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

/// Referrers kept, in characters, as Plausible truncates them.
const MAX_REFERRER: usize = 2000;

/// Families of hosts that are one source, however many subdomains they have.
const CUSTOM_SUFFIXES: [(&str, &str); 2] = [("officeapps.live.com", "Microsoft 365"), ("wikipedia.org", "Wikipedia")];

struct Data {
    referers: HashMap<String, String>,
    custom: indexmap::IndexMap<String, String>,
    /// Every name a source is known by, lowercased, to the one it is shown as.
    canonical: HashMap<String, String>,
    paid_sources: HashSet<String>,
    spammers: HashSet<String>,
}

fn data() -> &'static Data {
    static DATA: OnceLock<Data> = OnceLock::new();
    DATA.get_or_init(|| {
        let referers: HashMap<String, String> =
            serde_json::from_str(include_str!("../../assets/analytics/referers.json")).expect("referers.json");
        let custom: indexmap::IndexMap<String, String> =
            serde_json::from_str(include_str!("../../assets/analytics/custom-sources.json")).expect("custom-sources.json");

        // Built the way Plausible builds it: every referer-parser name, then
        // every custom source by both its key and its name.
        let mut canonical = HashMap::new();
        for name in referers.values() {
            canonical.insert(name.to_lowercase(), name.clone());
        }
        for (key, name) in &custom {
            canonical.insert(key.clone(), name.clone());
            canonical.insert(name.to_lowercase(), name.clone());
        }

        // `utm_source` values that mean paid traffic whatever the medium says.
        let mut paid_sources: HashSet<String> = HashSet::from(["adwords".to_string()]);
        paid_sources.extend(custom.keys().filter(|key| key.ends_with("ads") || key.ends_with("ad")).cloned());

        let spammers = include_str!("../../assets/analytics/spammers.txt")
            .lines()
            .map(|line| line.trim().to_lowercase())
            .filter(|line| !line.is_empty())
            .collect();

        Data { referers, custom, canonical, paid_sources, spammers }
    })
}

/// Whether a `utm_source` means paid traffic, like `facebook_ad` or `yt-ads`.
pub fn is_paid_source(utm_source: &str) -> bool {
    data().paid_sources.contains(utm_source)
}

pub struct Acquisition {
    /// The source, like `Google` or `news.example.com`, or empty for direct.
    pub source: String,
    /// The referring page, without scheme, query, or trailing slash.
    pub referrer: String,
}

/// Where a visit to `url` referred by `referrer` came from.
///
/// A source tagged in the URL wins over the referrer — `utm_source`, then
/// `source`, then `ref` — and is folded into a known source when it names one.
/// Otherwise the referrer decides, unless it is the site itself, localhost, or
/// not a web page at all, in which case the visit is direct.
pub fn acquisition(url: &url::Url, referrer: Option<&str>) -> Acquisition {
    let referral = valid_referral(url, referrer);
    let tagged = ["utm_source", "source", "ref"].iter().find_map(|name| query_param(url, name).filter(|v| !v.is_empty()));

    let source = match (&tagged, &referral) {
        (Some(tagged), _) => canonical(tagged),
        (None, Some(referral)) => from_referrer(referral),
        (None, None) => String::new(),
    };
    let referrer = referral
        .map(|referral| format!("{}{}", referrer_host(&referral), referral.path().trim_end_matches('/')))
        .unwrap_or_default();

    Acquisition { source, referrer }
}

/// The first value of a query parameter, as `URLSearchParams.get` reads it.
pub fn query_param(url: &url::Url, name: &str) -> Option<String> {
    url.query_pairs().find(|(key, _)| key == name).map(|(_, value)| value.into_owned())
}

/// Whether a referrer is on Matomo's spam list, by its host or any parent.
pub fn is_spam_referrer(referrer: Option<&str>) -> bool {
    let Some(referrer) = referrer.filter(|r| !r.is_empty()) else {
        return false;
    };
    let Ok(url) = url::Url::parse(referrer) else {
        return false;
    };
    let host = url.host_str().unwrap_or("").to_lowercase();
    let mut name = host.strip_prefix("www.").unwrap_or(&host);

    let spammers = &data().spammers;
    while name.contains('.') {
        if spammers.contains(name) {
            return true;
        }
        name = &name[name.find('.').map_or(name.len(), |i| i + 1)..];
    }
    false
}

/// A source as it is shown, for a name someone typed into a URL.
fn canonical(source: &str) -> String {
    data().canonical.get(&source.to_lowercase()).cloned().unwrap_or_else(|| source.to_string())
}

/// The referrer, if it points somewhere other than the site itself. Only web
/// pages and Android apps count.
fn valid_referral(url: &url::Url, referrer: Option<&str>) -> Option<url::Url> {
    let referrer: String = referrer?.chars().take(MAX_REFERRER).collect();
    let parsed = url::Url::parse(referrer.trim()).ok()?;

    if !["http", "https", "android-app"].contains(&parsed.scheme()) {
        return None;
    }
    let host = parsed.host_str().unwrap_or("");
    if host.is_empty() || host == "localhost" {
        return None;
    }
    if sanitize(host) == sanitize(url.host_str().unwrap_or("")) {
        return None;
    }
    Some(parsed)
}

/// The source a referring page belongs to: Plausible's own overrides first,
/// then referer-parser, and failing all of it the bare host.
fn from_referrer(referral: &url::Url) -> String {
    let host = referrer_host(referral);
    let lower = host.to_lowercase();

    if let Some(custom) = data().custom.get(&lower) {
        return custom.clone();
    }
    for (suffix, name) in CUSTOM_SUFFIXES {
        if lower == suffix || lower.ends_with(&format!(".{suffix}")) {
            return name.to_string();
        }
    }

    match referer_parser(&referral.host_str().unwrap_or("").to_lowercase(), referral.path()) {
        Some(known) => canonical(known),
        None => host,
    }
}

/// The referer-parser name for a host and path. The longest match wins: every
/// leading part of the path under the full host, then the host alone, then the
/// same again for each parent domain.
fn referer_parser(hostname: &str, pathname: &str) -> Option<&'static str> {
    let referers = &data().referers;
    let segments: Vec<&str> = pathname.split('/').filter(|s| !s.is_empty()).collect();

    let mut host = hostname;
    while host.contains('.') {
        for length in (1..=segments.len()).rev() {
            let prefix = format!("{host}/{}", segments[..length].join("/"));
            if let Some(found) = referers.get(&prefix).or_else(|| referers.get(&format!("{prefix}/"))) {
                return Some(found);
            }
        }
        if let Some(found) = referers.get(host) {
            return Some(found);
        }
        host = &host[host.find('.').map_or(host.len(), |i| i + 1)..];
    }
    None
}

/// A referrer's host as Plausible records it: no `www.`, and apps marked.
fn referrer_host(referral: &url::Url) -> String {
    let host = referral.host_str().unwrap_or("");
    let host = host.strip_prefix("www.").unwrap_or(host);
    if referral.scheme() == "android-app" {
        format!("android-app://{host}")
    } else {
        host.to_string()
    }
}

fn sanitize(hostname: &str) -> &str {
    let trimmed = hostname.trim();
    trimmed.strip_prefix("www.").unwrap_or(trimmed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(page: &str, referrer: Option<&str>) -> (String, String) {
        let a = acquisition(&url::Url::parse(page).unwrap(), referrer);
        (a.source, a.referrer)
    }

    #[test]
    fn sources() {
        assert_eq!(source("https://a.com/", Some("https://www.google.co.uk/search?q=x")).0, "Google");
        assert_eq!(source("https://a.com/", Some("https://news.example.org/post/")), ("news.example.org".into(), "news.example.org/post".into()));
        assert_eq!(source("https://a.com/?utm_source=google", None).0, "Google");
        assert_eq!(source("https://a.com/?ref=Some+Newsletter", None).0, "Some Newsletter");
        assert_eq!(source("https://a.com/", Some("https://www.a.com/other")), (String::new(), String::new()));
        assert_eq!(source("https://a.com/", Some("http://localhost:3000/")), (String::new(), String::new()));
        assert_eq!(source("https://a.com/", Some("https://gemini.google.com/app")).0, "Google Gemini");
        assert_eq!(source("https://a.com/", Some("https://en.wikipedia.org/wiki/X")).0, "Wikipedia");
        assert_eq!(source("https://a.com/", None), (String::new(), String::new()));
    }

    #[test]
    fn spam() {
        let spammer = data().spammers.iter().next().unwrap().clone();
        assert!(is_spam_referrer(Some(&format!("https://sub.{spammer}/x"))));
        assert!(!is_spam_referrer(Some("https://example.com/")));
        assert!(!is_spam_referrer(None));
    }
}
