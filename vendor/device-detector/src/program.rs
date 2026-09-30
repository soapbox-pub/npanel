//! A compiled Matomo pattern, and what it captured.
//!
//! Matomo runs its patterns with PCRE in byte mode: case folding, `\d`, `\w`
//! and friends are ASCII, and `.` is any byte. The `regex` crate's byte-mode
//! engine with Unicode off reads them the same way, and compiles them to
//! programs a fraction the size of the Unicode `&str` ones — which matters,
//! since a long-running detector compiles thousands. Only patterns that need
//! lookaround or backreferences go to `fancy_regex`, gated by a
//! lookaround-free superset so its backtracking VM rarely runs at all.

use std::ops::Range;
use std::sync::OnceLock;

use crate::parser::MATOMO_BOUNDARY_PREFIX;

pub(crate) enum Program {
    Bytes(regex::bytes::Regex),
    /// Compiled only once its gate has let a string through: most of these
    /// are long brand patterns, a few hundred kilobytes each as `fancy_regex`
    /// builds them, and most user agents that reach them fail the gate.
    Fancy { pattern: String, regex: OnceLock<Option<fancy_regex::Regex>>, gate: Option<regex::bytes::Regex> },
}

/// What a match captured, by group, as byte ranges of the text.
pub(crate) struct Caps<'t> {
    text: &'t str,
    groups: Vec<Option<Range<usize>>>,
}

impl<'t> Caps<'t> {
    /// A group's text. A byte-mode group that ends inside a multi-byte
    /// character has no text of its own, and reads as absent.
    pub fn get(&self, group: usize) -> Option<&'t str> {
        self.groups.get(group)?.clone().and_then(|range| self.text.get(range))
    }
}

impl Program {
    /// Compile `pattern` behind Matomo's boundary prefix, case-insensitively.
    pub fn compile(pattern: &str) -> Option<Program> {
        let full = format!("{MATOMO_BOUNDARY_PREFIX}(?i:{pattern})");
        if let Ok(regex) = regex::bytes::RegexBuilder::new(&full).unicode(false).build() {
            return Some(Program::Bytes(regex));
        }
        let fancy = format!("{MATOMO_BOUNDARY_PREFIX}(?i:{})", crate::gate::ascii_classes(pattern));
        fancy_regex::Expr::parse_tree(&fancy).ok()?;
        Some(Program::Fancy { pattern: fancy, regex: OnceLock::new(), gate: crate::gate::gate(pattern) })
    }

    /// The fancy program behind a gate that has passed.
    fn fancy<'a>(pattern: &str, regex: &'a OnceLock<Option<fancy_regex::Regex>>) -> Option<&'a fancy_regex::Regex> {
        regex.get_or_init(|| fancy_regex::Regex::new(pattern).ok()).as_ref()
    }

    pub fn is_match(&self, text: &str) -> bool {
        match self {
            Program::Bytes(regex) => regex.is_match(text.as_bytes()),
            Program::Fancy { pattern, regex, gate } => {
                gate.as_ref().is_none_or(|gate| gate.is_match(text.as_bytes()))
                    && Self::fancy(pattern, regex).is_some_and(|regex| regex.is_match(text).unwrap_or(false))
            }
        }
    }

    pub fn captures<'t>(&self, text: &'t str) -> Option<Caps<'t>> {
        let groups = match self {
            Program::Bytes(regex) => {
                let caps = regex.captures(text.as_bytes())?;
                caps.iter().map(|m| m.map(|m| m.range())).collect()
            }
            Program::Fancy { pattern, regex, gate } => {
                if gate.as_ref().is_some_and(|gate| !gate.is_match(text.as_bytes())) {
                    return None;
                }
                let caps = Self::fancy(pattern, regex)?.captures(text).ok().flatten()?;
                caps.iter().map(|m| m.map(|m| m.range())).collect()
            }
        };
        Some(Caps { text, groups })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compiles_to_bytes_where_it_can() {
        assert!(matches!(Program::compile(r"Chrome/(\d+[\.\d]+)"), Some(Program::Bytes(_))));
        assert!(matches!(Program::compile(r"Galaxy(?! Tab)"), Some(Program::Fancy { .. })));
    }

    #[test]
    fn matches_like_pcre() {
        let chrome = Program::compile(r"Chrome/(\d+[\.\d]+)").unwrap();
        let caps = chrome.captures("Mozilla/5.0 CHROME/128.0.1 Safari").unwrap();
        assert_eq!(caps.get(1), Some("128.0.1"));
        // The boundary prefix: not preceded by a letter or digit.
        assert!(chrome.captures("xChrome/1").is_none());

        let tab = Program::compile(r"Galaxy(?! Tab)").unwrap();
        assert!(tab.is_match("Samsung Galaxy S9"));
        assert!(!tab.is_match("Samsung Galaxy Tab"));
    }
}
