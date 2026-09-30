//! A fast pre-check for regexes that need `fancy_regex`.
//!
//! Matomo's patterns use lookaround in places, which puts the whole pattern
//! on `fancy_regex`'s backtracking VM — around a hundred times slower than the
//! `regex` crate's automata, and run against every candidate brand of every
//! user agent. Dropping the lookarounds gives a pattern the `regex` crate can
//! run that matches everything the original does and more, so when it doesn't
//! match, the original can't either.

/// A `regex`-crate gate for `pattern`, or `None` when the pattern needs no
/// gate (it is plain enough for `fancy_regex` to hand to `regex` itself) or
/// cannot be relaxed into one.
pub(crate) fn gate(pattern: &str) -> Option<regex::bytes::Regex> {
    let relaxed = relax(pattern)?;
    if relaxed == pattern {
        return None;
    }
    regex::bytes::RegexBuilder::new(&format!("(?i:{relaxed})")).unicode(false).build().ok()
}

/// `pattern` with every lookaround removed, or `None` if it uses something
/// with no superset the `regex` crate understands, like a backreference.
fn relax(pattern: &str) -> Option<String> {
    let bytes = pattern.as_bytes();
    let mut out = String::with_capacity(pattern.len());
    let mut i = 0;
    let mut in_class = false;

    while i < bytes.len() {
        let c = bytes[i];

        if c == b'\\' {
            let next = *bytes.get(i + 1)?;
            if !in_class && next.is_ascii_digit() && next != b'0' {
                return None;
            }
            out.push_str(&pattern[i..i + 2 + utf8_extra(next)]);
            i += 2 + utf8_extra(next);
            continue;
        }

        if in_class {
            if c == b']' {
                in_class = false;
            }
            let len = 1 + utf8_extra(c);
            out.push_str(&pattern[i..i + len]);
            i += len;
            continue;
        }

        if c == b'[' {
            in_class = true;
            out.push('[');
            i += 1;
            // A `]` or `^]` right after the opening bracket is a literal.
            if bytes.get(i) == Some(&b'^') {
                out.push('^');
                i += 1;
            }
            if bytes.get(i) == Some(&b']') {
                out.push(']');
                i += 1;
            }
            continue;
        }

        if c == b'(' && is_lookaround(&bytes[i..]) {
            i = skip_group(bytes, i)?;
            continue;
        }

        let len = 1 + utf8_extra(c);
        out.push_str(&pattern[i..i + len]);
        i += len;
    }

    Some(out)
}

fn is_lookaround(rest: &[u8]) -> bool {
    rest.starts_with(b"(?=") || rest.starts_with(b"(?!") || rest.starts_with(b"(?<=") || rest.starts_with(b"(?<!")
}

/// The index just past the group opening at `start`.
fn skip_group(bytes: &[u8], start: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut i = start;
    let mut in_class = false;

    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 1,
            b'[' if !in_class => in_class = true,
            b']' if in_class => in_class = false,
            b'(' if !in_class => depth += 1,
            b')' if !in_class => {
                depth -= 1;
                if depth == 0 {
                    return Some(i + 1);
                }
            }
            _ => {}
        }
        i += 1;
    }

    None
}

/// Continuation bytes after a UTF-8 lead byte.
fn utf8_extra(lead: u8) -> usize {
    match lead {
        0xF0..=0xFF => 3,
        0xE0..=0xEF => 2,
        0xC0..=0xDF => 1,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removes_lookarounds() {
        assert_eq!(relax("SM-(?!T)[A-Z]").as_deref(), Some("SM-[A-Z]"));
        assert_eq!(relax("(?<!Opera )Mini(?= Build)").as_deref(), Some("Mini"));
        assert_eq!(relax("a(?!(b|c))d").as_deref(), Some("ad"));
    }

    #[test]
    fn keeps_classes_and_escapes() {
        assert_eq!(relax(r"[(?!]x\(?!y").as_deref(), Some(r"[(?!]x\(?!y"));
        assert_eq!(relax(r"[]a]").as_deref(), Some(r"[]a]"));
    }

    #[test]
    fn refuses_backreferences() {
        assert_eq!(relax(r"(a)\1"), None);
    }

    #[test]
    fn gate_is_a_superset() {
        let g = gate("Galaxy(?! Tab)").unwrap();
        assert!(g.is_match(b"galaxy tab"));
        assert!(gate("Plain").is_none());
    }
}

/// `pattern` with its Perl classes spelled out in ASCII, as PCRE reads them
/// without the `u` flag — which is how Matomo's PHP runs these patterns.
///
/// The `regex` crate's Unicode `\d`, `\w` and `\s` are hundreds of ranges
/// each, and a pattern using them compiles to a program a hundred times the
/// size; across the database that is most of the detector's memory.
pub(crate) fn ascii_classes(pattern: &str) -> String {
    let bytes = pattern.as_bytes();
    let mut out = String::with_capacity(pattern.len() + 16);
    let mut i = 0;
    let mut in_class = false;

    while i < bytes.len() {
        let c = bytes[i];

        if c == b'\\' && i + 1 < bytes.len() {
            let next = bytes[i + 1];
            let replacement = match (in_class, next) {
                (false, b'd') => Some("[0-9]"),
                (false, b'D') => Some("[^0-9]"),
                (false, b'w') => Some("[0-9A-Za-z_]"),
                (false, b'W') => Some("[^0-9A-Za-z_]"),
                (false, b's') => Some(r"[\t\n\x0B\f\r ]"),
                (false, b'S') => Some(r"[^\t\n\x0B\f\r ]"),
                (false, b'b') => Some(r"(?-u:\b)"),
                (false, b'B') => Some(r"(?-u:\B)"),
                (true, b'd') => Some("0-9"),
                (true, b'w') => Some("0-9A-Za-z_"),
                (true, b's') => Some(r"\t\n\x0B\f\r "),
                _ => None,
            };
            match replacement {
                Some(text) => out.push_str(text),
                None => {
                    let len = 1 + utf8_extra(next);
                    out.push('\\');
                    out.push_str(&pattern[i + 1..i + 1 + len]);
                    i += len - 1;
                }
            }
            i += 2;
            continue;
        }

        if in_class {
            if c == b']' {
                in_class = false;
            }
        } else if c == b'[' {
            in_class = true;
            let len = 1 + usize::from(bytes.get(i + 1) == Some(&b'^'));
            out.push_str(&pattern[i..i + len]);
            i += len;
            // A `]` right after the opening bracket is a literal.
            if bytes.get(i) == Some(&b']') {
                out.push(']');
                i += 1;
            }
            continue;
        }

        let len = 1 + utf8_extra(c);
        out.push_str(&pattern[i..i + len]);
        i += len;
    }

    out
}

#[cfg(test)]
mod ascii_tests {
    use super::ascii_classes;

    #[test]
    fn spells_out_classes() {
        assert_eq!(ascii_classes(r"(\d+)[\d\.]+\w"), r"([0-9]+)[0-9\.]+[0-9A-Za-z_]");
        assert_eq!(ascii_classes(r"\\d"), r"\\d");
        assert_eq!(ascii_classes(r"[]\d]"), r"[]0-9]");
    }
}
