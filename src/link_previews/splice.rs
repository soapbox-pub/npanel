//! A site's page made into the page its preview describes: the title and
//! preview tags in its head replaced with the preview's, and the preview's
//! article added at the top of its body.
//!
//! Edited as bytes, as `inject.rs` adds scripts, so everything the site wrote
//! that isn't replaced comes back exactly as it was.

use super::preview::{iso_date, Preview};
use crate::html::{decode_attribute, escape};
use crate::inject::{attributes, head_end, tags, Token};

/// Where the page is.
pub struct Page<'a> {
    /// The page's own URL, which is its canonical one unless the preview names another.
    pub url: &'a str,
    /// The site's origin, `https://example.com`, with no trailing slash.
    pub origin: &'a str,
}

/// What a site's page says about the site, for a page about something else.
#[derive(Default, Debug, PartialEq)]
struct Site {
    name: Option<String>,
    image: Option<String>,
}

/// The site's page as its preview describes it, or `None` for a page that
/// isn't one npanel can edit: not UTF-8, or with no head to speak of.
pub fn splice(html: &[u8], preview: &Preview, page: &Page) -> Option<Vec<u8>> {
    let text = std::str::from_utf8(html).ok()?;
    let lower = html.to_ascii_lowercase();
    let tags = tags(&lower);
    let head_at = head_end(&lower)?;
    let body_at = tags
        .iter()
        .find(|(_, _, token)| *token == Token::Start(b"body".to_vec()))
        .or_else(|| tags.iter().find(|(_, _, token)| *token == Token::End(b"head".to_vec())))
        .map(|(_, end, _)| *end)?;

    let mut site = Site::default();
    let mut document_title = None;
    // Every tag that says what the page is about, which is no longer true.
    let mut cut: Vec<(usize, usize)> = Vec::new();

    for (index, (start, end, token)) in tags.iter().enumerate() {
        if *start >= body_at {
            break;
        }
        let Token::Start(name) = token else { continue };
        match name.as_slice() {
            b"title" => {
                let close = tags[index + 1..].iter().find(|(_, _, t)| *t == Token::End(b"title".to_vec()));
                let (text_end, close_end) = close.map_or((*end, *end), |(s, e, _)| (*s, *e));
                document_title.get_or_insert_with(|| decode_attribute(text[*end..text_end].trim()));
                cut.push((*start, close_end));
            }
            b"meta" => {
                let attributes = attributes(&html[*start..*end], b"meta");
                let get = |key: &str| attributes.iter().find(|(name, _)| name == key).map(|(_, value)| value.trim().to_string());
                let key = get("property").filter(|p| !p.is_empty()).or_else(|| get("name")).unwrap_or_default().to_lowercase();
                let content = get("content").filter(|c| !c.is_empty());
                match key.as_str() {
                    "og:site_name" => site.name = site.name.take().or(content),
                    "og:image" | "og:image:url" => site.image = site.image.take().or(content),
                    _ => {}
                }
                if key == "description" || key.starts_with("og:") || key.starts_with("twitter:") || key.starts_with("article:") {
                    cut.push((*start, *end));
                }
            }
            b"link" => {
                let attributes = attributes(&html[*start..*end], b"link");
                let canonical = attributes.iter().any(|(name, value)| name == "rel" && value.split_ascii_whitespace().any(|r| r.eq_ignore_ascii_case("canonical")));
                if canonical {
                    cut.push((*start, *end));
                }
            }
            _ => {}
        }
    }

    // A document's title is often the site's name and a slogan; the name is
    // the part before the first separator.
    site.name = site.name.or_else(|| {
        let title = document_title?;
        let name = [" — ", " – ", " - ", " | ", " · ", ": "].iter().fold(title.as_str(), |name, separator| name.split(separator).next().unwrap_or(name));
        Some(name.trim().to_string()).filter(|n| !n.is_empty())
    });
    site.image = site.image.and_then(|image| url::Url::parse(page.origin).ok()?.join(&image).ok()).filter(|u| matches!(u.scheme(), "https" | "http")).map(|u| u.to_string());

    let head = head(preview, page, &site);
    let mut edits: Vec<(usize, usize, &str)> = cut.into_iter().map(|(start, end)| (start, end, "")).collect();
    edits.push((head_at, head_at, &head));
    edits.push((body_at, body_at, &preview.body));
    edits.sort_by_key(|(start, end, _)| (*start, *end));

    let mut out = Vec::with_capacity(html.len() + head.len() + preview.body.len());
    let mut at = 0;
    for (start, end, replacement) in edits {
        // A tag inside one already cut — a meta inside an unclosed title.
        if start < at {
            continue;
        }
        out.extend_from_slice(&html[at..start]);
        out.extend_from_slice(replacement.as_bytes());
        at = end;
    }
    out.extend_from_slice(&html[at..]);
    Some(out)
}

