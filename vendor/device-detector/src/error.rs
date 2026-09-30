#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    IO(#[from] std::io::Error),
    #[error(transparent)]
    YAML(#[from] serde_yaml::Error),
    #[error(transparent)]
    Regex(#[from] fancy_regex::Error),
    #[error(transparent)]
    AhoCorasick(#[from] aho_corasick::BuildError),
}

pub type Result<T> = std::result::Result<T, Error>;
