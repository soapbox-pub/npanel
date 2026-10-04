//! What a page about a Nostr event says about it: the fields a link preview
//! shows, and the article a search engine reads. Taken from the event once,
//! when it is fetched, and cached; nothing here depends on which site serves
//! the page, so one card serves every site.

use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::render::{escape, markdown_text, media_kind, render_markdown, render_text, safe_url, video_type, MediaKind};
use crate::nostr::nip19;
use crate::nostr::Event;
use crate::site_preview::clamp;

/// The most of an event's content that is rendered. A long article is cut
/// here rather than left out: its first hundred-odd kilobytes are the page.
const MAX_CONTENT: usize = 128 * 1024;
const MAX_TITLE: usize = 200;
const MAX_DESCRIPTION: usize = 200;
const MAX_NAME: usize = 100;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CardKind {
    Profile,
    Article,
    Post,
    Video,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Author {
    pub pubkey: String,
    pub name: Option<String>,
    /// Their avatar, as an http(s) URL.
    pub picture: Option<String>,
}

impl Author {
    /// Their name, or a shortened npub for someone without one.
    pub fn display(&self) -> String {
        self.name.clone().unwrap_or_else(|| short_npub(&self.pubkey))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Video {
    pub url: String,
    pub mime: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Card {
    pub kind: CardKind,
    /// The event's own title, where it has one. A note has none, and its page
    /// is titled after its author.
    pub title: Option<String>,
    pub description: Option<String>,
    /// The event's own image, as an http(s) URL.
    pub image: Option<String>,
    pub video: Option<Video>,
    pub author: Author,
    pub published: Option<i64>,
    /// The `<article>` a crawler finds at the top of the page's body.
    pub body: String,
}

impl Card {
    /// Roughly what it takes up in memory, for the cache that holds them.
    pub fn weight(&self) -> usize {
        self.body.len() + self.description.as_ref().map_or(0, String::len) + 512
    }
}

/// A kind 0, as far as a preview reads it.
#[derive(Default)]
struct Profile {
    name: Option<String>,
    about: Option<String>,
    picture: Option<String>,
    website: Option<String>,
    nip05: Option<String>,
}

fn read_profile(event: &Event) -> Profile {
    let Ok(Value::Object(fields)) = serde_json::from_str::<Value>(&event.content) else {
        return Profile::default();
    };
    let field = |key: &str| fields.get(key).and_then(Value::as_str).map(str::trim).filter(|v| !v.is_empty()).map(str::to_string);
    Profile {
        name: field("display_name").or_else(|| field("displayName")).or_else(|| field("name")).map(|name| clamp(&name, MAX_NAME)),
        about: field("about").map(|about| truncate(&about, MAX_CONTENT).to_string()),
        picture: field("picture").and_then(|url| safe_url(&url)),
        website: field("website").and_then(|url| safe_url(&url)),
        nip05: field("nip05").map(|nip05| clamp(&nip05, MAX_NAME)),
    }
}

/// A profile's card, from its kind 0.
pub fn profile_card(event: &Event) -> Card {
    let profile = read_profile(event);
    let author = Author { pubkey: event.pubkey.clone(), name: profile.name.clone(), picture: profile.picture.clone() };
    let name = author.display();

    let mut body = format!("<article data-npanel-preview>\n<header>\n<h1>{}</h1>\n", escape(&name));
    if let Some(nip05) = &profile.nip05 {
        body.push_str(&format!("<p>{}</p>\n", escape(nip05.strip_prefix("_@").unwrap_or(nip05))));
    }
    body.push_str("</header>\n");
    if let Some(picture) = &profile.picture {
        body.push_str(&format!("<p><img src=\"{}\" alt=\"{}\"></p>\n", escape(picture), escape(&name)));
    }
    if let Some(about) = &profile.about {
        body.push_str(&render_text(about));
    }
    if let Some(website) = &profile.website {
        body.push_str(&format!("<p><a href=\"{0}\" rel=\"me nofollow ugc\">{0}</a></p>\n", escape(website)));
    }
    body.push_str("</article>\n");

    Card {
        kind: CardKind::Profile,
        title: None,
        description: profile.about.as_deref().map(describe).filter(|d| !d.is_empty()),
        image: None,
        video: None,
        author,
        published: None,
        body,
    }
}

/// An event's card. `author` is its author's kind 0, where one was found.
pub fn event_card(event: &Event, author: Option<&Event>) -> Card {
    if event.kind == 0 {
        return profile_card(event);
    }

    let profile = author.filter(|a| a.kind == 0 && a.pubkey == event.pubkey).map(read_profile).unwrap_or_default();
    let author = Author { pubkey: event.pubkey.clone(), name: profile.name, picture: profile.picture };
    let content = truncate(&event.content, MAX_CONTENT);
    let media = Media::of(event, content);
    let markdown = matches!(event.kind, 30023 | 30024);

    let mut title = tag(event, "title").or_else(|| tag(event, "name")).or_else(|| tag(event, "subject"));
    let mut summary = tag(event, "summary").or_else(|| tag(event, "description"));
    let mut image = tag(event, "image").or_else(|| tag(event, "thumb")).and_then(|url| safe_url(&url)).or(media.image);
    let mut video = media.video;
    let mut published = event.created_at;

    match event.kind {
        30023 | 30024 => {
            published = tag(event, "published_at").and_then(|at| at.parse().ok()).filter(|at| *at > 0).unwrap_or(published);
        }
        // NIP-94: the file itself is the image or the video.
        1063 => {
            let mime = tag(event, "m").unwrap_or_default();
            if let Some(url) = tag(event, "url").and_then(|url| safe_url(&url)) {
                if mime.starts_with("image/") {
                    image = Some(url);
                } else if mime.starts_with("video/") {
                    let (width, height) = dimensions(tag(event, "dim").as_deref());
                    video = Some(Video { url, mime, width, height });
                }
            }
            title = title.or_else(|| tag(event, "alt"));
        }
        30311 if tag(event, "status").as_deref() == Some("live") => title = title.map(|t| format!("{t} (live)")),
        31922 | 31923 => {
            if let Some(location) = tag(event, "location") {
                let what = summary.take().or_else(|| Some(content.trim().to_string())).filter(|s| !s.is_empty());
                summary = Some(match what {
                    Some(what) => format!("{what} — {location}"),
                    None => location,
                });
            }
        }
        _ => {}
    }

    let text = if markdown { markdown_text(content) } else { without_references(content) };
    // NIP-31: an `alt` describes an event its content doesn't.
    let description = summary.or_else(|| Some(text).filter(|t| !t.trim().is_empty())).or_else(|| tag(event, "alt"));

    let kind = if markdown {
        CardKind::Article
    } else if video.is_some() {
        CardKind::Video
    } else {
        CardKind::Post
    };
    let title = title.map(|t| clamp(&t, MAX_TITLE));

    let mut body = String::from("<article data-npanel-preview>\n<header>\n");
    if let Some(title) = &title {
        body.push_str(&format!("<h1>{}</h1>\n", escape(title)));
    }
    body.push_str(&format!(
        "<p><a href=\"/{}\" rel=\"author\">{}</a> · {}</p>\n</header>\n",
        nip19::npub_encode(&event.pubkey),
        escape(&author.display()),
        time(published)
    ));
    // Media a note names only in its tags is shown too; what its text names
    // is shown where the text names it.
    if let Some(video) = video.as_ref().filter(|v| !content.contains(&v.url)) {
        let poster = image.as_ref().map(|i| format!(" poster=\"{}\"", escape(i))).unwrap_or_default();
        body.push_str(&format!("<p><video src=\"{}\"{poster} controls preload=\"none\"></video></p>\n", escape(&video.url)));
    } else if let Some(image) = image.as_ref().filter(|i| !content.contains(i.as_str())) {
        body.push_str(&format!("<p><img src=\"{}\" alt=\"\"></p>\n", escape(image)));
    }
    body.push_str(&if markdown { render_markdown(content) } else { render_text(content) });
    body.push_str("</article>\n");

    Card {
        kind,
        title,
        description: description.map(|d| describe(&d)).filter(|d| !d.is_empty()),
        image,
        video,
        author,
        published: Some(published),
        body,
    }
}

/// The first image and video an event carries: from its NIP-92 `imeta` tags,
/// then from its `url` tags, then from URLs in its text.
#[derive(Default)]
struct Media {
    image: Option<String>,
    video: Option<Video>,
}

impl Media {
    fn of(event: &Event, content: &str) -> Media {
        let mut media = Media::default();
        let mut thumbnail = None;

        for entries in event.tags.iter().filter(|tag| tag.first().map(String::as_str) == Some("imeta")) {
            let field = |key: &str| {
                entries[1..].iter().find_map(|entry| entry.strip_prefix(key).and_then(|rest| rest.strip_prefix(' '))).map(str::trim)
            };
            let Some(url) = field("url").and_then(safe_url) else { continue };
            let mime = field("m").unwrap_or("");
            let kind = if mime.starts_with("image/") {
                Some(MediaKind::Image)
            } else if mime.starts_with("video/") {
                Some(MediaKind::Video)
            } else {
                media_kind(&url)
            };
            match kind {
                Some(MediaKind::Image) if media.image.is_none() => media.image = Some(url),
                Some(MediaKind::Video) if media.video.is_none() => {
                    let (width, height) = dimensions(field("dim"));
                    let mime = if mime.starts_with("video/") { mime.to_string() } else { video_type(&url).to_string() };
                    thumbnail = field("image").or(field("thumb")).and_then(safe_url);
                    media.video = Some(Video { url, mime, width, height });
                }
                _ => {}
            }
        }

        let tagged = event.tags.iter().filter(|tag| tag.first().map(String::as_str) == Some("url")).filter_map(|tag| tag.get(1));
        let written = super::render::urls(content);
        for url in tagged.cloned().chain(written).filter_map(|url| safe_url(&url)) {
            match media_kind(&url) {
                Some(MediaKind::Image) if media.image.is_none() => media.image = Some(url),
                Some(MediaKind::Video) if media.video.is_none() => {
                    media.video = Some(Video { mime: video_type(&url).to_string(), url, width: None, height: None })
                }
                _ => {}
            }
        }

        media.image = media.image.or(thumbnail);
        media
    }
}

/// `WxH`, as `imeta` and NIP-94 write a size.
fn dimensions(dim: Option<&str>) -> (Option<u32>, Option<u32>) {
    let Some((width, height)) = dim.and_then(|d| d.split_once('x')) else { return (None, None) };
    (width.trim().parse().ok().filter(|w| *w > 0), height.trim().parse().ok().filter(|h| *h > 0))
}

/// A note's text as a description: without the media and `nostr:`
/// references it carries, which say nothing in a preview.
fn without_references(content: &str) -> String {
    let mut text = content.to_string();
    for url in super::render::urls(content) {
        let reference = crate::util::strip_prefix_ci(&url, "nostr:").len() != url.len();
        if reference || safe_url(&url).and_then(|u| media_kind(&u)).is_some() {
            text = text.replacen(&url, " ", 1);
        }
    }
    text
}

/// Text cut to a description's length, on one line.
fn describe(text: &str) -> String {
    clamp(text, MAX_DESCRIPTION)
}

/// The first tag of a name's value, when it has a non-empty one.
fn tag(event: &Event, name: &str) -> Option<String> {
    event.tag(name).map(str::trim).filter(|v| !v.is_empty()).map(str::to_string)
}

/// At most `max` bytes of `text`, cut on a character boundary.
fn truncate(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

fn short_npub(pubkey: &str) -> String {
    let npub = nip19::npub_encode(pubkey);
    format!("{}…{}", &npub[..npub.len().min(12)], &npub[npub.len().saturating_sub(4)..])
}

/// A `<time>`, for a person and for a machine.
fn time(at: i64) -> String {
    match DateTime::<Utc>::from_timestamp(at, 0) {
        Some(at) => format!("<time datetime=\"{}\">{}</time>", at.to_rfc3339_opts(SecondsFormat::Secs, true), at.format("%B %-d, %Y")),
        None => String::new(),
    }
}

/// When an event was published, as JSON-LD writes a date.
pub fn iso_date(at: i64) -> Option<String> {
    DateTime::<Utc>::from_timestamp(at, 0).map(|at| at.to_rfc3339_opts(SecondsFormat::Secs, true))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "0000000000000000000000000000000000000000000000000000000000000003";

    fn sign(kind: i64, tags: Vec<Vec<&str>>, content: &str) -> Event {
        let tags = tags.into_iter().map(|t| t.into_iter().map(str::to_string).collect()).collect();
        Event::sign(SECRET, 1_700_000_000, kind, tags, content)
    }

    #[test]
    fn profiles() {
        let event = sign(0, vec![], r#"{"name":"alex","display_name":"Alex <G>","about":"Hi\n\nthere","picture":"https://x.example/a.png","website":"javascript:alert(1)","nip05":"_@alex.example"}"#);
        let card = profile_card(&event);
        assert_eq!(card.kind, CardKind::Profile);
        assert_eq!(card.author.name.as_deref(), Some("Alex <G>"));
        assert_eq!(card.author.picture.as_deref(), Some("https://x.example/a.png"));
        assert_eq!(card.description.as_deref(), Some("Hi there"));
        assert!(card.body.contains("<h1>Alex &lt;G&gt;</h1>"), "{}", card.body);
        assert!(card.body.contains("<p>alex.example</p>"), "{}", card.body);
        assert!(!card.body.contains("javascript"), "{}", card.body);

        let nameless = profile_card(&sign(0, vec![], "not json"));
        assert!(nameless.author.display().starts_with("npub1"));
    }

    #[test]
    fn notes() {
        let profile = sign(0, vec![], r#"{"name":"alex"}"#);
        let event = sign(1, vec![], "Look at this https://cdn.example/cat.png and nostr:npub180cvv07tjdrrgpa0j7j7tmnyl2yr6yr7l8j4s3evf6u64th6gkwsyjh6w6");
        let card = event_card(&event, Some(&profile));
        assert_eq!(card.kind, CardKind::Post);
        assert_eq!(card.title, None);
        assert_eq!(card.description.as_deref(), Some("Look at this and"));
        assert_eq!(card.image.as_deref(), Some("https://cdn.example/cat.png"));
        assert_eq!(card.author.display(), "alex");
        assert!(card.body.contains("<time datetime=\"2023-11-14T22:13:20Z\">November 14, 2023</time>"), "{}", card.body);
        // The image is shown where the text names it, and only there.
        assert_eq!(card.body.matches("cat.png").count(), 1, "{}", card.body);

        // Someone else's profile is not the author's.
        let other = Event::sign("0000000000000000000000000000000000000000000000000000000000000004", 1, 0, vec![], r#"{"name":"mallory"}"#);
        assert_ne!(event_card(&event, Some(&other)).author.name.as_deref(), Some("mallory"));
    }

    #[test]
    fn videos() {
        let event = sign(
            22,
            vec![vec!["title", "Clip"], vec!["imeta", "url https://cdn.example/v", "m video/mp4", "dim 720x1280", "image https://cdn.example/t.jpg"]],
            "",
        );
        let card = event_card(&event, None);
        assert_eq!(card.kind, CardKind::Video);
        assert_eq!(card.title.as_deref(), Some("Clip"));
        assert_eq!(card.video, Some(Video { url: "https://cdn.example/v".into(), mime: "video/mp4".into(), width: Some(720), height: Some(1280) }));
        assert_eq!(card.image.as_deref(), Some("https://cdn.example/t.jpg"));
        assert!(card.body.contains("<video src=\"https://cdn.example/v\" poster=\"https://cdn.example/t.jpg\""), "{}", card.body);
    }

    #[test]
    fn articles() {
        let event = sign(
            30023,
            vec![vec!["d", "post"], vec!["title", "My <Post>"], vec!["summary", "About things."], vec!["image", "https://cdn.example/c.jpg"], vec!["published_at", "1600000000"]],
            "# Heading\n\nBody text.",
        );
        let card = event_card(&event, None);
        assert_eq!(card.kind, CardKind::Article);
        assert_eq!(card.title.as_deref(), Some("My <Post>"));
        assert_eq!(card.description.as_deref(), Some("About things."));
        assert_eq!(card.published, Some(1_600_000_000));
        assert!(card.body.contains("<h1>My &lt;Post&gt;</h1>"), "{}", card.body);
        assert!(card.body.contains("<h1>Heading</h1>"), "{}", card.body);
        assert!(card.body.contains("<img src=\"https://cdn.example/c.jpg\""), "{}", card.body);

        let untitled = event_card(&sign(30023, vec![vec!["d", "x"]], "Just **words** here."), None);
        assert_eq!(untitled.description.as_deref(), Some("Just words here."));
    }

    #[test]
    fn falls_back_to_alt() {
        let card = event_card(&sign(31337, vec![vec!["alt", "A custom thing"]], ""), None);
        assert_eq!(card.description.as_deref(), Some("A custom thing"));
    }

    #[test]
    fn cuts_long_content() {
        let card = event_card(&sign(1, vec![], &"é".repeat(MAX_CONTENT)), None);
        assert!(card.body.len() < MAX_CONTENT * 2);
    }
}
