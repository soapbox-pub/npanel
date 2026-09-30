//! The Content Security Policy a site declares for itself, read from the
//! `<meta>` tags of its own `index.html`.

use std::num::NonZeroUsize;

use parking_lot::Mutex;
use rusqlite::OptionalExtension;

use crate::blobs::BlobStore;
use crate::db::Db;
use crate::fields;
use crate::log::Logger;

/// How much of a document npanel will read looking for meta tags. The tags
/// that matter are in `<head>`, so this only has to cover a large one.
pub const MAX_HTML_BYTES: u64 = 128 * 1024;

/// Largest policy npanel will send, in characters. A header field that
/// outgrows what proxies accept would take the whole response down with it;
/// over this, the site falls back to the gateway's own policy.
const MAX_POLICY_LENGTH: usize = 8 * 1024;

/// Parsed policies to keep. Each entry is a build id and a short string.
const MAX_CACHED: usize = 500;

/// The policy sites are served under, unless they declare one or it's switched
/// off: verbatim the `<meta>` policy mkstack ships, which is what most sites
/// reaching an npanel gateway are built from.
pub const GATEWAY_CSP: &str = "default-src 'none'; script-src 'self'; style-src 'self' 'unsafe-inline'; \
frame-src 'self' https:; font-src 'self'; base-uri 'self'; manifest-src 'self'; \
connect-src 'self' blob: https: wss:; img-src 'self' data: blob: https:; media-src 'self' https:";

/// Cached by build rather than by content hash, because that is the lookup
/// being avoided: a build's files are written once and never edited, so the
/// policy for a build cannot change. Read from the build rather than the
/// manifest, so a document npanel rewrote and the header it is sent under
/// always agree.
pub struct SiteCsp {
    db: Db,
    blobs: BlobStore,
    log: Logger,
    cache: Mutex<lru::LruCache<i64, Option<String>>>,
}

impl SiteCsp {
    pub fn new(db: Db, blobs: BlobStore, log: Logger) -> Self {
        Self { db, blobs, log, cache: Mutex::new(lru::LruCache::new(NonZeroUsize::new(MAX_CACHED).unwrap())) }
    }

    /// The policy this build declares, or `None` if it declares none.
    pub async fn policy(&self, build_id: i64) -> Option<String> {
        if let Some(cached) = self.cache.lock().get(&build_id) {
            return cached.clone();
        }

        let blobs = self.blobs.clone();
        let policy = self
            .db
            .call(move |conn| {
                let index: Option<String> = conn
                    .prepare_cached("SELECT sha256 FROM host_build_files WHERE build_id = ? AND path = '/index.html'")?
                    .query_row([build_id], |row| row.get(0))
                    .optional()?;
                Ok(index.and_then(|sha256| extract_csp(&blobs.read_text(&sha256, MAX_HTML_BYTES))))
            })
            .await
            .unwrap_or(None);

        if let Some(policy) = &policy {
            self.log.debug("site_csp_adopted", fields! { "build_id" => build_id, "policy" => policy.as_str() });
        }
        self.cache.lock().put(build_id, policy.clone());
        policy
    }
}

/// The enforcing Content Security Policy an HTML document declares.
///
/// Every matching `<meta http-equiv>` is taken, joined into the
/// comma-separated list the header defines, because a browser enforces each
/// meta policy on top of the last. A `-Report-Only` tag is skipped — a policy
/// the site asked only to be told about must not become the one it is held to.
pub fn extract_csp(html: &str) -> Option<String> {
    let document = scraper::Html::parse_document(html);
    let selector = scraper::Selector::parse("meta").ok()?;
    let mut policies = Vec::new();

    for element in document.select(&selector) {
        let element = element.value();
        let equiv = element.attr("http-equiv").map(|v| v.trim().to_lowercase());
        if equiv.as_deref() != Some("content-security-policy") {
            continue;
        }
        let Some(content) = element.attr("content") else {
            continue;
        };
        let policy = content.trim();
        // Anything but printable ASCII is either a policy that isn't one or an
        // attempt to write a second header from inside a site's markup.
        if policy.is_empty() || policy.chars().any(|c| c != '\t' && !(' '..='~').contains(&c)) {
            continue;
        }
        policies.push(policy.to_string());
    }

    if policies.is_empty() {
        return None;
    }
    let combined = policies.join(", ");
    (combined.len() <= MAX_POLICY_LENGTH).then_some(combined)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_policies() {
        assert_eq!(extract_csp("<html><head><title>x</title></head></html>"), None);
        assert_eq!(
            extract_csp(r#"<meta http-equiv="Content-Security-Policy" content=" default-src 'self' ">"#).as_deref(),
            Some("default-src 'self'")
        );
        assert_eq!(
            extract_csp(
                r#"<meta http-equiv="content-security-policy" content="a"><meta http-equiv="Content-Security-Policy-Report-Only" content="b"><meta http-equiv="CONTENT-SECURITY-POLICY" content="c &amp; d">"#
            )
            .as_deref(),
            Some("a, c & d")
        );
        assert_eq!(extract_csp("<meta http-equiv=\"content-security-policy\" content=\"a\nb\">"), None);
        let long = format!(r#"<meta http-equiv="content-security-policy" content="{}">"#, "a".repeat(9000));
        assert_eq!(extract_csp(&long), None);
    }
}
