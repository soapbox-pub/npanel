//! A site's page made into the page about one event: the title and preview
//! tags in its head replaced with the event's, and the event's article added
//! at the top of its body.
//!
//! Edited as bytes, as `inject.rs` adds scripts, so everything the site wrote
//! that isn't replaced comes back exactly as it was.

use serde_json::{json, Map, Value};

use super::card::{iso_date, Card, CardKind};
use super::render::escape;
use crate::html::decode_attribute;
use crate::inject::{attributes, head_end, tags, Token};
use crate::nostr::nip19;

/// Where the page is.
pub struct Page<'a> {
    /// The page's own URL, which is also its canonical one.
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

/// The site's page about the card's event, or `None` for a page that isn't
/// one npanel can edit: not UTF-8, or with no head to speak of.
pub fn splice(html: &[u8], card: &Card, page: &Page) -> Option<Vec<u8>> {
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

    let head = head(card, page, &site);
    let mut edits: Vec<(usize, usize, &str)> = cut.into_iter().map(|(start, end)| (start, end, "")).collect();
    edits.push((head_at, head_at, &head));
    edits.push((body_at, body_at, &card.body));
    edits.sort_by_key(|(start, end, _)| (*start, *end));

    let mut out = Vec::with_capacity(html.len() + head.len() + card.body.len());
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

/// Everything the head says about the page: its title, its description, and
/// the Open Graph, Twitter and schema.org versions of both.
fn head(card: &Card, page: &Page, site: &Site) -> String {
    let author = card.author.as_ref().map(|author| author.display());
    // A card with no author is a site's own, and wanting for a title is
    // titled as the site.
    let title = card.title.clone().or_else(|| match (&author, &site.name) {
        (Some(author), Some(site)) => Some(format!("{author} on {site}")),
        (Some(author), None) => Some(author.clone()),
        (None, site) => site.clone(),
    });
    let title = title.unwrap_or_default();

    // The event's own image is drawn large; an avatar standing in for one is
    // a square, and drawn small beside the text.
    let avatar = card.author.as_ref().and_then(|author| author.picture.as_ref());
    let (image, large) = match (&card.image, avatar, &site.image) {
        (Some(image), _, _) => (Some(image), true),
        (None, Some(avatar), _) => (Some(avatar), false),
        (None, None, image) => (image.as_ref(), true),
    };
    let og_type = match card.kind {
        CardKind::Profile => "profile",
        CardKind::Video => "video.other",
        // A music track is a song; other audio — a podcast, a voice
        // message — has no type of its own.
        CardKind::Audio if card.event_kind == 36787 => "music.song",
        CardKind::Article | CardKind::Post | CardKind::Audio => "article",
    };

    let mut out = String::new();
    let mut meta = |attribute: &str, key: &str, value: &str| {
        out.push_str(&format!("<meta {attribute}=\"{key}\" content=\"{}\">\n", escape(value)));
    };
    meta("property", "og:type", og_type);
    if let Some(name) = &site.name {
        meta("property", "og:site_name", name);
    }
    meta("property", "og:title", &title);
    meta("property", "og:url", page.url);
    if let Some(description) = &card.description {
        meta("name", "description", description);
        meta("property", "og:description", description);
        meta("name", "twitter:description", description);
    }
    if let Some(image) = image {
        meta("property", "og:image", image);
        if let Some(drawn) = card.drawn.as_ref().filter(|_| card.image.is_some()) {
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
    if let Some(video) = &card.video {
        meta("property", "og:video", &video.url);
        meta("property", "og:video:secure_url", &video.url);
        meta("property", "og:video:type", &video.mime);
        if let (Some(width), Some(height)) = (video.width, video.height) {
            meta("property", "og:video:width", &width.to_string());
            meta("property", "og:video:height", &height.to_string());
        }
    }
    if let Some(audio) = &card.audio {
        meta("property", "og:audio", &audio.url);
        meta("property", "og:audio:secure_url", &audio.url);
        meta("property", "og:audio:type", &audio.mime);
    }
    if card.kind != CardKind::Profile {
        if let Some(published) = card.published.and_then(iso_date) {
            meta("property", "article:published_time", &published);
        }
    }
    meta("name", "twitter:card", if image.is_some() && large { "summary_large_image" } else { "summary" });
    meta("name", "twitter:title", &title);

    format!(
        "<title>{}</title>\n<link rel=\"canonical\" href=\"{}\">\n{out}<script type=\"application/ld+json\">{}</script>\n",
        escape(&title),
        escape(page.url),
        linked_data(card, page, &title, image)
    )
}

/// The page as schema.org describes it, written so nothing in it can close
/// the script element it sits in.
fn linked_data(card: &Card, page: &Page, title: &str, image: Option<&String>) -> String {
    let person = card.author.as_ref().map(|author| {
        let npub = nip19::npub_encode(&author.pubkey);
        json!({
            "@type": "Person",
            "name": author.display(),
            "identifier": npub,
            "url": format!("{}/{npub}", page.origin),
            "image": author.picture,
        })
    });

    let data = match card.kind {
        CardKind::Profile => {
            let entity = person.map(|person| {
                let mut entity = as_object(person);
                entity.insert("description".into(), json!(card.description));
                entity
            });
            json!({
                "@context": "https://schema.org",
                "@type": "ProfilePage",
                "url": page.url,
                "name": if entity.is_none() { Some(title) } else { None },
                "description": if entity.is_none() { card.description.as_deref() } else { None },
                "image": if entity.is_none() { image } else { None },
                "mainEntity": entity,
            })
        }
        kind => json!({
            "@context": "https://schema.org",
            "@type": if kind == CardKind::Article { "Article" } else { "SocialMediaPosting" },
            "headline": card.title.as_deref().or(card.description.as_deref()).unwrap_or(title).chars().take(110).collect::<String>(),
            "description": card.description,
            "url": page.url,
            "mainEntityOfPage": page.url,
            "datePublished": card.published.and_then(iso_date),
            "image": image,
            "video": card.video.as_ref().map(|video| json!({
                "@type": "VideoObject",
                "name": title,
                "contentUrl": video.url,
                "encodingFormat": video.mime,
                "thumbnailUrl": card.image,
                "uploadDate": card.published.and_then(iso_date),
            })),
            "audio": card.audio.as_ref().map(|audio| json!({
                "@type": "AudioObject",
                "name": title,
                "contentUrl": audio.url,
                "encodingFormat": audio.mime,
                "uploadDate": card.published.and_then(iso_date),
            })),
            "author": person,
        }),
    };

    without_nulls(data).to_string().replace('<', "\\u003c").replace('>', "\\u003e").replace('&', "\\u0026")
}

fn as_object(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        _ => Map::new(),
    }
}

fn without_nulls(value: Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(map.into_iter().filter(|(_, v)| !v.is_null()).map(|(k, v)| (k, without_nulls(v))).collect()),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::link_previews::card::{event_card, profile_card};
    use crate::nostr::Event;

    const SECRET: &str = "0000000000000000000000000000000000000000000000000000000000000003";
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

    fn note(content: &str) -> Card {
        let event = Event::sign(SECRET, 1_700_000_000, 1, vec![], content);
        let profile = Event::sign(SECRET, 1, 0, vec![], r#"{"name":"Alex","picture":"https://cdn.example/a.png"}"#);
        event_card(&event, Some(&profile)).unwrap()
    }

    fn spliced(html: &str, card: &Card) -> String {
        String::from_utf8(splice(html.as_bytes(), card, &PAGE).unwrap()).unwrap()
    }

    #[test]
    fn replaces_what_the_page_says_about_itself() {
        let out = spliced(DITTO, &note("Hello world"));
        assert!(out.contains("<title>Alex on Ditto</title>"), "{out}");
        assert_eq!(out.matches("<title>").count(), 1, "{out}");
        assert!(!out.contains("A social app."), "{out}");
        assert!(!out.contains("og:image:width"), "{out}");
        assert!(!out.contains("href=\"https://ditto.example/\""), "{out}");
        assert!(out.contains(r#"<link rel="icon" href="/favicon.ico" />"#), "{out}");
        assert!(out.contains(r#"<meta property="og:site_name" content="Ditto">"#), "{out}");
        assert!(out.contains(r#"<meta property="og:description" content="Hello world">"#), "{out}");
        assert!(out.contains(r#"<link rel="canonical" href="https://ditto.example/nevent1abc">"#), "{out}");
        // An avatar is a small picture beside the text.
        assert!(out.contains(r#"<meta property="og:image" content="https://cdn.example/a.png">"#), "{out}");
        assert!(out.contains(r#"<meta name="twitter:card" content="summary">"#), "{out}");
        assert!(out.contains("\"@type\":\"SocialMediaPosting\""), "{out}");
        // The article opens the body, and the app's root is left alone.
        assert!(out.contains("<body class=\"x\"><article>"), "{out}");
        assert!(out.contains("<div id=\"root\"></div>"), "{out}");
        assert!(out.contains("<p>Hello world</p>"), "{out}");
    }

    #[test]
    fn falls_back_to_the_sites_image() {
        let event = Event::sign(SECRET, 1, 1, vec![], "No picture anywhere");
        let out = spliced(DITTO, &event_card(&event, None).unwrap());
        assert!(out.contains(r#"<meta property="og:image" content="https://ditto.example/og-image.jpg">"#), "{out}");
        assert!(out.contains(r#"<meta name="twitter:card" content="summary_large_image">"#), "{out}");
    }

    #[test]
    fn describes_profiles() {
        let profile = Event::sign(SECRET, 1, 0, vec![], r#"{"name":"Alex","about":"Builds things."}"#);
        let out = spliced(DITTO, &profile_card(&profile));
        assert!(out.contains(r#"<meta property="og:type" content="profile">"#), "{out}");
        assert!(out.contains("\"@type\":\"ProfilePage\""), "{out}");
        assert!(out.contains("\"description\":\"Builds things.\""), "{out}");
    }

    #[test]
    fn describes_audio() {
        let tags = vec![
            vec!["d".into(), "song".into()],
            vec!["title".into(), "A Song".into()],
            vec!["imeta".into(), "url https://cdn.example/s.mp3".into(), "m audio/mpeg".into(), "image https://cdn.example/cover.jpg".into()],
        ];
        let track = Event::sign(SECRET, 1, 36787, tags, "");
        let out = spliced(DITTO, &event_card(&track, None).unwrap());
        assert!(out.contains(r#"<meta property="og:type" content="music.song">"#), "{out}");
        assert!(out.contains(r#"<meta property="og:audio" content="https://cdn.example/s.mp3">"#), "{out}");
        assert!(out.contains(r#"<meta property="og:audio:type" content="audio/mpeg">"#), "{out}");
        assert!(out.contains(r#"<meta property="og:image" content="https://cdn.example/cover.jpg">"#), "{out}");
        assert!(out.contains("\"@type\":\"AudioObject\""), "{out}");
        assert!(out.contains(r#"<audio src="https://cdn.example/s.mp3" controls preload="none"></audio>"#), "{out}");

        let voice = Event::sign(SECRET, 1, 1222, vec![], "https://cdn.example/v.m4a");
        let out = spliced(DITTO, &event_card(&voice, None).unwrap());
        assert!(out.contains(r#"<meta property="og:type" content="article">"#), "{out}");
        assert!(out.contains(r#"<meta property="og:audio:type" content="audio/mp4">"#), "{out}");
    }

    #[test]
    fn nothing_an_author_writes_escapes_its_element() {
        let hostile = r#"</script><script>alert(1)</script>"><img src=x onerror=alert(1)>"#;
        let event = Event::sign(SECRET, 1, 30023, vec![vec!["title".into(), hostile.into()], vec!["d".into(), "x".into()]], hostile);
        let profile = Event::sign(SECRET, 1, 0, vec![], &serde_json::json!({ "name": hostile }).to_string());
        let out = spliced(DITTO, &event_card(&event, Some(&profile)).unwrap());
        assert!(!out.contains("<script>alert"), "{out}");
        assert!(!out.contains("<img src=x"), "{out}");
        assert!(!out.contains("\"><img"), "{out}");
        assert_eq!(out.matches("</script>").count(), 1, "{out}");
        assert!(out.contains("\\u003c/script\\u003e"), "{out}");
    }

    #[test]
    fn handles_pages_without_the_usual_parts() {
        let card = note("hi");
        let out = spliced("<html><head><title>Site</title></head><p>x</p></html>", &card);
        assert!(out.contains("</head><article>"), "{out}");
        assert!(out.contains("<title>Alex on Site</title>"), "{out}");
        let out = spliced("<html><head><meta name=description content=x></head><body>", &card);
        assert!(out.contains("<body><article"), "{out}");
        assert!(!out.contains("content=x"), "{out}");
        // An unclosed title holds the rest of the document, a body included.
        assert!(splice(b"<html><title>Unclosed<body>", &card, &PAGE).is_none());
        assert!(splice(b"<div>fragment</div>", &card, &PAGE).is_none());
        assert!(splice(b"<head>\xff</head><body>", &card, &PAGE).is_none());
    }
}
