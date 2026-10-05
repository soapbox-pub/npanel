//! Which pages are about something on Nostr: those whose path is one segment
//! naming a profile, an event or an address — the way Nostr web clients
//! route them — or a NIP-05 name. Only those run a site's preview script.

use crate::hostname::{is_hostname, normalize_hostname};
use crate::identifiers::parse_nip05_name;
use crate::nostr::nip19::{self, Decoded};

/// The longest path segment worth decoding. An nevent with a few relay hints
/// is a few hundred characters; NIP-19 itself allows 5000.
const MAX_SEGMENT: usize = 2000;

/// A page about something on Nostr.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NostrPage {
    /// The relays its NIP-19 form says to look on.
    pub relays: Vec<String>,
}

impl NostrPage {
    /// The page a path names: `/<nip19>` or `/<name@domain>`, either with an
    /// `@` in front, and nothing after but a trailing slash. Anything else
    /// names nothing, so the page is served as published. An nsec names
    /// nothing either: it isn't anything to preview, and decoding one would
    /// put a key in a cache.
    pub fn from_path(path: &str) -> Option<NostrPage> {
        let segment = path.strip_prefix('/')?;
        let segment = segment.strip_suffix('/').unwrap_or(segment);
        if segment.is_empty() || segment.len() > MAX_SEGMENT * 3 || segment.contains('/') {
            return None;
        }
        let segment = crate::util::decode_uri_component(segment)?;
        let segment = segment.strip_prefix('@').unwrap_or(&segment);
        if segment.len() > MAX_SEGMENT {
            return None;
        }

        if let Some((name, domain)) = segment.rsplit_once('@') {
            parse_nip05_name(name).ok()?;
            return is_hostname(&normalize_hostname(domain)).then_some(NostrPage { relays: Vec::new() });
        }

        let prefix = segment.get(..segment.find('1').unwrap_or(0)).unwrap_or("").to_ascii_lowercase();
        if !matches!(prefix.as_str(), "npub" | "nprofile" | "note" | "nevent" | "naddr") {
            return None;
        }
        let relays = match nip19::decode(segment).ok()? {
            Decoded::Nprofile { relays, .. } | Decoded::Nevent { relays, .. } | Decoded::Naddr { relays, .. } => relays,
            Decoded::Npub(_) | Decoded::Note(_) => Vec::new(),
            Decoded::Nsec(_) => return None,
        };
        Some(NostrPage { relays })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PUBKEY: &str = "3bf0c63fcb93463407af97a5e5ee64fa883d107ef9e558472c4eb9aaaefa459d";

    #[test]
    fn reads_paths() {
        let npub = nip19::npub_encode(PUBKEY);
        let none = Some(NostrPage { relays: vec![] });
        assert_eq!(NostrPage::from_path(&format!("/{npub}")), none);
        assert_eq!(NostrPage::from_path(&format!("/@{npub}/")), none);
        assert_eq!(NostrPage::from_path(&format!("/%40{npub}")), none);
        assert_eq!(NostrPage::from_path(&format!("/{npub}/replies")), None);
        assert_eq!(NostrPage::from_path("/"), None);
        assert_eq!(NostrPage::from_path("/about"), None);
        assert_eq!(NostrPage::from_path(&format!("/{}", nip19::nsec_encode(PUBKEY))), None);

        let id = "b".repeat(64);
        let nevent = nip19::nevent_encode(&id, &["wss://r.example.com".into()], Some(PUBKEY), Some(31337));
        assert_eq!(NostrPage::from_path(&format!("/{nevent}")), Some(NostrPage { relays: vec!["wss://r.example.com".into()] }));
        assert_eq!(NostrPage::from_path(&format!("/{}", nip19::note_encode(&id))), none);
        assert_eq!(NostrPage::from_path(&format!("/{}", nip19::naddr_encode(PUBKEY, 30023, "my-post", &[]))), none);

        assert_eq!(NostrPage::from_path("/@Alex@Example.com"), none);
        assert_eq!(NostrPage::from_path("/alex@localhost"), None);
        assert_eq!(NostrPage::from_path("/al ex@example.com"), None);
    }
}
