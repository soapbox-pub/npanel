//! Which events get a preview, and what each kind's is made of.
//!
//! Only the kinds named here are previewed: the kinds Ditto renders. Any
//! other gets no card, so its page is served exactly as published — a guess
//! at what an unknown event means is worse than the site's own preview.
//!
//! Each kind is read the way its NIP, or the app that made it, puts things:
//! which tag is its title, which its image, and whether its content is for
//! reading at all. Content that isn't — JSON, ciphertext, a memory card's hex
//! — never reaches a description or the page.

use chrono::{DateTime, NaiveDate, Utc};
use serde_json::Value;

use super::card::{short_npub, Audio, Video};
use super::render::{audio_type, escape, markdown_text, media_kind, mime_kind, safe_url, urls, video_type, MediaKind};
use crate::nostr::{is_hex64, nip19, Event};
use crate::site_preview::clamp;

/// The most of an event's content that is read. A long article is cut here
/// rather than left out: its first hundred-odd kilobytes are the page.
pub const MAX_CONTENT: usize = 128 * 1024;
/// The most entries a list on the page shows: people, files, emoji.
const MAX_LIST: usize = 50;

/// Every kind with a preview. [`read`] answers for each, and for nothing
/// else; kind 0 is a profile, and has a card of its own.
pub const SUPPORTED: &[i64] = &[
    0, 1, 3, 4, 6, 7, 8, 11, 16, 20, 21, 22, 62, 1018, 1063, 1068, 1111, 1222, 1244, 1311, 1617, 1618, 1619, 1621, 1630, 1631, 1632,
    1633, 1984, 2003, 2004, 2256, 2473, 3063, 3367, 5128, 7516, 7849, 8211, 8333, 9735, 9802, 10002, 10008, 10011, 12473, 15128,
    15683, 16767, 18678, 30000, 30008, 30009, 30023, 30024, 30030, 30054, 30055, 30063, 30311, 30312, 30313, 30315, 30402, 30617,
    30618, 30621, 30817, 31124, 31871, 31922, 31923, 31985, 31990, 32267, 33301, 33331, 33863, 33953, 34139, 34235, 34236, 34550,
    34609, 35128, 36767, 36787, 37381, 37516, 37849, 38192, 39089, 39701, 39731,
];

/// Whether a kind is previewed.
pub fn supported(kind: i64) -> bool {
    SUPPORTED.contains(&kind)
}

/// How an event's content is shown.
#[derive(Clone, Debug, PartialEq)]
pub enum Content {
    /// Plain text, as a note is written.
    Text(String),
    Markdown(String),
    /// Words quoted from somewhere else: a highlight.
    Quote(String),
    /// A patch, as written.
    Code(String),
    /// Nothing to read: JSON, ciphertext, hex, or empty.
    Hidden,
}

impl Content {
    /// What it was made from, to tell whether media is already shown in it.
    pub fn source(&self) -> &str {
        match self {
            Content::Text(c) | Content::Markdown(c) | Content::Quote(c) | Content::Code(c) => c,
            Content::Hidden => "",
        }
    }

    /// Its words, for a description, if it has any worth one.
    pub fn text(&self) -> Option<String> {
        let text = match self {
            Content::Text(c) | Content::Quote(c) => without_references(c),
            Content::Markdown(c) => markdown_text(c),
            Content::Code(_) | Content::Hidden => return None,
        };
        Some(text).filter(|t| !t.trim().is_empty())
    }
}

/// What an event's card is made of, before its author's profile is known.
#[derive(Clone, Debug)]
pub struct Parts {
    /// Who the card is attributed to: the author, but for a repost the
    /// reposted event's, and for a zap whoever sent it.
    pub author: String,
    pub published: i64,
    pub title: Option<String>,
    /// The description, where the event has one apart from its content.
    pub summary: Option<String>,
    /// The description when neither the summary nor the content gives one.
    pub fallback: Option<String>,
    pub image: Option<String>,
    pub video: Option<Video>,
    pub audio: Option<Audio>,
    pub content: Content,
    /// Markup for the article's header, below the byline. Already escaped.
    pub lead: String,
    /// Markup for after the content. Already escaped.
    pub extra: String,
}

/// An event's parts, or `None` for a kind that isn't previewed — or one
/// that is, but malformed in a way its app would refuse to show.
pub fn read(event: &Event) -> Option<Parts> {
    let content = truncate(&event.content, MAX_CONTENT);
    let media = Media::of(event, content);
    let mut p = Parts {
        author: event.pubkey.clone(),
        published: event.created_at,
        title: tag(event, "title"),
        summary: tag(event, "summary").or_else(|| tag(event, "description")),
        // NIP-31: an `alt` describes an event its content doesn't.
        fallback: tag(event, "alt"),
        image: url_tag(event, "image").or_else(|| url_tag(event, "thumb")).or(media.image),
        video: media.video,
        audio: media.audio,
        content: Content::Text(content.to_string()),
        lead: String::new(),
        extra: String::new(),
    };
    let e = event;

    match e.kind {
        // Notes, threads, comments, live chat, torrent comments.
        1 | 11 | 1111 | 1311 | 2004 => {}
        // NIP-68 pictures and NIP-71 videos: their media is in `imeta`.
        20 | 21 | 22 | 34235 | 34236 => {}
        1063 => file(&mut p, e),
        1222 | 1244 => voice(&mut p, e, content),
        36787 => track(&mut p, e, content),
        34139 => playlist(&mut p, e),
        30054 | 30055 => podcast(&mut p, e),
        30311 => stream(&mut p, e),
        30312 | 30313 => p.title = p.title.or_else(|| tag(e, "room")),
        2003 => torrent(&mut p, e),
        30023 | 30024 => {
            p.content = Content::Markdown(content.to_string());
            p.published = published_at(e).unwrap_or(p.published);
        }
        30402 => classified(&mut p, e, content)?,
        33953 | 34609 | 39731 => publication(&mut p, e),
        31922 | 31923 => calendar(&mut p, e),
        34550 => p.title = tag(e, "name").or_else(|| tag(e, "d")),
        31985 => book_review(&mut p, e),
        30030 => emoji_pack(&mut p, e),
        30009 => {
            p.title = tag(e, "name").or_else(|| tag(e, "d"));
            p.fallback = p.fallback.or_else(|| Some("A badge".into()));
        }
        8 => badge_award(&mut p, e),
        10008 | 30008 => badges(&mut p, e),
        33863 => fundraiser(&mut p, e, content)?,
        30617 => repository(&mut p, e),
        30618 => repository_state(&mut p, e),
        1617 => patch(&mut p, e, content),
        1618 | 1619 | 1621 => git_text(&mut p, e, content),
        1630..=1633 => git_status(&mut p, e),
        30817 => {
            p.title = p.title.or_else(|| tag(e, "d").map(|d| format!("NIP {d}")));
            p.content = Content::Markdown(content.to_string());
        }
        15128 | 35128 | 5128 => nsite(&mut p, e),
        32267 => app(&mut p, e),
        30063 => release(&mut p, e, content),
        3063 => asset(&mut p, e),
        31990 => handler(&mut p, e, content),
        31871 => attestation(&mut p, e),
        6 | 16 => return Some(repost(e)),
        7 => reaction(&mut p, e)?,
        9735 => zap(&mut p, e),
        8333 => onchain_zap(&mut p, e),
        9802 => highlight(&mut p, e, content),
        1068 => poll(&mut p, e),
        1018 => {
            p.summary = Some("Voted in a poll".into());
            p.content = Content::Hidden;
            p.lead = target(e).map(|link| paragraph_html(&format!("Voted in {link}"))).unwrap_or_default();
        }
        1984 => report(&mut p, e),
        3 | 30000 | 39089 => people_list(&mut p, e),
        10002 => relay_list(&mut p, e),
        10011 => identities(&mut p, e),
        15683 | 18678 => {
            p.title = Some(if e.kind == 15683 { "Love list" } else { "Top 8" }.into());
            people(&mut p, e);
        }
        30315 => status(&mut p, e),
        39701 => bookmark(&mut p, e),
        16767 | 36767 => theme(&mut p, e)?,
        37849 => quiz(&mut p, e)?,
        7849 => {
            p.title = p.fallback.take().or_else(|| Some("Quiz result".into()));
        }
        38192 => memory_card(&mut p, e, content),
        3367 => color_moment(&mut p, e, content),
        37516 => geocache(&mut p, e),
        7516 => {
            p.title = Some("Found a geocache".into());
            p.lead = coordinate(e).map(|link| paragraph_html(&format!("Logged a find at {link}"))).unwrap_or_default();
        }
        37381 => deck(&mut p, e),
        2473 => bird(&mut p, e),
        12473 => birdex(&mut p, e),
        30621 => p.title = p.title.or_else(|| tag(e, "d")),
        33331 => sno(&mut p, e, content),
        31124 => blobbi(&mut p, e),
        2256 => tarot(&mut p, e),
        4 | 8211 | 33301 => encrypted(&mut p, e),
        62 => vanish(&mut p, e),
        _ => return None,
    }

    // NIP-36: what its author put behind a warning stays behind one. A
    // preview has no way to ask first, so it shows none of it.
    if e.tags.iter().any(|t| t.first().map(String::as_str) == Some("content-warning")) {
        let reason = tag(e, "content-warning");
        p.summary = Some(reason.map_or_else(|| "Content warning".into(), |r| format!("Content warning: {r}")));
        p.image = None;
        p.video = None;
        p.audio = None;
        p.content = Content::Hidden;
        p.extra.clear();
    }
    Some(p)
}

