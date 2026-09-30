//! Link-preview metadata, read from a site's own `index.html`.
//!
//! Every field is optional because a site owes npanel nothing: plenty of
//! nsites ship a bare document with no Open Graph tags at all, and those still
//! deserve to be listed.

use std::num::NonZeroUsize;

use parking_lot::Mutex;
use rusqlite::{Connection, OptionalExtension};
use serde::Serialize;

use crate::hash::BlobHash;
use crate::blobs::BlobStore;
use crate::db::{Host, HostBuild};
use crate::site_csp::MAX_HTML_BYTES;
use crate::sites::resolve_site_path;

/// A card's worth of a site.
#[derive(Clone, Debug, Default, Serialize, PartialEq)]
pub struct SitePreview {
    pub title: Option<String>,
    pub description: Option<String>,
    /// Absolute https URL, or null.
    pub image: Option<String>,
    /// The site's own icon, as an absolute https URL, or null.
    pub icon: Option<String>,
}

/// An icon a document links to, as written.
#[derive(Clone, Debug)]
struct IconLink {
    href: String,
    /// `apple-touch-icon` is drawn full-bleed on a rounded square.
    apple: bool,
    /// The largest edge it claims; [`LARGEST`] for a vector, 0 if unstated.
    size: u32,
}

/// An icon a web app manifest names, as written.
#[derive(Clone, Debug, PartialEq)]
pub struct ManifestIcon {
    pub src: String,
    pub size: u32,
}

/// Preview metadata as it was written, before URLs are resolved.
#[derive(Clone, Debug, Default)]
pub struct RawMeta {
    pub title: Option<String>,
    pub description: Option<String>,
    pub image: Option<String>,
    icons: Vec<IconLink>,
    manifest: Option<String>,
}

const MAX_TITLE: usize = 200;
const MAX_DESCRIPTION: usize = 300;
const MAX_CACHED: usize = 500;
const MAX_MANIFEST_BYTES: u64 = 64 * 1024;

/// The edge past which a bigger icon is no better, and what a vector counts as.
const LARGEST: u32 = 1024;

/// Where an icon came from, ranked: an apple-touch-icon is drawn to be a home
/// screen icon, a manifest icon is the same thing by another name, and a
/// favicon is a glyph meant to be read at 16px in a tab. Higher wins.
const RANK_APPLE_TOUCH: f64 = 300.0;
const RANK_MANIFEST: f64 = 200.0;
const RANK_FAVICON: f64 = 100.0;

/// Files at the conventional names, for sites that ship an icon without ever
/// linking to it.
const CONVENTIONAL: [(&str, f64); 6] = [
    ("/apple-touch-icon.png", RANK_APPLE_TOUCH),
    ("/icon.svg", RANK_FAVICON + 0.9),
    ("/icon.png", RANK_FAVICON + 0.8),
    ("/favicon.svg", RANK_FAVICON + 0.7),
    ("/favicon.png", RANK_FAVICON + 0.6),
    ("/favicon.ico", RANK_FAVICON + 0.5),
];

/// Rank within a class by declared size, never outranking the class above.
fn score(rank: f64, size: u32) -> f64 {
    rank + f64::from(size.min(LARGEST)) / f64::from(LARGEST + 1)
}

/// Preview metadata, cached by the sha256 of the document it came from — so
/// the cache can never go stale, and two hosts serving the same document share
/// an entry while each resolves relative URLs against its own hostname.
pub struct SitePreviews {
    blobs: BlobStore,
    documents: Mutex<lru::LruCache<BlobHash, RawMeta>>,
    manifests: Mutex<lru::LruCache<BlobHash, Vec<ManifestIcon>>>,
}

impl SitePreviews {
    pub fn new(blobs: BlobStore) -> Self {
        let cap = NonZeroUsize::new(MAX_CACHED).unwrap();
        Self { blobs, documents: Mutex::new(lru::LruCache::new(cap)), manifests: Mutex::new(lru::LruCache::new(cap)) }
    }

