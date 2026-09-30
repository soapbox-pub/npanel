#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceType {
    Desktop,
    Smartphone,
    Tablet,
    Phablet,
    FeaturePhone,
    Console,
    Tv,
    CarBrowser,
    Camera,
    PortableMediaPlayer,
    Notebook,
    SmartDisplay,
    SmartSpeaker,
    Wearable,
    Peripheral,
}

impl DeviceType {
    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "desktop" => Some(Self::Desktop),
            "smartphone" => Some(Self::Smartphone),
            "tablet" => Some(Self::Tablet),
            "phablet" => Some(Self::Phablet),
            "feature phone" => Some(Self::FeaturePhone),
            "console" => Some(Self::Console),
            "tv" | "television" => Some(Self::Tv),
            "car browser" => Some(Self::CarBrowser),
            "camera" => Some(Self::Camera),
            "portable media player" => Some(Self::PortableMediaPlayer),
            "notebook" => Some(Self::Notebook),
            "smart display" => Some(Self::SmartDisplay),
            "smart speaker" => Some(Self::SmartSpeaker),
            "wearable" => Some(Self::Wearable),
            "peripheral" => Some(Self::Peripheral),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Desktop => "desktop",
            Self::Smartphone => "smartphone",
            Self::Tablet => "tablet",
            Self::Phablet => "phablet",
            Self::FeaturePhone => "feature phone",
            Self::Console => "console",
            Self::Tv => "tv",
            Self::CarBrowser => "car browser",
            Self::Camera => "camera",
            Self::PortableMediaPlayer => "portable media player",
            Self::Notebook => "notebook",
            Self::SmartDisplay => "smart display",
            Self::SmartSpeaker => "smart speaker",
            Self::Wearable => "wearable",
            Self::Peripheral => "peripheral",
        }
    }
}