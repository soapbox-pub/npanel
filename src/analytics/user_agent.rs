//! Browsers, operating systems and device types, read out of user agents the
//! way Plausible does.
//!
//! The detector is a port of Matomo's Device Detector over Matomo's own
//! database — the same one Plausible's UAInspector reads — so browsers and
//! systems come out under the names a Plausible dashboard would show.
//! Plausible's own adjustments are applied on top: mobile variants folded into
//! their browser, versions cut to major.minor, and the device type reduced to
//! three screen sizes.

use std::num::NonZeroUsize;

use device_detector::{ClientType, DeviceDetector, DeviceType};

/// What a visit is recorded as coming from. Empty strings mean unknown.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Device {
    pub browser: String,
    pub browser_version: String,
    pub os: String,
    pub os_version: String,
    /// `Desktop`, `Mobile` or `Tablet`, or empty.
    pub device: String,
}

/// User agents parsed and kept. Most traffic is a handful of browsers.
const MAX_CACHED: usize = 10_000;

/// Longer user agents are cut here before parsing.
const MAX_USER_AGENT: usize = 1024;

/// Patterns one detector may compile before it is replaced. Patterns compile
/// on first use and stay compiled, at roughly 100 KB each, so a detector that
/// has seen a great variety of devices is swapped for a fresh one rather than
/// being left to grow.
const MAX_COMPILED: usize = 2500;

/// The database, embedded so the binary is all there is to deploy.
fn regex_file(name: &str) -> Option<&'static str> {
    macro_rules! files {
        ($($path:literal),* $(,)?) => {
            match name {
                $($path => Some(include_str!(concat!("../../assets/device-detector/regexes/", $path))),)*
                _ => None,
            }
        };
    }
    files!(
        "bots.yml",
        "oss.yml",
        "vendorfragments.yml",
        "client/browser_engine.yml",
        "client/browsers.yml",
        "client/feed_readers.yml",
        "client/libraries.yml",
        "client/mediaplayers.yml",
        "client/mobile_apps.yml",
        "client/pim.yml",
        "client/hints/apps.yml",
        "client/hints/browsers.yml",
        "device/cameras.yml",
        "device/car_browsers.yml",
        "device/consoles.yml",
        "device/mobiles.yml",
        "device/notebooks.yml",
        "device/portable_media_player.yml",
        "device/shell_tv.yml",
        "device/televisions.yml",
    )
}

fn build_detector() -> DeviceDetector {
    DeviceDetector::from_source(|name| {
        regex_file(name)
            .map(str::to_string)
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, name.to_string()).into())
    })
    .expect("the embedded device database parses")
}

pub struct UserAgents {
    detector: Option<DeviceDetector>,
    /// `compiled_patterns()` when the current detector was made.
    compiled_at_build: usize,
    cache: lru::LruCache<String, Option<Device>>,
}

impl Default for UserAgents {
    fn default() -> Self {
        Self { detector: None, compiled_at_build: 0, cache: lru::LruCache::new(NonZeroUsize::new(MAX_CACHED).unwrap()) }
    }
}

impl UserAgents {
    /// What `user_agent` is, or `None` for a bot, which is never counted.
    pub fn parse(&mut self, user_agent: &str) -> Option<Device> {
        let ua: String = user_agent.chars().take(MAX_USER_AGENT).collect();
        if let Some(cached) = self.cache.get(&ua) {
            return cached.clone();
        }
        let result = self.detect(&ua);
        self.cache.put(ua, result.clone());
        result
    }

    fn detect(&mut self, ua: &str) -> Option<Device> {
        if self.detector.is_some() && DeviceDetector::compiled_patterns() - self.compiled_at_build > MAX_COMPILED {
            self.detector = None;
        }
        // Built on first use: a gateway with every site's analytics off should
        // never pay for it.
        let detector = self.detector.get_or_insert_with(|| {
            let detector = build_detector();
            self.compiled_at_build = DeviceDetector::compiled_patterns();
            detector
        });

        let detection = detector.parse(ua);
        if detection.bot().is_some() {
            return None;
        }

        let client = detection.client();
        // Headless Chrome announces itself, and is only ever automation.
        if client.is_some_and(|c| c.name == "Headless Chrome") {
            return None;
        }

        let mobile_app = client.is_some_and(|c| c.kind == ClientType::MobileApp);
        let browser = match client {
            Some(_) if mobile_app => "Mobile App".to_string(),
            Some(c) => browser_alias(&c.name).unwrap_or(&c.name).to_string(),
            None => String::new(),
        };
        let browser_version = if mobile_app { String::new() } else { major_minor(client.map(|c| c.version.as_ref())) };

        let os = detection.os();
        Some(Device {
            browser,
            browser_version,
            os: os.map(|o| o.name.to_string()).unwrap_or_default(),
            os_version: major_minor(os.map(|o| o.version.as_ref())),
            device: screen_size(detection.device().and_then(|d| d.kind)).to_string(),
        })
    }
}

