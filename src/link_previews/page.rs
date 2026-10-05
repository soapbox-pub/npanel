//! What npanel reads from a page's path itself, whatever the site's script
//! makes of it: the relays its NIP-19 segments say to look on, and whether it
//! holds a secret key, which no script is run for.

use crate::nostr::nip19::{self, Decoded};

/// The longest path segment worth decoding. An nevent with a few relay hints
/// is a few hundred characters; NIP-19 itself allows 5000.
const MAX_SEGMENT: usize = 2000;

/// What a page's path says, for its script's run.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Hints {
    /// The relays its NIP-19 segments say to look on.
    pub relays: Vec<String>,
}

impl Hints {
    /// What `path` says, or `None` for a path with an nsec in it: that isn't
    /// anything to preview, and running a script for it would put a key in a
    /// cache.
    pub fn of(path: &str) -> Option<Hints> {
        let mut relays = Vec::new();
        for segment in path.split('/').filter(|s| !s.is_empty()) {
            let Some(segment) = crate::util::decode_uri_component(segment) else { continue };
            let segment = segment.strip_prefix('@').unwrap_or(&segment);
            if segment.len() > MAX_SEGMENT || !segment.get(..segment.find('1').unwrap_or(0)).is_some_and(|prefix| prefix.starts_with('n')) {
                continue;
            }
            match nip19::decode(segment) {
                Ok(Decoded::Nsec(_)) => return None,
                Ok(Decoded::Nprofile { relays: found, .. } | Decoded::Nevent { relays: found, .. } | Decoded::Naddr { relays: found, .. }) => relays.extend(found),
                _ => {}
            }
        }
        Some(Hints { relays })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PUBKEY: &str = "3bf0c63fcb93463407af97a5e5ee64fa883d107ef9e558472c4eb9aaaefa459d";

    #[test]
    fn reads_paths() {
        let none = Some(Hints::default());
        assert_eq!(Hints::of(&format!("/{}", nip19::npub_encode(PUBKEY))), none);
        assert_eq!(Hints::of("/"), none);
        assert_eq!(Hints::of("/about/%ZZ"), none);
        assert_eq!(Hints::of("/@alex@example.com"), none);

        let id = "b".repeat(64);
        let relay = vec!["wss://r.example.com".to_string()];
        let nevent = nip19::nevent_encode(&id, &relay, Some(PUBKEY), Some(31337));
        assert_eq!(Hints::of(&format!("/{nevent}")), Some(Hints { relays: relay.clone() }));
        assert_eq!(Hints::of(&format!("/e/%40{nevent}/")), Some(Hints { relays: relay }));

        // A secret key anywhere in the path is never previewed.
        assert_eq!(Hints::of(&format!("/{}", nip19::nsec_encode(PUBKEY))), None);
        assert_eq!(Hints::of(&format!("/login/{}", nip19::nsec_encode(PUBKEY))), None);
    }
}
