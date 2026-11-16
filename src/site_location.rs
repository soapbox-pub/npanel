//! Which nsite a web page says it is.
//!
//! A site published both on Nostr and at an ordinary https address can say so
//! in an `Nsite-Location` header naming its `nsite://` URL, the way
//! `Onion-Location` names an onion. npanel sends one for every host it
//! serves. So staff can paste a site's own web address, off any gateway, and
//! npanel asks the page which nsite it is.
//!
//! The URL comes from staff, but where it redirects is up to whoever runs the
//! page, so the lookup only ever reaches the public internet, as blob
//! downloads do (see `net.rs`).

use std::sync::Arc;
use std::time::Duration;

use crate::net::{is_public_address, PublicResolver};
use crate::nsite_address::{parse_nsite_address, web_page_url, NsiteAddress};

/// The header a page names its nsite in.
const NSITE_LOCATION: &str = "nsite-location";

/// Long enough for a slow site, short enough that staff waiting on the
/// preview aren't left wondering.
const TIMEOUT: Duration = Duration::from_secs(10);

/// A site's address moving from `example.com` to `www.example.com` and on to
/// `https` is a couple of hops; anything longer is going nowhere.
const MAX_REDIRECTS: usize = 5;

#[derive(Clone)]
pub struct SiteLookup {
    http: reqwest::Client,
    /// Only tests turn this off: their pages listen on loopback over plain
    /// HTTP, which are exactly what the real lookup refuses.
    strict: bool,
}

impl SiteLookup {
    pub fn new() -> SiteLookup {
        Self::build(true)
    }

    /// A lookup that may reach plain-HTTP pages on private addresses. For tests.
    #[cfg(test)]
    pub fn allowing_private() -> SiteLookup {
        Self::build(false)
    }

    fn build(strict: bool) -> SiteLookup {
        let redirects = reqwest::redirect::Policy::custom(move |attempt| {
            if attempt.previous().len() >= MAX_REDIRECTS {
                attempt.error(Refused::TooManyRedirects)
            } else if !may_visit(attempt.url(), strict) {
                attempt.error(Refused::Redirect)
            } else {
                attempt.follow()
            }
        });
        let mut http = reqwest::Client::builder()
            .redirect(redirects)
            .user_agent(concat!("npanel/", env!("CARGO_PKG_VERSION")))
            .timeout(TIMEOUT);
        if strict {
            // A proxy from the environment would resolve and dial the name
            // itself, after the check, which is the window the resolver is
            // here to close.
            http = http.dns_resolver(Arc::new(PublicResolver)).no_proxy();
        }
        SiteLookup { http: http.build().expect("an HTTP client"), strict }
    }

    /// [`parse_nsite_address`], and for a web page that isn't on a gateway,
    /// the nsite the page names in its `Nsite-Location` header.
    ///
    /// Errors are sentences safe to show in the dashboard.
    pub async fn resolve(&self, input: &str) -> Result<NsiteAddress, String> {
        let unparsed = match parse_nsite_address(input) {
            Ok(address) => return Ok(address),
            Err(error) => error,
        };
        match web_page_url(input) {
            Some(url) => self.ask(url).await,
            None => Err(unparsed),
        }
    }

    /// HEAD the page and read its `Nsite-Location`.
    async fn ask(&self, url: url::Url) -> Result<NsiteAddress, String> {
        let host = url.host_str().unwrap_or_default().to_string();
        if self.strict && url.scheme() != "https" {
            return Err(format!("Only an https:// page can name its nsite. Try https://{host} instead."));
        }
        if self.strict {
            // The resolver enforces this for names too; asking first makes a
            // refusal a sentence rather than a connection error, and covers
            // IP literals, which skip the resolver.
            let public = crate::net::is_public_url(&url)
                .await
                .map_err(|_| format!("Couldn't find {host}. Check the address, or paste the site's npub or naddr instead."))?;
            if !public {
                return Err(format!("{host} is not on the public internet, so npanel can't ask it which nsite it is."));
            }
        }

        let response = self.http.head(url).send().await.map_err(|error| unreachable(&host, &error))?;
        let Some(location) = response.headers().get(NSITE_LOCATION) else {
            return Err(format!(
                "{host} doesn't say which nsite it is: its response has no Nsite-Location header. Paste the site's npub, naddr or nsite:// address instead."
            ));
        };
        let location = location.to_str().map_err(|_| format!("{host}'s Nsite-Location header isn't readable text."))?;
        let is_nsite_url = url::Url::parse(location.trim()).is_ok_and(|url| url.scheme() == "nsite");
        if !is_nsite_url {
            return Err(format!("{host}'s Nsite-Location header, {location}, isn't an nsite:// address."));
        }
        parse_nsite_address(location).map_err(|error| format!("{host}'s Nsite-Location header doesn't name an nsite. {error}"))
    }

}

/// Whether a redirect may be followed: to https, and, for an IP literal,
/// which the resolver never sees, to a public one.
fn may_visit(url: &url::Url, strict: bool) -> bool {
    if !strict {
        return true;
    }
    let public = match url.host() {
        Some(url::Host::Ipv4(ip)) => is_public_address(ip.into()),
        Some(url::Host::Ipv6(ip)) => is_public_address(ip.into()),
        Some(url::Host::Domain(_)) => true,
        None => false,
    };
    url.scheme() == "https" && public
}

