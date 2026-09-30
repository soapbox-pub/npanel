//! Adding npanel's scripts to a site's pages — the analytics tracker and
//! `window.ENV` — once, when the host is built.

use std::sync::OnceLock;

use regex::bytes::Regex;
use sha2::{Digest, Sha256};

use crate::html::decode_attribute;

/// Largest document a script is added to. A page bigger than this is almost
/// certainly a generated dump, and is served as published.
pub const MAX_INJECT_BYTES: u64 = 4 * 1024 * 1024;

/// The Expat entry's magnet in LibreJS's license_definitions.json, which is
/// all it checks. Every script npanel adds is wrapped in a license block
/// naming it, or a visitor running the extension would have it blocked.
pub const EXPAT_MAGNET: &str = "magnet:?xt=urn:btih:d3d9a9a6595521f9666a5e94cc830dab83b65699&dn=expat.txt";

/// What a page's tag loads for a script npanel adds: its path, versioned by the
/// script's contents.
///
/// The path serves the script whatever the query says; the version is there
/// so that a change is seen at once. Scripts are cached for an hour like any
/// other asset, but the pages naming them revalidate on every request, and
/// each change names a URL no cache has seen.
pub fn versioned_src(path: &str, body: &[u8]) -> String {
    format!("{path}?v={}", &hex::encode(Sha256::digest(body))[..16])
}

/// Where in a page a script goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Placement {
    /// The first thing in the head, so it has run before anything the site
    /// wrote starts — and before any `<base>` can move what its path means.
    Start,
    /// The end of the head, out of the way of everything the site wrote.
    End,
}

/// A script npanel adds to a site's pages, served from the site's own origin.
pub struct Script<'a> {
    /// The path the tag loads.
    pub src: &'a str,
    /// `sha384-…` of the script, for a policy that trusts scripts by hash alone.
    pub integrity: &'a str,
    pub module: bool,
    pub placement: Placement,
    /// Whether it `fetch`es its own origin, and so needs `connect-src` as well.
    pub connects: bool,
}

