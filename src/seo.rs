//! What npanel does for a site's standing in search engines, universally.
//!
//! Three things, all derived from data npanel already holds: a `sitemap.xml`
//! generated from the version's file list, a `robots.txt` that points at it,
//! and a canonical URL on every page. The files are made when a host is built
//! (see `builds.rs`) and are fallbacks — a site that published its own is
//! served as-is. The canonical URL is a header, and is set per request.

use std::collections::BTreeSet;

use rusqlite::Connection;

use crate::manifest::join_root;
use crate::serve::RequestOrigin;
use crate::sites::normalize_site_path;
use crate::util::extname;

/// The protocol's cap on URLs per sitemap file.
const MAX_SITEMAP_URLS: usize = 50_000;

/// The most of a `robots.txt` npanel will read before serving it as it is.
pub const MAX_ROBOTS_BYTES: u64 = 64 * 1024;

fn is_html(path: &str) -> bool {
    matches!(extname(path).to_lowercase().as_str(), ".html" | ".htm")
}

/// The one canonical spelling of a served file's URL path.
///
/// Extensions come off: `/foo`, `/foo/`, `/foo.html` and `/foo/index.html`
/// can all serve the same bytes here, and a crawler scores them as competing
/// pages. The clean form is the one static site generators link to, and it is
/// always servable: resolution tries a `.html` sibling for exactly this
/// reason. Only `.html`, and not `.htm`, since only the former has that step.
pub fn canonical_path(file_path: &str) -> &str {
    if let Some(dir) = file_path.strip_suffix("index.html").filter(|dir| dir.ends_with('/')) {
        return dir;
    }
    file_path.strip_suffix(".html").unwrap_or(file_path)
}

/// The `Link: rel="canonical"` value for a response, or `None` when the
/// response shouldn't carry one.
///
/// Only a real page gets it: a 200, for a document, reached under one of the
/// spellings that actually names it. That last condition keeps the header off
/// SPA-mode fallbacks, where a canonical would tell crawlers the whole app is
/// one page.
pub fn canonical_link(origin: &RequestOrigin, request_path: &str, file_path: &str, status: u16) -> Option<String> {
    if status != 200 || !is_html(file_path) {
        return None;
    }

    let normalized = normalize_site_path(request_path);
    let spells_file = file_path == normalized
        || file_path == format!("{normalized}.html")
        || file_path == join_root(&format!("{normalized}/index.html"));
    if !spells_file {
        return None;
    }

    Some(format!("<{}>; rel=\"canonical\"", absolute_url(origin, canonical_path(file_path))))
}

