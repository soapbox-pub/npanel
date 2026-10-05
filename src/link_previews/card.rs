//! What a page about a Nostr event says about it: the fields a link preview
//! shows, and the article a search engine reads. Taken from the event once,
//! when it is fetched, and cached; nothing here depends on which site serves
//! the page, so one card serves every site.

use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::kinds::{truncate, Content, Parts, MAX_CONTENT};
use super::render::{escape, render_markdown, render_text, safe_url};
use crate::nostr::nip19;
use crate::nostr::Event;
use crate::site_preview::clamp;

const MAX_TITLE: usize = 200;
const MAX_DESCRIPTION: usize = 200;
const MAX_NAME: usize = 100;
/// The longest article a site's script may write.
const MAX_BODY: usize = 256 * 1024;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CardKind {
    Profile,
    Article,
    Post,
    Video,
    Audio,
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
pub struct Audio {
    pub url: String,
    pub mime: String,
}

/// An image a site's script drew, which npanel serves.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Drawn {
    /// What names it: see `script.rs`.
    pub version: String,
    /// Its type and size, where it was drawn by the time the card was made.
    pub mime: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Card {
    pub kind: CardKind,
    /// The kind of the event it is of: 0 for a profile.
    pub event_kind: i64,
    /// The event's own title, where it has one. A note has none, and its page
    /// is titled after its author.
    pub title: Option<String>,
    pub description: Option<String>,
    /// The event's own image, as an http(s) URL.
    pub image: Option<String>,
    pub video: Option<Video>,
    pub audio: Option<Audio>,
    /// The image a site's script drew, which `image` is the URL of.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drawn: Option<Drawn>,
    /// Whoever it is attributed to: `None` for a card a site's script made,
    /// which names nobody.
    pub author: Option<Author>,
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

    let mut body = format!("<article>\n<header>\n<h1>{}</h1>\n", escape(&name));
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
        event_kind: 0,
        title: None,
        description: profile.about.as_deref().map(describe).filter(|d| !d.is_empty()),
        image: None,
        video: None,
        audio: None,
        drawn: None,
        author: Some(author),
        published: None,
        body,
    }
}

/// An event's card, or `None` for a kind that isn't previewed. `author` is
/// the kind 0 of whoever the card is attributed to, where one was found.
/// Lookups make one in two steps, since who that is depends on the event.
#[cfg(test)]
pub fn event_card(event: &Event, author: Option<&Event>) -> Option<Card> {
    if event.kind == 0 {
        return Some(profile_card(event));
    }
    super::kinds::read(event).map(|parts| card(event.kind, parts, author))
}

/// A card from what [`kinds::read`] made of its event.
pub fn card(event_kind: i64, parts: Parts, author: Option<&Event>) -> Card {
    let profile = author.filter(|a| a.kind == 0 && a.pubkey == parts.author).map(read_profile).unwrap_or_default();
    let author = Author { pubkey: parts.author.clone(), name: profile.name, picture: profile.picture };
    let description = parts.summary.or_else(|| parts.content.text()).or(parts.fallback);

    let kind = if matches!(parts.content, Content::Markdown(_)) {
        CardKind::Article
    } else if parts.video.is_some() {
        CardKind::Video
    } else if parts.audio.is_some() {
        CardKind::Audio
    } else {
        CardKind::Post
    };
    let title = parts.title.map(|t| clamp(&t, MAX_TITLE));

    let mut body = String::from("<article>\n<header>\n");
    if let Some(title) = &title {
        body.push_str(&format!("<h1>{}</h1>\n", escape(title)));
    }
    body.push_str(&format!(
        "<p><a href=\"/{}\" rel=\"author\">{}</a> · {}</p>\n",
        nip19::npub_encode(&author.pubkey),
        escape(&author.display()),
        time(parts.published)
    ));
    body.push_str(&parts.lead);
    body.push_str("</header>\n");

    // Media an event names only in its tags is shown too; what its text, or
    // what follows it, names is shown there.
    let source = parts.content.source();
    let unshown = |url: &str| !source.contains(url) && !parts.extra.contains(&escape(url));
    if let Some(video) = parts.video.as_ref().filter(|v| unshown(&v.url)) {
        let poster = parts.image.as_ref().map(|i| format!(" poster=\"{}\"", escape(i))).unwrap_or_default();
        body.push_str(&format!("<p><video src=\"{}\"{poster} controls preload=\"none\"></video></p>\n", escape(&video.url)));
    } else if let Some(image) = parts.image.as_ref().filter(|i| unshown(i)) {
        body.push_str(&format!("<p><img src=\"{}\" alt=\"\"></p>\n", escape(image)));
    }
    if let Some(audio) = parts.audio.as_ref().filter(|a| parts.video.is_none() && unshown(&a.url)) {
        body.push_str(&format!("<p><audio src=\"{}\" controls preload=\"none\"></audio></p>\n", escape(&audio.url)));
    }
    body.push_str(&match &parts.content {
        Content::Text(text) => render_text(text),
        Content::Markdown(text) => render_markdown(text),
        Content::Quote(text) => format!("<blockquote>\n{}</blockquote>\n", render_text(text)),
        Content::Code(text) => format!("<pre><code>{}</code></pre>\n", escape(text)),
        Content::Hidden => String::new(),
    });
    body.push_str(&parts.extra);
    body.push_str("</article>\n");

    Card {
        kind,
        event_kind,
        title,
        description: description.map(|d| describe(&d)).filter(|d| !d.is_empty()),
        image: parts.image,
        video: parts.video,
        audio: parts.audio,
        drawn: None,
        author: Some(author),
        published: Some(parts.published),
        body,
    }
}