    /// A host's preview: what its landing page says about itself, and its
    /// icon, read from the build already on disk. Nothing is fetched. Blocking.
    pub fn preview(&self, conn: &Connection, host: &Host, build: Option<&HostBuild>) -> rusqlite::Result<SitePreview> {
        let Some(build) = build else {
            return Ok(SitePreview::default());
        };

        let origin = url::Url::parse(&format!("https://{}/", host.hostname)).expect("a hostname makes a URL");
        // With the SPA flag on, the shell the site would serve for `/` is the
        // page being described, wherever it is published.
        let index = resolve_site_path(conn, build.id, "/", host.spa)?;
        let page = index.filter(|file| file.status == 200).map(|file| file.sha256);

        let mut lookup_stmt = conn.prepare_cached("SELECT sha256 FROM host_build_files WHERE build_id = ? AND path = ?")?;
        let mut lookup = |path: &str| -> Option<BlobHash> {
            lookup_stmt.query_row(rusqlite::params![build.id, path], |row| row.get(0)).optional().ok().flatten()
        };

        let meta = page.map(|sha| self.meta(sha)).unwrap_or_default();
        let icon = self.icon(&meta, &origin, &mut lookup);

        Ok(SitePreview {
            title: meta.title,
            description: meta.description,
            image: resolve_image(meta.image.as_deref(), &origin),
            icon,
        })
    }

    /// The site's icon, the way a home screen would pick it: every icon the
    /// site declares, ranked by kind and then by size, and the best one the
    /// build can actually serve. An icon on the site's own domain counts only
    /// if the build has the file, so a stale link never becomes a broken image.
    fn icon(&self, meta: &RawMeta, origin: &url::Url, lookup: &mut dyn FnMut(&str) -> Option<BlobHash>) -> Option<String> {
        let mut candidates: Vec<(String, url::Url, f64)> = meta
            .icons
            .iter()
            .map(|link| {
                (link.href.clone(), origin.clone(), score(if link.apple { RANK_APPLE_TOUCH } else { RANK_FAVICON }, link.size))
            })
            .collect();

        if let Some(manifest) = &meta.manifest {
            candidates.extend(self.manifest_icons(manifest, origin, lookup));
        }
        for (path, rank) in CONVENTIONAL {
            candidates.push((path.to_string(), origin.clone(), rank));
        }

        // Stable, as JavaScript's sort is: equal scores keep their order.
        candidates.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));

        candidates.into_iter().find_map(|(href, base, _)| usable(&href, &base, lookup))
    }

    fn manifest_icons(
        &self,
        href: &str,
        origin: &url::Url,
        lookup: &mut dyn FnMut(&str) -> Option<BlobHash>,
    ) -> Vec<(String, url::Url, f64)> {
        // A manifest elsewhere would have to be fetched, and nothing here fetches.
        let Some(url) = same_origin(href, origin) else {
            return Vec::new();
        };
        let Some(sha256) = lookup(&site_path(&url)) else {
            return Vec::new();
        };

        let cached = self.manifests.lock().get(&sha256).cloned();
        let icons = cached.unwrap_or_else(|| {
            let icons = extract_manifest_icons(&self.blobs.read_text(sha256, MAX_MANIFEST_BYTES));
            self.manifests.lock().put(sha256, icons.clone());
            icons
        });

        // Icons in a manifest are relative to the manifest, not to the page.
        icons.into_iter().map(|icon| (icon.src, url.clone(), score(RANK_MANIFEST, icon.size))).collect()
    }

    fn meta(&self, sha256: BlobHash) -> RawMeta {
        if let Some(meta) = self.documents.lock().get(&sha256) {
            return meta.clone();
        }
        let meta = extract_meta(&self.blobs.read_text(sha256, MAX_HTML_BYTES));
        self.documents.lock().put(sha256, meta.clone());
        meta
    }
}

/// An icon reference as a URL the dashboard can show: one on another https
/// host as it stands, one on the site's own domain only if the build has it.
fn usable(href: &str, base: &url::Url, lookup: &mut dyn FnMut(&str) -> Option<BlobHash>) -> Option<String> {
    let url = resolve_image(Some(href), base)?;
    let Some(own) = same_origin(&url, base) else {
        return Some(url);
    };
    lookup(&site_path(&own)).map(|_| own.to_string())
}

