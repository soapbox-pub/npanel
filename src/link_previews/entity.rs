//! What a link preview is of — a profile, an event, or an address — named by
//! the path a crawler asked for, or by whatever an admin pasted.

use super::kinds;
use crate::hostname::{is_hostname, normalize_hostname};
use crate::identifiers::parse_nip05_name;
use crate::nostr::nip19::{self, Decoded};

/// The longest path segment worth decoding. An nevent with a few relay hints
/// is a few hundred characters; NIP-19 itself allows 5000.
const MAX_SEGMENT: usize = 2000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Entity {
    /// An npub or nprofile.
    Profile { pubkey: String, relays: Vec<String> },
    /// A note or nevent.
    Event { id: String, author: Option<String>, relays: Vec<String> },
    /// An naddr.
    Address { kind: u32, pubkey: String, identifier: String, relays: Vec<String> },
    /// A NIP-05 name, which stands for a profile once it is looked up.
    Nip05 { name: String, domain: String },
}

impl Entity {
    /// What it is cached under: everything that names the same thing, and
    /// nothing else. Relay hints are where to look, not what is looked for.
    pub fn key(&self) -> String {
        match self {
            Entity::Profile { pubkey, .. } => format!("p:{pubkey}"),
            Entity::Event { id, .. } => format!("e:{id}"),
            Entity::Address { kind, pubkey, identifier, .. } => format!("a:{kind}:{pubkey}:{identifier}"),
            Entity::Nip05 { name, domain } => format!("n:{name}@{domain}"),
        }
    }

    /// The entity a page's path names, the way Nostr web clients route them:
    /// `/<nip19>` or `/<name@domain>`, either with an `@` in front, and
    /// nothing after but a trailing slash. Anything else names nothing, so
    /// the page is served as published — as does one that names its event's
    /// kind, when that kind isn't previewed, which costs no lookup at all.
    pub fn from_path(path: &str) -> Option<Entity> {
        let segment = path.strip_prefix('/')?;
        let segment = segment.strip_suffix('/').unwrap_or(segment);
        if segment.is_empty() || segment.len() > MAX_SEGMENT * 3 || segment.contains('/') {
            return None;
        }
        let segment = crate::util::decode_uri_component(segment)?;
        from_segment(&segment).ok()
    }

    /// The entity whatever an admin pasted names: anything [`Entity::from_path`]
    /// accepts, a `nostr:` URI, or the URL of a page that names one.
    pub fn parse(input: &str) -> Result<Entity, String> {
        let value = input.trim();
        if value.is_empty() {
            return Err("Paste an npub, a note, an naddr, a NIP-05 name, or the URL of a page that names one.".into());
        }

        let is_url = ["https://", "http://"].iter().any(|scheme| crate::util::strip_prefix_ci(value, scheme).len() != value.len());
        if is_url {
            let url = url::Url::parse(value).map_err(|_| format!("Not a valid URL: {value}"))?;
            let last = url.path_segments().and_then(|mut segments| segments.rfind(|s| !s.is_empty())).unwrap_or("");
            let segment = crate::util::decode_uri_component(last).unwrap_or_else(|| last.to_string());
            return from_segment(&segment).map_err(|error| match error {
                SegmentError::Unsupported(kind) => unsupported(kind),
                _ => format!("{value} doesn't end in an npub, a note, an naddr, or a NIP-05 name."),
            });
        }

        let value = crate::util::strip_prefix_ci(value, "web+nostr:");
        let value = crate::util::strip_prefix_ci(value, "nostr:");
        if value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err("A hex key could be a pubkey or an event id. Paste it as an npub or a note instead.".into());
        }
        from_segment(value).map_err(|error| match error {
            SegmentError::Secret => "That's a private key. Never paste an nsec anywhere.".into(),
            SegmentError::Unsupported(kind) => unsupported(kind),
            SegmentError::Unknown => format!("Not an npub, a note, an naddr, or a NIP-05 name: {value}"),
        })
    }

    /// How a person would write it: its NIP-19 form, or the NIP-05 name.
    pub fn label(&self) -> String {
        match self {
            Entity::Profile { pubkey, .. } => nip19::npub_encode(pubkey),
            Entity::Event { id, .. } => nip19::note_encode(id),
            Entity::Address { kind, pubkey, identifier, .. } => nip19::naddr_encode(pubkey, *kind, identifier, &[]),
            Entity::Nip05 { name, domain } => format!("{name}@{domain}"),
        }
    }
}

enum SegmentError {
    Secret,
    /// An naddr, or an nevent that says its kind, of a kind with no preview.
    Unsupported(u32),
    Unknown,
}

fn unsupported(kind: u32) -> String {
    format!("Kind {kind} events have no link preview, so crawlers get the page as published. There's nothing to refresh.")
}

