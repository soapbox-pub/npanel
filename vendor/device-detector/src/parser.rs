use aho_corasick::AhoCorasick;
use rayon::prelude::*;
use std::sync::OnceLock;

use crate::error::Result;
use crate::literal::extract_literals;
use crate::program::{Caps, Program};

/// A compiled entry: one regex rule plus its associated data.
pub(crate) struct CompiledEntry<T> {
    pattern: String,
    /// Compiled on first use. The database has twenty thousand patterns,
    /// most of them device models that real traffic never reaches, and
    /// compiling them all up front costs half a gigabyte.
    compiled: OnceLock<Option<Program>>,
    pub data: T,
}

/// Patterns compiled so far, across every detector in the process.
pub(crate) static COMPILED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

impl<T> CompiledEntry<T> {
    /// Checks the pattern parses, so a broken database fails at load rather
    /// than silently never matching.
    pub fn new(pattern: &str, data: T) -> Result<Self> {
        fancy_regex::Expr::parse_tree(pattern)?;
        Ok(Self { pattern: pattern.to_string(), compiled: OnceLock::new(), data })
    }

    pub fn captures<'t>(&self, text: &'t str) -> Option<Caps<'t>> {
        self.compiled
            .get_or_init(|| {
                COMPILED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                Program::compile(&self.pattern)
            })
            .as_ref()?
            .captures(text)
    }
}

/// Result of a successful match.
pub(crate) struct MatchResult<'a, T> {
    pub data: &'a T,
    pub captures: Caps<'a>,
}

// ---------------------------------------------------------------------------
// CompiledParser — flat list matching (bots, OS, clients, engines)
// ---------------------------------------------------------------------------

/// Core matching engine: Aho-Corasick pre-filter + fancy-regex matching.
///
/// `T` is the associated data for each entry (e.g. bot name, OS name, etc.).
pub(crate) struct CompiledParser<T> {
    index: LiteralIndex,
    entries: Vec<CompiledEntry<T>>,
}

impl<T> CompiledParser<T> {
    /// Build a CompiledParser from an iterator of (regex_pattern, data) pairs.
    ///
    /// Regex compilation is parallelized across all available cores via rayon.
    pub fn build(items: impl IntoIterator<Item = (String, T)>) -> Result<Self>
    where
        T: Send,
    {
        let items: Vec<(String, T)> = items.into_iter().collect();
        let index = LiteralIndex::build(items.iter().map(|(pattern, _)| pattern.as_str()))?;

        let entries = items
            .into_par_iter()
            .map(|(pattern, data)| CompiledEntry::new(&pattern, data))
            .collect::<Result<Vec<_>>>()?;

        Ok(Self { index, entries })
    }

    /// Find the first matching entry (preserving original order).
    pub fn match_first<'a>(&'a self, ua: &'a str) -> Option<MatchResult<'a, T>> {
        for idx in self.index.candidates(ua) {
            let entry = &self.entries[idx];
            if let Some(caps) = entry.captures(ua) {
                return Some(MatchResult {
                    data: &entry.data,
                    captures: caps,
                });
            }
        }

        None
    }
}

/// Aho-Corasick over the literal prefixes of a list of regexes: which of them
/// could match a string, in their original order.
///
/// A regex with no usable literal is always a candidate.
pub(crate) struct LiteralIndex {
    ac: Option<AhoCorasick>,
    /// For each AC pattern, the entries it maps to.
    pattern_entries: Vec<Vec<usize>>,
    /// Entries with no extractable literal — must always be tried.
    always: Vec<usize>,
}