fn file(p: &mut Parts, e: &Event) {
    let mime = tag(e, "m").unwrap_or_default();
    if let Some(url) = url_tag(e, "url") {
        match mime_kind(&mime).or_else(|| media_kind(&url)) {
            Some(MediaKind::Image) => p.image = Some(url.clone()),
            Some(MediaKind::Video) => {
                let (width, height) = dimensions(tag(e, "dim").as_deref());
                let mime = if mime_kind(&mime) == Some(MediaKind::Video) { mime.clone() } else { video_type(&url).into() };
                p.video = Some(Video { url: url.clone(), mime, width, height });
            }
            Some(MediaKind::Audio) => {
                let mime = if mime_kind(&mime) == Some(MediaKind::Audio) { mime.clone() } else { audio_type(&url).into() };
                p.audio = Some(Audio { url: url.clone(), mime });
            }
            None => p.extra = paragraph_html(&web_link(&url, file_name(&url).as_deref().unwrap_or(&url))),
        }
        p.title = p.title.take().or_else(|| p.fallback.clone()).or_else(|| file_name(&url));
    }
}

fn voice(p: &mut Parts, e: &Event, content: &str) {
    let first = imetas(e).next();
    if let Some(url) = safe_url(content.trim()) {
        let mime = first.and_then(|m| field(m, "m")).filter(|m| mime_kind(m) == Some(MediaKind::Audio));
        p.audio = Some(Audio { mime: mime.map_or_else(|| audio_type(&url).into(), str::to_string), url });
    }
    let duration = first.and_then(|m| field(m, "duration")).and_then(|d| d.parse::<f64>().ok()).filter(|d| d.is_finite() && *d > 0.0);
    p.summary = Some(match duration {
        Some(seconds) => format!("Voice message, {}", clock(seconds as u64)),
        None => "Voice message".into(),
    });
    p.content = Content::Hidden;
}

fn track(p: &mut Parts, e: &Event, content: &str) {
    let first_line = content.lines().map(str::trim).find(|l| !l.is_empty()).map(|l| clamp(l, 80));
    p.title = p.title.take().or_else(|| tag(e, "subject")).or(first_line);
    let artist = tag(e, "artist").or_else(|| tag(e, "creator"));
    let byline = match (artist, tag(e, "album")) {
        (Some(artist), Some(album)) => Some(format!("by {artist} on {album}")),
        (Some(artist), None) => Some(format!("by {artist}")),
        (None, Some(album)) => Some(format!("on {album}")),
        (None, None) => None,
    };
    p.summary = p.summary.take().or(byline);
    p.audio = imeta_audio(e).or_else(|| tagged_audio(e, "url")).or_else(|| tagged_audio(e, "media")).or(p.audio.take());
    p.image = imeta_thumbnail(e).or_else(|| url_tag(e, "image")).or_else(|| url_tag(e, "thumb")).or(p.image.take());
}

fn playlist(p: &mut Parts, e: &Event) {
    p.title = p.title.take().or_else(|| tag(e, "d"));
    p.fallback = p.fallback.take().or_else(|| Some(count(all(e, "a").count(), "track", "tracks")));
}

fn podcast(p: &mut Parts, e: &Event) {
    p.title = p.title.take().or_else(|| tag(e, "subject"));
    p.summary = tag(e, "description").or_else(|| p.summary.take());
    p.audio = tagged_audio(e, "audio").or_else(|| imeta_audio(e)).or_else(|| tagged_audio(e, "url")).or_else(|| tagged_audio(e, "media")).or(p.audio.take());
    p.image = url_tag(e, "image").or_else(|| imeta_thumbnail(e)).or_else(|| url_tag(e, "thumb")).or(p.image.take());
}

fn stream(p: &mut Parts, e: &Event) {
    match tag(e, "status").as_deref() {
        Some("live") => p.title = p.title.take().map(|t| format!("{t} (live)")),
        Some("ended") => {
            if let Some(url) = url_tag(e, "recording").filter(|u| media_kind(u) == Some(MediaKind::Video)) {
                p.video = Some(Video { mime: video_type(&url).into(), url, width: None, height: None });
            }
        }
        _ => {}
    }
    p.fallback = p.fallback.take().or_else(|| Some("A live stream".into()));
}

fn torrent(p: &mut Parts, e: &Event) {
    let files: Vec<(String, Option<u64>)> = all(e, "file")
        .filter_map(|t| {
            let path = t.first()?.trim();
            (!path.is_empty()).then(|| (path.to_string(), t.get(1).and_then(|s| s.parse().ok())))
        })
        .collect();
    p.title = p.title.take().or_else(|| files.first().and_then(|(path, _)| path.rsplit('/').next()).map(str::to_string));
    p.fallback = Some(if files.is_empty() { "A torrent".into() } else { count(files.len(), "file", "files") });
    p.extra = list_html(files.iter().map(|(path, size)| match size {
        Some(size) => format!("{} ({})", escape(path), bytes(*size)),
        None => escape(path),
    }), files.len());
}

fn classified(p: &mut Parts, e: &Event, content: &str) -> Option<()> {
    // NIP-99 makes the title required, and Ditto drops a listing without one.
    p.title.as_ref()?;
    p.content = Content::Markdown(content.to_string());
    p.published = published_at(e).unwrap_or(p.published);
    let price = all(e, "price").next().and_then(|t| {
        let amount = t.first()?.trim();
        let currency = t.get(1).map(|c| c.trim().to_uppercase()).filter(|c| !c.is_empty())?;
        let mut price = format!("{amount} {currency}");
        if let Some(frequency) = t.get(2).map(|f| f.trim()).filter(|f| !f.is_empty()) {
            price.push_str(&format!(" per {frequency}"));
        }
        Some(price)
    });
    let sold = tag(e, "status").as_deref() == Some("sold");
    let facts: Vec<String> = [sold.then(|| "Sold".to_string()), price, tag(e, "location")].into_iter().flatten().collect();
    if !facts.is_empty() {
        let facts = facts.join(" · ");
        p.lead = paragraph_html(&escape(&facts));
        p.summary = Some(match p.summary.take().or_else(|| p.content.text()) {
            Some(about) => format!("{facts} — {about}"),
            None => facts,
        });
    }
    Some(())
}

fn publication(p: &mut Parts, e: &Event) {
    p.published = published_at(e).unwrap_or(p.published);
    let authors = values(e, "author");
    let mut facts = Vec::new();
    if !authors.is_empty() {
        facts.push(format!("By {}", authors.join(", ")));
    }
    if let Some(issue) = tag(e, "issue") {
        facts.push(format!("Issue {issue}"));
    }
    if let Some(isbn) = tag(e, "isbn") {
        facts.push(format!("ISBN {isbn}"));
    }
    if !facts.is_empty() {
        p.lead = paragraph_html(&escape(&facts.join(" · ")));
    }
    if let Some(byline) = facts.first().filter(|_| !authors.is_empty()) {
        p.fallback = Some(byline.clone());
    }
}

fn calendar(p: &mut Parts, e: &Event) {
    let start = tag(e, "start");
    let when = match e.kind {
        // A date-based event's start is a day, `YYYY-MM-DD`.
        31922 => start.and_then(|s| NaiveDate::parse_from_str(&s, "%Y-%m-%d").ok()).map(|d| d.format("%B %-d, %Y").to_string()),
        // A time-based one's is a Unix time, shown where it happens.
        _ => start.and_then(|s| s.parse::<i64>().ok()).and_then(|at| DateTime::<Utc>::from_timestamp(at, 0)).map(|at| {
            match tag(e, "start_tzid").and_then(|zone| zone.parse::<chrono_tz::Tz>().ok()) {
                Some(zone) => at.with_timezone(&zone).format("%B %-d, %Y, %-I:%M %p %Z").to_string(),
                None => at.format("%B %-d, %Y, %H:%M UTC").to_string(),
            }
        }),
    };
    let facts: Vec<String> = [when, tag(e, "location")].into_iter().flatten().collect();
    if !facts.is_empty() {
        let facts = facts.join(" · ");
        p.lead = paragraph_html(&escape(&facts));
        p.summary = Some(match p.summary.take().or_else(|| p.content.text()) {
            Some(what) => format!("{what} — {facts}"),
            None => facts,
        });
    }
}

fn book_review(p: &mut Parts, e: &Event) {
    let isbn = tag(e, "d").and_then(|d| d.strip_prefix("isbn:").map(str::to_string));
    p.title = Some(match isbn {
        Some(isbn) => format!("Book review (ISBN {isbn})"),
        None => "Book review".into(),
    });
    // The rating is a fraction of five stars.
    let rating = tag(e, "rating").and_then(|r| r.parse::<f64>().ok()).filter(|r| (0.0..=1.0).contains(r));
    if let Some(rating) = rating {
        let stars = (rating * 5.0).round() as usize;
        let stars = format!("{}{}", "★".repeat(stars), "☆".repeat(5 - stars));
        p.lead = paragraph_html(&stars);
        p.summary = Some(match p.content.text() {
            Some(review) => format!("{stars} — {review}"),
            None => stars,
        });
    }
}

fn emoji_pack(p: &mut Parts, e: &Event) {
    let emoji: Vec<(String, String)> = all(e, "emoji")
        .filter_map(|t| Some((t.first()?.trim().to_string(), safe_url(t.get(1)?)?)))
        .filter(|(code, _)| !code.is_empty())
        .collect();
    p.title = p.title.take().or_else(|| tag(e, "name")).or_else(|| tag(e, "d"));
    p.summary = tag(e, "about").or_else(|| p.summary.take());
    p.fallback = Some(count(emoji.len(), "custom emoji", "custom emoji"));
    p.image = url_tag(e, "image").or_else(|| url_tag(e, "picture")).or_else(|| emoji.first().map(|(_, url)| url.clone()));
    p.content = Content::Hidden;
    if !emoji.is_empty() {
        let images: Vec<String> = emoji
            .iter()
            .take(MAX_LIST)
            .map(|(code, url)| format!("<img src=\"{}\" alt=\":{1}:\" title=\":{1}:\" width=\"32\" height=\"32\">", escape(url), escape(code)))
            .collect();
        p.extra = paragraph_html(&images.join(" "));
    }
}

