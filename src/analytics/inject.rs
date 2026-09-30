//! Adding the tracker to a site's pages, once, when the host is built.

use std::sync::OnceLock;

use regex::bytes::Regex;

use super::tracker::TRACKER_PATH;
use crate::html::decode_attribute;

/// Largest document the tracker is added to. A page bigger than this is
/// almost certainly a generated dump, and is served as published.
pub const MAX_INJECT_BYTES: u64 = 4 * 1024 * 1024;

fn regex(cell: &'static OnceLock<Regex>, pattern: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pattern).expect("regex"))
}

/// Whitespace as JavaScript's `\s` reads a Latin-1 string: the ASCII set plus
/// the no-break space, which is byte 0xA0.
const SPACE: &str = r"[\t\n\x0B\x0C\r \xA0]";

/// A page with the tracker added, or `None` for one that isn't a document.
///
/// Edited as bytes rather than parsed and re-serialized, so everything the
/// site wrote comes back exactly as it was apart from the one tag, whatever its
/// encoding: everything written into it is ASCII.
///
/// The tag goes at the end of `<head>`, and failing that where the head would
/// have ended — before `<body>`, after `<html>`, or after the doctype. A file
/// with none of those is a fragment some page fetches and inserts, and a
/// script tag inserted that way never runs anyway, so it is left alone.
///
/// Any `<meta>` Content Security Policy in the head is widened by
/// [`allow_analytics`], since otherwise the policy the site wrote would stop
/// the script npanel added.
pub fn inject_analytics(html: &[u8], integrity: &str) -> Option<Vec<u8>> {
    let at = insertion_point(&html.to_ascii_lowercase())?;

    let (head, strict_dynamic) = allow_meta_policies(&html[..at], integrity);

    // Only a policy that trusts by hash alone needs the tag to carry one: a
    // hash pins the script's exact bytes, so a browser still holding the last
    // build's copy would refuse this one.
    let tag = if strict_dynamic {
        format!(r#"<script type="module" src="{TRACKER_PATH}" integrity="{integrity}"></script>"#)
    } else {
        format!(r#"<script type="module" src="{TRACKER_PATH}"></script>"#)
    };

    let mut out = Vec::with_capacity(html.len() + tag.len() + 64);
    out.extend_from_slice(&head);
    out.extend_from_slice(tag.as_bytes());
    out.extend_from_slice(&html[at..]);
    Some(out)
}

/// Where in a document the tag goes, from its lowercased bytes.
fn insertion_point(lower: &[u8]) -> Option<usize> {
    if let Some(at) = find(lower, b"</head") {
        return Some(at);
    }

    static BODY: OnceLock<Regex> = OnceLock::new();
    if let Some(found) = regex(&BODY, &format!(r"(?-u)<body(?:{SPACE}|>)")).find(lower) {
        return Some(found.start());
    }

    static HTML: OnceLock<Regex> = OnceLock::new();
    static DOCTYPE: OnceLock<Regex> = OnceLock::new();
    for opening in [regex(&HTML, &format!(r"(?-u)<html(?:{SPACE}|>)[^>]*>|<html>")), regex(&DOCTYPE, r"(?-u)<!doctype[^>]*>")] {
        if let Some(found) = opening.find(lower) {
            return Some(found.end());
        }
    }

    None
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|window| window == needle)
}

/// Every `<meta http-equiv="Content-Security-Policy">` in `head`, widened.
/// Also reports whether any of them trusts scripts by hash alone.
fn allow_meta_policies(head: &[u8], integrity: &str) -> (Vec<u8>, bool) {
    static META: OnceLock<Regex> = OnceLock::new();
    static ATTRIBUTE: OnceLock<Regex> = OnceLock::new();
    let meta = regex(&META, r#"(?i-u)<meta\b[^>]*>"#);
    let attribute = regex(
        &ATTRIBUTE,
        &format!(r#"(?-u)([^\t\n\x0B\x0C\r \xA0"'=/>]+)(?:{SPACE}*={SPACE}*(?:"([^"]*)"|'([^']*)'|([^\t\n\x0B\x0C\r \xA0"'>]+)))?"#),
    );

    let mut strict_dynamic = false;
    let mut out = Vec::with_capacity(head.len());
    let mut last = 0;

    for found in meta.find_iter(head) {
        let tag = found.as_bytes();
        let mut http_equiv = None;
        let mut content: Option<(String, usize, usize)> = None;

        for captures in attribute.captures_iter(&tag[5..]) {
            let whole = captures.get(0).expect("match");
            let name = captures[1].to_ascii_lowercase();
            let value = captures.get(2).or(captures.get(3)).or(captures.get(4)).map(|m| m.as_bytes()).unwrap_or(b"");
            if name == b"http-equiv" {
                http_equiv = Some(latin1(value).trim().to_lowercase());
            }
            if name == b"content" {
                content = Some((latin1(value), 5 + whole.start(), 5 + whole.end()));
            }
        }

        let rewritten = (|| {
            if http_equiv.as_deref() != Some("content-security-policy") {
                return None;
            }
            let (value, start, end) = content?;
            let policy = decode_attribute(&value);
            // Nothing a policy can say is outside printable ASCII, and
            // anything that is would not survive being written back.
            if policy.chars().any(|c| c != '\t' && !(' '..='~').contains(&c)) {
                return None;
            }
            if uses_strict_dynamic(&policy) {
                strict_dynamic = true;
            }
            let widened = allow_analytics(&policy, integrity);
            if widened == policy {
                return None;
            }
            let mut tag_out = tag[..start].to_vec();
            tag_out.extend_from_slice(format!("content=\"{}\"", escape_attribute(&widened)).as_bytes());
            tag_out.extend_from_slice(&tag[end..]);
            Some(tag_out)
        })();

        out.extend_from_slice(&head[last..found.start()]);
        out.extend_from_slice(rewritten.as_deref().unwrap_or(tag));
        last = found.end();
    }
    out.extend_from_slice(&head[last..]);

    (out, strict_dynamic)
}

/// Bytes as Latin-1 text: one character per byte.
fn latin1(bytes: &[u8]) -> String {
    bytes.iter().map(|&b| b as char).collect()
}

struct Directive {
    name: String,
    sources: Vec<String>,
}

/// A Content Security Policy that lets the tracker load and report, and is
/// otherwise exactly what the site wrote.
///
/// Two things are needed from the site's own origin: loading a script, and
/// `fetch`ing to it. Each is granted by adding `'self'` to the directive that
/// governs it — `script-src-elem` and `script-src` where the site wrote them,
/// `connect-src` likewise, and a copy of `default-src` where it relied on the
/// fallback, so nothing that fallback allowed is taken away. `'none'` gives
/// way to `'self'`, and a directive that already allows the origin is
/// untouched.
///
/// The one policy `'self'` does nothing for uses `'strict-dynamic'`, which
/// trusts scripts by nonce or hash alone. There the tracker's hash is added
/// instead — only there, since a hash in a policy relying on `'unsafe-inline'`
/// would switch that off.
///
/// `policy` may be several policies joined with commas; each is widened on
/// its own.
pub fn allow_analytics(policy: &str, integrity: &str) -> String {
    let policies: Vec<&str> = policy.split(',').map(str::trim).collect();
    let widened: Vec<String> = policies.iter().map(|one| allow_in_policy(one, integrity)).collect();

    if widened.iter().zip(&policies).all(|(a, b)| a == b) {
        policy.to_string()
    } else {
        widened.join(", ")
    }
}

fn allow_in_policy(policy: &str, integrity: &str) -> String {
    let mut directives: Vec<Directive> = policy
        .split(';')
        .map(str::trim)
        .filter(|d| !d.is_empty())
        .map(|d| {
            let mut parts = d.split_whitespace();
            let name = parts.next().unwrap_or("").to_lowercase();
            Directive { name, sources: parts.map(str::to_string).collect() }
        })
        .collect();

    let mut changed = false;
    changed |= widen(&mut directives, &["script-src-elem", "script-src"], "script-src", Some(integrity));
    changed |= widen(&mut directives, &["connect-src"], "connect-src", None);

    if !changed {
        return policy.to_string();
    }

    directives
        .iter()
        .map(|d| std::iter::once(d.name.as_str()).chain(d.sources.iter().map(String::as_str)).collect::<Vec<_>>().join(" "))
        .collect::<Vec<_>>()
        .join("; ")
}

/// Widen whichever of `names` the policy wrote, or — where it wrote none and
/// leans on `default-src` — a copy of the fallback under `added`.
fn widen(directives: &mut Vec<Directive>, names: &[&str], added: &str, hash: Option<&str>) -> bool {
    let present: Vec<usize> = names
        .iter()
        .filter_map(|name| directives.iter().position(|d| d.name == *name))
        .collect();

    if !present.is_empty() {
        let mut changed = false;
        for index in present {
            if !allows(&directives[index], hash) {
                grant(&mut directives[index], hash);
                changed = true;
            }
        }
        return changed;
    }

    let Some(fallback) = directives.iter().find(|d| d.name == "default-src") else {
        return false;
    };
    let mut copy = Directive { name: added.to_string(), sources: fallback.sources.clone() };
    if allows(&copy, hash) {
        return false;
    }
    grant(&mut copy, hash);
    directives.push(copy);
    true
}

/// Whether a directive already lets the tracker through: by its hash, under
/// `'strict-dynamic'`, and by the site's own origin otherwise.
fn allows(directive: &Directive, hash: Option<&str>) -> bool {
    let lower: Vec<String> = directive.sources.iter().map(|s| s.to_lowercase()).collect();

    if let Some(hash) = hash {
        if lower.iter().any(|s| s == "'strict-dynamic'") {
            return directive.sources.contains(&format!("'{hash}'"));
        }
    }

    lower.iter().any(|s| s == "'self'" || s == "*")
}

/// Make a directive let the tracker through, in whichever way it needs.
fn grant(directive: &mut Directive, hash: Option<&str>) {
    if let Some(hash) = hash {
        if directive.sources.iter().any(|s| s.eq_ignore_ascii_case("'strict-dynamic'")) {
            directive.sources.push(format!("'{hash}'"));
            return;
        }
    }

    directive.sources.retain(|s| !s.eq_ignore_ascii_case("'none'"));
    directive.sources.push("'self'".into());
}

fn uses_strict_dynamic(policy: &str) -> bool {
    policy.to_lowercase().contains("'strict-dynamic'")
}

fn escape_attribute(value: &str) -> String {
    value.replace('&', "&amp;").replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;

    const HASH: &str = "sha384-abc";

    fn inject(html: &str) -> Option<String> {
        inject_analytics(html.as_bytes(), HASH).map(|bytes| String::from_utf8(bytes).unwrap())
    }

    #[test]
    fn places_the_tag() {
        let tag = r#"<script type="module" src="/.npanel/script.js"></script>"#;
        assert_eq!(inject("<html><head><title>x</title></HEAD><body></body></html>").unwrap(), format!("<html><head><title>x</title>{tag}</HEAD><body></body></html>"));
        assert_eq!(inject("<p>hi</p><BODY class=x>").unwrap(), format!("<p>hi</p>{tag}<BODY class=x>"));
        assert_eq!(inject("<!DOCTYPE html><p>x").unwrap(), format!("<!DOCTYPE html>{tag}<p>x"));
        assert_eq!(inject("<html lang=en><p>x").unwrap(), format!("<html lang=en>{tag}<p>x"));
        assert_eq!(inject("<div>fragment</div>"), None);
    }

    #[test]
    fn keeps_other_bytes() {
        let page = b"<head>\xff\xfe caf\xe9</head>".to_vec();
        let out = inject_analytics(&page, HASH).unwrap();
        assert!(out.starts_with(b"<head>\xff\xfe caf\xe9<script"));
    }

    #[test]
    fn widens_meta_policies() {
        let out = inject(r#"<head><meta http-equiv="Content-Security-Policy" content="default-src 'none'; img-src *"></head>"#).unwrap();
        assert!(out.contains(r#"content="default-src 'none'; img-src *; script-src 'self'; connect-src 'self'""#), "{out}");

        let out = inject(r#"<head><meta http-equiv="content-security-policy" content="script-src 'self'; connect-src https:"></head>"#).unwrap();
        assert!(out.contains(r#"content="script-src 'self'; connect-src https: 'self'""#), "{out}");

        let out = inject(r#"<head><meta http-equiv="content-security-policy" content="script-src 'strict-dynamic' 'nonce-x'"></head>"#).unwrap();
        assert!(out.contains(&format!("script-src 'strict-dynamic' 'nonce-x' '{HASH}'")), "{out}");
        assert!(out.contains(&format!(r#"integrity="{HASH}""#)), "{out}");
    }

    #[test]
    fn leaves_permissive_policies() {
        assert_eq!(allow_analytics("default-src 'self'", HASH), "default-src 'self'");
        assert_eq!(allow_analytics("img-src *", HASH), "img-src *");
        assert_eq!(allow_analytics("script-src 'none', connect-src *", HASH), "script-src 'self', connect-src *");
    }
}
