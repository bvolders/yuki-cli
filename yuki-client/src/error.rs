use thiserror::Error;

#[derive(Debug, Error)]
pub enum YukiError {
    /// A SOAP fault or local check that reads as an authentication failure.
    #[error("authentication failed: {0}")]
    AuthFailed(String),

    /// HTTP 401 or 403: refused before the request was processed.
    #[error("authentication failed: HTTP {0}")]
    Unauthorized(u16),

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

/// Whether a failed request may have been processed by Yuki.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delivery {
    /// The request never left: no connection, or it could not be built.
    NotSent,
    /// Yuki refused it unprocessed (HTTP 401, 403 or 429, or a fault that
    /// refuses every such request, such as [`YukiError::is_missing_client_id`]).
    Refused,
    /// It may have been processed.
    Unknown,
}

impl YukiError {
    /// Whether the failed request may have been processed.
    pub fn delivery(&self) -> Delivery {
        match self {
            Self::Request(e) if e.is_connect() || e.is_builder() => Delivery::NotSent,
            Self::Unauthorized(_) | Self::RateLimited => Delivery::Refused,
            e if e.is_missing_client_id() => Delivery::Refused,
            _ => Delivery::Unknown,
        }
    }

    /// Whether this is Yuki's refusal of a session opened without an API
    /// client: `soap:Client` "No Client ID supplied", which
    /// `ProcessPurchaseInvoices` answers to plain `Authenticate` before it
    /// processes anything.
    pub fn is_missing_client_id(&self) -> bool {
        matches!(self, Self::SoapFault { code, message }
            if code.to_lowercase().ends_with("client")
                && message.trim().eq_ignore_ascii_case("No Client ID supplied"))
    }

    pub fn exit_code(&self) -> u8 {
        match self {
            Self::AuthFailed(_) | Self::Unauthorized(_) => 2,
            Self::NotFound(_) => 3,
            Self::RateLimited => 4,
            _ => 1,
        }
    }
}