/// Everything the head says about the page: what the preview says, as the
/// Open Graph, Twitter and schema.org tags that say it, and nothing else. A
/// preview with no title is titled as the site, and one with no image shows
/// the site's.
fn head(preview: &Preview, page: &Page, site: &Site) -> String {
    let title = preview.title.as_ref().or(site.name.as_ref());
    let (image, large) = match (&preview.image, &site.image) {
        (Some(image), _) => (Some(image), preview.large.unwrap_or(true)),
        (None, image) => (image.as_ref(), true),
    };

    let canonical = preview.canonical.as_deref().unwrap_or(page.url);

    let mut out = String::new();
    if let Some(title) = title {
        out.push_str(&format!("<title>{}</title>\n", escape(title)));
    }
    out.push_str(&format!("<link rel=\"canonical\" href=\"{}\">\n", escape(canonical)));
    let mut meta = |attribute: &str, key: &str, value: &str| {
        out.push_str(&format!("<meta {attribute}=\"{key}\" content=\"{}\">\n", escape(value)));
    };
    meta("property", "og:type", preview.kind.as_deref().unwrap_or("website"));
    if let Some(name) = &site.name {
        meta("property", "og:site_name", name);
    }
    if let Some(title) = title {
        meta("property", "og:title", title);
        meta("name", "twitter:title", title);
    }
    meta("property", "og:url", canonical);
    if let Some(description) = &preview.description {
        meta("name", "description", description);
        meta("property", "og:description", description);
        meta("name", "twitter:description", description);
    }
    if let Some(image) = image {
        meta("property", "og:image", image);
        if let Some(drawn) = preview.drawn.as_ref().filter(|_| preview.image.is_some()) {
            if let Some(mime) = &drawn.mime {
                meta("property", "og:image:type", mime);
            }
            if let (Some(width), Some(height)) = (drawn.width, drawn.height) {
                meta("property", "og:image:width", &width.to_string());
                meta("property", "og:image:height", &height.to_string());
            }
        }
        meta("name", "twitter:image", image);
    }
    for (name, media) in [("og:video", &preview.video), ("og:audio", &preview.audio)] {
        let Some(media) = media else { continue };
        meta("property", name, &media.url);
        meta("property", &format!("{name}:secure_url"), &media.url);
        if let Some(mime) = &media.mime {
            meta("property", &format!("{name}:type"), mime);
        }
        if let (Some(width), Some(height)) = (media.width, media.height) {
            meta("property", &format!("{name}:width"), &width.to_string());
            meta("property", &format!("{name}:height"), &height.to_string());
        }
    }
    if let Some(published) = preview.published.and_then(iso_date) {
        meta("property", "article:published_time", &published);
    }
    meta("name", "twitter:card", if image.is_some() && large { "summary_large_image" } else { "summary" });
    if let Some(data) = &preview.json_ld {
        // Written so nothing in it can close the script element it sits in.
        let data = data.to_string().replace('<', "\\u003c").replace('>', "\\u003e").replace('&', "\\u0026");
        out.push_str(&format!("<script type=\"application/ld+json\">{data}</script>\n"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::link_previews::preview::{Drawn, Media};

    const PAGE: Page = Page { url: "https://ditto.example/nevent1abc", origin: "https://ditto.example" };

    const DITTO: &str = r#"<!doctype html>
<html lang="en">
  <head>
    <meta charset="UTF-8" />
    <title>Ditto — Your content. Your vibe.</title>
    <meta name="description" content="A social app." />
    <meta property="og:title" content="Ditto" />
    <meta property="og:image" content="/og-image.jpg" />
    <meta property="og:image:width" content="1200" />
    <meta name="twitter:card" content="summary_large_image" />
    <link rel="canonical" href="https://ditto.example/" />
    <link rel="icon" href="/favicon.ico" />
  </head>
  <body class="x">
    <div id="root"></div>
  </body>
</html>"#;

    fn note() -> Preview {
        Preview {
            title: Some("Alex on Ditto".into()),
            description: Some("Hello world".into()),
            body: "<article><p>Hello world</p></article>\n".into(),
            image: Some("https://cdn.example/a.png".into()),
            large: Some(false),
            kind: Some("article".into()),
            published: Some(1_700_000_000),
            ..Default::default()
        }
    }

    fn spliced(html: &str, preview: &Preview) -> String {
        String::from_utf8(splice(html.as_bytes(), preview, &PAGE).unwrap()).unwrap()
    }

    #[test]
    fn replaces_what_the_page_says_about_itself() {
        let out = spliced(DITTO, &note());
        assert!(out.contains("<title>Alex on Ditto</title>"), "{out}");
        assert_eq!(out.matches("<title>").count(), 1, "{out}");
        assert!(!out.contains("A social app."), "{out}");
        assert!(!out.contains("og:image:width"), "{out}");
        assert!(!out.contains("href=\"https://ditto.example/\""), "{out}");
        assert!(out.contains(r#"<link rel="icon" href="/favicon.ico" />"#), "{out}");
        assert!(out.contains(r#"<meta property="og:site_name" content="Ditto">"#), "{out}");
        assert!(out.contains(r#"<meta property="og:type" content="article">"#), "{out}");
        assert!(out.contains(r#"<meta property="og:description" content="Hello world">"#), "{out}");
        assert!(out.contains(r#"<meta property="article:published_time" content="2023-11-14T22:13:20Z">"#), "{out}");
        assert!(out.contains(r#"<link rel="canonical" href="https://ditto.example/nevent1abc">"#), "{out}");
        assert!(out.contains(r#"<meta property="og:image" content="https://cdn.example/a.png">"#), "{out}");
        assert!(out.contains(r#"<meta name="twitter:card" content="summary">"#), "{out}");
        assert!(!out.contains("ld+json"), "{out}");
        // The article opens the body, and the app's root is left alone.
        assert!(out.contains("<body class=\"x\"><article>"), "{out}");
        assert!(out.contains("<div id=\"root\"></div>"), "{out}");
    }

    #[test]
    fn names_the_canonical_copy() {
        let preview = Preview { canonical: Some("https://ditto.example/npub1abc".into()), ..note() };
        let out = spliced(DITTO, &preview);
        assert!(out.contains(r#"<link rel="canonical" href="https://ditto.example/npub1abc">"#), "{out}");
        assert!(out.contains(r#"<meta property="og:url" content="https://ditto.example/npub1abc">"#), "{out}");
        assert!(!out.contains("nevent1abc"), "{out}");
    }

    #[test]
    fn falls_back_to_the_site() {
        let out = spliced(DITTO, &Preview::default());
        assert!(out.contains("<title>Ditto</title>"), "{out}");
        assert!(out.contains(r#"<meta property="og:type" content="website">"#), "{out}");
        assert!(out.contains(r#"<meta property="og:image" content="https://ditto.example/og-image.jpg">"#), "{out}");
        assert!(out.contains(r#"<meta name="twitter:card" content="summary_large_image">"#), "{out}");
    }

    #[test]
    fn writes_media_and_drawn_images() {
        let preview = Preview {
            image: Some("https://ditto.example/_npanel/preview/x?v=1".into()),
            drawn: Some(Drawn { version: "1".into(), mime: Some("image/jpeg".into()), width: Some(1200), height: Some(630) }),
            audio: Some(Media { url: "https://cdn.example/s.mp3".into(), mime: Some("audio/mpeg".into()), ..Default::default() }),
            ..Default::default()
        };
        let out = spliced(DITTO, &preview);
        assert!(out.contains(r#"<meta property="og:image:width" content="1200">"#), "{out}");
        assert!(out.contains(r#"<meta property="og:audio:type" content="audio/mpeg">"#), "{out}");
    }

    #[test]
    fn nothing_a_script_writes_escapes_its_element() {
        let hostile = r#"</script><script>alert(1)</script>"><img src=x onerror=alert(1)>"#;
        let preview = Preview { title: Some(hostile.into()), json_ld: Some(serde_json::json!({ "name": hostile })), ..Default::default() };
        let out = spliced(DITTO, &preview);
        assert!(!out.contains("<script>alert"), "{out}");
        assert!(!out.contains("\"><img"), "{out}");
        assert_eq!(out.matches("</script>").count(), 1, "{out}");
        assert!(out.contains("\\u003c/script\\u003e"), "{out}");
    }

    #[test]
    fn handles_pages_without_the_usual_parts() {
        let preview = note();
        let out = spliced("<html><head><title>Site</title></head><p>x</p></html>", &preview);
        assert!(out.contains("</head><article>"), "{out}");
        let out = spliced("<html><head><meta name=description content=x></head><body>", &preview);
        assert!(out.contains("<body><article"), "{out}");
        assert!(!out.contains("content=x"), "{out}");
        // An unclosed title holds the rest of the document, a body included.
        assert!(splice(b"<html><title>Unclosed<body>", &preview, &PAGE).is_none());
        assert!(splice(b"<div>fragment</div>", &preview, &PAGE).is_none());
        assert!(splice(b"<head>\xff</head><body>", &preview, &PAGE).is_none());
    }
}
