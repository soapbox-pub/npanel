//! Event content as HTML, for the article a crawler finds in a page's body.
//!
//! Whatever is written here is served on a site's own origin to anyone who
//! sends a crawler's User-Agent, which anyone can. So nothing an author wrote
//! reaches the page as markup: text is escaped, a URL is kept only when it is
//! http(s), and the HTML a Markdown article embeds is read as text.

use std::sync::OnceLock;

use pulldown_cmark::{CowStr, Event as Md, LinkType, Options, Parser, Tag, TagEnd};
use regex::Regex;

use crate::nostr::nip19::{self, Decoded};

/// Text made safe for an element's content or a quoted attribute value.
pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

/// A URL an author gave, if it is one a page may link to or load: absolute,
/// and http(s). `javascript:`, `data:` and the rest are dropped rather than
/// neutralized, since nothing a crawler needs is behind them.
pub fn safe_url(raw: &str) -> Option<String> {
    let url = url::Url::parse(raw.trim()).ok()?;
    (matches!(url.scheme(), "https" | "http") && url.host_str().is_some()).then(|| url.to_string())
}

/// What a URL's file is, by its extension.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaKind {
    Image,
    Video,
}

pub fn media_kind(url: &str) -> Option<MediaKind> {
    let path = url::Url::parse(url).ok()?.path().to_ascii_lowercase();
    let extension = path.rsplit_once('.')?.1;
    match extension {
        "jpg" | "jpeg" | "png" | "gif" | "webp" | "avif" | "svg" => Some(MediaKind::Image),
        "mp4" | "webm" | "mov" | "m4v" | "ogv" => Some(MediaKind::Video),
        _ => None,
    }
}

/// The MIME type of a video URL, by its extension, for `og:video:type`.
pub fn video_type(url: &str) -> &'static str {
    let path = url::Url::parse(url).map(|u| u.path().to_ascii_lowercase()).unwrap_or_default();
    match path.rsplit_once('.').map(|(_, extension)| extension) {
        Some("webm") => "video/webm",
        Some("mov") => "video/quicktime",
        Some("ogv") => "video/ogg",
        _ => "video/mp4",
    }
}

/// URLs and `nostr:` references in a note's text.
fn tokens() -> &'static Regex {
    static TOKENS: OnceLock<Regex> = OnceLock::new();
    TOKENS.get_or_init(|| {
        Regex::new(r#"(?i)\bhttps?://[^\s<>"']+|\bnostr:(?:npub|nprofile|note|nevent|naddr)1[02-9ac-hj-np-z]+"#).expect("regex")
    })
}

/// Every URL and `nostr:` reference in a text, as written, without the
/// punctuation around it.
pub fn urls(text: &str) -> Vec<String> {
    tokens().find_iter(text).map(|found| trim_trailing(found.as_str()).0.to_string()).collect()
}

/// A plain-text note as paragraphs, its links made links, its images and
/// videos shown, and the profiles and events it mentions linked to the page
/// for each on the same site.
pub fn render_text(content: &str) -> String {
    static PARAGRAPHS: OnceLock<Regex> = OnceLock::new();
    let paragraphs = PARAGRAPHS.get_or_init(|| Regex::new(r"\r?\n[ \t]*\r?\n").expect("regex"));

    let mut out = String::new();
    for paragraph in paragraphs.split(content).map(str::trim).filter(|p| !p.is_empty()) {
        out.push_str("<p>");
        let mut last = 0;
        for found in tokens().find_iter(paragraph) {
            out.push_str(&lines(&paragraph[last..found.start()]));
            let (token, trailing) = trim_trailing(found.as_str());
            out.push_str(&reference(token));
            out.push_str(&escape(trailing));
            last = found.end();
        }
        out.push_str(&lines(&paragraph[last..]));
        out.push_str("</p>\n");
    }
    out
}

/// Text with its line breaks kept.
fn lines(text: &str) -> String {
    escape(text).replace("\r\n", "\n").replace('\n', "<br>\n")
}

/// A URL found in running text, without the punctuation that ends the
/// sentence around it. A closing parenthesis stays when the URL opened one.
fn trim_trailing(token: &str) -> (&str, &str) {
    let mut end = token.len();
    while let Some(c) = token[..end].chars().last() {
        let unbalanced = c == ')' && token[..end].matches('(').count() < token[..end].matches(')').count();
        if matches!(c, '.' | ',' | ';' | ':' | '!' | '?') || unbalanced {
            end -= c.len_utf8();
        } else {
            break;
        }
    }
    (&token[..end], &token[end..])
}

