//! Site addresses: every form a person might paste, reduced to the one shape
//! npanel stores.
//!
//! The dashboard carries a TypeScript twin of this parser
//! (`dashboard/src/shared/nsite-address.ts`) so the preview shown while typing
//! is what the server will store. Change both together.

use serde::Serialize;

use crate::nostr::nip19::{self, Decoded};

/// Replaceable manifest event for a user's root site.
pub const ROOT_SITE_KIND: i64 = 15128;
/// Addressable manifest event for a named site, identified by its `d` tag.
pub const NAMED_SITE_KIND: i64 = 35128;

pub const SITE_KINDS: [i64; 2] = [ROOT_SITE_KIND, NAMED_SITE_KIND];

const CANONICAL_PUBKEY_LENGTH: usize = 50;

/// A site, decomposed into the only shape npanel stores.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize)]
pub struct NsiteAddress {
    /// Hex pubkey of the author.
    pub pubkey: String,
    pub kind: i64,
    /// `d` tag of a named site; empty string for a root site.
    pub identifier: String,
}

/// Identifiers that fit in a DNS label alongside the base36 pubkey:
/// `^(?=.{1,13}$)[a-z0-9-]*[a-z0-9]$`.
pub fn is_canonical_identifier(value: &str) -> bool {
    (1..=13).contains(&value.len())
        && value.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !value.ends_with('-')
}

/// Parse any user-supplied site address into its canonical form.
///
/// Errors are sentences safe to show in the dashboard.
pub fn parse_nsite_address(input: &str) -> Result<NsiteAddress, String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err("Enter an address.".into());
    }

    let mut value = crate::util::strip_prefix_ci(trimmed, "nostr:");
    value = crate::util::strip_prefix_ci(value, "web+nostr:");
    let mut value = value.to_string();

    // Full URL, eg https://<label>.nsite.lol/some/page
    if is_url_like(&value) {
        let url = url::Url::parse(&value).map_err(|_| format!("Not a valid URL: {trimmed}"))?;
        value = url.host_str().unwrap_or("").to_string();
    }

    // Hostname, eg <label>.nsite.lol — only the first label identifies the site.
    // `a` tag values also contain no dots, so this is unambiguous.
    if value.contains('.') && !value.contains(':') {
        value = value.split('.').next().unwrap_or("").to_string();
    }

    let value = value.trim();

    let lower = value.to_lowercase();
    if lower.len() == 64 && lower.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Ok(NsiteAddress { pubkey: lower, kind: ROOT_SITE_KIND, identifier: String::new() });
    }

    if let Some(address) = parse_a_tag(value) {
        return normalize(address);
    }

    if ["npub1", "nprofile1", "naddr1", "nsec1", "note1", "nevent1"].iter().any(|p| lower.starts_with(p)) {
        return from_nip19(&lower, trimmed);
    }

    if let Some(address) = parse_canonical_site_label(&lower) {
        return Ok(address);
    }

    Err(format!(
        "Unrecognized address: {trimmed}. Use an npub, naddr, {NAMED_SITE_KIND}:<pubkey>:<identifier>, or an nsite URL."
    ))
}

/// `^[a-z][a-z0-9+.-]*://`, case-insensitively.
fn is_url_like(value: &str) -> bool {
    let Some((scheme, _)) = value.split_once("://") else {
        return false;
    };
    let mut bytes = scheme.bytes();
    bytes.next().is_some_and(|b| b.is_ascii_alphabetic())
        && bytes.all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'.' | b'-'))
}

/// `^(\d+):([0-9a-fA-F]{64}):(.*)$`
fn parse_a_tag(value: &str) -> Option<NsiteAddress> {
    let (kind, rest) = value.split_once(':')?;
    if kind.is_empty() || !kind.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let pubkey = rest.get(..64)?;
    if !pubkey.bytes().all(|b| b.is_ascii_hexdigit()) || rest.as_bytes().get(64) != Some(&b':') {
        return None;
    }
    let identifier = &rest[65..];
    if identifier.contains(['\n', '\r']) {
        return None;
    }
    Some(NsiteAddress { kind: kind.parse().ok()?, pubkey: pubkey.to_lowercase(), identifier: identifier.to_string() })
}

