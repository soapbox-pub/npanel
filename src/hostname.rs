//! Hostname validation and root-domain rules.

use crate::psl;

/// The one subdomain npanel offers to fold into the domain beneath it.
const WWW: &str = "www.";

/// Whether an already-normalized string is a v3 onion address: 56 characters
/// of base32, then `.onion`.
///
/// The label is a public key, a version byte and a checksum, so the length and
/// alphabet are fixed and there is no such thing as a short one. v2 addresses
/// were 16 characters; Tor stopped supporting them in 2021 and they are
/// deliberately not matched here.
pub fn is_onion_address(value: &str) -> bool {
    match value.strip_suffix(".onion") {
        Some(label) => label.len() == 56 && label.bytes().all(|b| matches!(b, b'a'..=b'z' | b'2'..=b'7')),
        None => false,
    }
}

/// Lowercase and drop the trailing dot, since `example.com.` is `example.com`.
pub fn normalize_hostname(input: &str) -> String {
    let lower = input.trim().to_lowercase();
    match lower.strip_suffix('.') {
        Some(stripped) => stripped.to_string(),
        None => lower,
    }
}

/// The hostname a request asked for, from its `Host` header.
///
/// `X-Forwarded-Host` is deliberately not consulted. npanel has no way to tell
/// a proxy's copy of that header from a client's, and not every proxy sets one
/// — nginx does not — so honouring it would let a visitor to any site reach the
/// dashboard and the whole JSON API by adding one header. A proxy that rewrites
/// `Host` has to be configured not to; see docs/deployment.md.
///
/// That is also what lets onion connections share this function. Tor hands a
/// visitor's bytes on unaltered, so reading forwarding headers there would be
/// reading the visitor's own.
///
/// The port a client may have appended is dropped, along with the trailing dot
/// of a fully-qualified name.
pub fn request_hostname(host_header: Option<&str>) -> String {
    let host = host_header.unwrap_or("").trim();
    // Port first: in `example.com.:443` the trailing dot is not trailing yet.
    let without_port = match host.rsplit_once(':') {
        Some((name, port)) if !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) => name,
        _ => host,
    };
    normalize_hostname(without_port)
}

/// Whether an already-normalized string is a usable hostname: a multi-label
/// LDH name — letters, digits, hyphens, no leading or trailing hyphen.
pub fn is_hostname(value: &str) -> bool {
    if value.len() > 253 {
        return false;
    }
    let labels: Vec<&str> = value.split('.').collect();
    labels.len() >= 2 && labels.iter().all(|label| is_label(label))
}

fn is_label(label: &str) -> bool {
    let bytes = label.as_bytes();
    !bytes.is_empty()
        && bytes.iter().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
        && bytes[0] != b'-'
        && bytes[bytes.len() - 1] != b'-'
}

/// Whether a hostname is, or sits under, a suffix.
pub fn matches_suffix(hostname: &str, suffix: &str) -> bool {
    hostname == suffix || hostname.strip_suffix(suffix).is_some_and(|rest| rest.ends_with('.'))
}

/// Parse a hostname staff typed into the dashboard.
///
/// Hostnames must be resolvable names, not URLs or wildcards.
/// `allowed_suffixes` comes from the setting of the same name and is required
/// rather than optional, because a caller that forgot it would silently let
/// anyone authorize a certificate for any domain.
pub fn parse_hostname(input: &str, allowed_suffixes: &[String]) -> Result<String, String> {
    let hostname = normalize_hostname(input);

    if hostname.is_empty() {
        return Err("Enter a domain.".into());
    }
    let labels: Vec<&str> = hostname.split('.').collect();
    if labels.len() < 2 || !labels.iter().all(|label| is_label(label)) {
        return Err(format!("Not a valid domain: {}", input.trim()));
    }
    if hostname.len() > 253 {
        return Err("Domain is too long.".into());
    }
    if hostname.ends_with(".onion") {
        // npanel mints these itself, one per site, and a key it did not
        // generate is an address it cannot answer at. Matched on the suffix
        // rather than on `is_onion_address` so that a mistyped address gets
        // this sentence too.
        return Err("npanel gives each site its own .onion address. Add the site's regular domain instead.".into());
    }
    if !allowed_suffixes.is_empty() && !allowed_suffixes.iter().any(|suffix| matches_suffix(&hostname, suffix)) {
        return Err(format!(
            "{hostname} is outside the domains this gateway serves ({}). An admin can change that under Settings.",
            allowed_suffixes.join(", ")
        ));
    }

    Ok(hostname)
}

/// Whether a hostname is a registrable domain with nothing in front of it —
/// `example.com` or `example.co.uk`, but not `docs.example.com`.
///
/// Label counting cannot answer this: `example.co.uk` has three labels and
/// `foo.github.io` has three too, and they are not the same shape. So it goes
/// through the Public Suffix List.
///
/// Onion addresses are excluded even though `onion` is in the list. Tor reads
/// the whole label as a key, so `www.<address>.onion` is not an onion address
/// at all, and the redirect would point into nothing.
pub fn is_root_domain(hostname: &str) -> bool {
    if hostname.ends_with(".onion") {
        return false;
    }
    psl::parse(hostname).is_some_and(|parsed| parsed.subdomain.is_empty())
}

/// The root domain a `www.` hostname belongs to, if it is one.
///
/// `None` for anything else, including `www.docs.example.com`, whose apex is
/// not what the label in front of it suggests.
pub fn www_apex(hostname: &str) -> Option<&str> {
    let apex = hostname.strip_prefix(WWW)?;
    is_root_domain(apex).then_some(apex)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_hostnames() {
        assert_eq!(request_hostname(Some("Example.COM:443")), "example.com");
        assert_eq!(request_hostname(Some("example.com.:443")), "example.com");
        assert_eq!(request_hostname(None), "");
    }

    #[test]
    fn parses_hostnames() {
        assert_eq!(parse_hostname(" Docs.Example.com. ", &[]).unwrap(), "docs.example.com");
        assert!(parse_hostname("", &[]).is_err());
        assert!(parse_hostname("localhost", &[]).is_err());
        assert!(parse_hostname("-a.com", &[]).is_err());
        assert!(parse_hostname("a_b.com", &[]).is_err());
        assert!(parse_hostname("https://a.com", &[]).is_err());
        assert!(parse_hostname("abc.onion", &[]).unwrap_err().contains(".onion"));
        let allowed = vec!["example.com".to_string()];
        assert!(parse_hostname("blog.example.com", &allowed).is_ok());
        assert!(parse_hostname("notexample.com", &allowed).is_err());
    }

    #[test]
    fn root_domains() {
        assert!(is_root_domain("example.com"));
        assert!(is_root_domain("example.co.uk"));
        assert!(!is_root_domain("docs.example.com"));
        assert!(!is_root_domain("foo.github.io"));
        assert!(!is_root_domain(&format!("{}.onion", "a".repeat(56))));
        assert_eq!(www_apex("www.example.com"), Some("example.com"));
        assert_eq!(www_apex("www.docs.example.com"), None);
        assert_eq!(www_apex("example.com"), None);
    }

    #[test]
    fn onion_addresses() {
        assert!(is_onion_address(&format!("{}.onion", "a".repeat(56))));
        assert!(!is_onion_address(&format!("{}.onion", "a".repeat(16))));
        assert!(!is_onion_address(&format!("{}.onion", "1".repeat(56))));
    }
}
