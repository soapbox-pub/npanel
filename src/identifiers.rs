//! The small parsers the settings, the API and the dashboard share: relay
//! URLs, pubkeys, and NIP-05 names.
//!
//! Each has a TypeScript twin under `dashboard/src/shared/`, so a value a form
//! accepts is a value the server will store. Change both together.

use crate::nostr::nip19::{self, Decoded};

/// Accept a `wss://` URL; always return it normalized.
///
/// Plain `ws://` is refused everywhere. Anything on the path can read or
/// rewrite an unencrypted relay connection — which manifests npanel sees, which
/// relays a NIP-05 name points clients at — and no relay worth naming lacks TLS.
///
/// The trailing slash a URL gains on a bare origin is dropped, because a relay
/// is named by its origin and `wss://relay.example.com/` is the same relay as
/// `wss://relay.example.com`. A path that isn't just `/` is kept.
pub fn parse_relay_url(input: &str) -> Result<String, String> {
    let value = input.trim();
    let url = url::Url::parse(value).map_err(|_| format!("Not a valid relay URL: {value}"))?;

    if url.scheme() != "wss" {
        return Err(format!("A relay URL has to start with wss://: {value}"));
    }

    let serialized = url.to_string();
    Ok(serialized.strip_suffix('/').map(str::to_string).unwrap_or(serialized))
}

/// Accept an npub, nprofile, or hex pubkey; always return hex.
pub fn parse_pubkey(input: &str) -> Result<String, String> {
    let trimmed = input.trim();
    let value = if trimmed.len() >= 6 && trimmed[..6].eq_ignore_ascii_case("nostr:") { &trimmed[6..] } else { trimmed };

    if value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Ok(value.to_lowercase());
    }

    match nip19::decode(value) {
        Ok(Decoded::Npub(pubkey)) | Ok(Decoded::Nprofile { pubkey, .. }) => Ok(pubkey),
        _ => Err(format!("Not a valid pubkey: {trimmed}")),
    }
}

/// The one path NIP-05 documents are served at.
pub const NIP05_PATH: &str = "/.well-known/nostr.json";

/// Longest name npanel will store. The NIP sets no limit; this keeps a name
/// something a person types in front of an `@`.
const MAX_NAME_LENGTH: usize = 64;

/// Accept a NIP-05 local part; always return it lowercased.
///
/// The NIP restricts a local part to `a-z0-9-_.`, so anything else is refused
/// rather than escaped: a name outside that set is one no client will look up.
pub fn parse_nip05_name(input: &str) -> Result<String, String> {
    let name = input.trim().to_lowercase();

    if name.is_empty() {
        return Err("Enter a name for this identifier.".into());
    }
    if name.chars().count() > MAX_NAME_LENGTH {
        return Err(format!("A name can be at most {MAX_NAME_LENGTH} characters."));
    }
    if !name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'-' | b'_' | b'.')) {
        return Err(format!("A name may only use letters, digits, and - _ . — {name} has something else."));
    }

    Ok(name)
}

/// Read a stored relay list. A row that isn't a list of strings is corruption,
/// and a name that resolves with no relays beats a lookup that fails.
pub fn parse_relay_list(json: &str) -> Vec<String> {
    match serde_json::from_str::<serde_json::Value>(json) {
        Ok(serde_json::Value::Array(items)) => {
            items.into_iter().filter_map(|item| item.as_str().map(str::to_string)).collect()
        }
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relay_urls() {
        assert_eq!(parse_relay_url(" wss://Relay.Example.com/ ").unwrap(), "wss://relay.example.com");
        assert_eq!(parse_relay_url("wss://relay.example.com/sub/").unwrap(), "wss://relay.example.com/sub");
        assert!(parse_relay_url("ws://relay.example.com").unwrap_err().contains("wss://"));
        assert!(parse_relay_url("relay").unwrap_err().starts_with("Not a valid relay URL"));
    }

    #[test]
    fn pubkeys() {
        let hex = "3bf0c63fcb93463407af97a5e5ee64fa883d107ef9e558472c4eb9aaaefa459d";
        assert_eq!(parse_pubkey(&hex.to_uppercase()).unwrap(), hex);
        assert_eq!(parse_pubkey(&format!("nostr:{}", nip19::npub_encode(hex))).unwrap(), hex);
        assert!(parse_pubkey("nope").is_err());
    }

    #[test]
    fn nip05_names() {
        assert_eq!(parse_nip05_name(" Bob ").unwrap(), "bob");
        assert_eq!(parse_nip05_name("_").unwrap(), "_");
        assert!(parse_nip05_name("").is_err());
        assert!(parse_nip05_name("bob smith").is_err());
        assert!(parse_nip05_name(&"a".repeat(65)).is_err());
    }
}