fn badge_award(p: &mut Parts, e: &Event) {
    let badge = all(e, "a").filter_map(|t| t.first()).find_map(|a| a.strip_prefix("30009:")?.split_once(':').map(|(_, d)| d.to_string())).filter(|d| !d.is_empty());
    p.title = Some(match &badge {
        Some(badge) => format!("Awarded the {badge} badge"),
        None => "Badge award".into(),
    });
    p.content = Content::Hidden;
    people(p, e);
}

fn badges(p: &mut Parts, e: &Event) {
    let names: Vec<String> = all(e, "a").filter_map(|t| t.first()).filter_map(|a| a.strip_prefix("30009:")?.split_once(':').map(|(_, d)| d.to_string())).collect();
    let set = e.kind == 30008 && tag(e, "d").as_deref() != Some("profile_badges");
    p.title = if set { p.title.take().or_else(|| tag(e, "name")).or_else(|| tag(e, "d")) } else { Some("Badges".into()) };
    p.fallback = Some(count(names.len(), "badge", "badges"));
    p.content = Content::Hidden;
    p.extra = list_html(names.iter().map(|n| escape(n)), names.len());
}

fn fundraiser(p: &mut Parts, e: &Event, content: &str) -> Option<()> {
    // Ditto shows a fundraiser only with somewhere to send to and a name.
    tag(e, "d")?;
    p.title.as_ref()?;
    values(e, "w").iter().find(|w| ["bc1", "sp1"].iter().any(|prefix| w.to_ascii_lowercase().starts_with(prefix)))?;
    p.content = Content::Markdown(content.to_string());
    p.image = url_tag(e, "banner").filter(|u| u.starts_with("https://")).or(p.image.take());
    let goal = tag(e, "goal").and_then(|g| g.parse::<u64>().ok()).filter(|g| *g > 0).map(|g| format!("Goal: ${}", thousands(g)));
    let deadline = tag(e, "deadline").and_then(|d| d.parse::<i64>().ok()).and_then(|at| DateTime::<Utc>::from_timestamp(at, 0)).map(|at| format!("Ends {}", at.format("%B %-d, %Y")));
    let facts: Vec<String> = [goal, deadline].into_iter().flatten().collect();
    if !facts.is_empty() {
        p.lead = paragraph_html(&escape(&facts.join(" · ")));
    }
    Some(())
}

fn repository(p: &mut Parts, e: &Event) {
    p.title = tag(e, "name").or_else(|| tag(e, "d"));
    p.content = Content::Hidden;
    let links: Vec<String> = all(e, "web").flat_map(|t| t.iter()).filter_map(|u| safe_url(u)).map(|u| web_link(&u, &u)).collect();
    let clones: Vec<String> = all(e, "clone").flat_map(|t| t.iter()).map(|u| format!("<code>{}</code>", escape(u.trim()))).collect();
    let total = links.len() + clones.len();
    p.extra = list_html(links.into_iter().chain(clones), total);
}

fn repository_state(p: &mut Parts, e: &Event) {
    let branches = e.tags.iter().filter(|t| t.first().is_some_and(|name| name.starts_with("refs/heads/"))).count();
    p.title = tag(e, "d").map(|d| format!("{d}: repository state"));
    p.fallback = Some(count(branches, "branch", "branches"));
    p.content = Content::Hidden;
}

fn patch(p: &mut Parts, e: &Event, content: &str) {
    // `git format-patch` writes the subject as a header.
    let subject = content.lines().find_map(|line| line.strip_prefix("Subject:")).or_else(|| content.lines().map(str::trim).find(|l| !l.is_empty()));
    let subject = subject.map(|s| strip_patch_prefix(s.trim()).to_string()).filter(|s| !s.is_empty());
    p.title = tag(e, "subject").or(subject);
    p.fallback = Some("A patch".into());
    p.content = Content::Code(content.to_string());
}

/// A subject without its `[PATCH v2 1/3]`.
fn strip_patch_prefix(subject: &str) -> &str {
    match subject.strip_prefix('[').and_then(|rest| rest.split_once(']')) {
        Some((inside, rest)) if inside.to_ascii_uppercase().contains("PATCH") => rest.trim(),
        _ => subject,
    }
}

fn git_text(p: &mut Parts, e: &Event, content: &str) {
    let first_line = content.lines().map(|l| l.trim().trim_start_matches('#').trim()).find(|l| !l.is_empty()).map(|l| clamp(l, 120));
    p.title = tag(e, "subject").or(first_line);
    p.content = Content::Markdown(content.to_string());
    p.fallback = Some(match e.kind {
        1618 => "A pull request",
        1619 => "A pull request update",
        _ => "An issue",
    }.into());
}

fn git_status(p: &mut Parts, e: &Event) {
    p.title = Some(match e.kind {
        1630 => "Marked open",
        1631 => "Marked applied",
        1632 => "Closed",
        _ => "Marked as a draft",
    }.into());
    p.lead = target(e).map(|link| paragraph_html(&format!("On {link}"))).unwrap_or_default();
}

fn nsite(p: &mut Parts, e: &Event) {
    p.title = p.title.take().or_else(|| match e.kind {
        35128 => tag(e, "d"),
        5128 => Some("Site snapshot".into()),
        // A root site is its author's, and its page is titled after them.
        _ => None,
    });
    p.fallback = Some(format!("A site of {}", count(all(e, "path").count(), "file", "files")));
    p.content = Content::Hidden;
}

fn app(p: &mut Parts, e: &Event) {
    p.title = tag(e, "name").or_else(|| tag(e, "d"));
    p.image = url_tag(e, "icon").or_else(|| url_tag(e, "image"));
    let links: Vec<String> = ["url", "repository"].iter().filter_map(|name| url_tag(e, name)).map(|u| web_link(&u, &u)).collect();
    p.extra = list_html(links.iter().cloned(), links.len());
}

fn release(p: &mut Parts, e: &Event, content: &str) {
    let app = tag(e, "i").or_else(|| tag(e, "d"));
    p.title = match (app, tag(e, "version")) {
        (Some(app), Some(version)) => Some(format!("{app} {version}")),
        (app, version) => app.or(version),
    };
    p.content = Content::Markdown(content.to_string());
    p.fallback = Some("A release".into());
}

fn asset(p: &mut Parts, e: &Event) {
    let url = url_tag(e, "url");
    p.title = url.as_deref().and_then(file_name).or_else(|| tag(e, "m"));
    p.summary = tag(e, "version").map(|v| format!("Version {v}"));
    p.image = None;
    p.content = Content::Hidden;
    if let Some(url) = url {
        p.extra = paragraph_html(&web_link(&url, &url));
    }
}

fn handler(p: &mut Parts, e: &Event, content: &str) {
    // A NIP-89 handler's content is a kind 0's.
    let fields = match serde_json::from_str::<Value>(content) {
        Ok(Value::Object(fields)) => fields,
        _ => Default::default(),
    };
    let field = |key: &str| fields.get(key).and_then(Value::as_str).map(str::trim).filter(|v| !v.is_empty()).map(str::to_string);
    p.title = field("name").or_else(|| tag(e, "name")).or_else(|| tag(e, "d"));
    p.summary = field("about");
    p.image = field("banner").and_then(|u| safe_url(&u)).or_else(|| field("picture").and_then(|u| safe_url(&u)));
    p.content = Content::Hidden;
    if let Some(website) = field("website").and_then(|u| safe_url(&u)) {
        p.extra = paragraph_html(&web_link(&website, &website));
    }
}

fn attestation(p: &mut Parts, e: &Event) {
    let state = tag(e, "s").map(|s| s.to_ascii_lowercase());
    p.title = Some(match state.as_deref() {
        Some("valid") => "Attestation: valid",
        Some("invalid") => "Attestation: invalid",
        Some("verifying") => "Attestation: verifying",
        Some("revoked") => "Attestation: revoked",
        _ => "Attestation",
    }.into());
    p.lead = coordinate(e).or_else(|| target(e)).map(|link| paragraph_html(&format!("Attesting to {link}"))).unwrap_or_default();
}

/// A repost (NIP-18) is the event it reposts, when it carries one that is
/// what it says it is: the reposter's word for what its author wrote is no
/// better than anyone's, but the author's signature is.
fn repost(e: &Event) -> Parts {
    let reposted = serde_json::from_str::<Event>(truncate(&e.content, MAX_CONTENT)).ok().filter(|inner| !matches!(inner.kind, 6 | 16) && inner.verify());
    let by = person_link(&e.pubkey);
    match reposted.as_ref().and_then(read) {
        Some(mut parts) => {
            parts.lead = format!("{}{}", paragraph_html(&format!("Reposted by {by}")), parts.lead);
            parts
        }
        None => Parts {
            author: e.pubkey.clone(),
            published: e.created_at,
            title: None,
            summary: Some("Reposted a post".into()),
            fallback: None,
            image: None,
            video: None,
            audio: None,
            content: Content::Hidden,
            lead: target(e).map(|link| paragraph_html(&format!("Reposted {link}"))).unwrap_or_default(),
            extra: String::new(),
        },
    }
}

fn reaction(p: &mut Parts, e: &Event) -> Option<()> {
    let content = e.content.trim();
    let (shown, image) = match content {
        "" | "+" => ("👍".to_string(), None),
        "-" => ("👎".to_string(), None),
        _ if content.len() > 2 && content.starts_with(':') && content.ends_with(':') => {
            // A custom emoji (NIP-30) is its image, or a reaction its app drops.
            let code = &content[1..content.len() - 1];
            let url = all(e, "emoji").find(|t| t.first().map(String::as_str) == Some(code)).and_then(|t| safe_url(t.get(1)?))?;
            (content.to_string(), Some(url))
        }
        _ if content.chars().count() <= 8 && !content.contains(char::is_whitespace) && safe_url(content).is_none() => (content.to_string(), None),
        _ => return None,
    };
    p.summary = Some(format!("Reacted {shown}"));
    p.image = image;
    p.video = None;
    p.audio = None;
    p.content = Content::Hidden;
    p.lead = target(e).map(|link| paragraph_html(&format!("Reacted {} to {link}", escape(&shown)))).unwrap_or_default();
    Some(())
}