impl LiteralIndex {
    pub fn build<'p>(patterns: impl IntoIterator<Item = &'p str>) -> Result<Self> {
        let mut literals: Vec<String> = Vec::new();
        let mut pattern_entries: Vec<Vec<usize>> = Vec::new();
        let mut always: Vec<usize> = Vec::new();
        let mut seen: std::collections::HashMap<String, usize> = Default::default();

        for (idx, pattern) in patterns.into_iter().enumerate() {
            let found = extract_literals(pattern, 3);
            if found.is_empty() {
                always.push(idx);
                continue;
            }
            for lit in found {
                if let Some(&ac_idx) = seen.get(&lit) {
                    pattern_entries[ac_idx].push(idx);
                } else {
                    seen.insert(lit.clone(), literals.len());
                    literals.push(lit);
                    pattern_entries.push(vec![idx]);
                }
            }
        }

        let ac = if literals.is_empty() {
            None
        } else {
            Some(AhoCorasick::builder().ascii_case_insensitive(true).build(&literals)?)
        };

        Ok(Self { ac, pattern_entries, always })
    }

    /// Indices of the entries that could match `text`, ascending.
    pub fn candidates(&self, text: &str) -> Vec<usize> {
        let mut candidates = self.always.clone();

        if let Some(ac) = &self.ac {
            for mat in ac.find_overlapping_iter(text) {
                candidates.extend_from_slice(&self.pattern_entries[mat.pattern().as_usize()]);
            }
        }

        candidates.sort_unstable();
        candidates.dedup();
        candidates
    }
}

// ---------------------------------------------------------------------------
// DeviceBrandParser — two-level brand-gate + model matching (device files)
// ---------------------------------------------------------------------------

/// A compiled brand with its gate regex and optional model sub-regexes.
pub(crate) struct CompiledBrand<B, M> {
    pub entry: CompiledEntry<B>,
    pub models: Vec<CompiledEntry<M>>,
}

/// Result of a device brand match.
pub(crate) struct BrandMatchResult<'a, B, M> {
    pub brand_data: &'a B,
    /// Captures from the brand regex (used if no model matches).
    pub brand_captures: Caps<'a>,
    /// If a model regex matched, its data and captures.
    pub model_match: Option<MatchResult<'a, M>>,
}

/// Two-level matching engine for device brand/model detection.
///
/// Matches the Matomo PHP logic: iterate brands in order, try the brand regex
/// as a gate, and only if it matches, try model regexes within that brand.
pub(crate) struct DeviceBrandParser<B, M> {
    brands: Vec<CompiledBrand<B, M>>,
    /// Which brands could match a UA, from literal prefixes of their regexes,
    /// so a parse runs a handful of brand regexes rather than all of them.
    prefilter: LiteralIndex,
}

impl<B, M> DeviceBrandParser<B, M> {
    pub fn new(brands: Vec<CompiledBrand<B, M>>, patterns: &[String]) -> Result<Self> {
        let prefilter = LiteralIndex::build(patterns.iter().map(String::as_str))?;
        Ok(Self { brands, prefilter })
    }

    /// Find the first matching brand, then try model regexes within it.
    pub fn match_first<'a>(&'a self, ua: &'a str) -> Option<BrandMatchResult<'a, B, M>> {
        for brand in self.prefilter.candidates(ua).into_iter().map(|index| &self.brands[index]) {
            let Some(brand_caps) = brand.entry.captures(ua) else {
                continue;
            };

            // Brand matched — now try model regexes.
            let model_match = brand.models.iter().find_map(|model| {
                model.captures(ua).map(|caps| MatchResult { data: &model.data, captures: caps })
            });

            return Some(BrandMatchResult {
                brand_data: &brand.entry.data,
                brand_captures: brand_caps,
                model_match,
            });
        }

        None
    }
}

/// Matomo's word-boundary-like prefix applied to all regexes.
/// Matches: start of string, or a non-alphanumeric boundary, or special prefixes.
///
/// Written out in both cases rather than under `(?i)`, so the negated classes
/// mean the same thing in both the byte-mode and the Unicode engine.
pub(crate) const MATOMO_BOUNDARY_PREFIX: &str = r"(?:^|[^A-Za-z0-9_\-]|[^A-Za-z0-9\-]_|(?i:sprd\-|MZ\-))";