fn from_segment(segment: &str) -> Result<Entity, SegmentError> {
    let segment = segment.strip_prefix('@').unwrap_or(segment);
    if segment.len() > MAX_SEGMENT {
        return Err(SegmentError::Unknown);
    }

    if let Some((name, domain)) = segment.rsplit_once('@') {
        let domain = normalize_hostname(domain);
        let name = parse_nip05_name(name).map_err(|_| SegmentError::Unknown)?;
        if !is_hostname(&domain) {
            return Err(SegmentError::Unknown);
        }
        return Ok(Entity::Nip05 { name, domain });
    }

    // Only the prefixes that name something public: decoding an nsec would
    // put a key in a cache, and it isn't anything to preview.
    let prefix = segment.get(..segment.find('1').unwrap_or(0)).unwrap_or("").to_ascii_lowercase();
    if prefix == "nsec" {
        return Err(SegmentError::Secret);
    }
    if !matches!(prefix.as_str(), "npub" | "nprofile" | "note" | "nevent" | "naddr") {
        return Err(SegmentError::Unknown);
    }
    match nip19::decode(segment).map_err(|_| SegmentError::Unknown)? {
        Decoded::Npub(pubkey) => Ok(Entity::Profile { pubkey, relays: Vec::new() }),
        Decoded::Nprofile { pubkey, relays } => Ok(Entity::Profile { pubkey, relays }),
        Decoded::Note(id) => Ok(Entity::Event { id, author: None, relays: Vec::new() }),
        Decoded::Nevent { kind: Some(kind), .. } | Decoded::Naddr { kind, .. } if !kinds::supported(i64::from(kind)) => {
            Err(SegmentError::Unsupported(kind))
        }
        Decoded::Nevent { id, relays, author, .. } => Ok(Entity::Event { id, author, relays }),
        Decoded::Naddr { pubkey, kind, identifier, relays } => Ok(Entity::Address { kind, pubkey, identifier, relays }),
        Decoded::Nsec(_) => Err(SegmentError::Secret),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PUBKEY: &str = "3bf0c63fcb93463407af97a5e5ee64fa883d107ef9e558472c4eb9aaaefa459d";

    #[test]
    fn reads_paths() {
        let npub = nip19::npub_encode(PUBKEY);
        let profile = Entity::Profile { pubkey: PUBKEY.into(), relays: vec![] };
        assert_eq!(Entity::from_path(&format!("/{npub}")), Some(profile.clone()));
        assert_eq!(Entity::from_path(&format!("/@{npub}/")), Some(profile.clone()));
        assert_eq!(Entity::from_path(&format!("/%40{npub}")), Some(profile));
        assert_eq!(Entity::from_path(&format!("/{npub}/replies")), None);
        assert_eq!(Entity::from_path("/"), None);
        assert_eq!(Entity::from_path("/about"), None);
        assert_eq!(Entity::from_path(&format!("/{}", nip19::nsec_encode(PUBKEY))), None);

        let id = "b".repeat(64);
        let nevent = nip19::nevent_encode(&id, &["wss://r.example.com".into()], Some(PUBKEY), None);
        assert_eq!(
            Entity::from_path(&format!("/{nevent}")),
            Some(Entity::Event { id: id.clone(), author: Some(PUBKEY.into()), relays: vec!["wss://r.example.com".into()] })
        );
        assert_eq!(Entity::from_path(&format!("/{}", nip19::note_encode(&id))).unwrap().key(), format!("e:{id}"));

        let naddr = nip19::naddr_encode(PUBKEY, 30023, "my-post", &[]);
        assert_eq!(Entity::from_path(&format!("/{naddr}")).unwrap().key(), format!("a:30023:{PUBKEY}:my-post"));

        // A kind with no preview names nothing, when the path says the kind.
        assert_eq!(Entity::from_path(&format!("/{}", nip19::naddr_encode(PUBKEY, 31337, "x", &[]))), None);
        assert_eq!(Entity::from_path(&format!("/{}", nip19::nevent_encode(&id, &[], None, Some(31337)))), None);
        assert!(Entity::from_path(&format!("/{}", nip19::nevent_encode(&id, &[], None, Some(1)))).is_some());

        assert_eq!(Entity::from_path("/@Alex@Example.com"), Some(Entity::Nip05 { name: "alex".into(), domain: "example.com".into() }));
        assert_eq!(Entity::from_path("/alex@localhost"), None);
        assert_eq!(Entity::from_path("/al ex@example.com"), None);
    }

    #[test]
    fn reads_what_an_admin_pastes() {
        let npub = nip19::npub_encode(PUBKEY);
        let key = format!("p:{PUBKEY}");
        assert_eq!(Entity::parse(&format!("  nostr:{npub} ")).unwrap().key(), key);
        assert_eq!(Entity::parse(&format!("https://ditto.pub/{npub}")).unwrap().key(), key);
        assert_eq!(Entity::parse(&format!("https://ditto.pub/@{npub}/?x=1")).unwrap().key(), key);
        assert_eq!(Entity::parse("bob@example.com").unwrap().key(), "n:bob@example.com");
        assert!(Entity::parse(&nip19::nsec_encode(PUBKEY)).unwrap_err().contains("private key"));
        assert!(Entity::parse(PUBKEY).unwrap_err().contains("npub"));
        assert!(Entity::parse("https://ditto.pub/feed").unwrap_err().contains("doesn't end in"));
        let naddr = nip19::naddr_encode(PUBKEY, 31337, "x", &[]);
        assert!(Entity::parse(&naddr).unwrap_err().contains("Kind 31337"));
        assert!(Entity::parse(&format!("https://ditto.pub/{naddr}")).unwrap_err().contains("Kind 31337"));
        assert!(Entity::parse("").is_err());
        assert_eq!(Entity::parse(&npub).unwrap().label(), npub);
    }
}