fn same_origin(href: &str, base: &url::Url) -> Option<url::Url> {
    let url = base.join(href).ok()?;
    (url.origin() == base.origin()).then_some(url)
}

/// The path a URL names within a build, which stores paths decoded.
fn site_path(url: &url::Url) -> String {
    crate::util::decode_path(url.path())
}

/// Resolve an image reference against the site that named it. An absolute one
/// is honoured only over https: the dashboard's own policy would block
/// anything else.
pub fn resolve_image(value: Option<&str>, origin: &url::Url) -> Option<String> {
    let value = value.filter(|v| !v.is_empty())?;
    let url = origin.join(value).ok()?;
    (url.scheme() == "https").then(|| url.to_string())
}

/// The icons a web app manifest names that could stand for the site. A
/// `monochrome` icon is a silhouette meant to be tinted, useless as a tile, so
/// one declared for nothing else is left out.
pub fn extract_manifest_icons(json: &str) -> Vec<ManifestIcon> {
    let Ok(manifest) = serde_json::from_str::<serde_json::Value>(json) else {
        return Vec::new();
    };
    let Some(icons) = manifest.get("icons").and_then(|icons| icons.as_array()) else {
        return Vec::new();
    };

    icons
        .iter()
        .filter_map(|icon| {
            let src = icon.get("src")?.as_str()?.trim();
            if src.is_empty() {
                return None;
            }
            let purposes: Vec<String> = icon
                .get("purpose")
                .and_then(|p| p.as_str())
                .map(|p| p.trim().to_lowercase().split_whitespace().map(str::to_string).collect())
                .unwrap_or_default();
            if !purposes.is_empty() && purposes.iter().all(|p| p == "monochrome") {
                return None;
            }
            let sizes = icon.get("sizes").and_then(|s| s.as_str());
            let kind = icon.get("type").and_then(|t| t.as_str());
            Some(ManifestIcon { src: src.to_string(), size: largest_size(sizes, src, kind) })
        })
        .collect()
}

/// The largest edge an icon claims, from its `sizes` or — for a vector — its type.
fn largest_size(sizes: Option<&str>, href: &str, kind: Option<&str>) -> u32 {
    let lower_href = href.to_lowercase();
    let svg = lower_href.ends_with(".svg") || lower_href.contains(".svg?") || lower_href.contains(".svg#");
    if kind == Some("image/svg+xml") || svg || sizes.is_some_and(|s| s.to_lowercase().contains("any")) {
        return LARGEST;
    }

    static SIZE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let size = SIZE.get_or_init(|| regex::Regex::new(r"(?i)(\d+)x(\d+)").expect("regex"));

    let mut largest = 0;
    for captures in size.captures_iter(sizes.unwrap_or("")) {
        for group in [1, 2] {
            largest = largest.max(captures[group].parse::<u32>().unwrap_or(u32::MAX));
        }
    }
    largest
}

