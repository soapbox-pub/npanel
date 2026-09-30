use std::collections::HashMap;

use indexmap::IndexMap;
use serde::Deserialize;

// ---------------------------------------------------------------------------
// Bots  (regexes/bots.yml)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub(crate) struct BotEntry {
    pub regex: String,
    pub name: String,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub producer: Option<BotProducer>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct BotProducer {
    pub name: Option<String>,
    pub url: Option<String>,
}

// ---------------------------------------------------------------------------
// Operating Systems  (regexes/oss.yml)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub(crate) struct OsEntry {
    pub regex: String,
    pub name: String,
    #[serde(default)]
    pub version: Option<String>,
    /// Versions told apart by a further regex each, first match wins —
    /// Fire OS is numbered by the Android release it is built on.
    #[serde(default)]
    pub versions: Option<Vec<OsVersionEntry>>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct OsVersionEntry {
    pub regex: String,
    pub version: String,
}

// ---------------------------------------------------------------------------
// Browsers  (regexes/client/browsers.yml)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub(crate) struct BrowserEntry {
    pub regex: String,
    pub name: String,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub engine: Option<EngineRef>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct EngineRef {
    pub default: Option<String>,
    #[serde(default)]
    pub versions: Option<IndexMap<String, String>>,
}

// ---------------------------------------------------------------------------
// Browser Engines  (regexes/client/browser_engine.yml)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub(crate) struct EngineEntry {
    pub regex: String,
    pub name: String,
}

// ---------------------------------------------------------------------------
// Device files  (regexes/device/*.yml)
//
// Format: top-level mapping  brand_name → DeviceBrandEntry
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub(crate) struct DeviceBrandEntry {
    pub regex: Option<String>,
    #[serde(default)]
    pub device: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub models: Option<Vec<ModelEntry>>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ModelEntry {
    pub regex: String,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub device: Option<String>,
    #[serde(default)]
    pub brand: Option<String>,
}

/// Raw deserialization target for a device YAML file.
/// Uses IndexMap to preserve YAML insertion order (first-match-wins).
pub(crate) type DeviceBrandMap = IndexMap<String, DeviceBrandEntry>;

// ---------------------------------------------------------------------------
// Vendor Fragments  (regexes/vendorfragments.yml)
//
// Format: top-level mapping  brand_name → [regex_pattern, ...]
// ---------------------------------------------------------------------------

/// Raw deserialization target for vendorfragments.yml.
/// Uses IndexMap to preserve YAML insertion order (first-match-wins).
pub(crate) type VendorFragmentMap = IndexMap<String, Vec<String>>;

// ---------------------------------------------------------------------------
// Client Hints  (regexes/client/hints/*.yml)
//
// Format: simple key→value YAML maps (package ID → name).
// ---------------------------------------------------------------------------

/// Deserialization target for client hint YAML files (apps.yml, browsers.yml).
pub(crate) type HintMap = HashMap<String, String>;