/// Every canonical page URL of a version, for its sitemap.
pub fn sitemap_paths(conn: &Connection, event_id: &str) -> rusqlite::Result<Vec<String>> {
    let paths: Vec<String> = conn
        .prepare_cached("SELECT path FROM site_files WHERE event_id = ?")?
        .query_map([event_id], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;

    let mut canonical = BTreeSet::new();
    for path in &paths {
        // The not-found page is the one document a crawler must not be invited to.
        if path == "/404.html" || !is_html(path) {
            continue;
        }
        canonical.insert(canonical_path(path).to_string());
    }

    // JavaScript's default sort compares UTF-16 code units, which for the
    // characters a path holds orders the same as bytes.
    let mut sorted: Vec<String> = canonical.into_iter().collect();
    sorted.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
    sorted.truncate(MAX_SITEMAP_URLS);
    Ok(sorted)
}

/// Render a sitemap for the site as reached at `origin`.
pub fn sitemap_xml(origin: &RequestOrigin, paths: &[String], lastmod: &str) -> String {
    let mut lines = vec![
        r#"<?xml version="1.0" encoding="UTF-8"?>"#.to_string(),
        r#"<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">"#.to_string(),
    ];
    for path in paths {
        lines.push(format!(
            "<url><loc>{}</loc><lastmod>{lastmod}</lastmod></url>",
            escape_xml(&absolute_url(origin, path))
        ));
    }
    lines.push("</urlset>".into());
    lines.push(String::new());
    lines.join("\n")
}

/// `<lastmod>` for a version: the manifest's own date, capped at when npanel
/// saw it, since an author can date a manifest however they please.
pub fn version_date(created_at: i64, fetched_at: i64) -> String {
    let seconds = created_at.min(fetched_at).max(0);
    chrono::DateTime::from_timestamp(seconds, 0).unwrap_or_default().format("%Y-%m-%d").to_string()
}

/// The `robots.txt` a site gets when it published none: allow all, name the
/// sitemap.
pub fn robots_txt(origin: &RequestOrigin) -> String {
    format!("User-agent: *\nAllow: /\n\nSitemap: {}\n", absolute_url(origin, "/sitemap.xml"))
}

/// Whether npanel should append its `Sitemap:` line to a site's own
/// `robots.txt`: not when the file already names a sitemap, whichever file
/// that is, and not when it shuts a crawler out of the whole site.
pub fn robots_needs_sitemap(text: &str) -> bool {
    !text.lines().any(|line| {
        let line = line.trim_start_matches([' ', '\t']).to_lowercase();
        let names_sitemap = line
            .strip_prefix("sitemap")
            .is_some_and(|rest| rest.trim_start_matches([' ', '\t']).starts_with(':'));
        let disallows_all = line.strip_prefix("disallow").is_some_and(|rest| {
            rest.trim_start_matches([' ', '\t'])
                .strip_prefix(':')
                .is_some_and(|value| value.trim_matches([' ', '\t']) == "/")
        });
        names_sitemap || disallows_all
    })
}

/// A site's own `robots.txt` with npanel's `Sitemap:` line appended.
///
/// `Sitemap` is a non-group directive: it grants no crawler anything, and
/// appending one cannot change what an `Allow` or `Disallow` above it means.
/// The original is copied through verbatim, so the only difference is the
/// line at the end.
pub fn augment_robots(text: &str, origin: &RequestOrigin) -> String {
    let sitemap = format!("Sitemap: {}\n", absolute_url(origin, "/sitemap.xml"));

    // `\s*$` as JavaScript reads `\s`: ASCII and Unicode whitespace alike.
    let body = text.trim_end_matches(|c: char| c.is_whitespace() || c == '\u{feff}');
    if body.is_empty() {
        return sitemap;
    }

    // A blank line before, so the directive can't be read as part of the last
    // group.
    format!("{body}\n\n{sitemap}")
}

/// A site path as an absolute URL at `origin`, through a URL parser rather
/// than concatenation, because stored paths are decoded and a sitemap or
/// header needs them encoded again.
pub fn absolute_url(origin: &RequestOrigin, path: &str) -> String {
    let base = format!("{}://{}", origin.scheme, origin.hostname);
    match url::Url::parse(&base).and_then(|base| base.join(path)) {
        Ok(url) => url.to_string(),
        Err(_) => format!("{base}{path}"),
    }
}

fn escape_xml(value: &str) -> String {
    value.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn origin() -> RequestOrigin {
        RequestOrigin { hostname: "docs.example.com".into(), scheme: "https" }
    }

    #[test]
    fn canonical_paths() {
        assert_eq!(canonical_path("/index.html"), "/");
        assert_eq!(canonical_path("/guide/index.html"), "/guide/");
        assert_eq!(canonical_path("/hacking.html"), "/hacking");
        assert_eq!(canonical_path("/old.htm"), "/old.htm");
        assert_eq!(canonical_path("/myindex.html"), "/myindex");
    }

    #[test]
    fn canonical_links() {
        let o = origin();
        assert_eq!(
            canonical_link(&o, "/guide", "/guide/index.html", 200).as_deref(),
            Some("<https://docs.example.com/guide/>; rel=\"canonical\"")
        );
        assert_eq!(
            canonical_link(&o, "/hacking.html", "/hacking.html", 200).as_deref(),
            Some("<https://docs.example.com/hacking>; rel=\"canonical\"")
        );
        assert_eq!(canonical_link(&o, "/client/route", "/index.html", 200), None);
        assert_eq!(canonical_link(&o, "/missing", "/404.html", 404), None);
        assert_eq!(canonical_link(&o, "/app.js", "/app.js", 200), None);
        assert_eq!(
            canonical_link(&o, "/caf%C3%A9", "/café.html", 200).as_deref(),
            Some("<https://docs.example.com/caf%C3%A9>; rel=\"canonical\"")
        );
    }

    #[test]
    fn robots() {
        assert!(robots_needs_sitemap("User-agent: *\nAllow: /\n"));
        assert!(!robots_needs_sitemap("User-agent: *\n  SITEMAP : https://x/s.xml\n"));
        assert!(!robots_needs_sitemap("User-agent: *\nDisallow: /\n"));
        assert!(robots_needs_sitemap("User-agent: *\nDisallow: /private\n"));
        assert_eq!(
            augment_robots("User-agent: *\nAllow: /\n\n\n", &origin()),
            "User-agent: *\nAllow: /\n\nSitemap: https://docs.example.com/sitemap.xml\n"
        );
        assert_eq!(augment_robots("  \n", &origin()), "Sitemap: https://docs.example.com/sitemap.xml\n");
    }

    #[test]
    fn sitemaps() {
        let xml = sitemap_xml(&origin(), &["/".into(), "/a&b".into()], "2026-01-02");
        assert!(xml.contains("<url><loc>https://docs.example.com/a&amp;b</loc><lastmod>2026-01-02</lastmod></url>"));
        assert!(xml.ends_with("</urlset>\n"));
        assert_eq!(version_date(1_000_000_000, 2_000_000_000), "2001-09-09");
        assert_eq!(version_date(i64::MAX, 0), "1970-01-01");
    }
}