/// A zap receipt (NIP-57) is signed by the recipient's wallet, but is the
/// sender's: the zap request it carries is theirs, and signed by them.
fn zap(p: &mut Parts, e: &Event) {
    let request = tag(e, "description").and_then(|d| serde_json::from_str::<Event>(&d).ok()).filter(|r| r.kind == 9734 && r.verify());
    let sats = tag(e, "bolt11").and_then(|invoice| bolt11_sats(&invoice));
    let zapped = match sats {
        Some(sats) => format!("Zapped {} {}", thousands(sats), if sats == 1 { "sat" } else { "sats" }),
        None => "Zapped".into(),
    };
    let recipient = tag(e, "p").filter(|p| is_hex64(p)).map(|p| person_link(&p));
    let mut lead = escape(&zapped);
    if let Some(recipient) = recipient {
        lead.push_str(&format!(" to {recipient}"));
    }
    if let Some(link) = target(e) {
        lead.push_str(&format!(" for {link}"));
    }

    p.title = Some(zapped);
    p.summary = None;
    p.lead = paragraph_html(&lead);
    p.image = None;
    p.video = None;
    p.audio = None;
    p.content = match &request {
        Some(request) => Content::Text(truncate(&request.content, MAX_CONTENT).to_string()),
        None => Content::Hidden,
    };
    if let Some(request) = request {
        p.author = request.pubkey;
    }
}

/// An on-chain zap names an amount only its transaction can vouch for, and
/// a preview can't look, so it says none.
fn onchain_zap(p: &mut Parts, e: &Event) {
    p.title = Some("Sent an on-chain zap".into());
    p.summary = None;
    let recipients: Vec<String> = values(e, "p").into_iter().filter(|p| is_hex64(p)).collect();
    let mut lead = String::from("Sent an on-chain zap");
    if !recipients.is_empty() {
        lead.push_str(&format!(" to {}", recipients.iter().take(MAX_LIST).map(|r| person_link(r)).collect::<Vec<_>>().join(", ")));
    }
    if let Some(link) = coordinate(e).or_else(|| target(e)) {
        lead.push_str(&format!(" for {link}"));
    }
    p.lead = paragraph_html(&lead);
}

fn highlight(p: &mut Parts, e: &Event, content: &str) {
    p.content = Content::Quote(content.to_string());
    let comment = tag(e, "comment");
    if let Some(comment) = &comment {
        p.extra = paragraph_html(&escape(comment));
    }
    // Where it is from: a URL, marked `source` by preference, then an event.
    let sources: Vec<&[String]> = all(e, "r").filter(|t| t.get(1).map(String::as_str) != Some("mention")).collect();
    let url = sources.iter().find(|t| t.get(1).map(String::as_str) == Some("source")).or(sources.first()).and_then(|t| safe_url(t.first()?));
    let from = url.map(|u| web_link(&u, &u)).or_else(|| coordinate(e)).or_else(|| target(e));
    if let Some(from) = from {
        p.extra.push_str(&paragraph_html(&format!("From {from}")));
    }
    p.summary = p.content.text().map(|t| format!("“{}”", clamp(&t, 180)));
    p.fallback = comment;
}

fn poll(p: &mut Parts, e: &Event) {
    p.title = Some("Poll".into());
    let options: Vec<String> = all(e, "option").filter_map(|t| t.get(1)).map(|o| o.trim().to_string()).filter(|o| !o.is_empty()).collect();
    if !options.is_empty() {
        p.fallback = Some(options.join(" · "));
    }
    p.extra = list_html(options.iter().map(|o| escape(o)), options.len());
    let ends = tag(e, "endsAt").and_then(|at| at.parse::<i64>().ok()).and_then(|at| DateTime::<Utc>::from_timestamp(at, 0));
    if let Some(ends) = ends {
        p.extra.push_str(&paragraph_html(&format!("Ends {}", ends.format("%B %-d, %Y"))));
    }
}

fn report(p: &mut Parts, e: &Event) {
    // The reason is a target tag's third value: an event's, a blob's, or a person's.
    let reason = ["e", "x", "p"].iter().find_map(|name| all(e, name).find_map(|t| t.get(1).map(|r| r.trim().to_ascii_lowercase()).filter(|r| !r.is_empty() && !r.starts_with("ws"))));
    let label = match reason.as_deref() {
        Some("nudity") => "nudity",
        Some("malware") => "malware",
        Some("profanity") => "hateful speech",
        Some("illegal") => "illegal content",
        Some("spam") => "spam",
        Some("impersonation") => "impersonation",
        _ => "",
    };
    p.title = Some(if label.is_empty() { "Report".to_string() } else { format!("Reported for {label}") });
    p.lead = tag(e, "p").filter(|p| is_hex64(p)).map(|reported| paragraph_html(&format!("Reported {}", person_link(&reported)))).unwrap_or_default();
    p.image = None;
    p.video = None;
}

fn people_list(p: &mut Parts, e: &Event) {
    p.title = match e.kind {
        3 => Some("Follows".into()),
        _ => p.title.take().or_else(|| tag(e, "name")).or_else(|| tag(e, "d")),
    };
    people(p, e);
}

/// The people an event names, listed, and counted for its description.
fn people(p: &mut Parts, e: &Event) {
    let mut seen = std::collections::HashSet::new();
    let pubkeys: Vec<String> = values(e, "p").into_iter().filter(|p| is_hex64(p) && seen.insert(p.clone())).collect();
    p.fallback = p.fallback.take().or_else(|| Some(count(pubkeys.len(), "person", "people")));
    p.content = Content::Hidden;
    p.extra = list_html(pubkeys.iter().map(|pubkey| person_link(pubkey)), pubkeys.len());
}

fn relay_list(p: &mut Parts, e: &Event) {
    let relays: Vec<(String, Option<String>)> = all(e, "r").filter_map(|t| Some((t.first()?.trim().to_string(), t.get(1).cloned()))).filter(|(r, _)| !r.is_empty()).collect();
    p.title = Some("Relays".into());
    p.fallback = Some(count(relays.len(), "relay", "relays"));
    p.content = Content::Hidden;
    p.extra = list_html(relays.iter().map(|(relay, marker)| match marker.as_deref() {
        Some(marker @ ("read" | "write")) => format!("<code>{}</code> ({marker} only)", escape(relay)),
        _ => format!("<code>{}</code>", escape(relay)),
    }), relays.len());
}

fn identities(p: &mut Parts, e: &Event) {
    let accounts = values(e, "i");
    p.title = Some("Linked accounts".into());
    p.fallback = Some(count(accounts.len(), "account", "accounts"));
    p.content = Content::Hidden;
    p.extra = list_html(accounts.iter().map(|a| escape(a)), accounts.len());
}

fn status(p: &mut Parts, e: &Event) {
    p.title = Some(match tag(e, "d").as_deref().unwrap_or("general") {
        "general" => "Status".to_string(),
        "music" => "Listening to".to_string(),
        other => format!("{} status", clamp(other, 40)),
    });
    if let Some(url) = url_tag(e, "r") {
        p.extra = paragraph_html(&web_link(&url, &url));
    }
}

fn bookmark(p: &mut Parts, e: &Event) {
    // NIP-B0 names the page by its `d`, the URL without its scheme.
    let url = url_tag(e, "r").or_else(|| tag(e, "d").and_then(|d| safe_url(&format!("https://{}", d.trim_start_matches('/')))));
    if let Some(url) = &url {
        p.lead = paragraph_html(&web_link(url, url));
    }
    p.fallback = p.fallback.take().or(url);
}

fn theme(p: &mut Parts, e: &Event) -> Option<()> {
    if e.kind == 36767 {
        p.title.as_ref()?;
    } else {
        p.title = p.title.take().or_else(|| Some("Profile theme".into()));
    }
    let background = all(e, "bg").find_map(|t| field(t, "url")).and_then(safe_url);
    p.image = background.or(p.image.take());
    p.fallback = Some("A theme".into());
    p.content = Content::Hidden;
    Some(())
}

fn quiz(p: &mut Parts, e: &Event) -> Option<()> {
    p.title.as_ref()?;
    p.fallback = Some(count(all(e, "question").count(), "question", "questions"));
    Some(())
}

fn memory_card(p: &mut Parts, e: &Event, content: &str) {
    p.title = save_title(content).or_else(|| tag(e, "title")).or_else(|| tag(e, "filename"));
    p.summary = Some("A PlayStation memory card save".into());
    p.image = None;
    p.content = Content::Hidden;
}

fn color_moment(p: &mut Parts, e: &Event, content: &str) {
    let colors: Vec<String> = values(e, "c").into_iter().filter(|c| c.len() == 7 && c.starts_with('#') && c[1..].bytes().all(|b| b.is_ascii_hexdigit())).collect();
    let emoji = content.trim();
    let emoji = (!emoji.is_empty() && emoji.chars().count() <= 2).then_some(emoji);
    p.title = tag(e, "name").or_else(|| Some("Color moment".into()));
    p.summary = match (emoji, colors.is_empty()) {
        (Some(emoji), false) => Some(format!("{emoji} {}", colors.join(" "))),
        (Some(emoji), true) => Some(emoji.to_string()),
        (None, false) => Some(colors.join(" ")),
        (None, true) => None,
    };
    p.content = Content::Hidden;
}

fn geocache(p: &mut Parts, e: &Event) {
    p.title = tag(e, "name");
    let rating: Vec<String> = [("D", "Difficulty"), ("T", "Terrain")].iter().filter_map(|(name, label)| tag(e, name).map(|v| format!("{label} {v}"))).collect();
    if !rating.is_empty() {
        let rating = rating.join(" · ");
        p.lead = paragraph_html(&escape(&rating));
        p.summary = Some(match p.content.text() {
            Some(about) => format!("{rating} — {about}"),
            None => rating,
        });
    }
}

