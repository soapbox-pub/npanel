//! A page's preview, as a site's script made it and npanel keeps it: held to
//! what npanel will write into a page, and nothing more.

use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};

use crate::inject::{attributes, tags, Token};
use crate::site_preview::clamp;

const MAX_TITLE: usize = 200;
const MAX_DESCRIPTION: usize = 300;
/// The longest article a script may write.
const MAX_BODY: usize = 256 * 1024;
/// The largest schema.org object a script may describe a page with.
const MAX_JSON_LD: usize = 64 * 1024;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Preview {
    pub title: Option<String>,
    pub description: Option<String>,
    /// The `<article>` a crawler finds at the top of the page's body, cleaned
    /// of anything that runs.
    pub body: String,
    /// The page's image, as an http(s) URL.
    pub image: Option<String>,
    /// The URL the page is one copy of, on the same site: the page's own when
    /// it's missing.
    pub canonical: Option<String>,
    /// The image the script drew, which `image` is the URL of.
    pub drawn: Option<Drawn>,
    /// `og:type`.
    pub kind: Option<String>,
    /// Whether the image is shown large beside the link, or small.
    pub large: Option<bool>,
    pub video: Option<Media>,
    pub audio: Option<Media>,
    /// When what the page is about was published, in seconds.
    pub published: Option<i64>,
    /// The page as schema.org describes it.
    pub json_ld: Option<serde_json::Value>,
}

