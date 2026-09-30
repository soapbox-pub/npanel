use super::db;
use super::device_prefilter::DevicePrefilter;
use super::error::Result;
use super::helpers::*;
use super::os_helpers::*;
use super::parser::{
    CompiledBrand, CompiledEntry, CompiledParser, DeviceBrandParser,
};
use super::parser_data::*;
use super::substitution::substitute;
use super::types::*;
use rayon::prelude::*;
use std::borrow::Cow;
use std::path::Path;

pub struct DeviceDetector {
    bot_parser: CompiledParser<BotData>,
    os_parser: CompiledParser<OsData>,
    browser_parser: CompiledParser<ClientData>,
    feed_reader_parser: CompiledParser<ClientData>,
    mobile_app_parser: CompiledParser<ClientData>,
    library_parser: CompiledParser<ClientData>,
    media_player_parser: CompiledParser<ClientData>,
    pim_parser: CompiledParser<ClientData>,
    engine_parser: CompiledParser<EngineData>,
    vendor_fragment_parser: CompiledParser<VendorFragmentData>,
    /// Each device parser tuple: (default_type, prefilter, claims_type, brand_parser).
    ///
    /// `claims_type`: when `true`, a prefilter match claims the device type even
    /// if no brand regex matches.  This mirrors Matomo's HbbTv/ShellTv parsers
    /// which always set device_type=TV when their marker is present.
    device_parsers: Vec<(
        DeviceType,
        DevicePrefilter,
        bool,
        DeviceBrandParser<DeviceBrandData, DeviceModelData>,
    )>,
    /// Package-ID → app name (from `client/hints/apps.yml`).
    app_hints: db::HintMap,
    /// Package-ID → browser name (from `client/hints/browsers.yml`).
    browser_hints: db::HintMap,
}

impl DeviceDetector {
    /// Load all Matomo YAML regex files from `dir` and build the detector.
    ///
    /// `dir` should point to the `regexes/` directory of a Matomo device-detector
    /// checkout (containing `bots.yml`, `oss.yml`, `client/`, `device/`, etc.).
    pub fn from_dir(dir: impl AsRef<Path>) -> Result<Self> {
        let dir = dir.as_ref().to_path_buf();
        Self::from_source(move |name: &str| Ok(std::fs::read_to_string(dir.join(name))?))
    }