fn deck(p: &mut Parts, e: &Event) {
    p.image = url_tag(e, "banner").or(p.image.take());
    let commanders = values(e, "C");
    if !commanders.is_empty() {
        p.summary = Some(format!("Commander: {}", commanders.join(", ")));
    }
    let cards: Vec<String> = all(e, "c")
        .filter_map(|t| {
            let name = t.first()?.trim();
            let quantity = t.get(1)?.trim().parse::<u32>().ok().filter(|q| *q > 0)?;
            (!name.is_empty()).then(|| format!("{quantity} {}", escape(name)))
        })
        .collect();
    p.fallback = Some(count(cards.len(), "card", "cards"));
    p.content = Content::Hidden;
    p.extra = list_html(cards.iter().cloned(), cards.len());
}

fn bird(p: &mut Parts, e: &Event) {
    // Its `alt` reads `Bird detection: Common Name (Scientific name)`.
    let named = p.fallback.as_deref().and_then(|alt| alt.rsplit_once(": ")).map(|(_, name)| name.trim().to_string());
    let common = named.as_deref().map(|n| n.split_once(" (").map_or(n, |(common, _)| common).trim().to_string()).filter(|n| !n.is_empty());
    let scientific = tag(e, "n").or_else(|| named.as_deref().and_then(|n| n.split_once(" (")).map(|(_, s)| s.trim_end_matches(')').trim().to_string()));
    p.title = common.clone().or_else(|| scientific.clone()).or_else(|| Some("Bird sighting".into()));
    p.fallback = None;
    if common.is_some() {
        p.lead = scientific.as_ref().map(|s| paragraph_html(&format!("<i>{}</i>", escape(s)))).unwrap_or_default();
        p.fallback = scientific;
    }
}

fn birdex(p: &mut Parts, e: &Event) {
    let species: std::collections::HashSet<&String> = all(e, "i").filter_map(|t| t.first()).filter(|i| i.contains("wikidata.org/")).collect();
    let names = values(e, "n");
    p.title = Some("Birdex".into());
    p.fallback = Some(count(species.len(), "species", "species"));
    p.content = Content::Hidden;
    p.extra = list_html(names.iter().map(|n| format!("<i>{}</i>", escape(n))), names.len());
}

fn sno(p: &mut Parts, e: &Event, content: &str) {
    let name = serde_json::from_str::<Value>(content).ok().and_then(|v| v.get("name")?.as_str().map(|n| clamp(n.trim(), 64))).filter(|n| !n.is_empty());
    p.title = name.or_else(|| tag(e, "name"));
    p.fallback = Some("A 3D object".into());
    p.content = Content::Hidden;
}

fn blobbi(p: &mut Parts, e: &Event) {
    p.title = tag(e, "name").or_else(|| Some("Blobbi".into()));
    p.summary = match tag(e, "stage").as_deref() {
        Some("egg") => Some("A Blobbi egg".into()),
        Some(stage @ ("baby" | "adult")) => Some(format!("A {stage} Blobbi")),
        _ => Some("A Blobbi".into()),
    };
    p.content = Content::Hidden;
}

fn tarot(p: &mut Parts, e: &Event) {
    let cards: Vec<String> = all(e, "c")
        .filter_map(|t| {
            let name = card_name(t.first()?.trim());
            (!name.is_empty()).then(|| if t.get(1).map(String::as_str) == Some("reversed") { format!("{name} (reversed)") } else { name })
        })
        .collect();
    p.title = Some("Tarot reading".into());
    if !cards.is_empty() {
        p.lead = paragraph_html(&escape(&cards.join(" · ")));
        p.fallback = Some(cards.join(", "));
    }
}

/// `two-of-cups` as a card is named: `Two of Cups`.
fn card_name(slug: &str) -> String {
    let words: Vec<String> = slug
        .split('-')
        .filter(|w| !w.is_empty())
        .enumerate()
        .map(|(i, word)| match word {
            "of" | "the" if i > 0 => word.to_string(),
            _ => word.chars().take(1).flat_map(char::to_uppercase).chain(word.chars().skip(1)).collect(),
        })
        .collect();
    words.join(" ")
}

/// A message only its recipient can read: who sent it to whom, and nothing
/// of what it says.
fn encrypted(p: &mut Parts, e: &Event) {
    let recipient = tag(e, "p").filter(|p| is_hex64(p)).map(|p| person_link(&p));
    let (title, what) = match e.kind {
        4 => (None, "Sent a direct message"),
        8211 => (None, "Sent a letter"),
        _ => (Some("Encrypted community invite"), "An invite to an encrypted community"),
    };
    p.title = title.map(str::to_string);
    p.summary = Some(what.into());
    p.fallback = None;
    p.image = None;
    p.video = None;
    p.audio = None;
    p.content = Content::Hidden;
    p.lead = match recipient.filter(|_| e.kind != 33301) {
        Some(recipient) => paragraph_html(&format!("{what} to {recipient}")),
        None => paragraph_html(what),
    };
}

fn vanish(p: &mut Parts, e: &Event) {
    let everywhere = values(e, "relay").iter().any(|r| r == "ALL_RELAYS");
    p.title = Some(if everywhere { "Request to vanish from every relay" } else { "Request to vanish" }.into());
    p.fallback = Some("Asked relays to erase this identity".into());
    p.image = None;
    p.video = None;
    p.audio = None;
}

/// The first image, video and audio an event carries: from its NIP-92
/// `imeta` tags, then from its `url` tags, then from URLs in its text.
#[derive(Default)]
struct Media {
    image: Option<String>,
    video: Option<Video>,
    audio: Option<Audio>,
}

impl Media {
    fn of(event: &Event, content: &str) -> Media {
        let mut media = Media::default();
        let mut thumbnail = None;

        for entries in imetas(event) {
            let Some(url) = field(entries, "url").and_then(safe_url) else { continue };
            let mime = field(entries, "m").unwrap_or("");
            match mime_kind(mime).or_else(|| media_kind(&url)) {
                Some(MediaKind::Image) if media.image.is_none() => media.image = Some(url),
                Some(MediaKind::Video) if media.video.is_none() => {
                    let (width, height) = dimensions(field(entries, "dim"));
                    let mime = if mime_kind(mime) == Some(MediaKind::Video) { mime.to_string() } else { video_type(&url).to_string() };
                    thumbnail = thumbnail.or_else(|| field(entries, "image").or(field(entries, "thumb")).and_then(safe_url));
                    media.video = Some(Video { url, mime, width, height });
                }
                Some(MediaKind::Audio) if media.audio.is_none() => {
                    let mime = if mime_kind(mime) == Some(MediaKind::Audio) { mime.to_string() } else { audio_type(&url).to_string() };
                    thumbnail = thumbnail.or_else(|| field(entries, "image").or(field(entries, "thumb")).and_then(safe_url));
                    media.audio = Some(Audio { url, mime });
                }
                _ => {}
            }
        }

        let tagged = all(event, "url").filter_map(|t| t.first()).cloned();
        for url in tagged.chain(urls(content)).filter_map(|url| safe_url(&url)) {
            match media_kind(&url) {
                Some(MediaKind::Image) if media.image.is_none() => media.image = Some(url),
                Some(MediaKind::Video) if media.video.is_none() => {
                    media.video = Some(Video { mime: video_type(&url).to_string(), url, width: None, height: None })
                }
                Some(MediaKind::Audio) if media.audio.is_none() => media.audio = Some(Audio { mime: audio_type(&url).to_string(), url }),
                _ => {}
            }
        }

        media.image = media.image.or(thumbnail);
        media
    }
}

/// The first `imeta` whose file is audio, by its type or its name.
fn imeta_audio(e: &Event) -> Option<Audio> {
    imetas(e).find_map(|entries| {
        let url = field(entries, "url").and_then(safe_url)?;
        let mime = field(entries, "m").filter(|m| mime_kind(m) == Some(MediaKind::Audio));
        (mime.is_some() || media_kind(&url) == Some(MediaKind::Audio)).then(|| Audio { mime: mime.map_or_else(|| audio_type(&url).into(), str::to_string), url })
    })
}

/// A tag's URL as audio, whatever its name says: a tag named for audio is.
fn tagged_audio(e: &Event, name: &str) -> Option<Audio> {
    url_tag(e, name).map(|url| Audio { mime: audio_type(&url).into(), url })
}

fn imeta_thumbnail(e: &Event) -> Option<String> {
    imetas(e).find_map(|entries| field(entries, "image").or(field(entries, "thumb")).and_then(safe_url))
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
    for url in urls(content) {
        let reference = crate::util::strip_prefix_ci(&url, "nostr:").len() != url.len();
        if reference || safe_url(&url).and_then(|u| media_kind(&u)).is_some() {
            text = text.replacen(&url, " ", 1);
        }
    }
    text
}

/// What a lightning invoice is for, in sats, from its human-readable part
/// (BOLT 11): `lnbc2500u…` is 2,500 µBTC. `None` for one with no amount.
fn bolt11_sats(invoice: &str) -> Option<u64> {
    let invoice = invoice.trim().to_ascii_lowercase();
    let invoice = invoice.strip_prefix("lightning:").unwrap_or(&invoice);
    let hrp = invoice.get(..invoice.rfind('1')?)?.strip_prefix("ln")?;
    // The currency, `bc`, `tb`, `bcrt`; then the amount and its multiplier.
    let amount = hrp.trim_start_matches(|c: char| c.is_ascii_lowercase());
    let (digits, multiplier) = match amount.chars().last()? {
        c @ ('m' | 'u' | 'n' | 'p') => (&amount[..amount.len() - 1], Some(c)),
        _ => (amount, None),
    };
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let amount: u128 = digits.parse().ok()?;
    let msat = match multiplier {
        None => amount.checked_mul(100_000_000_000)?,
        Some('m') => amount.checked_mul(100_000_000)?,
        Some('u') => amount.checked_mul(100_000)?,
        Some('n') => amount.checked_mul(100)?,
        Some(_) => amount / 10,
    };
    u64::try_from(msat / 1000).ok()
}

