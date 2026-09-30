//! NIP-98 HTTP auth.
//!
//! The client signs a kind 27235 event naming the method and URL, and sends it
//! base64-encoded in `Authorization: Nostr <event>`. A valid signature proves
//! identity, not authorization: npanel is not permissionless, so a pubkey with
//! no row in `users` is refused by the caller.

use base64::Engine;
use sha2::{Digest, Sha256};

use crate::nostr::Event;

/// NIP-98 HTTP auth event kind.
pub const NIP98_KIND: i64 = 27235;

/// How far an auth event's `created_at` may be from now, in seconds.
pub const MAX_AUTH_AGE: i64 = 60;

/// Why a request isn't authenticated, as a sentence for the dashboard.
#[derive(Debug, PartialEq)]
pub struct AuthError(pub String);

fn fail<T>(message: impl Into<String>) -> Result<T, AuthError> {
    Err(AuthError(message.into()))
}

/// Verify an `Authorization: Nostr <base64 event>` header against a request.
///
/// The signed `u` tag is compared against the dashboard's public hostname
/// rather than the URL this process sees: behind a reverse proxy those differ,
/// and comparing the raw request URL would reject every legitimate request.
/// Host and path must match exactly; scheme and port are tolerated.
///
/// A body is only bound to the signature by the `payload` tag, so a request
/// with one must carry it — otherwise anyone who saw the header could replay
/// it within the window with a body of their own.
pub fn verify_nip98(
    header: Option<&str>,
    method: &str,
    path_and_query: &str,
    body: &[u8],
    dashboard_host: &str,
    now: i64,
) -> Result<Event, AuthError> {
    let Some(header) = header else {
        return fail("Missing Authorization header. Sign in to continue.");
    };
    let Some(token) = header.strip_prefix("Nostr ").filter(|t| !t.is_empty()) else {
        return fail("Authorization header must use the Nostr scheme.");
    };

    let decoded = base64::engine::general_purpose::STANDARD
        .decode(token.trim())
        .or_else(|_| base64::engine::general_purpose::STANDARD_NO_PAD.decode(token.trim()))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Event>(&bytes).ok());
    let Some(event) = decoded else {
        return fail("Authorization token is not a base64-encoded event.");
    };

    if event.kind != NIP98_KIND {
        return fail(format!("Auth event must be kind {NIP98_KIND}."));
    }
    if !event.verify() {
        return fail("Auth event signature is invalid.");
    }
    if (now - event.created_at).abs() > MAX_AUTH_AGE {
        return fail("Auth event expired. Check your device's clock.");
    }
    if !event.tag("method").is_some_and(|m| m.eq_ignore_ascii_case(method)) {
        return fail("Auth event method does not match the request.");
    }

    let Some(signed) = event.tag("u") else {
        return fail("Auth event is missing a u tag.");
    };
    let Ok(signed) = url::Url::parse(signed) else {
        return fail("Auth event u tag is not a valid URL.");
    };
    let signed_host = signed.host_str().unwrap_or("");
    if !signed_host.eq_ignore_ascii_case(dashboard_host) {
        return fail(format!("Auth event was signed for {signed_host}, not {dashboard_host}."));
    }
    let requested = url::Url::parse(&format!("http://request{path_and_query}")).ok();
    let same = requested.is_some_and(|r| r.path() == signed.path() && r.query().unwrap_or("") == signed.query().unwrap_or(""));
    if !same {
        return fail("Auth event was signed for a different URL.");
    }

    match event.tag("payload") {
        None if !body.is_empty() => return fail("Auth event must carry a payload tag for a request with a body."),
        Some(payload) if hex::encode(Sha256::digest(body)) != payload => {
            return fail("Auth event payload hash does not match the request body.")
        }
        _ => {}
    }

    Ok(event)
}

/// A signed `Authorization` header, for tests.
#[cfg(test)]
pub fn sign_nip98(secret: &str, method: &str, url: &str, body: Option<&[u8]>, now: i64) -> String {
    let mut tags = vec![vec!["u".to_string(), url.to_string()], vec!["method".to_string(), method.to_string()]];
    if let Some(body) = body {
        tags.push(vec!["payload".into(), hex::encode(Sha256::digest(body))]);
    }
    let event = Event::sign(secret, now, NIP98_KIND, tags, "");
    format!("Nostr {}", base64::engine::general_purpose::STANDARD.encode(serde_json::to_vec(&event).unwrap()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "0000000000000000000000000000000000000000000000000000000000000003";
    const NOW: i64 = 1_800_000_000;

    fn check(header: &str, method: &str, path: &str, body: &[u8]) -> Result<Event, AuthError> {
        verify_nip98(Some(header), method, path, body, "panel.example.com", NOW)
    }

    #[test]
    fn verifies() {
        let header = sign_nip98(SECRET, "GET", "https://panel.example.com/api/hosts?x=1", None, NOW);
        assert!(check(&header, "GET", "/api/hosts?x=1", b"").is_ok());
        assert_eq!(check(&header, "POST", "/api/hosts?x=1", b"").unwrap_err().0, "Auth event method does not match the request.");
        assert_eq!(check(&header, "GET", "/api/hosts", b"").unwrap_err().0, "Auth event was signed for a different URL.");
        assert!(check(&header, "GET", "/api/hosts?x=1", b"{}").unwrap_err().0.contains("payload tag"));

        let with_body = sign_nip98(SECRET, "POST", "http://panel.example.com:8080/api/hosts", Some(b"{}"), NOW);
        assert!(check(&with_body, "POST", "/api/hosts", b"{}").is_ok());
        assert!(check(&with_body, "POST", "/api/hosts", b"{ }").unwrap_err().0.contains("does not match"));

        let old = sign_nip98(SECRET, "GET", "https://panel.example.com/api/me", None, NOW - 61);
        assert!(check(&old, "GET", "/api/me", b"").unwrap_err().0.contains("expired"));

        let other = sign_nip98(SECRET, "GET", "https://evil.example.com/api/me", None, NOW);
        assert!(check(&other, "GET", "/api/me", b"").unwrap_err().0.contains("signed for evil.example.com"));

        assert!(verify_nip98(None, "GET", "/", b"", "x", NOW).unwrap_err().0.starts_with("Missing"));
        assert!(check("Bearer x", "GET", "/", b"").unwrap_err().0.contains("Nostr scheme"));
        assert!(check("Nostr !!!", "GET", "/", b"").unwrap_err().0.contains("base64"));
    }
}