    /// Build the detector from file contents supplied by `read`, which is
    /// given paths relative to the `regexes/` directory (`bots.yml`,
    /// `client/browsers.yml`, …). This is how a database embedded in the
    /// binary is loaded.
    pub fn from_source<F>(read: F) -> Result<Self>
    where
        F: Fn(&str) -> Result<String> + Sync,
    {
        let read = &read;

        // Build flat-list parsers and device parsers concurrently.
        let (flat_result, device_parsers_result) = rayon::join(
            || -> Result<_> {
                // Bots
                let bots: Vec<db::BotEntry> = load_yaml(read, "bots.yml")?;
                let bot_parser = CompiledParser::build(bots.into_iter().map(|b| {
                    (
                        b.regex,
                        BotData {
                            name: b.name,
                            category: b.category,
                            url: b.url,
                            producer: b.producer,
                        },
                    )
                }))?;

                // OS
                let oss: Vec<db::OsEntry> = load_yaml(read, "oss.yml")?;
                let os_parser = CompiledParser::build(
                    oss.into_iter()
                        .map(|o| {
                            let versions = o
                                .versions
                                .unwrap_or_default()
                                .into_iter()
                                .map(|v| CompiledEntry::new(&v.regex, v.version))
                                .collect::<Result<Vec<_>>>()?;
                            Ok((o.regex, OsData { name: o.name, version_template: o.version, versions }))
                        })
                        .collect::<Result<Vec<_>>>()?,
                )?;

                // Client parsers — build all 6 in parallel
                let client_parsers: Vec<CompiledParser<ClientData>> = vec![
                    ("browsers.yml", ClientType::Browser),
                    ("feed_readers.yml", ClientType::FeedReader),
                    ("mobile_apps.yml", ClientType::MobileApp),
                    ("libraries.yml", ClientType::Library),
                    ("mediaplayers.yml", ClientType::MediaPlayer),
                    ("pim.yml", ClientType::Pim),
                ]
                .into_par_iter()
                .map(|(file, ct)| build_client_parser(read, &format!("client/{file}"), ct))
                .collect::<Result<Vec<_>>>()?;

                let mut clients = client_parsers.into_iter();
                let browser_parser = clients.next().unwrap();
                let feed_reader_parser = clients.next().unwrap();
                let mobile_app_parser = clients.next().unwrap();
                let library_parser = clients.next().unwrap();
                let media_player_parser = clients.next().unwrap();
                let pim_parser = clients.next().unwrap();

                // Browser engines
                let engines: Vec<db::EngineEntry> =
                    load_yaml(read, "client/browser_engine.yml")?;
                let engine_parser = CompiledParser::build(
                    engines
                        .into_iter()
                        .map(|e| (e.regex, EngineData { name: e.name })),
                )?;

                // Vendor fragments
                let vf_map: db::VendorFragmentMap = load_yaml(read, "vendorfragments.yml")?;
                let vendor_fragment_parser =
                    CompiledParser::build(vf_map.into_iter().flat_map(|(brand, patterns)| {
                        // Each pattern gets `[^a-z0-9]+` appended (Matomo's VendorFragment.php).
                        patterns.into_iter().map(move |pat| {
                            (
                                format!("{}[^a-z0-9]+", pat),
                                VendorFragmentData {
                                    brand: brand.clone(),
                                },
                            )
                        })
                    }))?;

                Ok((
                    bot_parser,
                    os_parser,
                    browser_parser,
                    feed_reader_parser,
                    mobile_app_parser,
                    library_parser,
                    media_player_parser,
                    pim_parser,
                    engine_parser,
                    vendor_fragment_parser,
                ))
            },
            || -> Result<_> {
                // Device parsers — order preserved by par_iter collect.
                //
                // Each entry: (file, device_type, prefilter_kind)
                // PrefilterKind mirrors Matomo's PHP device parser prefilters:
                //   Specific  — UA must match a hardcoded regex (ShellTv, HbbTv, Notebook)
                //   Overall   — build a mega-regex from all brand regexes (Console, etc.)
                //   None      — always run (Mobiles)
                #[derive(Clone)]
                enum PrefilterKind {
                    Specific(&'static str),
                    Overall,
                    None,
                }

                //            (file, type, prefilter, claims_type)
                // claims_type=true means the prefilter match alone claims the
                // device type, preventing fallthrough (HbbTv/ShellTv → TV).
                let specs: Vec<(&str, DeviceType, PrefilterKind, bool)> = vec![
                    (
                        "shell_tv.yml",
                        DeviceType::Tv,
                        PrefilterKind::Specific(r"(?i)[a-z]+[ _]Shell[ _]\w{6}|tclwebkit"),
                        true,
                    ),
                    (
                        "televisions.yml",
                        DeviceType::Tv,
                        PrefilterKind::Specific(r"(?i)(?:HbbTV|SmartTvA)/"),
                        true,
                    ),
                    (
                        "consoles.yml",
                        DeviceType::Console,
                        PrefilterKind::Overall,
                        false,
                    ),
                    (
                        "car_browsers.yml",
                        DeviceType::CarBrowser,
                        PrefilterKind::Overall,
                        false,
                    ),
                    (
                        "cameras.yml",
                        DeviceType::Camera,
                        PrefilterKind::Overall,
                        false,
                    ),
                    (
                        "portable_media_player.yml",
                        DeviceType::PortableMediaPlayer,
                        PrefilterKind::Overall,
                        false,
                    ),
                    (
                        "notebooks.yml",
                        DeviceType::Notebook,
                        PrefilterKind::Specific(r"FBMD/"),
                        false,
                    ),
                    (
                        "mobiles.yml",
                        DeviceType::Smartphone,
                        PrefilterKind::None,
                        false,
                    ),
                ];

                specs
                    .into_par_iter()
                    .map(
                        |(file, device_type, prefilter_kind, claims_type)| -> Result<_> {
                            let (parser, brand_regexes) =
                                build_device_brand_parser(read, &format!("device/{file}"), device_type)?;

                            let prefilter = match prefilter_kind {
                                PrefilterKind::Specific(pat) => {
                                    let re = fancy_regex::Regex::new(pat)?;
                                    DevicePrefilter::Regex(re)
                                }
                                PrefilterKind::Overall => {
                                    DevicePrefilter::build_overall_prefilter(&brand_regexes)?
                                }
                                PrefilterKind::None => DevicePrefilter::None,
                            };

                            Ok((device_type, prefilter, claims_type, parser))
                        },
                    )
                    .collect::<Result<Vec<_>>>()
            },
        );

        let (
            bot_parser,
            os_parser,
            browser_parser,
            feed_reader_parser,
            mobile_app_parser,
            library_parser,
            media_player_parser,
            pim_parser,
            engine_parser,
            vendor_fragment_parser,
        ) = flat_result?;
        let device_parsers = device_parsers_result?;

        // Client hints lookup maps.
        let app_hints: db::HintMap = load_yaml(read, "client/hints/apps.yml")?;
        let browser_hints: db::HintMap = load_yaml(read, "client/hints/browsers.yml")?;

        Ok(Self {
            bot_parser,
            os_parser,
            browser_parser,
            feed_reader_parser,
            mobile_app_parser,
            library_parser,
            media_player_parser,
            pim_parser,
            engine_parser,
            vendor_fragment_parser,
            device_parsers,
            app_hints,
            browser_hints,
        })
    }

    /// How many patterns have been compiled so far, in every detector in the
    /// process. Patterns compile on first use and stay compiled for the life
    /// of their detector, so this is what a caller watches to decide when a
    /// long-lived detector has grown large enough to replace.
    pub fn compiled_patterns() -> usize {
        crate::parser::COMPILED.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Parse a User-Agent string and return detection results.
    ///
    /// The returned `Detection` borrows from both `self` (detector data) and `ua`,
    /// avoiding heap allocations for fields that can reference existing data.
    pub fn parse<'a>(&'a self, ua: &'a str) -> Detection<'a> {
        self.parse_with_hints(ua, None)
    }

    /// Parse a User-Agent string with optional client hints and return detection results.
    pub fn parse_with_hints<'a>(
        &'a self,
        ua: &'a str,
        hints: Option<&ClientHints>,
    ) -> Detection<'a> {
        // 1. Bot check
        if let Some(m) = self.bot_parser.match_first(ua) {
            return Detection {
                bot: Some(Bot {
                    name: substitute(&m.data.name, &m.captures),
                    category: m.data.category.as_deref(),
                    url: m.data.url.as_deref(),
                    producer: m.data.producer.as_ref().map(|p| BotProducer {
                        name: p.name.as_deref(),
                        url: p.url.as_deref(),
                    }),
                }),
                os: None,
                client: None,
                device: None,
            };
        }

        // 2. OS detection
        // As Matomo's OperatingSystem parser: the entry's own version, or
        // nothing when it has none, then the first of its `versions` whose
        // regex matches.
        let os = self.os_parser.match_first(ua).map(|m| {
            let mut version = dotted(match &m.data.version_template {
                Some(tpl) => substitute(tpl, &m.captures),
                None => Cow::Borrowed(""),
            });
            if let Some((tpl, caps)) = m.data.versions.iter().find_map(|v| v.captures(ua).map(|caps| (&v.data, caps))) {
                version = Cow::Owned(dotted(substitute(tpl, &caps)).into_owned());
            }
            Os {
                name: substitute(&m.data.name, &m.captures),
                version,
            }
        });

        // 3. Client detection (try each client parser in order)
        let mut client = self.detect_client(ua);

        // 4. X-Requested-With client override from hints.
        if let Some(xrw) = hints.and_then(|h| h.x_requested_with.as_deref()) {
            if let Some(app_name) = self.app_hints.get(xrw) {
                let keep_version = client
                    .as_ref()
                    .map_or(false, |c| c.name.eq_ignore_ascii_case(app_name));
                let version = if keep_version {
                    client.as_ref().unwrap().version.clone()
                } else {
                    Cow::Borrowed("")
                };
                client = Some(Client {
                    kind: ClientType::MobileApp,
                    name: Cow::Owned(app_name.clone()),
                    version,
                    engine: Cow::Borrowed(""),
                    engine_version: Cow::Borrowed(""),
                });
            } else if let Some(browser_name) = self.browser_hints.get(xrw) {
                let keep_version = client
                    .as_ref()
                    .map_or(false, |c| c.name.eq_ignore_ascii_case(browser_name));
                let (version, engine, engine_version) = if keep_version {
                    let c = client.as_ref().unwrap();
                    (
                        c.version.clone(),
                        c.engine.clone(),
                        c.engine_version.clone(),
                    )
                } else {
                    (Cow::Borrowed(""), Cow::Borrowed(""), Cow::Borrowed(""))
                };
                client = Some(Client {
                    kind: ClientType::Browser,
                    name: Cow::Owned(browser_name.clone()),
                    version,
                    engine,
                    engine_version,
                });
            }
        }

        // 5. Device detection (brand parsers)
        let device = self.detect_device(ua);

        // Decompose device into its parts so we can merge results from
        // multiple heuristic steps (vendor fragments, Apple inference, desktop
        // inference) — mirrors Matomo's mutable $device/$brand/$model fields.
        let (mut device_type, mut brand, mut model): (
            Option<DeviceType>,
            Cow<'a, str>,
            Cow<'a, str>,
        ) = match device {
            Some(d) => (d.kind, d.brand, d.model),
            None => (None, Cow::Borrowed(""), Cow::Borrowed("")),
        };

        // Matomo treats the "Unknown" brand as empty (AbstractDeviceParser.php:2390).
        if brand == "Unknown" {
            brand = Cow::Borrowed("");
        }

        // 6. Vendor fragment fallback (Matomo's VendorFragment.php).
        if brand.is_empty() {
            if let Some(m) = self.vendor_fragment_parser.match_first(ua) {
                brand = Cow::Borrowed(m.data.brand.as_str());
            }
        }

        // 7. Apple brand heuristics (Matomo DeviceDetector.php:920-934).
        let os_name = os.as_ref().map(|o| o.name.as_ref()).unwrap_or("");
        let os_version = os.as_ref().map(|o| o.version.as_ref()).unwrap_or("");
        let is_apple_os = matches!(os_name, "iPadOS" | "tvOS" | "watchOS" | "iOS" | "Mac");
        let is_android_family = os.as_ref().map_or(false, |o| is_android_os(&o.name));
        let client_name = client.as_ref().map(|c| c.name.as_ref()).unwrap_or("");

        if brand == "Apple" && !is_apple_os {
            device_type = None;
            brand = Cow::Borrowed("");
            model = Cow::Borrowed("");
        }

        if brand.is_empty() && is_apple_os {
            brand = Cow::Borrowed("Apple");
        }

        // --- Device-type heuristics (Matomo DeviceDetector.php:936-1128) ---

        // VR fragment → wearable
        if device_type.is_none() && ua_matches(ua, r"Android( [.0-9]+)?; Mobile VR;| VR ") {
            device_type = Some(DeviceType::Wearable);
        }

        // Chrome on Android: "Mobile"/"eliboM" → smartphone, else → tablet
        if device_type.is_none() && is_android_family && ua_matches(ua, r"Chrome/[.0-9]*") {
            if ua_matches(ua, r"(?:Mobile|eliboM)") {
                device_type = Some(DeviceType::Smartphone);
            } else {
                device_type = Some(DeviceType::Tablet);
            }
        }

        // Pad/APad → tablet
        if device_type == Some(DeviceType::Smartphone) && ua_matches(ua, r"Pad/APad") {
            device_type = Some(DeviceType::Tablet);
        }

        // "Android; Tablet;" or "Opera Tablet" → tablet
        if device_type.is_none()
            && (ua_matches(ua, r"Android( [.0-9]+)?; Tablet;|Tablet(?! PC)|.*\-tablet$")
                || ua_matches(ua, r"Opera Tablet"))
        {
            device_type = Some(DeviceType::Tablet);
        }

        // "Android; Mobile;" → smartphone
        if device_type.is_none() && ua_matches(ua, r"Android( [.0-9]+)?; Mobile;|.*\-mobile$") {
            device_type = Some(DeviceType::Smartphone);
        }

        // Android version heuristics
        if device_type.is_none() && os_name == "Android" && !os_version.is_empty() {
            if version_lt(os_version, "2.0") {
                device_type = Some(DeviceType::Smartphone);
            } else if version_ge(os_version, "3.0") && version_lt(os_version, "4.0") {
                device_type = Some(DeviceType::Tablet);
            }
        }

        // Feature phone on Android → smartphone
        if device_type == Some(DeviceType::FeaturePhone) && is_android_family {
            device_type = Some(DeviceType::Smartphone);
        }

        // Java ME → feature phone
        if os_name == "Java ME" && device_type.is_none() {
            device_type = Some(DeviceType::FeaturePhone);
        }

        // KaiOS → feature phone
        if os_name == "KaiOS" {
            device_type = Some(DeviceType::FeaturePhone);
        }

        // Windows 8+ touch → tablet
        if device_type.is_none()
            && (os_name == "Windows RT"
                || (os_name == "Windows" && !os_version.is_empty() && version_ge(os_version, "8")))
            && ua_matches(ua, "Touch")
        {
            device_type = Some(DeviceType::Tablet);
        }

        // Puffin heuristics
        if device_type.is_none() && ua_matches(ua, r"Puffin/(?:\d+[.\d]+)[LMW]D") {
            device_type = Some(DeviceType::Desktop);
        }
        if device_type.is_none() && ua_matches(ua, r"Puffin/(?:\d+[.\d]+)[AIFLW]P") {
            device_type = Some(DeviceType::Smartphone);
        }
        if device_type.is_none() && ua_matches(ua, r"Puffin/(?:\d+[.\d]+)[AILW]T") {
            device_type = Some(DeviceType::Tablet);
        }

        // Opera TV Store / OMI → tv
        if ua_matches(ua, r"Opera TV Store| OMI/") {
            device_type = Some(DeviceType::Tv);
        }

        // Coolita OS → tv + coocaa brand
        if os_name == "Coolita OS" {
            device_type = Some(DeviceType::Tv);
            brand = Cow::Borrowed("coocaa");
        }

        // Andr0id / Android TV / Google TV / BRAVIA etc. → tv
        if !matches!(
            device_type,
            Some(DeviceType::Tv) | Some(DeviceType::Peripheral)
        ) && ua_matches(
            ua,
            r#"Andr0id|(?:Android(?: UHD)?|Google) TV|\(lite\) TV|BRAVIA|Firebolt| TV$"#,
        ) {
            device_type = Some(DeviceType::Tv);
        }

        // Tizen TV / SmartTV → tv
        if device_type.is_none() && ua_matches(ua, r"SmartTV|Tizen.+ TV .+$") {
            device_type = Some(DeviceType::Tv);
        }

        // Known TV client names → tv
        if matches!(
            client_name,
            "Kylo"
                | "Espial TV Browser"
                | "LUJO TV Browser"
                | "LogicUI TV Browser"
                | "Open TV Browser"
                | "Seraphic Sraf"
                | "Opera Devices"
                | "Crow Browser"
                | "Vewd Browser"
                | "TiviMate"
                | "Quick Search TV"
                | "QJY TV Browser"
                | "TV Bro"
                | "Redline"
        ) {
            device_type = Some(DeviceType::Tv);
        }

        // (TV; fragment → tv
        if device_type.is_none() && ua_matches(ua, r"\(TV;") {
            device_type = Some(DeviceType::Tv);
        }

        // "Desktop" fragment → desktop
        if device_type != Some(DeviceType::Desktop)
            && ua.contains("Desktop")
            && ua_matches(ua, r"Desktop(?: (?:x(?:32|64)|WOW64))?;")
        {
            device_type = Some(DeviceType::Desktop);
        }

        // Desktop OS inference (Matomo DeviceDetector.php:1123-1128).
        if device_type.is_none() {
            if os.as_ref().map_or(false, |o| is_desktop_os(&o.name)) {
                device_type = Some(DeviceType::Desktop);
            }
        }

        // --- Client hints: device model fallback ---
        if model.is_empty() {
            if let Some(hint_model) = hints.and_then(|h| h.model.as_deref()) {
                if !hint_model.is_empty() {
                    model = Cow::Owned(hint_model.to_string());
                }
            }
        }

        // --- Client hints: mobile flag ---
        if device_type.is_none() {
            if hints.and_then(|h| h.mobile) == Some(true) {
                device_type = Some(DeviceType::Smartphone);
            }
        }

        // Build final device if we determined a type or a brand.
        let device = if device_type.is_some() || !brand.is_empty() {
            Some(Device {
                kind: device_type,
                brand,
                model,
            })
        } else {
            None
        };

        Detection {
            bot: None,
            os,
            client,
            device,
        }
    }