/// Scrape the preview metadata out of an HTML document: Open Graph first,
/// then Twitter's tags, then the plain document title and description.
pub fn extract_meta(html: &str) -> RawMeta {
    let document = scraper::Html::parse_document(html);
    let mut meta = RawMeta::default();

    let mut tags: indexmap::IndexMap<String, String> = indexmap::IndexMap::new();
    let meta_selector = scraper::Selector::parse("meta").expect("selector");
    for element in document.select(&meta_selector) {
        let element = element.value();
        let name = element
            .attr("property")
            .filter(|p| !p.is_empty())
            .or_else(|| element.attr("name"))
            .unwrap_or("")
            .to_lowercase();
        let content = element.attr("content").unwrap_or("");
        // First tag of a kind wins, matching how og:image is repeated for sizes.
        if !name.is_empty() && !content.is_empty() && !tags.contains_key(&name) {
            tags.insert(name, content.to_string());
        }
    }

    let title_selector = scraper::Selector::parse("title").expect("selector");
    let document_title =
        document.select(&title_selector).next().map(|title| title.text().collect::<String>()).filter(|t| !t.is_empty());

    let title = tags.get("og:title").or(tags.get("twitter:title")).cloned().or(document_title);
    let description = tags.get("og:description").or(tags.get("twitter:description")).or(tags.get("description")).cloned();
    let image = tags.get("og:image").or(tags.get("og:image:url")).or(tags.get("twitter:image")).cloned();

    meta.title = title.filter(|t| !t.is_empty()).map(|t| clamp(&t, MAX_TITLE));
    meta.description = description.map(|d| clamp(&d, MAX_DESCRIPTION));
    meta.image = image;

    let link_selector = scraper::Selector::parse("link").expect("selector");
    for element in document.select(&link_selector) {
        let element = element.value();
        let rel: Vec<String> = element.attr("rel").unwrap_or("").to_lowercase().split_whitespace().map(str::to_string).collect();
        let Some(href) = element.attr("href").filter(|h| !h.is_empty()) else {
            continue;
        };

        let apple = rel.iter().any(|token| token.starts_with("apple-touch-icon"));
        if rel.iter().any(|token| token == "manifest") {
            meta.manifest.get_or_insert_with(|| href.to_string());
        } else if rel.iter().any(|token| token == "icon") || apple {
            meta.icons.push(IconLink {
                href: href.to_string(),
                apple,
                size: largest_size(element.attr("sizes"), href, element.attr("type")),
            });
        }
    }

    meta
}

/// Trim, and cut to length on a word boundary where one is close enough.
fn clamp(value: &str, max: usize) -> String {
    let text = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.chars().count() <= max {
        return text;
    }

    let cut: String = text.chars().take(max).collect();
    let kept = match cut.rfind(' ') {
        Some(space) if cut[..space].chars().count() as f64 > max as f64 * 0.6 => &cut[..space],
        _ => cut.as_str(),
    };
    format!("{}…", kept.trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_meta() {
        let meta = extract_meta(
            r#"<html><head><title>Doc &amp; Title</title>
            <meta property="og:title" content="OG Title"><meta property="og:title" content="Second">
            <meta name="description" content="  A   site. ">
            <meta property="og:image" content="/cover.png">
            <link rel="icon" href="/favicon.ico" sizes="32x32">
            <link rel="apple-touch-icon" href="/touch.png" sizes="180x180">
            <link rel="manifest" href="/site.webmanifest">
            </head></html>"#,
        );
        assert_eq!(meta.title.as_deref(), Some("OG Title"));
        assert_eq!(meta.description.as_deref(), Some("A site."));
        assert_eq!(meta.image.as_deref(), Some("/cover.png"));
        assert_eq!(meta.manifest.as_deref(), Some("/site.webmanifest"));
        assert_eq!(meta.icons.len(), 2);
        assert!(meta.icons[1].apple);
        assert_eq!(meta.icons[1].size, 180);

        let bare = extract_meta("<title>Only &amp; title</title>");
        assert_eq!(bare.title.as_deref(), Some("Only & title"));
    }

    #[test]
    fn manifest_icons() {
        let icons = extract_manifest_icons(
            r#"{"icons":[{"src":"/a.png","sizes":"192x192 512x512"},{"src":"/m.png","purpose":"monochrome"},{"src":"/v.svg"},{"src":" "}]}"#,
        );
        assert_eq!(icons, vec![ManifestIcon { src: "/a.png".into(), size: 512 }, ManifestIcon { src: "/v.svg".into(), size: 1024 }]);
    }

    #[test]
    fn resolves_images() {
        let origin = url::Url::parse("https://docs.example.com/").unwrap();
        assert_eq!(resolve_image(Some("/c.png"), &origin).as_deref(), Some("https://docs.example.com/c.png"));
        assert_eq!(resolve_image(Some("http://x.com/c.png"), &origin), None);
        assert_eq!(resolve_image(None, &origin), None);
    }

    #[test]
    fn clamps() {
        assert_eq!(clamp("word ".repeat(100).as_str(), 12), "word word…");
        assert_eq!(clamp(&"x".repeat(20), 10), format!("{}…", "x".repeat(10)));
    }
}
