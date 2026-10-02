#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    IO(#[from] std::io::Error),
    #[error(transparent)]
    YAML(#[from] serde_yaml::Error),
    /// Boxed because it is over a hundred bytes, and every `Result` here would
    /// otherwise be at least that big.
    #[error(transparent)]
    Regex(Box<fancy_regex::Error>),
    #[error(transparent)]
    AhoCorasick(#[from] aho_corasick::BuildError),
}

impl From<fancy_regex::Error> for Error {
    fn from(error: fancy_regex::Error) -> Self {
        Error::Regex(Box::new(error))
    }
}

pub type Result<T> = std::result::Result<T, Error>;