/// One URL or `nostr:` reference from a note, as markup.
fn reference(token: &str) -> String {
    if let Some(entity) = nostr_path(token) {
        return format!(r#"<a href="{}">{}</a>"#, escape(&entity.0), escape(&entity.1));
    }
    let Some(url) = safe_url(token) else { return escape(token) };
    match media_kind(&url) {
        Some(MediaKind::Image) => format!(r#"<img src="{}" alt="" loading="lazy">"#, escape(&url)),
        Some(MediaKind::Video) => format!(r#"<video src="{}" controls preload="none"></video>"#, escape(&url)),
        None => format!(r#"<a href="{}" rel="nofollow ugc">{}</a>"#, escape(&url), escape(token)),
    }
}

/// The site's own path for a `nostr:` reference, and how to show it — or
/// `None` for anything that isn't one, or names a secret.
fn nostr_path(token: &str) -> Option<(String, String)> {
    let bech32 = crate::util::strip_prefix_ci(token, "nostr:");
    if bech32.len() == token.len() {
        return None;
    }
    let bech32 = bech32.to_ascii_lowercase();
    let decoded = nip19::decode(&bech32).ok()?;
    if matches!(decoded, Decoded::Nsec(_)) {
        return None;
    }
    let mention = matches!(decoded, Decoded::Npub(_) | Decoded::Nprofile { .. });
    let short = format!("{}{}…{}", if mention { "@" } else { "" }, &bech32[..bech32.len().min(12)], &bech32[bech32.len().saturating_sub(4)..]);
    Some((format!("/{bech32}"), short))
}

/// A Markdown article (NIP-23) as HTML.
///
/// HTML the article embeds is read as text — a tag's text kept, the tag
/// itself dropped — since nothing an author writes may reach the page as
/// markup. Links and images keep only http(s) URLs, `#` fragments, and
/// `nostr:` references, which become the site's own path for each.
pub fn render_markdown(content: &str) -> String {
    let options = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_FOOTNOTES | Options::ENABLE_TASKLISTS;
    let mut events: Vec<Md> = Vec::new();
    // A link or image whose URL was dropped keeps its text, unlinked.
    let (mut dropped_link, mut dropped_image) = (false, false);
    let (mut in_link, mut in_image, mut in_code) = (false, false, false);

    for event in Parser::new_ext(content, options) {
        match event {
            Md::Html(html) => events.push(Md::Text(strip_tags(&html).into())),
            Md::InlineHtml(_) => {}
            Md::Start(Tag::Link { link_type, dest_url, title, id }) => match markdown_url(&dest_url) {
                Some(url) => {
                    in_link = true;
                    events.push(Md::Start(Tag::Link { link_type, dest_url: url.into(), title, id }));
                }
                None => dropped_link = true,
            },
            Md::End(TagEnd::Link) if dropped_link => dropped_link = false,
            Md::End(TagEnd::Link) => {
                in_link = false;
                events.push(Md::End(TagEnd::Link));
            }
            Md::Start(Tag::Image { link_type, dest_url, title, id }) => match safe_url(&dest_url) {
                Some(url) => {
                    in_image = true;
                    events.push(Md::Start(Tag::Image { link_type, dest_url: url.into(), title, id }));
                }
                None => dropped_image = true,
            },
            Md::End(TagEnd::Image) if dropped_image => dropped_image = false,
            Md::End(TagEnd::Image) => {
                in_image = false;
                events.push(Md::End(TagEnd::Image));
            }
            Md::Start(Tag::CodeBlock(kind)) => {
                in_code = true;
                events.push(Md::Start(Tag::CodeBlock(kind)));
            }
            Md::End(TagEnd::CodeBlock) => {
                in_code = false;
                events.push(Md::End(TagEnd::CodeBlock));
            }
            Md::Text(text) if !in_link && !in_image && !in_code => link_mentions(text, &mut events),
            other => events.push(other),
        }
    }

    let mut html = String::new();
    pulldown_cmark::html::push_html(&mut html, events.into_iter());
    html
}

/// Where a Markdown link or image may point.
fn markdown_url(dest: &str) -> Option<String> {
    if let Some((path, _)) = nostr_path(dest) {
        return Some(path);
    }
    if dest.starts_with('#') {
        return Some(dest.to_string());
    }
    safe_url(dest)
}

/// Text, with each `nostr:` reference in it made a link.
fn link_mentions<'a>(text: CowStr<'a>, events: &mut Vec<Md<'a>>) {
    let mut last = 0;
    let mut linked = false;
    for found in tokens().find_iter(&text) {
        let Some((path, short)) = nostr_path(found.as_str()) else { continue };
        linked = true;
        if found.start() > last {
            events.push(Md::Text(text[last..found.start()].to_string().into()));
        }
        events.push(Md::Start(Tag::Link { link_type: LinkType::Inline, dest_url: path.into(), title: "".into(), id: "".into() }));
        events.push(Md::Text(short.into()));
        events.push(Md::End(TagEnd::Link));
        last = found.end();
    }
    if !linked {
        events.push(Md::Text(text));
    } else if last < text.len() {
        events.push(Md::Text(text[last..].to_string().into()));
    }
}

/// An article's words, without its markup, for a description.
pub fn markdown_text(content: &str) -> String {
    let mut words = String::new();
    for event in Parser::new(content) {
        match event {
            Md::Text(text) | Md::Code(text) => {
                words.push_str(&text);
                words.push(' ');
            }
            Md::Html(html) => {
                words.push_str(&strip_tags(&html));
                words.push(' ');
            }
            Md::SoftBreak | Md::HardBreak | Md::End(_) => words.push(' '),
            _ => {}
        }
    }
    words
}

fn strip_tags(html: &str) -> String {
    static TAG: OnceLock<Regex> = OnceLock::new();
    TAG.get_or_init(|| Regex::new(r"<[^>]*>?").expect("regex")).replace_all(html, "").into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    const PUBKEY: &str = "3bf0c63fcb93463407af97a5e5ee64fa883d107ef9e558472c4eb9aaaefa459d";

    #[test]
    fn escapes() {
        assert_eq!(escape(r#"<a href="x" onclick='y'>&"#), "&lt;a href=&quot;x&quot; onclick=&#39;y&#39;&gt;&amp;");
    }

    #[test]
    fn keeps_only_web_urls() {
        assert_eq!(safe_url("https://example.com/a b").as_deref(), Some("https://example.com/a%20b"));
        assert_eq!(safe_url("javascript:alert(1)"), None);
        assert_eq!(safe_url("data:text/html,<script>"), None);
        assert_eq!(safe_url("/relative"), None);
        assert_eq!(safe_url("https://"), None);
    }

    #[test]
    fn renders_notes() {
        let npub = nip19::npub_encode(PUBKEY);
        let html = render_text(&format!(
            "Hello <b>world</b> & friends.\nSee https://example.com/page. (https://example.com/x_(y))\n\nhttps://cdn.example.com/cat.JPG nostr:{npub}"
        ));
        assert!(html.starts_with("<p>Hello &lt;b&gt;world&lt;/b&gt; &amp; friends.<br>\nSee "), "{html}");
        assert!(html.contains(r#"<a href="https://example.com/page" rel="nofollow ugc">https://example.com/page</a>."#), "{html}");
        assert!(html.contains(r#"<a href="https://example.com/x_(y)" rel="nofollow ugc">https://example.com/x_(y)</a>)"#), "{html}");
        assert!(html.contains(r#"<p><img src="https://cdn.example.com/cat.JPG" alt="" loading="lazy">"#), "{html}");
        assert!(html.contains(&format!(r#"<a href="/{npub}">@{}…{}</a>"#, &npub[..12], &npub[npub.len() - 4..])), "{html}");
    }

    #[test]
    fn refuses_markup_in_notes() {
        for hostile in [
            "</script><script>alert(1)</script>",
            "https://example.com/\"onmouseover=\"alert(1)",
            "javascript:alert(1)",
            "<img src=x onerror=alert(1)>",
        ] {
            let html = render_text(hostile);
            assert!(!html.contains("<script"), "{html}");
            assert!(!html.contains("onmouseover=\""), "{html}");
            assert!(!html.contains("<img src=x"), "{html}");
            assert!(!html.contains("href=\"javascript"), "{html}");
        }
        // A secret is never linked, and so never put in a URL.
        let nsec = nip19::nsec_encode(PUBKEY);
        assert!(!render_text(&format!("nostr:{nsec}")).contains("href"));
    }

    #[test]
    fn renders_articles() {
        let npub = nip19::npub_encode(PUBKEY);
        let html = render_markdown(&format!(
            "# Title\n\nSome *text* by nostr:{npub}, [a link](https://example.com) and [mention](nostr:{npub}).\n\n![cover](https://cdn.example.com/c.png)\n"
        ));
        assert!(html.contains("<h1>Title</h1>"), "{html}");
        assert!(html.contains("<em>text</em>"), "{html}");
        assert!(html.contains(r#"<a href="https://example.com/">a link</a>"#), "{html}");
        assert!(html.contains(&format!(r#"<a href="/{npub}">mention</a>"#)), "{html}");
        assert!(html.contains(&format!(r#"<a href="/{npub}">@{}"#, &npub[..12])), "{html}");
        assert!(html.contains(r#"<img src="https://cdn.example.com/c.png" alt="cover" />"#), "{html}");
    }

    #[test]
    fn refuses_markup_in_articles() {
        let html = render_markdown(
            "<script>alert(1)</script>\n\nHi <img src=x onerror=alert(1)> there\n\n[x](javascript:alert(1)) ![y](data:image/png;base64,AA) [z](/relative)\n\n```\nnostr:npub1\n```\n\n<div onclick=\"x\">kept text</div>\n",
        );
        assert!(!html.contains("<script"), "{html}");
        assert!(!html.contains("<img src=x"), "{html}");
        assert!(!html.contains("onerror"), "{html}");
        assert!(!html.contains("javascript:"), "{html}");
        assert!(!html.contains("data:"), "{html}");
        assert!(!html.contains("/relative"), "{html}");
        assert!(!html.contains("onclick"), "{html}");
        assert!(html.contains("kept text"), "{html}");
        assert!(html.contains("<p>Hi  there</p>"), "{html}");
        assert!(html.contains("<p>x y z</p>"), "{html}");
    }

    #[test]
    fn reads_articles_as_text() {
        assert_eq!(markdown_text("# Head\n\nSome *em* `code`.").split_whitespace().collect::<Vec<_>>().join(" "), "Head Some em code .");
    }
}
