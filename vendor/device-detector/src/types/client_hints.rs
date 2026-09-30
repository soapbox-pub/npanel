/// Client hints extracted from HTTP headers (e.g. `X-Requested-With`,
/// `Sec-CH-UA-Mobile`, `Sec-CH-UA-Model`).
#[derive(Debug, Clone, Default)]
pub struct ClientHints {
    /// Value of the `X-Requested-With` header (Android app/browser package ID).
    pub x_requested_with: Option<String>,
    /// Device model from `Sec-CH-UA-Model`.
    pub model: Option<String>,
    /// Mobile flag from `Sec-CH-UA-Mobile` (`?1` → true).
    pub mobile: Option<bool>,
}