/// A card from what a site's script returned. Its body is the script's own
/// markup, cleaned; everything else is held to what npanel's cards are.
pub fn script_card(event_kind: i64, title: Option<String>, description: Option<String>, body: Option<String>, image: Option<String>, drawn: Option<Drawn>) -> Card {
    let body = body.map(|body| {
        ammonia::Builder::default().link_rel(Some("nofollow ugc noopener")).clean(truncate(&body, MAX_BODY)).to_string()
    });
    Card {
        kind: if event_kind == 0 { CardKind::Profile } else { CardKind::Post },
        event_kind,
        title: title.map(|t| clamp(&t, MAX_TITLE)).filter(|t| !t.is_empty()),
        description: description.map(|d| describe(&d)).filter(|d| !d.is_empty()),
        image,
        video: None,
        audio: None,
        drawn,
        author: None,
        published: None,
        body: body.map(|body| format!("{body}\n")).unwrap_or_default(),
    }
}

/// Text cut to a description's length, on one line.
fn describe(text: &str) -> String {
    clamp(text, MAX_DESCRIPTION)
}

pub fn short_npub(pubkey: &str) -> String {
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
        assert_eq!(card.author.as_ref().unwrap().name.as_deref(), Some("Alex <G>"));
        assert_eq!(card.author.as_ref().unwrap().picture.as_deref(), Some("https://x.example/a.png"));
        assert_eq!(card.description.as_deref(), Some("Hi there"));
        assert!(card.body.contains("<h1>Alex &lt;G&gt;</h1>"), "{}", card.body);
        assert!(card.body.contains("<p>alex.example</p>"), "{}", card.body);
        assert!(!card.body.contains("javascript"), "{}", card.body);

        let nameless = profile_card(&sign(0, vec![], "not json"));
        assert!(nameless.author.unwrap().display().starts_with("npub1"));
    }

    #[test]
    fn notes() {
        let profile = sign(0, vec![], r#"{"name":"alex"}"#);
        let event = sign(1, vec![], "Look at this https://cdn.example/cat.png and nostr:npub180cvv07tjdrrgpa0j7j7tmnyl2yr6yr7l8j4s3evf6u64th6gkwsyjh6w6");
        let card = event_card(&event, Some(&profile)).unwrap();
        assert_eq!(card.kind, CardKind::Post);
        assert_eq!(card.title, None);
        assert_eq!(card.description.as_deref(), Some("Look at this and"));
        assert_eq!(card.image.as_deref(), Some("https://cdn.example/cat.png"));
        assert_eq!(card.author.unwrap().display(), "alex");
        assert!(card.body.contains("<time datetime=\"2023-11-14T22:13:20Z\">November 14, 2023</time>"), "{}", card.body);
        // The image is shown where the text names it, and only there.
        assert_eq!(card.body.matches("cat.png").count(), 1, "{}", card.body);

        // Someone else's profile is not the author's.
        let other = Event::sign("0000000000000000000000000000000000000000000000000000000000000004", 1, 0, vec![], r#"{"name":"mallory"}"#);
        assert_ne!(event_card(&event, Some(&other)).unwrap().author.unwrap().name.as_deref(), Some("mallory"));
    }

    #[test]
    fn videos() {
        let event = sign(
            22,
            vec![vec!["title", "Clip"], vec!["imeta", "url https://cdn.example/v", "m video/mp4", "dim 720x1280", "image https://cdn.example/t.jpg"]],
            "",
        );
        let card = event_card(&event, None).unwrap();
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
        let card = event_card(&event, None).unwrap();
        assert_eq!(card.kind, CardKind::Article);
        assert_eq!(card.title.as_deref(), Some("My <Post>"));
        assert_eq!(card.description.as_deref(), Some("About things."));
        assert_eq!(card.published, Some(1_600_000_000));
        assert!(card.body.contains("<h1>My &lt;Post&gt;</h1>"), "{}", card.body);
        assert!(card.body.contains("<h1>Heading</h1>"), "{}", card.body);
        assert!(card.body.contains("<img src=\"https://cdn.example/c.jpg\""), "{}", card.body);

        let untitled = event_card(&sign(30023, vec![vec!["d", "x"]], "Just **words** here."), None).unwrap();
        assert_eq!(untitled.description.as_deref(), Some("Just words here."));
    }

    #[test]
    fn falls_back_to_alt() {
        let card = event_card(&sign(1, vec![vec!["alt", "A note with nothing to say"]], ""), None).unwrap();
        assert_eq!(card.description.as_deref(), Some("A note with nothing to say"));
    }

    #[test]
    fn previews_only_the_kinds_it_knows() {
        assert_eq!(event_card(&sign(31337, vec![vec!["alt", "A custom thing"]], "hello"), None), None);
        assert_eq!(event_card(&sign(10000, vec![], ""), None), None);
    }

    #[test]
    fn cuts_long_content() {
        let card = event_card(&sign(1, vec![], &"é".repeat(MAX_CONTENT)), None).unwrap();
        assert!(card.body.len() < MAX_CONTENT * 2);
    }
}
