use std::time::Duration;

/// Top-level error type shared by the NEXIUM engine.
#[derive(Debug, thiserror::Error)]
pub enum NxError {
    #[error("invalid URL: {0}")]
    InvalidUrl(String),

    #[error("unsupported URL scheme: {0}")]
    UnsupportedScheme(String),

    #[error("network error: {0}")]
    Network(String),

    #[error("request timed out after {0:?}")]
    Timeout(Duration),

    #[error("too many redirects (limit: {0})")]
    TooManyRedirects(usize),

    #[error("invalid request header: {0}")]
    InvalidHeader(String),

    #[error("request body exceeds the {0} byte limit")]
    BodyTooLarge(usize),

    #[error("response body exceeds the {0} byte limit")]
    ResponseTooLarge(usize),

    #[error("configuration error: {0}")]
    Config(String),

    #[error("rendering error: {0}")]
    Render(String),

    #[error("image error: {0}")]
    Image(String),

    #[error("storage error: {0}")]
    Storage(String),

    #[error("browser error: {0}")]
    Browser(String),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, NxError>;
