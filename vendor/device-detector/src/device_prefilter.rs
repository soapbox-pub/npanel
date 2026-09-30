use super::error::Result;

/// Prefilter applied before running a device brand parser.
///
/// Mirrors Matomo's PHP logic where certain device parsers (TV, console, etc.)
/// skip processing entirely unless the UA contains specific markers.
pub(crate) enum DevicePrefilter {
    /// No prefilter — always run (used for mobiles).
    None,
    /// UA must match this regex to proceed (used for shell_tv, televisions, notebooks).
    Regex(fancy_regex::Regex),
    /// UA must match any of the brand regexes (OR'd into one mega-regex).
    /// Used for consoles, cameras, car_browsers, portable_media_player.
    OverallMatch(crate::program::Program),
}

impl DevicePrefilter {
    /// Build a `preMatchOverall` prefilter: OR all brand regexes into one mega-regex
    /// with Matomo's boundary prefix.  If the combined regex doesn't match the UA,
    /// none of the individual brand regexes can match either, so we skip the parser.
    pub fn build_overall_prefilter(brand_regexes: &[String]) -> Result<DevicePrefilter> {
        if brand_regexes.is_empty() {
            return Ok(DevicePrefilter::None);
        }
        // Join all brand patterns with | inside a non-capturing group, apply
        // Matomo's boundary prefix and case-insensitive flag.
        let combined = brand_regexes.join("|");
        match crate::program::Program::compile(&combined) {
            Some(program) => Ok(DevicePrefilter::OverallMatch(program)),
            None => Ok(DevicePrefilter::None),
        }
    }

    pub fn matches(&self, ua: &str) -> bool {
        match self {
            Self::None => true,
            Self::Regex(re) => re.is_match(ua).unwrap_or(false),
            Self::OverallMatch(program) => program.is_match(ua),
        }
    }
}
