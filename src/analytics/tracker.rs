//! The tracker a counting site serves, and where it reports.
//!
//! Both on the site's own origin: a script from anywhere else would need every
//! site's Content Security Policy widened to let it in, and a visitor on the
//! site's onion address could not reach another origin at all. The prefix is
//! a neutral one because `plausible` and `analytics` are exactly what
//! blocklists match on.

use std::sync::OnceLock;

use sha2::{Digest, Sha256};

pub const TRACKER_PATH: &str = "/.npanel/script.js";
pub const EVENT_PATH: &str = "/.npanel/api/event";

/// `@plausible-analytics/tracker` as npm ships it (MIT; see
/// `assets/analytics/plausible-LICENSE.md`), unmodified: it is what the ingest
/// endpoint is modelled on, so the two can't disagree about a field.
const CODE: &str = include_str!("../../assets/analytics/plausible.js");

/// The Expat entry's magnet in LibreJS's license_definitions.json, which is
/// all it checks.
const EXPAT_MAGNET: &str = "magnet:?xt=urn:btih:d3d9a9a6595521f9666a5e94cc830dab83b65699&dn=expat.txt";

/// Hex sha256 of the tracker module, so a new release rebuilds every site.
pub fn tracker_sha256() -> &'static str {
    static SHA: OnceLock<String> = OnceLock::new();
    SHA.get_or_init(|| hex::encode(Sha256::digest(CODE.as_bytes())))
}

/// The script one host serves at [`TRACKER_PATH`]: the tracker, and the call
/// that starts it.
///
/// The domain is baked in rather than read from the page, so a visit over the
/// site's onion address is counted under the site's name. `bindToWindow` is
/// off so a site running Plausible of its own keeps its `window.plausible`,
/// and `logging` is off because a visitor's console is no place for a
/// gateway's chatter.
///
/// The whole file is wrapped in a GNU LibreJS license block, without which a
/// visitor running the extension would never be counted. The tracker is MIT,
/// and the `init` call npanel adds is offered under the same terms.
pub fn tracker_script(domain: &str) -> String {
    let config = serde_json::json!({
        "domain": domain,
        "endpoint": EVENT_PATH,
        "bindToWindow": false,
        "logging": false,
    });
    // Both tags must start their own line, and nothing but whitespace may
    // follow the closing one, or LibreJS treats what's outside as unlicensed.
    [
        format!("// @license {EXPAT_MAGNET} Expat"),
        CODE.trim_end().to_string(),
        format!("init({config});"),
        "// @license-end".to_string(),
        String::new(),
    ]
    .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_the_tracker() {
        let script = tracker_script("docs.example.com");
        assert!(script.starts_with("// @license magnet:"));
        assert!(script.ends_with(
            "init({\"domain\":\"docs.example.com\",\"endpoint\":\"/.npanel/api/event\",\"bindToWindow\":false,\"logging\":false});\n// @license-end\n"
        ));
        assert_eq!(tracker_sha256().len(), 64);
    }
}