/// A page with the script added, or `None` for one that isn't a document.
///
/// Edited as bytes rather than parsed and re-serialized, so everything the
/// site wrote comes back exactly as it was apart from the one tag, whatever its
/// encoding: everything written into it is ASCII.
///
/// At the end, the tag goes before `</head>`, and failing that where the head
/// would have ended — before `<body>`, after `<html>`, or after the doctype.
/// At the start, it goes after `<head>`, and failing that where the head
/// would have begun — after `<html>` or the doctype, or before `<body>`. A
/// file with none of those is a fragment some page fetches and inserts, and a
/// script tag inserted that way never runs anyway, so it is left alone.
///
/// Any `<meta>` Content Security Policy in the head is widened by
/// [`allow_script`], since otherwise the policy the site wrote would stop the
/// script npanel added.
pub fn inject_script(html: &[u8], script: &Script) -> Option<Vec<u8>> {
    let lower = html.to_ascii_lowercase();
    let head_end = head_end(&lower);
    let at = match script.placement {
        Placement::End => head_end?,
        Placement::Start => head_start(&lower)?,
    };
    // A tag at the start of the head comes before the policies in it, which
    // still have to let it through: npanel sends a declared policy as a
    // header, and a header applies to the whole page.
    let end = head_end.unwrap_or(html.len()).max(at);

    let (before, strict_before) = allow_meta_policies(&html[..at], script);
    let (after, strict_after) = allow_meta_policies(&html[at..end], script);

    // Only a policy that trusts by hash alone needs the tag to carry one: a
    // hash pins the script's exact bytes, so a browser still holding the last
    // build's copy would refuse this one.
    let kind = if script.module { r#" type="module""# } else { "" };
    let integrity = if strict_before || strict_after { format!(r#" integrity="{}""#, script.integrity) } else { String::new() };
    let tag = format!(r#"<script{kind} src="{}"{integrity}></script>"#, script.src);

    let mut out = Vec::with_capacity(html.len() + tag.len() + 64);
    out.extend_from_slice(&before);
    out.extend_from_slice(tag.as_bytes());
    out.extend_from_slice(&after);
    out.extend_from_slice(&html[end..]);
    Some(out)
}

/// Whitespace as HTML's tokenizer reads it between a tag's name and its
/// attributes.
fn is_space(byte: u8) -> bool {
    matches!(byte, b'\t' | b'\n' | b'\x0C' | b'\r' | b' ')
}

/// One piece of markup a page's tokenizer would see.
#[derive(Debug, PartialEq, Eq)]
enum Token {
    /// `<name …>`, by its lowercased name.
    Start(Vec<u8>),
    /// `</name …>`.
    End(Vec<u8>),
    /// `<!doctype …>`.
    Doctype,
}

/// The tags of a document, from its lowercased bytes, with where each begins
/// and ends — found the way a browser's tokenizer finds them, so a `</head>`
/// in a comment or an inline script is not the end of the head, `</header>`
/// is not `</head>`, and a `>` inside a quoted attribute doesn't end its tag.
///
/// Not a parser: nothing is built from the tags, and no tag is implied. Only
/// what the tokenizer decides matters here, and all of it is ASCII, so the
/// site's own bytes need no decoding.
fn tags(lower: &[u8]) -> Vec<(usize, usize, Token)> {
    let mut tags = Vec::new();
    let mut at = 0;

    while let Some(offset) = lower[at..].iter().position(|&b| b == b'<') {
        let start = at + offset;
        let rest = &lower[start..];

        if rest.starts_with(b"<!--") {
            at = find(&lower[start + 4..], b"-->").map_or(lower.len(), |end| start + 4 + end + 3);
            continue;
        }
        if rest.starts_with(b"<!") || rest.starts_with(b"<?") {
            let end = find(rest, b">").map_or(lower.len(), |end| start + end + 1);
            if rest.starts_with(b"<!doctype") {
                tags.push((start, end, Token::Doctype));
            }
            at = end;
            continue;
        }

        let closing = rest.get(1) == Some(&b'/');
        let name_start = start + 1 + usize::from(closing);
        if !lower.get(name_start).is_some_and(u8::is_ascii_alphabetic) {
            at = start + 1;
            continue;
        }
        let name_end = lower[name_start..].iter().position(|&b| is_space(b) || b == b'/' || b == b'>').map_or(lower.len(), |n| name_start + n);
        let name = lower[name_start..name_end].to_vec();

        // Attribute values may hold a `>` when quoted.
        let mut end = name_end;
        let mut quote = None;
        while end < lower.len() {
            match (quote, lower[end]) {
                (None, b'>') => break,
                (None, q @ (b'"' | b'\'')) if !closing => quote = Some(q),
                (Some(q), b) if b == q => quote = None,
                _ => {}
            }
            end += 1;
        }
        end = (end + 1).min(lower.len());

        if closing {
            tags.push((start, end, Token::End(name)));
            at = end;
            continue;
        }

        // The text of these is not markup, whatever it looks like.
        let raw = matches!(name.as_slice(), b"script" | b"style" | b"textarea" | b"title" | b"xmp" | b"iframe" | b"noembed" | b"noframes");
        let closer = [b"</".as_slice(), &name].concat();
        tags.push((start, end, Token::Start(name)));
        at = end;
        if raw {
            let mut search = end;
            at = lower.len();
            while let Some(found) = find(&lower[search..], &closer) {
                let after = search + found + closer.len();
                if lower.get(after).is_none_or(|&b| is_space(b) || b == b'/' || b == b'>') {
                    at = search + found;
                    break;
                }
                search = after;
            }
        }
    }

    tags
}

/// Where the head begins, from a document's lowercased bytes: after `<head>`,
/// or failing that after `<html>` or the doctype, or before `<body>`.
fn head_start(lower: &[u8]) -> Option<usize> {
    let tags = tags(lower);
    let start = |name: &[u8]| tags.iter().find(|(_, _, t)| *t == Token::Start(name.to_vec()));

    start(b"head")
        .or_else(|| start(b"html"))
        .or_else(|| tags.iter().find(|(_, _, t)| *t == Token::Doctype))
        .map(|(_, end, _)| *end)
        .or_else(|| start(b"body").map(|(at, _, _)| *at))
}

/// Where the head ends, from a document's lowercased bytes: before `</head>`,
/// or failing that before `<body>`, or after `<html>` or the doctype.
fn head_end(lower: &[u8]) -> Option<usize> {
    let tags = tags(lower);
    let find_tag = |token: Token| tags.iter().find(|(_, _, t)| *t == token);

    find_tag(Token::End(b"head".to_vec()))
        .or_else(|| find_tag(Token::Start(b"body".to_vec())))
        .map(|(at, _, _)| *at)
        .or_else(|| find_tag(Token::Start(b"html".to_vec())).or_else(|| find_tag(Token::Doctype)).map(|(_, end, _)| *end))
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|window| window == needle)
}

/// Every `<meta http-equiv="Content-Security-Policy">` in `head`, widened.
/// Also reports whether any of them trusts scripts by hash alone.
fn allow_meta_policies(head: &[u8], script: &Script) -> (Vec<u8>, bool) {
    static ATTRIBUTE: OnceLock<Regex> = OnceLock::new();
    let attribute = ATTRIBUTE.get_or_init(|| {
        Regex::new(r#"(?-u)([^\t\n\x0C\r "'=/>]+)(?:[\t\n\x0C\r ]*=[\t\n\x0C\r ]*(?:"([^"]*)"|'([^']*)'|([^\t\n\x0C\r "'>]+)))?"#)
            .expect("regex")
    });

    let mut strict_dynamic = false;
    let mut out = Vec::with_capacity(head.len());
    let mut last = 0;

    let metas = tags(&head.to_ascii_lowercase()).into_iter().filter(|(_, _, token)| *token == Token::Start(b"meta".to_vec()));
    for (start, end, _) in metas {
        let tag = &head[start..end];
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

        let policy = match (http_equiv.as_deref(), content) {
            (Some("content-security-policy"), Some((value, from, to))) => Some((decode_attribute(&value), from, to)),
            _ => None,
        };
        // Nothing a policy can say is outside printable ASCII, and anything
        // that is would not survive being written back.
        let policy = policy.filter(|(policy, ..)| policy.chars().all(|c| c == '\t' || (' '..='~').contains(&c)));
        strict_dynamic |= policy.as_ref().is_some_and(|(policy, ..)| uses_strict_dynamic(policy));
        let rewritten = policy.and_then(|(policy, from, to)| {
            let widened = allow_script(&policy, script);
            (widened != policy).then(|| [&tag[..from], format!("content=\"{}\"", escape_attribute(&widened)).as_bytes(), &tag[to..]].concat())
        });

        out.extend_from_slice(&head[last..start]);
        out.extend_from_slice(rewritten.as_deref().unwrap_or(tag));
        last = end;
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

/// A Content Security Policy that lets the script load — and, if it
/// [`connects`](Script::connects), report — and is otherwise exactly what the
/// site wrote.
///
/// Up to two things are needed from the site's own origin: loading a script,
/// and `fetch`ing to it. Each is granted by adding `'self'` to the directive that
/// governs it — `script-src-elem` and `script-src` where the site wrote them,
/// `connect-src` likewise, and a copy of `default-src` where it relied on the
/// fallback, so nothing that fallback allowed is taken away. `'none'` gives
/// way to `'self'`, and a directive that already allows the origin is
/// untouched.
///
/// The one policy `'self'` does nothing for uses `'strict-dynamic'`, which
/// trusts scripts by nonce or hash alone. There the script's hash is added
/// instead — only there, since a hash in a policy relying on `'unsafe-inline'`
/// would switch that off.
///
/// `policy` may be several policies joined with commas; each is widened on
/// its own.
pub fn allow_script(policy: &str, script: &Script) -> String {
    let policies: Vec<&str> = policy.split(',').map(str::trim).collect();
    let widened: Vec<String> = policies.iter().map(|one| allow_in_policy(one, script)).collect();

    if widened.iter().zip(&policies).all(|(a, b)| a == b) {
        policy.to_string()
    } else {
        widened.join(", ")
    }
}

fn allow_in_policy(policy: &str, script: &Script) -> String {
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
    changed |= widen(&mut directives, &["script-src-elem", "script-src"], "script-src", Some(script.integrity));
    if script.connects {
        changed |= widen(&mut directives, &["connect-src"], "connect-src", None);
    }

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

/// Whether a directive already lets the script through: by its hash, under
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

/// Make a directive let the script through, in whichever way it needs.
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

    const TRACKER: Script = Script { src: "/.npanel/script.js?v=1", integrity: HASH, module: true, placement: Placement::End, connects: true };
    const ENV: Script = Script { src: "/.npanel/env.js?v=1", integrity: HASH, module: false, placement: Placement::Start, connects: false };

    fn inject(html: &str) -> Option<String> {
        inject_with(html, &TRACKER)
    }

    fn inject_with(html: &str, script: &Script) -> Option<String> {
        inject_script(html.as_bytes(), script).map(|bytes| String::from_utf8(bytes).unwrap())
    }

    #[test]
    fn places_the_tag() {
        let tag = r#"<script type="module" src="/.npanel/script.js?v=1"></script>"#;
        assert_eq!(inject("<html><head><title>x</title></HEAD><body></body></html>").unwrap(), format!("<html><head><title>x</title>{tag}</HEAD><body></body></html>"));
        assert_eq!(inject("<p>hi</p><BODY class=x>").unwrap(), format!("<p>hi</p>{tag}<BODY class=x>"));
        assert_eq!(inject("<!DOCTYPE html><p>x").unwrap(), format!("<!DOCTYPE html>{tag}<p>x"));
        assert_eq!(inject("<html lang=en><p>x").unwrap(), format!("<html lang=en>{tag}<p>x"));
        assert_eq!(inject("<div>fragment</div>"), None);
    }

    #[test]
    fn places_the_tag_first() {
        let tag = r#"<script src="/.npanel/env.js?v=1"></script>"#;
        let env = |html: &str| inject_with(html, &ENV);
        assert_eq!(
            env("<!doctype html><html><HEAD lang=en><script src=/app.js></script></head><body></body></html>").unwrap(),
            format!("<!doctype html><html><HEAD lang=en>{tag}<script src=/app.js></script></head><body></body></html>")
        );
        assert_eq!(env("<html><header>x</header>").unwrap(), format!("<html>{tag}<header>x</header>"));
        assert_eq!(env("<!DOCTYPE html><script src=/a.js></script>").unwrap(), format!("<!DOCTYPE html>{tag}<script src=/a.js></script>"));
        assert_eq!(env("<p>hi</p><body>").unwrap(), format!("<p>hi</p>{tag}<body>"));
        assert_eq!(env("<div>fragment</div>"), None);
    }

    #[test]
    fn finds_tags_as_a_browser_does() {
        let tag = r#"<script type="module" src="/.npanel/script.js?v=1"></script>"#;
        // `</header>` is not `</head>`, and `<html>` is one tag, not two.
        assert_eq!(inject("<html><header>x</header><p>y").unwrap(), format!("<html>{tag}<header>x</header><p>y"));
        assert_eq!(inject("<html><p>x").unwrap(), format!("<html>{tag}<p>x"));
        // A `</head>` in a comment or a script is text, not the end of the head.
        assert_eq!(
            inject(r#"<head><!-- </head> --><script>document.write("</head>")</script></head>"#).unwrap(),
            format!(r#"<head><!-- </head> --><script>document.write("</head>")</script>{tag}</head>"#)
        );
        // A quoted `>` doesn't end a tag.
        assert_eq!(inject(r#"<html data-x="a>b"><body>"#).unwrap(), format!(r#"<html data-x="a>b">{tag}<body>"#));
        // No-break space is not HTML whitespace, so this is no head.
        assert_eq!(inject_with("<head\u{a0}x><p>", &ENV), None);
    }

    #[test]
    fn keeps_other_bytes() {
        let page = b"<head>\xff\xfe caf\xe9</head>".to_vec();
        let out = inject_script(&page, &TRACKER).unwrap();
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
    fn widens_policies_after_the_tag() {
        let out = inject_with(r#"<head><meta http-equiv="Content-Security-Policy" content="default-src 'none'"></head><body><meta http-equiv="Content-Security-Policy" content="default-src 'none'">"#, &ENV).unwrap();
        assert_eq!(
            out,
            r#"<head><script src="/.npanel/env.js?v=1"></script><meta http-equiv="Content-Security-Policy" content="default-src 'none'; script-src 'self'"></head><body><meta http-equiv="Content-Security-Policy" content="default-src 'none'">"#
        );

        let out = inject_with(r#"<head><meta http-equiv="content-security-policy" content="script-src 'strict-dynamic'"></head>"#, &ENV).unwrap();
        assert!(out.starts_with(&format!(r#"<head><script src="/.npanel/env.js?v=1" integrity="{HASH}"></script>"#)), "{out}");
        assert!(out.contains(&format!("script-src 'strict-dynamic' '{HASH}'")), "{out}");
    }

    #[test]
    fn versions_sources() {
        let src = versioned_src("/.npanel/script.js", b"one");
        assert!(src.starts_with("/.npanel/script.js?v="));
        assert_eq!(src.len(), "/.npanel/script.js?v=".len() + 16);
        assert_eq!(versioned_src("/.npanel/script.js", b"one"), src);
        assert_ne!(versioned_src("/.npanel/script.js", b"two"), src);
    }

    #[test]
    fn leaves_permissive_policies() {
        assert_eq!(allow_script("default-src 'self'", &TRACKER), "default-src 'self'");
        assert_eq!(allow_script("img-src *", &TRACKER), "img-src *");
        assert_eq!(allow_script("script-src 'none', connect-src *", &TRACKER), "script-src 'self', connect-src *");
        assert_eq!(allow_script("default-src 'none'; script-src 'self'", &ENV), "default-src 'none'; script-src 'self'");
    }
}
