//! Manifest events: which site one belongs to, and what it lists.

use indexmap::IndexMap;

use crate::nostr::Event;
use crate::nsite_address::{NsiteAddress, NAMED_SITE_KIND, SITE_KINDS};

/// Whether an event is an nsite manifest.
pub fn is_site_manifest(event: &Event) -> bool {
    SITE_KINDS.contains(&event.kind)
}

/// The address a manifest event belongs to.
pub fn manifest_address(event: &Event) -> NsiteAddress {
    NsiteAddress {
        pubkey: event.pubkey.clone(),
        kind: event.kind,
        identifier: if event.kind == NAMED_SITE_KIND { event.tag("d").unwrap_or("").to_string() } else { String::new() },
    }
}

/// The file map of a manifest: absolute site path to blob hash.
///
/// Tags whose hash isn't a sha256 are dropped rather than failing the whole
/// deploy — a single malformed tag shouldn't take a site offline.
pub fn manifest_paths(event: &Event) -> IndexMap<String, String> {
    let mut paths = IndexMap::new();

    for tag in &event.tags {
        if tag.first().map(String::as_str) != Some("path") {
            continue;
        }
        let (Some(path), Some(hash)) = (tag.get(1), tag.get(2)) else {
            continue;
        };
        if path.is_empty() || hash.is_empty() {
            continue;
        }
        let sha256 = hash.to_lowercase();
        if !crate::nostr::is_hex64(&sha256) {
            continue;
        }
        // A later tag for the same path wins, and keeps the first one's place.
        paths.insert(join_root(path), sha256);
    }

    paths
}

/// Blossom servers embedded in a manifest, tried before the author's list.
pub fn manifest_servers(event: &Event) -> Vec<String> {
    tag_values(event, "server")
}

/// Blossom servers from a kind 10063 list (BUD-03).
pub fn parse_server_list(event: &Event) -> Vec<String> {
    tag_values(event, "server")
}

/// Relays from a kind 10002 list, split by read/write markers.
///
/// npanel only ever reads, but write relays are where a user's own events
/// land, which is exactly where their manifests will be. Both are returned.
pub fn parse_relay_list(event: &Event) -> (Vec<String>, Vec<String>) {
    let mut read = Vec::new();
    let mut write = Vec::new();

    for tag in &event.tags {
        if tag.first().map(String::as_str) != Some("r") {
            continue;
        }
        let Some(url) = tag.get(1).filter(|url| !url.is_empty()) else {
            continue;
        };
        match tag.get(2).map(String::as_str) {
            Some("read") => read.push(url.clone()),
            Some("write") => write.push(url.clone()),
            _ => {
                read.push(url.clone());
                write.push(url.clone());
            }
        }
    }

    (read, write)
}

fn tag_values(event: &Event, name: &str) -> Vec<String> {
    event
        .tags
        .iter()
        .filter(|tag| tag.first().map(String::as_str) == Some(name))
        .filter_map(|tag| tag.get(1).filter(|value| !value.is_empty()).cloned())
        .collect()
}

/// `path.posix.join("/", path)`: rooted, with `.`, `..` and repeated slashes
/// resolved, and a trailing slash kept.
pub fn join_root(path: &str) -> String {
    let mut segments: Vec<&str> = Vec::new();
    for segment in path.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                segments.pop();
            }
            other => segments.push(other),
        }
    }

    let mut joined = format!("/{}", segments.join("/"));
    if path.ends_with('/') && joined != "/" {
        joined.push('/');
    }
    joined
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joins_like_posix() {
        assert_eq!(join_root("index.html"), "/index.html");
        assert_eq!(join_root("/a//b/./c.html"), "/a/b/c.html");
        assert_eq!(join_root("/../../etc/passwd"), "/etc/passwd");
        assert_eq!(join_root("/docs/"), "/docs/");
        assert_eq!(join_root(""), "/");
        assert_eq!(join_root("/a/.."), "/");
    }

    #[test]
    fn reads_paths() {
        let sha = "a".repeat(64);
        let event = Event {
            id: String::new(),
            pubkey: String::new(),
            created_at: 0,
            kind: 15128,
            tags: vec![
                vec!["path".into(), "/index.html".into(), sha.to_uppercase()],
                vec!["path".into(), "/bad".into(), "nothex".into()],
                vec!["path".into(), "about.html".into(), sha.clone()],
                vec!["server".into(), "https://b.example.com".into()],
            ],
            content: String::new(),
            sig: String::new(),
        };
        let paths = manifest_paths(&event);
        assert_eq!(paths.len(), 2);
        assert_eq!(paths["/index.html"], sha);
        assert!(paths.contains_key("/about.html"));
        assert_eq!(manifest_servers(&event), ["https://b.example.com"]);
    }
}
