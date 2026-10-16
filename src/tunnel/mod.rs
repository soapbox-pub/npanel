//! Tunnels: a hostname npanel serves by passing its requests to an agent on
//! somebody's own machine, which passes them on to a server there.
//!
//! The agent is this same binary, run as `npanel tunnel`. It dials out to the
//! dashboard host over HTTPS, the way a browser would, so it works from behind
//! NAT and through the proxy in front of npanel, and asks to upgrade the
//! connection. From then on the roles are reversed: npanel is the HTTP/2
//! client, sending each visitor's request down the connection as a stream of
//! its own, and the agent is the HTTP/2 server answering them. A WebSocket is
//! an extended CONNECT stream (RFC 8441) that the agent opens to its origin as
//! an ordinary HTTP/1.1 upgrade, and both ends then copy bytes.
//!
//! An agent is known by a key it makes for itself, and every request it sends
//! the API is NIP-98-signed with it. An admin authorizes the key once, from
//! the dashboard, by a code the agent shows; after that it connects with no
//! one's help. Where the agent sends traffic is settled only on its own
//! machine: npanel can choose which hostname reaches it, but cannot make it
//! dial anything else.

pub mod agent;
pub mod gateway;

use http::header::{self, HeaderMap, HeaderName};

/// The `Upgrade` token an agent asks for.
pub const PROTOCOL: &str = "npanel-tunnel";

/// Where an agent asks what its key is, and is told whether it is a tunnel.
/// Singular, as `/api/me` is beside `/api/users`: the tunnel signing the
/// request, where `/api/tunnels` is every tunnel, for admins.
pub const STATUS_PATH: &str = "/api/tunnel";
/// Where an agent asks to be authorized.
pub const REQUEST_PATH: &str = "/api/tunnel/request";
/// Where an agent connects.
pub const CONNECT_PATH: &str = "/api/tunnel/connect";

/// The letters a code is made of: consonants that can't be misread as one
/// another, or spell anything.
pub const CODE_ALPHABET: &[u8] = b"BCDFGHJKLMNPQRSTVWXZ";

/// Whether `code` is one the gateway makes: two groups of four letters from
/// [`CODE_ALPHABET`], joined by a dash.
pub fn is_code(code: &str) -> bool {
    let bytes = code.as_bytes();
    bytes.len() == 9 && bytes.iter().enumerate().all(|(i, b)| if i == 4 { *b == b'-' } else { CODE_ALPHABET.contains(b) })
}

/// What the agent forwards to, as it says when it connects. Shown in the
/// dashboard, and nothing else.
pub const TARGET_HEADER: &str = "npanel-target";

/// The buffer each direction of a WebSocket is copied through, on either end
/// of the tunnel. Under tokio's 8 KiB: an open socket mostly sits idle, and a
/// server holding thousands keeps two of these each; a large message only
/// takes a few more copies.
pub const WEBSOCKET_BUFFER: usize = 4 * 1024;

/// How often each end pings the other, and how long an answer may take,
/// before a quiet connection is given up for dead. Under the minute most NAT
/// and proxy idle timeouts allow.
pub const KEEP_ALIVE: std::time::Duration = std::time::Duration::from_secs(20);

/// Remove the headers that describe one hop rather than the request: those a
/// connection names in `Connection`, and the ones that always do. HTTP/2
/// refuses them outright, and they mean nothing on the next hop anyway.
pub fn strip_hop_by_hop(headers: &mut HeaderMap) {
    let named: Vec<HeaderName> = headers
        .get_all(header::CONNECTION)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .filter_map(|name| HeaderName::from_bytes(name.trim().as_bytes()).ok())
        .collect();
    for name in named {
        headers.remove(name);
    }
    for name in [header::CONNECTION, header::TE, header::TRANSFER_ENCODING, header::UPGRADE] {
        headers.remove(name);
    }
    headers.remove("keep-alive");
    headers.remove("proxy-connection");
}

/// Whether a request asks to become a WebSocket. The only upgrade a tunnel
/// carries: anything else is passed on as the plain request it also is.
pub fn is_websocket(headers: &HeaderMap) -> bool {
    let connection_upgrades = headers
        .get_all(header::CONNECTION)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .any(|token| token.trim().eq_ignore_ascii_case("upgrade"));
    let websocket = headers.get(header::UPGRADE).and_then(|v| v.to_str().ok()).is_some_and(|v| v.trim().eq_ignore_ascii_case("websocket"));
    connection_upgrades && websocket
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_what_belongs_to_one_hop() {
        let mut headers = HeaderMap::new();
        headers.insert(header::CONNECTION, "keep-alive, X-Secret".parse().unwrap());
        headers.insert("keep-alive", "timeout=5".parse().unwrap());
        headers.insert("x-secret", "1".parse().unwrap());
        headers.insert(header::TRANSFER_ENCODING, "chunked".parse().unwrap());
        headers.insert(header::ACCEPT, "*/*".parse().unwrap());
        strip_hop_by_hop(&mut headers);
        assert_eq!(headers.keys().map(|k| k.as_str()).collect::<Vec<_>>(), ["accept"]);
    }

    #[test]
    fn knows_a_websocket() {
        let mut headers = HeaderMap::new();
        headers.insert(header::CONNECTION, "keep-alive, Upgrade".parse().unwrap());
        headers.insert(header::UPGRADE, "WebSocket".parse().unwrap());
        assert!(is_websocket(&headers));
        headers.insert(header::UPGRADE, "h2c".parse().unwrap());
        assert!(!is_websocket(&headers));
        headers.insert(header::UPGRADE, "websocket".parse().unwrap());
        headers.remove(header::CONNECTION);
        assert!(!is_websocket(&headers));
    }
}
