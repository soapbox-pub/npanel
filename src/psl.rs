//! The Public Suffix List, ICANN section only.
//!
//! This is the question `tldts` answered for the TypeScript npanel, answered
//! the same way: private suffixes like `github.io` are not suffixes (so
//! `foo.github.io` sits under the registrable domain `github.io`), and a name
//! under a TLD the list doesn't know takes the implicit `*` rule, so
//! `foo.localhost` is a registrable domain of its own. An IP address has no
//! domain at all.

use std::collections::HashSet;
use std::net::IpAddr;
use std::sync::OnceLock;

const LIST: &str = include_str!("../assets/public_suffix_list_icann.dat");

struct Rules {
    exact: HashSet<String>,
    wildcard: HashSet<String>,
    exception: HashSet<String>,
}

fn rules() -> &'static Rules {
    static RULES: OnceLock<Rules> = OnceLock::new();
    RULES.get_or_init(|| {
        let mut rules = Rules { exact: HashSet::new(), wildcard: HashSet::new(), exception: HashSet::new() };

        for line in LIST.lines() {
            let rule = line.split_whitespace().next().unwrap_or("");
            if rule.is_empty() || rule.starts_with("//") {
                continue;
            }

            // Rules are published in Unicode; hostnames arrive as punycode.
            let (set, name) = if let Some(rest) = rule.strip_prefix('!') {
                (&mut rules.exception, rest)
            } else if let Some(rest) = rule.strip_prefix("*.") {
                (&mut rules.wildcard, rest)
            } else {
                (&mut rules.exact, rule)
            };
            let ascii = idna::domain_to_ascii(name).unwrap_or_else(|_| name.to_lowercase());
            set.insert(ascii);
        }

        rules
    })
}

/// A hostname split around its registrable domain.
#[derive(Debug, PartialEq, Eq)]
pub struct Parsed<'a> {
    /// The registrable domain, eg `example.co.uk`.
    pub domain: &'a str,
    /// What sits in front of it, eg `docs` — empty for the domain itself.
    pub subdomain: &'a str,
}

/// Split `hostname` around its registrable domain, or `None` when it has none:
/// an IP address, or a name that is itself a public suffix.
pub fn parse(hostname: &str) -> Option<Parsed<'_>> {
    let host = hostname.strip_suffix('.').unwrap_or(hostname);
    if host.is_empty() || host.trim_matches(['[', ']']).parse::<IpAddr>().is_ok() {
        return None;
    }

    let suffix_labels = suffix_len(host);
    let labels: Vec<&str> = host.split('.').collect();
    if labels.len() <= suffix_labels {
        return None;
    }

    // Byte offset where the registrable domain starts.
    let domain_labels = suffix_labels + 1;
    let skip = labels.len() - domain_labels;
    let start: usize = labels[..skip].iter().map(|label| label.len() + 1).sum();
    let domain = &host[start..];
    let subdomain = if start == 0 { "" } else { &host[..start - 1] };

    Some(Parsed { domain, subdomain })
}

/// The registrable domain of `hostname`, if it has one.
pub fn domain(hostname: &str) -> Option<&str> {
    parse(hostname).map(|parsed| parsed.domain)
}

/// How many labels of `host`, counted from the right, are its public suffix.
fn suffix_len(host: &str) -> usize {
    let rules = rules();
    let lower = host.to_ascii_lowercase();
    let labels: Vec<&str> = lower.split('.').collect();

    // The implicit `*` rule: every TLD is a suffix.
    let mut best = 1;

    for take in 1..=labels.len() {
        let candidate = labels[labels.len() - take..].join(".");

        if rules.exception.contains(&candidate) {
            // An exception names a registrable domain, one label past its suffix.
            return take - 1;
        }
        if rules.exact.contains(&candidate) {
            best = best.max(take);
        }
        if take < labels.len() && rules.wildcard.contains(&candidate) {
            best = best.max(take + 1);
        }
    }

    best
}

#[cfg(test)]
mod tests {
    use super::*;

    fn split(host: &str) -> Option<(&str, &str)> {
        parse(host).map(|p| (p.domain, p.subdomain))
    }

    /// Every case here is what `tldts.parse` answers.
    #[test]
    fn matches_tldts() {
        assert_eq!(split("example.com"), Some(("example.com", "")));
        assert_eq!(split("docs.example.com"), Some(("example.com", "docs")));
        assert_eq!(split("example.co.uk"), Some(("example.co.uk", "")));
        assert_eq!(split("foo.github.io"), Some(("github.io", "foo")));
        assert_eq!(split("github.io"), Some(("github.io", "")));
        assert_eq!(split("localhost"), None);
        assert_eq!(split("foo.localhost"), Some(("foo.localhost", "")));
        assert_eq!(split("a.b.localhost"), Some(("b.localhost", "a")));
        assert_eq!(split("127.0.0.1"), None);
        assert_eq!(split("x.example.unknowntld"), Some(("example.unknowntld", "x")));
        assert_eq!(split("com"), None);
        assert_eq!(split("foo.blogspot.com"), Some(("blogspot.com", "foo")));
        assert_eq!(split("abc.onion"), Some(("abc.onion", "")));
        assert_eq!(split("example.com."), Some(("example.com", "")));
        assert_eq!(split("foo.s3.amazonaws.com"), Some(("amazonaws.com", "foo.s3")));
        assert_eq!(split("foo.bar.ck"), Some(("foo.bar.ck", "")));
        assert_eq!(split("www.ck"), Some(("www.ck", "")));
        assert_eq!(split("city.kawasaki.jp"), Some(("city.kawasaki.jp", "")));
        assert_eq!(split("foo.city.kawasaki.jp"), Some(("city.kawasaki.jp", "foo")));
    }
}
