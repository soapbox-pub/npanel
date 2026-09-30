#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientType {
    Browser,
    FeedReader,
    MobileApp,
    Pim,
    Library,
    MediaPlayer,
}

impl ClientType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Browser => "browser",
            Self::FeedReader => "feed reader",
            Self::MobileApp => "mobile app",
            Self::Pim => "pim",
            Self::Library => "library",
            Self::MediaPlayer => "mediaplayer",
        }
    }
}