    fn detect_client<'a>(&'a self, ua: &'a str) -> Option<Client<'a>> {
        let parsers: &[(&CompiledParser<ClientData>, ClientType)] = &[
            (&self.feed_reader_parser, ClientType::FeedReader),
            (&self.mobile_app_parser, ClientType::MobileApp),
            (&self.media_player_parser, ClientType::MediaPlayer),
            (&self.pim_parser, ClientType::Pim),
            (&self.browser_parser, ClientType::Browser),
            (&self.library_parser, ClientType::Library),
        ];

        for (parser, _default_kind) in parsers {
            if let Some(m) = parser.match_first(ua) {
                let version = dotted(match &m.data.version_template {
                    Some(tpl) => substitute(tpl, &m.captures),
                    None => Cow::Borrowed(""),
                });

                // Resolve engine: use default from browser entry, or fall back to engine parser.
                let (engine, engine_version) = self.resolve_engine(ua, m.data, &version);

                return Some(Client {
                    kind: m.data.kind,
                    name: substitute(&m.data.name, &m.captures),
                    version,
                    engine,
                    engine_version,
                });
            }
        }

        None
    }

    fn resolve_engine<'a>(
        &'a self,
        ua: &'a str,
        client_data: &'a ClientData,
        browser_version: &str,
    ) -> (Cow<'a, str>, Cow<'a, str>) {
        if let Some(default_engine) = &client_data.engine_default {
            // Determine the engine name: start with the default, then apply
            // version-threshold overrides (last threshold where browser_version
            // >= threshold wins).
            let mut engine_name: &str = default_engine;
            if !browser_version.is_empty() {
                if let Some(ref versions) = client_data.engine_versions {
                    for (threshold, name) in versions {
                        if version_ge(browser_version, threshold) {
                            engine_name = name;
                        }
                    }
                }
            }

            if !engine_name.is_empty() {
                // Try engine parser to get the engine version from the UA.
                if let Some(m) = self.engine_parser.match_first(ua) {
                    if m.data.name.eq_ignore_ascii_case(engine_name) {
                        return (
                            Cow::Borrowed(m.data.name.as_str()),
                            capture_or_empty(&m.captures, 1),
                        );
                    }
                }
                return (Cow::Borrowed(engine_name), Cow::Borrowed(""));
            }
        }

        // No default engine → try engine parser directly
        if let Some(m) = self.engine_parser.match_first(ua) {
            return (
                Cow::Borrowed(m.data.name.as_str()),
                capture_or_empty(&m.captures, 1),
            );
        }

        (Cow::Borrowed(""), Cow::Borrowed(""))
    }

    fn detect_device<'a>(&'a self, ua: &'a str) -> Option<Device<'a>> {
        for (default_type, prefilter, claims_type, parser) in &self.device_parsers {
            if !prefilter.matches(ua) {
                continue;
            }
            if let Some(m) = parser.match_first(ua) {
                let brand_data = m.brand_data;

                if let Some(model_match) = &m.model_match {
                    // Model regex matched — use model data, falling back to brand data.
                    let device_type = model_match
                        .data
                        .device_type
                        .or(brand_data.device_type)
                        .unwrap_or(*default_type);
                    let brand = model_match
                        .data
                        .brand
                        .as_deref()
                        .unwrap_or(&brand_data.brand);
                    let model = match &model_match.data.model_template {
                        Some(tpl) => substitute(tpl, &model_match.captures),
                        None => Cow::Borrowed(""),
                    };

                    return Some(Device {
                        kind: Some(device_type),
                        brand: Cow::Borrowed(brand),
                        model,
                    });
                } else {
                    // Only brand regex matched, no specific model.
                    let device_type = brand_data.device_type.unwrap_or(*default_type);
                    let model = match &brand_data.model_template {
                        Some(tpl) => substitute(tpl, &m.brand_captures),
                        None => Cow::Borrowed(""),
                    };

                    return Some(Device {
                        kind: Some(device_type),
                        brand: Cow::Borrowed(&brand_data.brand),
                        model,
                    });
                }
            }

            // Prefilter matched but no brand matched.  For parsers that
            // "claim" the device type (HbbTv, ShellTv), return a typeless
            // device to prevent later parsers from producing false positives.
            if *claims_type {
                return Some(Device {
                    kind: Some(*default_type),
                    brand: Cow::Borrowed(""),
                    model: Cow::Borrowed(""),
                });
            }
        }

        None
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// A version as Matomo builds it: underscores read as dots, as in iOS's
/// `17_5`, and no trailing dots or spaces.
fn dotted(version: Cow<'_, str>) -> Cow<'_, str> {
    if !version.contains('_') {
        return version;
    }
    let replaced = version.replace('_', ".");
    Cow::Owned(replaced.trim_end_matches(|c: char| c.is_whitespace() || c == '.').to_string())
}

fn load_yaml<T, F>(read: &F, name: &str) -> Result<T>
where
    T: serde::de::DeserializeOwned,
    F: Fn(&str) -> Result<String>,
{
    Ok(serde_yaml::from_str(&read(name)?)?)
}

fn build_client_parser<F>(read: &F, name: &str, kind: ClientType) -> Result<CompiledParser<ClientData>>
where
    F: Fn(&str) -> Result<String>,
{
    // All client YAML files share the same flat-list schema with regex/name/version/engine.
    // We use BrowserEntry as a superset that works for all of them.
    let entries: Vec<db::BrowserEntry> = load_yaml(read, name)?;
    CompiledParser::build(entries.into_iter().map(|e| {
        let (engine_default, engine_versions) = match e.engine {
            Some(eng) => (eng.default, eng.versions),
            None => (None, None),
        };
        (
            e.regex,
            ClientData {
                kind,
                name: e.name,
                version_template: e.version,
                engine_default,
                engine_versions,
            },
        )
    }))
}

/// Returns `(parser, brand_regex_strings)`.  The second element contains the
/// raw regex patterns for each brand; callers that need a `preMatchOverall`
/// prefilter use these to build a combined mega-regex.
fn build_device_brand_parser<F>(
    read: &F,
    name: &str,
    default_type: DeviceType,
) -> Result<(
    DeviceBrandParser<DeviceBrandData, DeviceModelData>,
    Vec<String>,
)>
where
    F: Fn(&str) -> Result<String>,
{
    let brands: db::DeviceBrandMap = load_yaml(read, name)?;

    // Collect brands that have a regex, preserving YAML insertion order (IndexMap).
    let brand_items: Vec<(String, String, db::DeviceBrandEntry)> = brands
        .into_iter()
        .filter_map(|(brand_name, mut entry)| {
            let brand_regex_str = entry.regex.take()?;
            Some((brand_name, brand_regex_str, entry))
        })
        .collect();

    // Keep a copy of the raw brand regex strings for prefilter construction.
    let brand_regex_strings: Vec<String> = brand_items.iter().map(|(_, r, _)| r.clone()).collect();

    // Compile brand + model regexes in parallel across brands.
    let compiled_brands: Vec<CompiledBrand<DeviceBrandData, DeviceModelData>> = brand_items
        .into_par_iter()
        .map(|(brand_name, brand_regex_str, entry)| {

            let device_type = entry
                .device
                .as_deref()
                .and_then(DeviceType::from_str)
                .or(Some(default_type));

            // Compile model regexes in parallel within each brand.
            let model_entries: Vec<CompiledEntry<DeviceModelData>> = entry
                .models
                .unwrap_or_default()
                .into_par_iter()
                .map(|model| {
                    let model_device_type = model.device.as_deref().and_then(DeviceType::from_str);
                    CompiledEntry::new(
                        &model.regex,
                        DeviceModelData {
                            brand: model.brand,
                            model_template: model.model,
                            device_type: model_device_type,
                        },
                    )
                })
                .collect::<Result<Vec<_>>>()?;

            Ok(CompiledBrand {
                entry: CompiledEntry::new(
                    &brand_regex_str,
                    DeviceBrandData {
                        brand: brand_name,
                        model_template: entry.model,
                        device_type,
                    },
                )?,
                models: model_entries,
            })
        })
        .collect::<Result<Vec<_>>>()?;

    Ok((DeviceBrandParser::new(compiled_brands, &brand_regex_strings)?, brand_regex_strings))
}