fn from_nip19(value: &str, original: &str) -> Result<NsiteAddress, String> {
    let decoded = nip19::decode(value).map_err(|_| format!("Not a valid Nostr identifier: {original}"))?;

    match decoded {
        Decoded::Npub(pubkey) => Ok(NsiteAddress { pubkey, kind: ROOT_SITE_KIND, identifier: String::new() }),
        Decoded::Nprofile { pubkey, .. } => Ok(NsiteAddress { pubkey, kind: ROOT_SITE_KIND, identifier: String::new() }),
        Decoded::Naddr { pubkey, kind, identifier, .. } => normalize(NsiteAddress { pubkey, kind: kind.into(), identifier }),
        Decoded::Nsec(_) => Err("That's a private key. Never paste an nsec anywhere.".into()),
        other => Err(format!("A {} does not point to a site. Use an npub or naddr.", other.type_name())),
    }
}

/// Parse a NIP-5A subdomain label: an npub, or base36 pubkey + identifier.
fn parse_canonical_site_label(label: &str) -> Option<NsiteAddress> {
    if label.starts_with("npub1") {
        return match nip19::decode(label) {
            Ok(Decoded::Npub(pubkey)) => Some(NsiteAddress { pubkey, kind: ROOT_SITE_KIND, identifier: String::new() }),
            _ => None,
        };
    }

    // ^[0-9a-z]{50}[a-z0-9-]{1,13}$, not ending in a hyphen.
    if !(51..=63).contains(&label.len())
        || !label.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        || label[..CANONICAL_PUBKEY_LENGTH].contains('-')
        || label.ends_with('-')
    {
        return None;
    }

    let pubkey = decode_pubkey_b36(&label[..CANONICAL_PUBKEY_LENGTH])?;
    let identifier = &label[CANONICAL_PUBKEY_LENGTH..];
    if !is_canonical_identifier(identifier) {
        return None;
    }

    Some(NsiteAddress { pubkey, kind: NAMED_SITE_KIND, identifier: identifier.to_string() })
}

/// Validate a decomposed address and canonicalize its casing.
fn normalize(address: NsiteAddress) -> Result<NsiteAddress, String> {
    let pubkey = address.pubkey.to_lowercase();

    if !crate::nostr::is_hex64(&pubkey) {
        return Err("Address contains an invalid pubkey.".into());
    }
    if !SITE_KINDS.contains(&address.kind) {
        return Err(format!(
            "Kind {} is not an nsite. Expected {ROOT_SITE_KIND} (root site) or {NAMED_SITE_KIND} (named site).",
            address.kind
        ));
    }
    if address.kind == NAMED_SITE_KIND && address.identifier.is_empty() {
        return Err(format!("Kind {NAMED_SITE_KIND} requires an identifier."));
    }
    if address.kind == ROOT_SITE_KIND && !address.identifier.is_empty() {
        return Err(format!("Kind {ROOT_SITE_KIND} is a root site and cannot have an identifier."));
    }

    Ok(NsiteAddress { pubkey, kind: address.kind, identifier: address.identifier })
}

/// The canonical `a` tag value, eg `35128:<pubkey>:docs`.
pub fn format_nsite_address(address: &NsiteAddress) -> String {
    format!("{}:{}:{}", address.kind, address.pubkey, address.identifier)
}

/// An `naddr` for the address, useful for links and copy buttons.
pub fn encode_naddr(address: &NsiteAddress) -> String {
    nip19::naddr_encode(&address.pubkey, address.kind as u32, &address.identifier, &[])
}

/// Decode a 50-character base36 pubkey label to hex.
pub fn decode_pubkey_b36(label: &str) -> Option<String> {
    if label.len() != CANONICAL_PUBKEY_LENGTH {
        return None;
    }

    // Big-endian 256-bit accumulator, with a spare byte to catch overflow.
    let mut value = [0u8; 33];
    for c in label.bytes() {
        let digit = match c {
            b'0'..=b'9' => c - b'0',
            b'a'..=b'z' => c - b'a' + 10,
            _ => return None,
        };
        let mut carry = u32::from(digit);
        for byte in value.iter_mut().rev() {
            let next = u32::from(*byte) * 36 + carry;
            *byte = next as u8;
            carry = next >> 8;
        }
        if carry != 0 || value[0] != 0 {
            return None;
        }
    }

    Some(hex::encode(&value[1..]))
}

