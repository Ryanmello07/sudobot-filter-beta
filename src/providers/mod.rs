//! HTTP clients for Paxmod, Jev and OCR.space.

pub mod jev;
pub mod ocr;
pub mod paxmod;

use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProviderError {
    #[error("timeout")]
    Timeout,
    #[error("transport: {0}")]
    Transport(String),
    #[error("server error {0}")]
    Server(u16),
    /// Plan quota used up (HTTP 429). Not retried; stops the provider for the day.
    #[error("quota exceeded")]
    Quota,
    /// 4xx or an explicit API refusal. Says nothing about provider health.
    #[error("rejected {0}: {1}")]
    Rejected(u16, String),
    #[error("malformed response: {0}")]
    Malformed(String),
}

impl ProviderError {
    /// Whether this should count toward the circuit breaker.
    pub fn is_health_failure(&self) -> bool {
        matches!(self, Self::Timeout | Self::Transport(_) | Self::Server(_) | Self::Malformed(_))
    }
    /// Whether one quick retry makes sense.
    pub fn is_retryable(&self) -> bool {
        matches!(self, Self::Timeout | Self::Transport(_) | Self::Server(_))
    }
}

pub fn map_reqwest(e: reqwest::Error) -> ProviderError {
    if e.is_timeout() {
        ProviderError::Timeout
    } else {
        // Strip the URL: it may embed nothing secret, but keep logs tidy.
        ProviderError::Transport(e.without_url().to_string())
    }
}

pub fn map_status(status: u16, body: &str) -> Option<ProviderError> {
    match status {
        200..=299 => None,
        429 => Some(ProviderError::Quota),
        500..=599 => Some(ProviderError::Server(status)),
        _ => Some(ProviderError::Rejected(status, body.chars().take(200).collect())),
    }
}

pub fn http_client(timeout: Duration) -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(timeout)
        .user_agent("sudobot-chat-guard/0.1")
        .build()
        .expect("reqwest client")
}
