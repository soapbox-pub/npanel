use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::{OnceLock, RwLock};

/// Simple semver-ish comparison: is `a < b`?  Compares dot-separated numeric
/// components left to right (missing components treated as 0).
pub(crate) fn version_lt(a: &str, b: &str) -> bool {
    let mut ai = a.split('.');
    let mut bi = b.split('.');
    loop {
        match (ai.next(), bi.next()) {
            (None, None) => return false,
            (None, Some(bv)) => return bv.parse::<u32>().unwrap_or(0) > 0,
            (Some(_), None) => return false,
            (Some(av), Some(bv)) => {
                let an = av.parse::<u32>().unwrap_or(0);
                let bn = bv.parse::<u32>().unwrap_or(0);
                if an < bn {
                    return true;
                }
                if an > bn {
                    return false;
                }
            }
        }
    }
}

/// Simple semver-ish comparison: is `a >= b`?
pub(crate) fn version_ge(a: &str, b: &str) -> bool {
    !version_lt(a, b)
}

pub(crate) fn capture_or_empty<'a>(captures: &crate::program::Caps<'a>, group: usize) -> Cow<'a, str> {
    Cow::Borrowed(captures.get(group).unwrap_or(""))
}

/// Quick regex match against a UA, using Matomo's boundary prefix + case-insensitive.
///
/// The heuristics call this with a fixed set of patterns on every parse, so
/// each is compiled once and kept: compiling them per call was most of what a
/// parse cost.
pub(crate) fn ua_matches(ua: &str, pattern: &'static str) -> bool {
    static CACHE: OnceLock<RwLock<HashMap<&'static str, Option<crate::program::Program>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);

    if let Some(re) = cache.read().unwrap_or_else(|e| e.into_inner()).get(pattern) {
        return re.as_ref().is_some_and(|re| re.is_match(ua));
    }

    let re = crate::program::Program::compile(pattern);
    let matched = re.as_ref().is_some_and(|re| re.is_match(ua));
    cache.write().unwrap_or_else(|e| e.into_inner()).insert(pattern, re);
    matched
}