/// Encode a hex pubkey as a 50-character base36 label.
#[cfg(test)]
pub fn encode_pubkey_b36(pubkey: &str) -> Option<String> {
    let bytes = hex::decode(pubkey).ok().filter(|bytes| bytes.len() == 32)?;
    let mut number = bytes;
    let mut digits = Vec::with_capacity(CANONICAL_PUBKEY_LENGTH);

    while number.iter().any(|&b| b != 0) {
        let mut remainder = 0u32;
        for byte in number.iter_mut() {
            let current = (remainder << 8) | u32::from(*byte);
            *byte = (current / 36) as u8;
            remainder = current % 36;
        }
        digits.push(b"0123456789abcdefghijklmnopqrstuvwxyz"[remainder as usize]);
    }

    while digits.len() < CANONICAL_PUBKEY_LENGTH {
        digits.push(b'0');
    }
    digits.reverse();
    String::from_utf8(digits).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const PUBKEY: &str = "3bf0c63fcb93463407af97a5e5ee64fa883d107ef9e558472c4eb9aaaefa459d";

    fn root() -> NsiteAddress {
        NsiteAddress { pubkey: PUBKEY.into(), kind: ROOT_SITE_KIND, identifier: String::new() }
    }

    fn named(identifier: &str) -> NsiteAddress {
        NsiteAddress { pubkey: PUBKEY.into(), kind: NAMED_SITE_KIND, identifier: identifier.into() }
    }

    #[test]
    fn parses_every_form() {
        let npub = nip19::npub_encode(PUBKEY);
        assert_eq!(parse_nsite_address(PUBKEY).unwrap(), root());
        assert_eq!(parse_nsite_address(&PUBKEY.to_uppercase()).unwrap(), root());
        assert_eq!(parse_nsite_address(&npub).unwrap(), root());
        assert_eq!(parse_nsite_address(&format!("nostr:{npub}")).unwrap(), root());
        assert_eq!(parse_nsite_address(&format!("https://{npub}.nsite.lol/a/b")).unwrap(), root());
        assert_eq!(parse_nsite_address(&format!("35128:{PUBKEY}:docs")).unwrap(), named("docs"));
        let naddr = nip19::naddr_encode(PUBKEY, 35128, "docs", &[]);
        assert_eq!(parse_nsite_address(&naddr).unwrap(), named("docs"));
        let label = format!("{}docs", encode_pubkey_b36(PUBKEY).unwrap());
        assert_eq!(parse_nsite_address(&format!("https://{label}.nsite.lol")).unwrap(), named("docs"));
    }

    #[test]
    fn refuses_what_isnt_a_site() {
        assert_eq!(parse_nsite_address("  ").unwrap_err(), "Enter an address.");
        assert!(parse_nsite_address(&format!("1:{PUBKEY}:x")).unwrap_err().contains("not an nsite"));
        assert!(parse_nsite_address(&format!("35128:{PUBKEY}:")).unwrap_err().contains("requires an identifier"));
        assert!(parse_nsite_address(&format!("15128:{PUBKEY}:x")).unwrap_err().contains("cannot have an identifier"));
        let nsec = nip19::nsec_encode(PUBKEY);
        assert!(parse_nsite_address(&nsec).unwrap_err().contains("private key"));
        assert!(parse_nsite_address("hello").unwrap_err().starts_with("Unrecognized address"));
    }

    #[test]
    fn base36_round_trip() {
        let encoded = encode_pubkey_b36(PUBKEY).unwrap();
        assert_eq!(encoded.len(), 50);
        assert_eq!(decode_pubkey_b36(&encoded).unwrap(), PUBKEY);
        assert_eq!(decode_pubkey_b36(&"z".repeat(50)), None);
        let zero = "0".repeat(64);
        assert_eq!(decode_pubkey_b36(&encode_pubkey_b36(&zero).unwrap()).unwrap(), zero);
    }
}