/// A PlayStation save's title, from the header of its first block: the
/// bytes after `SC`, in Shift-JIS. Full-width letters, digits and
/// punctuation are read as the ASCII they stand for; anything else (kana,
/// kanji) is left out, and a title that is all of it falls back to the tag.
fn save_title(content: &str) -> Option<String> {
    let header = hex::decode(content.trim().get(..0x44 * 2)?).ok()?;
    if &header[..2] != b"SC" {
        return None;
    }
    let raw = &header[4..];
    let raw = &raw[..raw.iter().position(|b| *b == 0).unwrap_or(raw.len())];
    let mut title = String::new();
    let mut i = 0;
    while i < raw.len() {
        let lead = raw[i];
        if (0x20..0x7f).contains(&lead) {
            title.push(lead as char);
            i += 1;
        } else if (0x81..=0x9f).contains(&lead) || (0xe0..=0xfc).contains(&lead) {
            if let Some(c) = raw.get(i + 1).and_then(|trail| full_width(lead, *trail)) {
                title.push(c);
            }
            i += 2;
        } else {
            i += 1;
        }
    }
    let title = title.split_whitespace().collect::<Vec<_>>().join(" ");
    (!title.is_empty()).then_some(title)
}

fn full_width(lead: u8, trail: u8) -> Option<char> {
    let c = match (lead, trail) {
        (0x82, 0x4f..=0x58) => b'0' + (trail - 0x4f),
        (0x82, 0x60..=0x79) => b'A' + (trail - 0x60),
        (0x82, 0x81..=0x9a) => b'a' + (trail - 0x81),
        (0x81, _) => match trail {
            0x40 => b' ',
            0x43 => b',',
            0x44 => b'.',
            0x46 => b':',
            0x47 => b';',
            0x48 => b'?',
            0x49 => b'!',
            0x5b..=0x5d | 0x7c => b'-',
            0x5e => b'/',
            0x60 => b'~',
            0x62 => b'|',
            0x65 | 0x66 => b'\'',
            0x67 | 0x68 => b'"',
            0x69 => b'(',
            0x6a => b')',
            0x6d => b'[',
            0x6e => b']',
            0x7b => b'+',
            0x81 => b'=',
            0x83 => b'<',
            0x84 => b'>',
            0x90 => b'$',
            0x93 => b'%',
            0x94 => b'#',
            0x95 => b'&',
            0x96 => b'*',
            0x97 => b'@',
            _ => return None,
        },
        _ => return None,
    };
    Some(c as char)
}

/// The first tag of a name's value, when it has a non-empty one.
fn tag(event: &Event, name: &str) -> Option<String> {
    event.tag(name).map(str::trim).filter(|v| !v.is_empty()).map(str::to_string)
}

/// A tag's URL, if it is one a page may load.
fn url_tag(event: &Event, name: &str) -> Option<String> {
    tag(event, name).and_then(|url| safe_url(&url))
}

/// Every tag of a name, each without it.
fn all<'a>(event: &'a Event, name: &'a str) -> impl Iterator<Item = &'a [String]> + 'a {
    event.tags.iter().filter(move |t| t.first().map(String::as_str) == Some(name)).map(|t| &t[1..])
}

/// Every non-empty value of a tag's name.
fn values(event: &Event, name: &str) -> Vec<String> {
    all(event, name).filter_map(|t| t.first()).map(|v| v.trim().to_string()).filter(|v| !v.is_empty()).collect()
}

fn imetas(event: &Event) -> impl Iterator<Item = &[String]> {
    all(event, "imeta")
}

/// An `imeta`-style entry, `key value`.
fn field<'a>(entries: &'a [String], key: &str) -> Option<&'a str> {
    entries.iter().find_map(|entry| entry.strip_prefix(key)?.strip_prefix(' ')).map(str::trim).filter(|v| !v.is_empty())
}

fn published_at(e: &Event) -> Option<i64> {
    tag(e, "published_at").and_then(|at| at.parse().ok()).filter(|at| *at > 0)
}

/// The event an event is about, by its last `e` tag, linked to its page on
/// the same site.
fn target(e: &Event) -> Option<String> {
    let id = all(e, "e").filter_map(|t| t.first()).filter(|id| is_hex64(id)).last()?;
    let note = nip19::note_encode(id);
    Some(format!("<a href=\"/{note}\">a post</a>"))
}

/// The address an event is about, by its `a` tag, linked the same way.
fn coordinate(e: &Event) -> Option<String> {
    let a = all(e, "a").filter_map(|t| t.first()).next()?;
    let mut parts = a.splitn(3, ':');
    let (kind, pubkey, identifier) = (parts.next()?.parse::<u32>().ok()?, parts.next()?, parts.next().unwrap_or(""));
    if !is_hex64(pubkey) {
        return None;
    }
    let naddr = nip19::naddr_encode(pubkey, kind, identifier, &[]);
    let label = if identifier.is_empty() { "this".to_string() } else { escape(identifier) };
    Some(format!("<a href=\"/{naddr}\">{label}</a>"))
}

fn person_link(pubkey: &str) -> String {
    format!("<a href=\"/{}\">@{}</a>", nip19::npub_encode(pubkey), escape(&short_npub(pubkey)))
}

fn web_link(url: &str, text: &str) -> String {
    format!("<a href=\"{}\" rel=\"nofollow ugc\">{}</a>", escape(url), escape(text))
}

/// Markup in a paragraph. The caller has escaped it.
fn paragraph_html(html: &str) -> String {
    format!("<p>{html}</p>\n")
}

/// Items in a list, at most [`MAX_LIST`] of the `total`, each markup the
/// caller has escaped.
fn list_html(items: impl Iterator<Item = String>, total: usize) -> String {
    let items: Vec<String> = items.take(MAX_LIST).collect();
    if items.is_empty() {
        return String::new();
    }
    let mut out = String::from("<ul>\n");
    for item in &items {
        out.push_str(&format!("<li>{item}</li>\n"));
    }
    if total > items.len() {
        out.push_str(&format!("<li>and {} more</li>\n", thousands((total - items.len()) as u64)));
    }
    out.push_str("</ul>\n");
    out
}

fn count(n: usize, one: &str, many: &str) -> String {
    format!("{} {}", thousands(n as u64), if n == 1 { one } else { many })
}

/// `1234567` as `1,234,567`.
fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, digit) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

fn bytes(n: u64) -> String {
    match n {
        n if n >= 1 << 30 => format!("{:.1} GB", n as f64 / (1u64 << 30) as f64),
        n if n >= 1 << 20 => format!("{:.1} MB", n as f64 / (1u64 << 20) as f64),
        n if n >= 1 << 10 => format!("{:.1} KB", n as f64 / (1u64 << 10) as f64),
        n => format!("{n} bytes"),
    }
}