/// An image a site's script drew, which npanel serves.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Drawn {
    /// What names it: see `script/mod.rs`.
    pub version: String,
    /// Its type and size, where it was drawn by the time the preview was made.
    pub mime: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Media {
    pub url: String,
    pub mime: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

/// What a script returned besides its title, description, article and
/// image, as it wrote it.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Extras {
    #[serde(rename = "type")]
    pub kind: Option<String>,
    pub twitter: Option<String>,
    pub video: Option<RawMedia>,
    pub audio: Option<RawMedia>,
    pub published: Option<f64>,
    pub canonical: Option<String>,
    #[serde(rename = "jsonLd")]
    pub json_ld: Option<serde_json::Value>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct RawMedia {
    pub url: Option<String>,
    #[serde(rename = "type")]
    pub mime: Option<String>,
    pub width: Option<f64>,
    pub height: Option<f64>,
}

impl Preview {
    /// A preview from what a script returned, relative to the page at `url`.
    pub fn new(url: &str, title: Option<String>, description: Option<String>, body: Option<String>, extras: Extras) -> Preview {
        let body = body.map(|body| vouch_for_none_elsewhere(&ammonia::Builder::default().link_rel(None).clean(truncate(&body, MAX_BODY)).to_string(), url));
        let media = |raw: Option<RawMedia>| {
            let raw = raw?;
            let size = |n: Option<f64>| n.filter(|n| *n >= 1.0 && *n <= 100_000.0).map(|n| n as u32);
            Some(Media {
                url: absolute(url, raw.url.as_deref()?)?,
                mime: raw.mime.map(|m| clamp(&m, 100)).filter(|m| !m.is_empty()),
                width: size(raw.width),
                height: size(raw.height),
            })
        };
        let json_ld = extras.json_ld.filter(|value| value.is_object() || value.is_array()).filter(|value| value.to_string().len() <= MAX_JSON_LD);
        Preview {
            title: title.map(|t| clamp(&t, MAX_TITLE)).filter(|t| !t.is_empty()),
            description: description.map(|d| clamp(&d, MAX_DESCRIPTION)).filter(|d| !d.is_empty()),
            body: body.map(|body| format!("{body}\n")).unwrap_or_default(),
            image: None,
            canonical: extras.canonical.and_then(|named| on_site(url, &named)).map(|url| url.to_string()),
            drawn: None,
            kind: extras.kind.map(|k| clamp(&k, 100)).filter(|k| !k.is_empty()),
            large: extras.twitter.as_deref().and_then(|card| match card {
                "summary_large_image" => Some(true),
                "summary" => Some(false),
                _ => None,
            }),
            video: media(extras.video),
            audio: media(extras.audio),
            published: extras.published.filter(|at| at.is_finite() && *at >= 0.0).map(|at| at as i64),
            json_ld,
        }
    }

    /// Roughly what it takes up in memory, for the cache that holds them.
    pub fn weight(&self) -> usize {
        self.body.len() + self.json_ld.as_ref().map_or(0, |v| v.to_string().len()) + 1024
    }
}

/// A URL a script named, taken relative to the page, if it's http(s).
pub fn absolute(page: &str, named: &str) -> Option<String> {
    let url = url::Url::parse(page).ok()?.join(named.trim()).ok()?;
    matches!(url.scheme(), "https" | "http").then(|| url.to_string())
}

/// `named`, taken relative to the page, if it's on the page's own site.
fn on_site(page: &str, named: &str) -> Option<url::Url> {
    let page = url::Url::parse(page).ok()?;
    let url = page.join(named.trim()).ok()?;
    (matches!(url.scheme(), "https" | "http") && url.origin() == page.origin()).then_some(url)
}

/// A sanitized article with `nofollow` on every link that leaves the site.
/// What an article links to elsewhere is what Nostr's authors wrote, which
/// the site doesn't vouch for; its links to its own pages are how a crawler
/// finds them, and a crawler may not follow a `nofollow` one.
fn vouch_for_none_elsewhere(body: &str, page: &str) -> String {
    let mut out = String::with_capacity(body.len());
    let mut at = 0;
    for (start, end, token) in tags(body.to_ascii_lowercase().as_bytes()) {
        if token != Token::Start(b"a".to_vec()) {
            continue;
        }
        let href = attributes(&body.as_bytes()[start..end], b"a").into_iter().find(|(name, _)| name == "href").map(|(_, href)| href);
        if href.is_none_or(|href| on_site(page, &href).is_some()) {
            continue;
        }
        // The sanitizer writes every tag out whole, ending in `>`.
        out.push_str(&body[at..end - 1]);
        out.push_str(" rel=\"nofollow ugc noopener\"");
        at = end - 1;
    }
    out.push_str(&body[at..]);
    out
}

/// Text cut to at most `max` bytes, on a character's boundary.
fn truncate(text: &str, max: usize) -> &str {
    let mut end = text.len().min(max);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// A time, as Open Graph writes one.
pub fn iso_date(at: i64) -> Option<String> {
    DateTime::<Utc>::from_timestamp(at, 0).map(|at| at.to_rfc3339_opts(SecondsFormat::Secs, true))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn holds_a_script_to_what_a_page_may_say() {
        let extras: Extras = serde_json::from_value(serde_json::json!({
            "type": "article",
            "twitter": "summary",
            "video": { "url": "/v.mp4", "type": "video/mp4", "width": 720, "height": -1 },
            "audio": { "url": "javascript:alert(1)" },
            "published": 1_600_000_000,
            "canonical": "https://elsewhere.example/npub1",
            "jsonLd": "not an object",
        }))
        .unwrap();
        let body = Some(r#"<p onclick="steal()">Hi<script>alert(1)</script><a href="https://x.example">x</a></p>"#.into());
        let preview = Preview::new("https://site.example/npub1", Some(" A   title ".into()), Some(String::new()), body, extras);
        assert_eq!(preview.title.as_deref(), Some("A title"));
        assert_eq!(preview.description, None);
        assert_eq!(preview.body, "<p>Hi<a href=\"https://x.example\" rel=\"nofollow ugc noopener\">x</a></p>\n");
        assert_eq!((preview.kind.as_deref(), preview.large), (Some("article"), Some(false)));
        assert_eq!(preview.video, Some(Media { url: "https://site.example/v.mp4".into(), mime: Some("video/mp4".into()), width: Some(720), height: None }));
        assert_eq!(preview.audio, None);
        assert_eq!(preview.published, Some(1_600_000_000));
        assert_eq!(preview.json_ld, None);
        assert_eq!(preview.canonical, None);
    }

    #[test]
    fn follows_links_on_the_site_only() {
        let body = r#"<a href="/npub1x" rel="author">a</a><a href="https://site.example/nevent1y">b</a><a href="//elsewhere.example/">c</a><a href="https://x.example" rel="me">d</a><a>e</a>"#;
        let extras = Extras { canonical: Some("/npub1x".into()), ..Default::default() };
        let preview = Preview::new("https://site.example/alice@site.example", None, None, Some(body.into()), extras);
        assert_eq!(
            preview.body,
            concat!(
                r#"<a href="/npub1x">a</a><a href="https://site.example/nevent1y">b</a>"#,
                r#"<a href="//elsewhere.example/" rel="nofollow ugc noopener">c</a><a href="https://x.example" rel="nofollow ugc noopener">d</a><a>e</a>"#,
                "\n"
            )
        );
        assert_eq!(preview.canonical.as_deref(), Some("https://site.example/npub1x"));
    }
}