/// Why a redirect wasn't followed, for [`unreachable`] to put into words.
#[derive(Debug)]
enum Refused {
    TooManyRedirects,
    Redirect,
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Refused::TooManyRedirects => "too many redirects",
            Refused::Redirect => "a redirect off the public https web",
        })
    }
}

impl std::error::Error for Refused {}

fn unreachable(host: &str, error: &reqwest::Error) -> String {
    let refused = std::error::Error::source(error).and_then(|source| source.downcast_ref::<Refused>());
    match refused {
        Some(Refused::TooManyRedirects) => format!("{host} redirected too many times. Paste the address it ends up at instead."),
        Some(Refused::Redirect) => format!("{host} redirected to a page that isn't on the public https web."),
        None if error.is_timeout() => format!("{host} took too long to answer. Try again, or paste the site's npub or naddr instead."),
        None => format!("Couldn't reach {host}. Check the address, or paste the site's npub or naddr instead."),
    }
}

#[cfg(test)]
mod tests {
    use axum::http::{HeaderMap, HeaderValue};
    use axum::response::IntoResponse;

    use super::*;
    use crate::nostr::nip19;
    use crate::nsite_address::{NAMED_SITE_KIND, ROOT_SITE_KIND};

    const PUBKEY: &str = "3bf0c63fcb93463407af97a5e5ee64fa883d107ef9e558472c4eb9aaaefa459d";

    /// A web page on loopback answering every request with these headers.
    async fn page(headers: Vec<(&'static str, String)>) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let router = axum::Router::new()
            .route("/old", axum::routing::get(|| async { axum::response::Redirect::permanent("/") }))
            .fallback(move || {
                let headers = headers.clone();
                async move {
                    let mut map = HeaderMap::new();
                    for (name, value) in headers {
                        map.insert(name, HeaderValue::from_str(&value).unwrap());
                    }
                    map.into_response()
                }
            });
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        format!("http://{address}")
    }

    #[tokio::test]
    async fn reads_the_pages_nsite_location() {
        let naddr = nip19::naddr_encode(PUBKEY, 35128, "docs", &[]);
        let url = page(vec![(NSITE_LOCATION, format!("nsite://{naddr}/"))]).await;
        let lookup = SiteLookup::allowing_private();

        let expected = NsiteAddress { pubkey: PUBKEY.into(), kind: NAMED_SITE_KIND, identifier: "docs".into() };
        assert_eq!(lookup.resolve(&format!("{url}/some/page")).await.unwrap(), expected);
        assert_eq!(lookup.resolve(&format!("{url}/old")).await.unwrap(), expected);
    }

    #[tokio::test]
    async fn parses_without_asking_when_it_can() {
        let npub = nip19::npub_encode(PUBKEY);
        let root = NsiteAddress { pubkey: PUBKEY.into(), kind: ROOT_SITE_KIND, identifier: String::new() };
        // Nothing listens at nsite.invalid, so an attempt to ask would fail.
        assert_eq!(SiteLookup::new().resolve(&format!("https://{npub}.nsite.invalid/")).await.unwrap(), root);
        assert_eq!(SiteLookup::new().resolve(&format!("nsite://{npub}/")).await.unwrap(), root);
        assert!(SiteLookup::new().resolve("hello").await.unwrap_err().starts_with("Unrecognized address"));
    }

    #[tokio::test]
    async fn says_what_a_page_didnt() {
        let lookup = SiteLookup::allowing_private();

        let silent = page(vec![]).await;
        assert!(lookup.resolve(&silent).await.unwrap_err().contains("no Nsite-Location header"));

        let wrong = page(vec![(NSITE_LOCATION, "https://example.com/".into())]).await;
        assert!(lookup.resolve(&wrong).await.unwrap_err().contains("isn't an nsite:// address"));

        let note = nip19::note_encode(PUBKEY);
        let not_a_site = page(vec![(NSITE_LOCATION, format!("nsite://{note}/"))]).await;
        assert!(lookup.resolve(&not_a_site).await.unwrap_err().contains("doesn't name an nsite"));
    }

    #[tokio::test]
    async fn only_asks_the_public_https_web() {
        let naddr = nip19::naddr_encode(PUBKEY, 35128, "docs", &[]);
        let url = page(vec![(NSITE_LOCATION, format!("nsite://{naddr}/"))]).await;
        let lookup = SiteLookup::new();

        assert!(lookup.resolve(&url).await.unwrap_err().starts_with("Only an https:// page"));
        let https = url.replacen("http://", "https://", 1);
        assert!(lookup.resolve(&https).await.unwrap_err().contains("not on the public internet"));

        assert!(may_visit(&url::Url::parse("https://example.com/").unwrap(), true));
        assert!(!may_visit(&url::Url::parse("http://example.com/").unwrap(), true));
        assert!(!may_visit(&url::Url::parse("https://127.0.0.1/").unwrap(), true));
        assert!(!may_visit(&url::Url::parse("https://[::1]/").unwrap(), true));
    }
}