/// `m:ss`, or `h:mm:ss`.
fn clock(seconds: u64) -> String {
    let (h, m, s) = (seconds / 3600, seconds / 60 % 60, seconds % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// A file's name, from its URL, unless it is only a Blossom hash.
fn file_name(url: &str) -> Option<String> {
    let name = url::Url::parse(url).ok()?.path_segments()?.rfind(|s| !s.is_empty())?.to_string();
    let name = crate::util::decode_uri_component(&name).unwrap_or(name);
    let stem = name.split('.').next().unwrap_or(&name);
    (!is_hex64(stem)).then_some(name)
}

/// At most `max` bytes of `text`, cut on a character boundary.
pub fn truncate(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::link_previews::card::{event_card, Card, CardKind};

    const SECRET: &str = "0000000000000000000000000000000000000000000000000000000000000003";
    const OTHER: &str = "0000000000000000000000000000000000000000000000000000000000000004";
    const PUBKEY: &str = "3bf0c63fcb93463407af97a5e5ee64fa883d107ef9e558472c4eb9aaaefa459d";

    fn signed(secret: &str, kind: i64, tags: &[&[&str]], content: &str) -> Event {
        let tags = tags.iter().map(|t| t.iter().map(|v| v.to_string()).collect()).collect();
        Event::sign(secret, 1_700_000_000, kind, tags, content)
    }

    fn sign(kind: i64, tags: &[&[&str]], content: &str) -> Event {
        signed(SECRET, kind, tags, content)
    }

    fn card(kind: i64, tags: &[&[&str]], content: &str) -> Card {
        event_card(&sign(kind, tags, content), None).unwrap_or_else(|| panic!("kind {kind} has no card"))
    }

    /// An event of each kind, as the app that makes it would.
    fn example(kind: i64) -> Event {
        let id = "b".repeat(64);
        let a = format!("30009:{PUBKEY}:bravery");
        let e: &[&str] = &["e", &id];
        let p: &[&str] = &["p", PUBKEY];
        let image: &[&str] = &["image", "https://cdn.example/i.jpg"];
        match kind {
            1 | 11 | 1111 | 1311 | 2004 => sign(kind, &[], "Hello"),
            3 | 15683 | 18678 => sign(kind, &[p], ""),
            4 | 8211 => sign(kind, &[p], "c2VjcmV0?iv=YWJj"),
            6 | 16 => sign(kind, &[e], &sign(1, &[], "Reposted words").to_json()),
            7 => sign(kind, &[e, p], "+"),
            8 => sign(kind, &[&["a", &a], p], ""),
            20 => sign(kind, &[&["imeta", "url https://cdn.example/p", "m image/jpeg"]], "A photo"),
            21 | 22 | 34235 | 34236 => sign(kind, &[&["d", "v"], &["title", "Clip"], &["imeta", "url https://cdn.example/v.mp4"]], ""),
            62 => sign(kind, &[&["relay", "ALL_RELAYS"]], "Goodbye"),
            1018 => sign(kind, &[e, &["response", "a"]], ""),
            1063 => sign(kind, &[&["url", "https://cdn.example/f.png"], &["m", "image/png"]], ""),
            1068 => sign(kind, &[&["option", "a", "Yes"], &["option", "b", "No"]], "Well?"),
            1222 | 1244 => sign(kind, &[], "https://cdn.example/voice.m4a"),
            1617 => sign(kind, &[], "From abc\nSubject: [PATCH] Fix things\n\n---\n a | 1 +"),
            1618 | 1619 | 1621 => sign(kind, &[&["subject", "Do the thing"]], "Because."),
            1630..=1633 => sign(kind, &[e], ""),
            1984 => sign(kind, &[&["p", PUBKEY, "spam"]], "Lots of it"),
            2003 => sign(kind, &[&["x", "a".repeat(40).as_str()], &["file", "movie/clip.mkv", "1048576"]], "A clip"),
            2256 => sign(kind, &[&["c", "the-fool"], &["c", "two-of-cups", "reversed"]], ""),
            2473 => sign(kind, &[&["alt", "Bird detection: Northern Cardinal (Cardinalis cardinalis)"], &["n", "Cardinalis cardinalis"]], ""),
            3063 => sign(kind, &[&["url", "https://cdn.example/app.apk"], &["version", "1.2"]], ""),
            3367 => sign(kind, &[&["c", "#ff0000"], &["c", "#00ff00"], &["name", "Spring"]], "🌸"),
            5128 | 15128 | 35128 => sign(kind, &[&["d", "blog"], &["path", "/index.html", &id]], ""),
            7516 => sign(kind, &[&["a", &format!("37516:{PUBKEY}:cache")]], "Found it!"),
            7849 => sign(kind, &[&["alt", "Quiz result: Wizard on \"Which are you?\""]], ""),
            8333 => sign(kind, &[p, &["amount", "5000"]], "Thanks"),
            9735 => zap_receipt(),
            9802 => sign(kind, &[&["r", "https://example.com/post", "source"]], "Quoted words"),
            10002 => sign(kind, &[&["r", "wss://relay.example.com"]], ""),
            10008 | 30008 => sign(kind, &[&["d", "profile_badges"], &["a", &a]], ""),
            10011 => sign(kind, &[&["i", "github:alex", "proof"]], ""),
            12473 => sign(kind, &[&["i", "https://www.wikidata.org/entity/Q1"], &["n", "Cardinalis cardinalis"]], ""),
            16767 => sign(kind, &[&["bg", "url https://cdn.example/bg.jpg"]], ""),
            30000 | 39089 => sign(kind, &[&["d", "friends"], &["title", "Friends"], p], ""),
            30009 => sign(kind, &[&["d", "bravery"], &["name", "Bravery"], image], ""),
            30023 | 30024 => sign(kind, &[&["d", "post"], &["title", "Post"]], "Some *words*."),
            30030 => sign(kind, &[&["d", "pack"], &["emoji", "wave", "https://cdn.example/wave.png"]], ""),
            30054 | 30055 => sign(kind, &[&["d", "ep"], &["title", "Episode"], &["audio", "https://cdn.example/ep.mp3"]], "Show notes"),
            30063 => sign(kind, &[&["d", "app@1.2"], &["i", "app"], &["version", "1.2"]], "Fixed *bugs*."),
            30311 => sign(kind, &[&["d", "s"], &["title", "Stream"], &["status", "live"]], ""),
            30312 | 30313 => sign(kind, &[&["d", "r"], &["room", "Room"]], ""),
            30315 => sign(kind, &[&["d", "general"]], "Working"),
            30402 => sign(kind, &[&["d", "l"], &["title", "Bike"], &["price", "100", "usd"]], "A bike."),
            30617 => sign(kind, &[&["d", "npanel"], &["description", "A gateway"]], ""),
            30618 => sign(kind, &[&["d", "npanel"], &["refs/heads/main", &"c".repeat(40)]], ""),
            30621 => sign(kind, &[&["d", "orion"], &["title", "Orion"]], "My Orion"),
            30817 => sign(kind, &[&["d", "xx"], &["title", "A NIP"]], "# A NIP\n\nIt does things."),
            31124 => sign(kind, &[&["d", "b"], &["name", "Blob"], &["stage", "baby"]], ""),
            31871 => sign(kind, &[&["d", "x"], &["s", "valid"], e], "Checked"),
            31922 => sign(kind, &[&["d", "c"], &["title", "Party"], &["start", "2025-06-01"]], ""),
            31923 => sign(kind, &[&["d", "c"], &["title", "Party"], &["start", "1748790000"]], ""),
            31985 => sign(kind, &[&["d", "isbn:9780000000000"], &["rating", "0.8"]], "Good book"),
            31990 => sign(kind, &[&["d", "h"]], r#"{"name":"App","about":"Does things"}"#),
            32267 => sign(kind, &[&["d", "app"], &["name", "App"], &["icon", "https://cdn.example/icon.png"]], "An app"),
            33301 => sign(kind, &[&["d", "invite"]], "ciphertext"),
            33331 => sign(kind, &[&["d", "cube"]], r#"{"v":1,"name":"Cube","vertices":[]}"#),
            33863 => sign(kind, &[&["d", "f"], &["title", "Fund"], &["w", "bc1qexample"], &["goal", "1000"]], "Help *us*."),
            33953 | 34609 | 39731 => sign(kind, &[&["d", "b"], &["title", "Book"], &["author", "Ann"]], ""),
            34139 => sign(kind, &[&["d", "mix"], &["title", "Mix"], &["a", &format!("36787:{PUBKEY}:song")]], ""),
            34550 => sign(kind, &[&["d", "c"], &["name", "Community"], &["description", "A place"]], ""),
            36767 => sign(kind, &[&["d", "t"], &["title", "Dark"]], ""),
            36787 => sign(kind, &[&["d", "song"], &["title", "Song"], &["artist", "Band"], &["url", "https://cdn.example/s.mp3"]], ""),
            37381 => sign(kind, &[&["d", "deck"], &["title", "Deck"], &["c", "Lightning Bolt", "4"]], ""),
            37516 => sign(kind, &[&["d", "cache"], &["name", "Cache"], &["D", "2"], &["T", "3"]], "Under a rock"),
            37849 => sign(kind, &[&["d", "q"], &["title", "Which are you?"], &["question", "1", "Pick one"]], ""),
            38192 => sign(kind, &[&["d", "save-0"], &["title", "Save"]], &memory_block(b"SC\x11\x01MY GAME")),
            39701 => sign(kind, &[&["d", "example.com/page"], &["title", "A page"]], "Worth reading"),
            other => panic!("no example of kind {other}"),
        }
    }

    trait Json {
        fn to_json(&self) -> String;
    }

    impl Json for Event {
        fn to_json(&self) -> String {
            serde_json::to_string(self).unwrap()
        }
    }

    fn zap_receipt() -> Event {
        let request = signed(OTHER, 9734, &[&["p", PUBKEY], &["amount", "21000"]], "Great post!");
        sign(9735, &[&["p", PUBKEY], &["bolt11", "lnbc210n1pjexample"], &["description", &request.to_json()]], "")
    }

    fn memory_block(header: &[u8]) -> String {
        let mut block = header.to_vec();
        block.resize(8192, 0);
        hex::encode(block)
    }

    /// Text that is markup, JSON, or a long run of hex has no place in a
    /// description.
    fn readable(text: &str) -> bool {
        let hex_run = text.split(|c: char| !c.is_ascii_hexdigit()).any(|run| run.len() >= 64);
        !text.trim_start().starts_with(['{', '[', '<']) && !hex_run && !text.contains("\"sig\"")
    }

    #[test]
    fn every_kind_ditto_renders_is_previewed() {
        for &kind in SUPPORTED.iter().filter(|k| **k != 0) {
            let event = example(kind);
            let card = event_card(&event, None).unwrap_or_else(|| panic!("kind {kind} has no card"));
            assert_eq!(card.event_kind, kind);
            for text in card.title.iter().chain(card.description.iter()) {
                assert!(readable(text), "kind {kind}: {text}");
            }
            assert!(card.title.is_some() || card.description.is_some(), "kind {kind} says nothing: {card:?}");
            assert!(!card.body.contains("\"sig\""), "kind {kind}: {}", card.body);
            assert!(card.body.starts_with("<article>") && card.body.ends_with("</article>\n"), "kind {kind}");
        }
    }

    #[test]
    fn nothing_else_is_previewed() {
        for kind in 0..=65_535 {
            if supported(kind) {
                continue;
            }
            let event = Event { id: "a".repeat(64), pubkey: PUBKEY.into(), created_at: 1, kind, tags: vec![vec!["alt".into(), "Something".into()]], content: "Hello".into(), sig: String::new() };
            assert!(read(&event).is_none(), "kind {kind} has a reader but isn't listed");
        }
    }

    #[test]
    fn reposts_are_the_event_they_carry() {
        let original = signed(OTHER, 1, &[], "Original words");
        let repost = sign(6, &[&["e", &original.id]], &original.to_json());
        let parts = read(&repost).unwrap();
        assert_eq!(parts.author, original.pubkey);
        let card = event_card(&repost, None).unwrap();
        assert_eq!(card.description.as_deref(), Some("Original words"));
        assert_eq!(card.event_kind, 6);
        assert!(card.body.contains("Reposted by <a href=\"/npub1"), "{}", card.body);

        // One whose signature doesn't hold is not taken at its word.
        let mut forged = original.clone();
        forged.content = "Words they never wrote".into();
        let card = event_card(&sign(6, &[&["e", &original.id]], &forged.to_json()), None).unwrap();
        assert_eq!(card.description.as_deref(), Some("Reposted a post"));
        assert!(!card.body.contains("never wrote"), "{}", card.body);
        assert_eq!(card.author.unwrap().pubkey, crate::nostr::event::public_key(SECRET));

        // Nor is a repost of an event with no preview.
        let unknown = signed(OTHER, 31337, &[], "Mystery");
        let card = event_card(&sign(16, &[], &unknown.to_json()), None).unwrap();
        assert_eq!(card.description.as_deref(), Some("Reposted a post"));
    }

    #[test]
    fn zaps_are_their_senders() {
        let receipt = zap_receipt();
        let parts = read(&receipt).unwrap();
        assert_eq!(parts.author, crate::nostr::event::public_key(OTHER));
        let card = event_card(&receipt, None).unwrap();
        assert_eq!(card.title.as_deref(), Some("Zapped 21 sats"));
        assert_eq!(card.description.as_deref(), Some("Great post!"));
        assert!(!card.body.contains("9734"), "{}", card.body);

        // A request that isn't signed by its sender is no one's comment.
        let mut request = signed(OTHER, 9734, &[], "Great post!");
        request.content = "Forged".into();
        let receipt = sign(9735, &[&["bolt11", "lnbc1u1pexample"], &["description", &request.to_json()]], "");
        let card = event_card(&receipt, None).unwrap();
        assert_eq!(card.title.as_deref(), Some("Zapped 100 sats"));
        assert_eq!(card.description, None);
        assert_eq!(card.author.unwrap().pubkey, crate::nostr::event::public_key(SECRET));
    }

    #[test]
    fn reads_invoice_amounts() {
        assert_eq!(bolt11_sats("lnbc2500u1pvjluez"), Some(250_000));
        assert_eq!(bolt11_sats("LNBC210N1PJ"), Some(21));
        assert_eq!(bolt11_sats("lnbc1m1p"), Some(100_000));
        assert_eq!(bolt11_sats("lntb20m1p"), Some(2_000_000));
        assert_eq!(bolt11_sats("lnbcrt5000p1p"), Some(0));
        assert_eq!(bolt11_sats("lnbc1pvjluez"), None);
        assert_eq!(bolt11_sats("lnbc+5u1p"), None);
        assert_eq!(bolt11_sats("nonsense"), None);
    }

    #[test]
    fn keeps_secrets() {
        for kind in [4, 8211, 33301] {
            let card = card(kind, &[&["p", PUBKEY]], "dGhlIHNlY3JldA==?iv=abc https://cdn.example/x.png");
            assert!(!card.body.contains("dGhl"), "{}", card.body);
            assert!(!card.description.as_deref().unwrap_or("").contains("dGhl"));
            assert_eq!(card.image, None);
        }
        assert_eq!(card(4, &[&["p", PUBKEY]], "x").description.as_deref(), Some("Sent a direct message"));
    }

    #[test]
    fn content_warnings_hide_what_they_warn_of() {
        let card = card(20, &[&["content-warning", "Gore"], &["imeta", "url https://cdn.example/p.jpg"]], "Look https://cdn.example/q.jpg");
        assert_eq!(card.description.as_deref(), Some("Content warning: Gore"));
        assert_eq!(card.image, None);
        assert!(!card.body.contains("cdn.example"), "{}", card.body);
    }

    #[test]
    fn reads_json_content_as_fields() {
        let card = card(31990, &[&["d", "h"]], r#"{"name":"Handler","about":"Opens things","picture":"https://cdn.example/p.png","website":"https://handler.example"}"#);
        assert_eq!(card.title.as_deref(), Some("Handler"));
        assert_eq!(card.description.as_deref(), Some("Opens things"));
        assert_eq!(card.image.as_deref(), Some("https://cdn.example/p.png"));
        assert!(!card.body.contains("\"about\""), "{}", card.body);
    }

    #[test]
    fn refuses_what_its_app_would() {
        assert!(read(&sign(30402, &[&["d", "l"]], "No title")).is_none());
        assert!(read(&sign(33863, &[&["d", "f"], &["title", "Fund"]], "Nowhere to send")).is_none());
        assert!(read(&sign(36767, &[&["d", "t"]], "")).is_none());
        assert!(read(&sign(7, &[], ":unknown:")).is_none());
        assert!(read(&sign(7, &[], "not a reaction at all")).is_none());
    }

    #[test]
    fn reactions() {
        let card = card(7, &[&["e", &"b".repeat(64)], &["emoji", "wave", "https://cdn.example/wave.png"]], ":wave:");
        assert_eq!(card.description.as_deref(), Some("Reacted :wave:"));
        assert_eq!(card.image.as_deref(), Some("https://cdn.example/wave.png"));
        assert!(card.body.contains("to <a href=\"/note1"), "{}", card.body);
        assert_eq!(super::tests::card(7, &[], "-").description.as_deref(), Some("Reacted 👎"));
    }

    #[test]
    fn audio() {
        let track = card(36787, &[&["d", "s"], &["title", "Song"], &["artist", "Band"], &["album", "Record"], &["imeta", "url https://cdn.example/s", "m audio/flac", "image https://cdn.example/c.jpg"]], "");
        assert_eq!(track.kind, CardKind::Audio);
        assert_eq!(track.audio, Some(Audio { url: "https://cdn.example/s".into(), mime: "audio/flac".into() }));
        assert_eq!(track.image.as_deref(), Some("https://cdn.example/c.jpg"));
        assert_eq!(track.description.as_deref(), Some("by Band on Record"));

        let voice = card(1222, &[&["imeta", "url https://cdn.example/v.webm", "m audio/webm", "duration 75"]], "https://cdn.example/v.webm");
        assert_eq!(voice.audio.as_ref().map(|a| a.mime.as_str()), Some("audio/webm"));
        assert_eq!(voice.description.as_deref(), Some("Voice message, 1:15"));
        assert_eq!(voice.body.matches("v.webm").count(), 1, "{}", voice.body);

        let podcast = card(30054, &[&["d", "e"], &["title", "Ep"], &["audio", "https://cdn.example/e"], &["description", "Notes"]], "");
        assert_eq!(podcast.audio.as_ref().map(|a| a.url.as_str()), Some("https://cdn.example/e"));

        let file = card(1063, &[&["url", "https://cdn.example/f.ogg"], &["m", "audio/ogg"], &["alt", "A recording"]], "");
        assert_eq!((file.kind, file.title.as_deref()), (CardKind::Audio, Some("A recording")));
    }

    #[test]
    fn lists() {
        let follows = card(3, &[&["p", PUBKEY], &["p", PUBKEY], &["p", "nope"]], r#"{"wss://r.example":{"read":true}}"#);
        assert_eq!(follows.description.as_deref(), Some("1 person"));
        assert_eq!(follows.body.matches("<li>").count(), 1, "{}", follows.body);
        assert!(!follows.body.contains("wss://r.example"), "{}", follows.body);

        let many: Vec<String> = (0..60).map(|i| format!("{i:064x}")).collect();
        let tags: Vec<Vec<&str>> = many.iter().map(|p| vec!["p", p.as_str()]).collect();
        let tags: Vec<&[&str]> = tags.iter().map(Vec::as_slice).collect();
        let pack = card(39089, &tags, "");
        assert_eq!(pack.description.as_deref(), Some("60 people"));
        assert!(pack.body.contains("<li>and 10 more</li>"), "{}", pack.body);
    }

    #[test]
    fn polls_and_highlights() {
        let poll = card(1068, &[&["option", "a", "Yes"], &["option", "b", "No <b>"]], "Should we?");
        assert_eq!((poll.title.as_deref(), poll.description.as_deref()), (Some("Poll"), Some("Should we?")));
        assert!(poll.body.contains("<li>No &lt;b&gt;</li>"), "{}", poll.body);

        let highlight = card(9802, &[&["r", "https://example.com/a", "source"], &["comment", "So true"]], "The quoted part");
        assert_eq!(highlight.description.as_deref(), Some("“The quoted part”"));
        assert!(highlight.body.contains("<blockquote>\n<p>The quoted part</p>\n</blockquote>"), "{}", highlight.body);
        assert!(highlight.body.contains("From <a href=\"https://example.com/a\""), "{}", highlight.body);
    }

    #[test]
    fn particulars() {
        assert_eq!(card(1617, &[], "From abc\nSubject: [PATCH v2 1/3] Fix <things>\n\n---").title.as_deref(), Some("Fix <things>"));
        assert!(card(1617, &[], "diff --git a/x b/x\n+<b>").body.contains("<pre><code>diff --git a/x b/x\n+&lt;b&gt;</code></pre>"));
        assert_eq!(card(2256, &[&["c", "the-fool"], &["c", "king-of-the-wands", "reversed"]], "").description.as_deref(), Some("The Fool, King of the Wands (reversed)"));
        assert_eq!(card(2473, &[&["alt", "Bird detection: Blue Jay (Cyanocitta cristata)"]], "").title.as_deref(), Some("Blue Jay"));
        assert_eq!(card(38192, &[], &memory_block(b"SC\x11\x01\x82\x6c\x82\x81\x82\x92\x82\x89\x82\x8f\x81\x40\x82\x50")).title.as_deref(), Some("Mario 1"));
        assert_eq!(card(38192, &[&["title", "Fallback"]], "zz").title.as_deref(), Some("Fallback"));
        assert_eq!(card(31985, &[&["d", "isbn:123"], &["rating", "0.6"]], "Fine").description.as_deref(), Some("★★★☆☆ — Fine"));
        assert_eq!(card(39701, &[&["d", "example.com/a"]], "").description.as_deref(), Some("https://example.com/a"));
        assert_eq!(card(30402, &[&["d", "l"], &["title", "Bike"], &["price", "100", "usd", "month"], &["status", "sold"]], "Fast").description.as_deref(), Some("Sold · 100 USD per month — Fast"));
        let party = card(31923, &[&["d", "p"], &["title", "Party"], &["start", "1748790000"], &["start_tzid", "America/New_York"], &["location", "Park"]], "");
        assert_eq!(party.description.as_deref(), Some("June 1, 2025, 11:00 AM EDT · Park"));
        assert_eq!(card(1984, &[&["e", &"b".repeat(64), "nudity"]], "").title.as_deref(), Some("Reported for nudity"));
        assert_eq!(card(30030, &[&["d", "p"], &["emoji", "a", "https://cdn.example/a.png"]], "").image.as_deref(), Some("https://cdn.example/a.png"));
        assert_eq!(card(30617, &[&["d", "npanel"], &["clone", "https://git.example/npanel.git"]], "").title.as_deref(), Some("npanel"));
    }
}
