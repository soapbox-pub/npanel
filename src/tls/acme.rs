//! Getting a certificate over ACME (RFC 8555), one hostname at a time.
//!
//! Each order is validated by one challenge: TLS-ALPN-01, answered on the
//! HTTPS port, or HTTP-01, answered on the plain one. Which alternates from
//! one attempt to the next. Trying both in one attempt would spend two of
//! the five failed validations an hour Let's Encrypt allows a name, and a
//! name whose DNS points elsewhere fails either way.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use bytes::Bytes;
use http_body_util::BodyExt;
use instant_acme::{
    Account, AccountCredentials, AuthorizationStatus, BodyWrapper, BytesResponse, ChallengeType, HttpClient, Identifier, NewAccount,
    NewOrder, OrderStatus, RetryPolicy,
};
use rcgen::{CertificateParams, CustomExtension, KeyPair};
use rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::sign::CertifiedKey;

use super::Shared;

/// How long the CA has to validate a challenge, and then to issue.
const ORDER_TIMEOUT: Duration = Duration::from_secs(120);

/// How long one request to the CA may take.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Which challenge attempt `attempt`, counted from zero, is validated by.
pub fn challenge_for(attempt: i64) -> ChallengeType {
    if attempt % 2 == 0 {
        ChallengeType::TlsAlpn01
    } else {
        ChallengeType::Http01
    }
}

/// An HTTP client for the CA: reqwest, trusting the roots compiled in.
pub fn http_client(extra_root: Option<reqwest::Certificate>) -> Result<Box<dyn HttpClient>> {
    let mut builder = reqwest::Client::builder().timeout(REQUEST_TIMEOUT).user_agent(concat!("npanel/", env!("CARGO_PKG_VERSION")));
    if let Some(root) = extra_root {
        builder = builder.add_root_certificate(root);
    }
    Ok(Box::new(Http(builder.build()?)))
}

struct Http(reqwest::Client);

impl HttpClient for Http {
    fn request(
        &self,
        request: http::Request<BodyWrapper<Bytes>>,
    ) -> Pin<Box<dyn Future<Output = Result<BytesResponse, instant_acme::Error>> + Send>> {
        let client = self.0.clone();
        Box::pin(async move {
            let (parts, body) = request.into_parts();
            let body = match body.collect().await {
                Ok(collected) => collected.to_bytes(),
                Err(never) => match never {},
            };
            let request = reqwest::Request::try_from(http::Request::from_parts(parts, body)).map_err(other)?;
            let response = client.execute(request).await.map_err(other)?;
            Ok(BytesResponse::from(http::Response::from(response)))
        })
    }
}

fn other(error: impl std::error::Error + Send + Sync + 'static) -> instant_acme::Error {
    instant_acme::Error::Other(Box::new(error))
}

/// The account for `directory`: `stored` if there is one, else a new one,
/// which the caller keeps. Its contact is brought in line with `email`.
pub async fn account(
    http: impl Fn() -> Result<Box<dyn HttpClient>>,
    directory: &str,
    email: &str,
    stored: Option<(String, String)>,
) -> Result<(Account, Option<AccountCredentials>)> {
    let contact = (!email.is_empty()).then(|| format!("mailto:{email}"));
    let contact: Vec<&str> = contact.iter().map(String::as_str).collect();
    if let Some((credentials, registered_with)) = stored {
        let credentials: AccountCredentials = serde_json::from_str(&credentials).context("The stored ACME account can't be read")?;
        let account = Account::builder_with_http(http()?).from_credentials(credentials).await.context("Could not reach the ACME CA")?;
        if registered_with == email {
            return Ok((account, None));
        }
        account.update_contacts(&contact).await.context("Could not update the ACME account's email")?;
        return Ok((account, None));
    }
    // Agreeing to the CA's terms is what setting HTTPS_PORT does; the
    // deployment guide says so.
    let new = NewAccount { contact: &contact, terms_of_service_agreed: true, only_return_existing: false };
    let (account, credentials) =
        Account::builder_with_http(http()?).create(&new, directory.to_string(), None).await.context("Could not create an ACME account")?;
    Ok((account, Some(credentials)))
}

/// One order for `hostname`, validated by `kind`: the certificate chain and
/// its private key, both PEM.
pub async fn order(account: &Account, shared: &Shared, hostname: &str, kind: ChallengeType) -> Result<(String, String)> {
    let identifiers = [Identifier::Dns(hostname.to_string())];
    let mut order = account.new_order(&NewOrder::new(&identifiers)).await.context("Could not place an order")?;
    // However the order ends, the challenge stops being answered.
    let _cleanup = Cleanup { shared, hostname };

    let mut authorizations = order.authorizations();
    while let Some(authorization) = authorizations.next().await {
        let mut authorization = authorization?;
        match authorization.status {
            AuthorizationStatus::Pending => {}
            AuthorizationStatus::Valid => continue,
            status => bail!("The CA has the authorization {status:?}"),
        }
        let mut challenge = authorization.challenge(kind.clone()).ok_or_else(|| anyhow!("The CA doesn't offer {kind:?}"))?;
        let key_authorization = challenge.key_authorization();
        match kind {
            ChallengeType::TlsAlpn01 => {
                let certificate = alpn_challenge_certificate(hostname, key_authorization.digest().as_ref())?;
                shared.alpn_challenges.lock().insert(hostname.to_string(), Arc::new(certificate));
            }
            _ => {
                shared.http_challenges.lock().insert(challenge.token.clone(), (hostname.to_string(), key_authorization.as_str().to_string()));
            }
        }
        challenge.set_ready().await?;
    }

    let retries = RetryPolicy::new().initial_delay(Duration::from_secs(1)).timeout(ORDER_TIMEOUT);
    // A failed validation comes back as the CA's own words, which are the
    // useful part: "connection refused", "DNS problem: NXDOMAIN".
    let status = order.poll_ready(&retries).await.with_context(|| format!("The CA could not validate {hostname}"))?;
    if status != OrderStatus::Ready {
        bail!("The CA left the order for {hostname} {status:?}");
    }
    let key = order.finalize().await.context("Could not finalize the order")?;
    let chain = order.poll_certificate(&retries).await.context("Could not fetch the certificate")?;
    Ok((chain, key))
}

struct Cleanup<'a> {
    shared: &'a Shared,
    hostname: &'a str,
}

impl Drop for Cleanup<'_> {
    fn drop(&mut self) {
        self.shared.alpn_challenges.lock().remove(self.hostname);
        self.shared.http_challenges.lock().retain(|_, (hostname, _)| hostname != self.hostname);
    }
}

/// The self-signed certificate a TLS-ALPN-01 challenge is answered with: for
/// the hostname, carrying the key authorization's digest (RFC 8737 §3).
pub fn alpn_challenge_certificate(hostname: &str, digest: &[u8]) -> Result<CertifiedKey> {
    let mut params = CertificateParams::new(vec![hostname.to_string()])?;
    params.custom_extensions = vec![CustomExtension::new_acme_identifier(digest)];
    let key = KeyPair::generate()?;
    let certificate = params.self_signed(&key)?;
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der()));
    let signer = rustls::crypto::ring::sign::any_supported_type(&key)?;
    Ok(CertifiedKey::new(vec![certificate.der().clone()], signer))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alternates_challenges() {
        assert_eq!(challenge_for(0), ChallengeType::TlsAlpn01);
        assert_eq!(challenge_for(1), ChallengeType::Http01);
        assert_eq!(challenge_for(2), ChallengeType::TlsAlpn01);
    }
}
