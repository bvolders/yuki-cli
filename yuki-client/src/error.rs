use thiserror::Error;

#[derive(Debug, Error)]
pub enum YukiError {
    #[error("authentication failed: {0}")]
    AuthFailed(String),

    #[error("not found: {0}")]
    NotFound(String),

    #[error("rate limited: 1000 calls/day exceeded")]
    RateLimited,

    #[error("HTTP error {status}: {body}")]
    Http { status: u16, body: String },

    #[error("SOAP fault [{code}]: {message}")]
    SoapFault { code: String, message: String },

    #[error("configuration error: {0}")]
    Config(String),

    #[error("XML error: {0}")]
    Xml(String),

    #[error("{}", request_message(.0))]
    Request(#[from] reqwest::Error),
}

/// A transport error, saying plainly when it was a timeout.
fn request_message(e: &reqwest::Error) -> String {
    if e.is_timeout() {
        format!("request timed out: {e}")
    } else {
        e.to_string()
    }
}

impl YukiError {
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::AuthFailed(_) => 2,
            Self::NotFound(_) => 3,
            Self::RateLimited => 4,
            _ => 1,
        }
    }
}
