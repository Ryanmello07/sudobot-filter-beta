//! Traits at the edges, so the pipeline can be tested with fakes.

use crate::model::{JevResult, PaxmodResult};
use crate::providers::jev::{JevClient, JevState};
use crate::providers::ocr::OcrClient;
use crate::providers::paxmod::PaxmodClient;
use crate::providers::ProviderError;
use async_trait::async_trait;

#[async_trait]
pub trait TextModerator: Send + Sync {
    async fn moderate(&self, msg: &str, user: Option<u64>, context: Option<u64>) -> Result<PaxmodResult, ProviderError>;
}

#[async_trait]
pub trait Classifier: Send + Sync {
    async fn assess(&self, state: &JevState) -> Result<JevResult, ProviderError>;
}

#[async_trait]
pub trait TextReader: Send + Sync {
    async fn read(&self, bytes: &[u8], mime: &str) -> Result<String, ProviderError>;
}

#[async_trait]
impl TextModerator for PaxmodClient {
    async fn moderate(&self, msg: &str, user: Option<u64>, context: Option<u64>) -> Result<PaxmodResult, ProviderError> {
        PaxmodClient::moderate(self, msg, user, context).await
    }
}

#[async_trait]
impl Classifier for JevClient {
    async fn assess(&self, state: &JevState) -> Result<JevResult, ProviderError> {
        JevClient::assess(self, state).await
    }
}

#[async_trait]
impl TextReader for OcrClient {
    async fn read(&self, bytes: &[u8], mime: &str) -> Result<String, ProviderError> {
        OcrClient::read(self, bytes, mime).await
    }
}

/// Downloads an image. Implementations must enforce the host allowlist and size cap.
#[async_trait]
pub trait ImageFetcher: Send + Sync {
    async fn fetch(&self, url: &str, max_bytes: usize) -> anyhow::Result<Vec<u8>>;
}

pub struct HttpImageFetcher {
    http: reqwest::Client,
}

/// Image fetches never follow redirects: a redirect would send us to an arbitrary
/// host after the allowlist check on the original URL. The shared provider client
/// keeps default redirect behavior; this one is built separately on purpose.
fn fetch_client(timeout: std::time::Duration) -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(timeout)
        .user_agent("sudobot-chat-guard/0.1")
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("reqwest client")
}

impl HttpImageFetcher {
    pub fn new() -> Self {
        Self { http: fetch_client(std::time::Duration::from_secs(8)) }
    }
}

impl Default for HttpImageFetcher {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ImageFetcher for HttpImageFetcher {
    async fn fetch(&self, url: &str, max_bytes: usize) -> anyhow::Result<Vec<u8>> {
        anyhow::ensure!(crate::imaging::is_allowed_host(url), "host not allowed");
        let mut resp = self.http.get(url).send().await?.error_for_status()?;
        if resp.content_length().is_some_and(|l| l as usize > max_bytes) {
            anyhow::bail!("image too large");
        }
        let mut buf = Vec::new();
        while let Some(chunk) = resp.chunk().await? {
            buf.extend_from_slice(&chunk);
            anyhow::ensure!(buf.len() <= max_bytes, "image too large");
        }
        Ok(buf)
    }
}

/// Discord side effects. Implementations perform the hierarchy checks in `can_act`.
#[async_trait]
pub trait ActionSink: Send + Sync {
    /// Ok if the bot may moderate this member (permissions, role hierarchy, not owner).
    async fn can_act(&self, guild: u64, user: u64) -> Result<(), String>;
    async fn delete(&self, channel: u64, message: u64) -> anyhow::Result<()>;
    async fn bulk_delete(&self, channel: u64, messages: &[u64]) -> anyhow::Result<()>;
    async fn timeout(&self, guild: u64, user: u64, minutes: u32) -> anyhow::Result<()>;
    async fn kick(&self, guild: u64, user: u64, reason: &str) -> anyhow::Result<()>;
    async fn ban(&self, guild: u64, user: u64, delete_seconds: u32, reason: &str) -> anyhow::Result<()>;
}

#[derive(Debug, Clone)]
pub struct IncidentReport {
    pub incident_id: i64,
    pub guild_id: u64,
    pub user_id: u64,
    pub channel_id: u64,
    pub message_id: u64,
    pub tier: String,
    pub label: String,
    pub score: f64,
    pub planned: String,
    pub taken: String,
    pub observe: bool,
    pub account_age_days: i64,
    pub minutes_in_guild: Option<i64>,
    pub coverage: String,
    pub evidence: Vec<String>,
    pub excerpt: String,
    pub image_text: Option<String>,
    pub failures: Vec<String>,
    pub swept: usize,
}

#[async_trait]
pub trait ModLog: Send + Sync {
    async fn incident(&self, report: &IncidentReport);
    async fn notice(&self, guild: u64, text: &str);
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    // S1: the image fetcher's client must not follow redirects (a redirect would
    // defeat the CDN host allowlist). wiremock can't stand in for an allowlisted
    // host, so this asserts the client's redirect policy directly.
    #[tokio::test]
    async fn fetch_client_does_not_follow_redirects() {
        let s = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(302).insert_header("location", "http://evil.example/steal"))
            .mount(&s)
            .await;
        let resp = fetch_client(std::time::Duration::from_secs(1)).get(s.uri()).send().await.unwrap();
        assert_eq!(resp.status(), 302, "redirect must surface, not be followed");
    }
}