/// Browser names Plausible folds together, so a site's mobile and desktop
/// visitors on the same browser are one row rather than two.
fn browser_alias(name: &str) -> Option<&'static str> {
    Some(match name {
        "Mobile Safari" => "Safari",
        "Chrome Mobile" | "Chrome Mobile iOS" => "Chrome",
        "Firefox Mobile" | "Firefox Mobile iOS" => "Firefox",
        "Opera Mobile" | "Opera Mini" | "Opera Mini iOS" => "Opera",
        "Yandex Browser Lite" => "Yandex Browser",
        "Chrome Webview" => "Mobile App",
        _ => return None,
    })
}

/// Device types Plausible reports as each screen size.
fn screen_size(kind: Option<DeviceType>) -> &'static str {
    match kind {
        Some(
            DeviceType::Smartphone
            | DeviceType::FeaturePhone
            | DeviceType::PortableMediaPlayer
            | DeviceType::Phablet
            | DeviceType::Wearable
            | DeviceType::Camera,
        ) => "Mobile",
        Some(DeviceType::CarBrowser | DeviceType::Tablet) => "Tablet",
        Some(DeviceType::Tv | DeviceType::Console | DeviceType::Desktop) => "Desktop",
        _ => "",
    }
}

fn major_minor(version: Option<&str>) -> String {
    match version.filter(|v| !v.is_empty()) {
        Some(version) => version.split('.').take(2).collect::<Vec<_>>().join("."),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_user_agents() {
        let mut agents = UserAgents::default();
        let iphone = agents
            .parse("Mozilla/5.0 (iPhone; CPU iPhone OS 17_5 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.5 Mobile/15E148 Safari/604.1")
            .unwrap();
        assert_eq!(
            iphone,
            Device { browser: "Safari".into(), browser_version: "17.5".into(), os: "iOS".into(), os_version: "17.5".into(), device: "Mobile".into() }
        );

        let desktop = agents
            .parse("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/128.0.0.0 Safari/537.36")
            .unwrap();
        assert_eq!(desktop.browser, "Chrome");
        assert_eq!(desktop.browser_version, "128.0");
        assert_eq!(desktop.device, "Desktop");

        let facebook = agents
            .parse("Mozilla/5.0 (iPhone; CPU iPhone OS 17_4 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Mobile/15E148 [FBAN/FBIOS;FBAV/470.0.0.35.108;FBBV/600000000;FBDV/iPhone15,2;FBMD/iPhone;FBSN/iOS;FBSV/17.4;FBSS/3;FBCR/;FBID/phone;FBLC/en_US;FBOP/80]")
            .unwrap();
        assert_eq!(facebook.browser, "Mobile App");

        assert_eq!(agents.parse("Mozilla/5.0 (compatible; Googlebot/2.1; +http://www.google.com/bot.html)"), None);
        assert_eq!(agents.parse("Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) HeadlessChrome/127.0.0.0 Safari/537.36"), None);
        assert_eq!(agents.parse("curl/8.5.0").unwrap().browser, "curl");

        // Fire OS is numbered by the Android release it's built on.
        let fire = agents
            .parse("Mozilla/5.0 (Linux; Android 9; KFTRWI) AppleWebKit/537.36 (KHTML, like Gecko) Silk/127.4.1 like Chrome/127.0.6533.103 Safari/537.36")
            .unwrap();
        assert_eq!((fire.os.as_str(), fire.os_version.as_str(), fire.device.as_str()), ("Fire OS", "7", "Tablet"));
    }
}
