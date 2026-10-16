use super::*;
use crate::secret::Secret;
use base64::Engine;
use serde_json::Value;

#[derive(Clone)]
pub struct OcrClient {
    http: reqwest::Client,
    base: String,
    key: Secret,
    engine: u8,
}

impl OcrClient {
    pub fn new(base: &str, key: Secret, engine: u8, timeout: Duration) -> Self {
        Self { http: http_client(timeout), base: base.trim_end_matches('/').into(), key, engine }
    }

    /// `mime` is e.g. "image/png". Returns the extracted text (possibly empty).
    pub async fn read(&self, bytes: &[u8], mime: &str) -> Result<String, ProviderError> {
        let data = format!("data:{mime};base64,{}", base64::engine::general_purpose::STANDARD.encode(bytes));
        let form = reqwest::multipart::Form::new()
            .text("base64Image", data)
            .text("language", "auto")
            .text("OCREngine", self.engine.to_string())
            .text("detectOrientation", "true")
            .text("scale", "true")
            .text("isOverlayRequired", "false");
        let resp = self
            .http
            .post(format!("{}/parse/image", self.base))
            .header("apikey", self.key.expose())
            .multipart(form)
            .send()
            .await
            .map_err(map_reqwest)?;
        let status = resp.status().as_u16();
        let text = resp.text().await.map_err(map_reqwest)?;
        if let Some(e) = map_status(status, &text) {
            return Err(e);
        }
        parse(&text)
    }
}

pub fn parse(text: &str) -> Result<String, ProviderError> {
    let v: Value = serde_json::from_str(text).map_err(|e| ProviderError::Malformed(e.to_string()))?;
    if v.get("IsErroredOnProcessing").and_then(Value::as_bool).unwrap_or(false) {
        let msg = match v.get("ErrorMessage") {
            Some(Value::Array(a)) => a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join("; "),
            Some(Value::String(s)) => s.clone(),
            _ => "ocr error".into(),
        };
        return Err(ProviderError::Rejected(200, msg));
    }
    let results = v
        .get("ParsedResults")
        .and_then(Value::as_array)
        .ok_or_else(|| ProviderError::Malformed("missing ParsedResults".into()))?;
    Ok(results
        .iter()
        .filter_map(|r| r.get("ParsedText").and_then(Value::as_str))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn client(s: &MockServer) -> OcrClient {
        OcrClient::new(&s.uri(), Secret::new("k"), 2, Duration::from_millis(300))
    }

    #[tokio::test]
    async fn extracts_text_and_sends_expected_form() {
        let s = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/parse/image"))
            .and(header("apikey", "k"))
            .respond_with(move |req: &wiremock::Request| {
                let body = String::from_utf8_lossy(&req.body).to_string();
                assert!(body.contains("base64Image"));
                assert!(body.contains("data:image/png;base64,"));
                assert!(body.contains("OCREngine"));
                ResponseTemplate::new(200).set_body_string(
                    r#"{"OCRExitCode":1,"IsErroredOnProcessing":false,"ParsedResults":[{"ParsedText":"FREE NITRO\r\nclaim at scam.xyz ","FileParseExitCode":1}]}"#,
                )
            })
            .expect(1)
            .mount(&s)
            .await;
        let t = client(&s).read(b"\x89PNG", "image/png").await.unwrap();
        assert!(t.contains("FREE NITRO") && t.contains("scam.xyz"));
    }

    #[test]
    fn empty_text_is_ok() {
        assert_eq!(parse(r#"{"IsErroredOnProcessing":false,"ParsedResults":[{"ParsedText":"  "}]}"#).unwrap(), "");
    }

    #[test]
    fn processing_error_is_rejected_not_health_failure() {
        let e = parse(r#"{"IsErroredOnProcessing":true,"ErrorMessage":["File size exceeds limit"]}"#).unwrap_err();
        assert!(matches!(e, ProviderError::Rejected(_, _)));
        assert!(!e.is_health_failure());
    }

    #[tokio::test]
    async fn quota_and_timeout() {
        let s = MockServer::start().await;
        Mock::given(method("POST")).respond_with(ResponseTemplate::new(429)).mount(&s).await;
        assert_eq!(client(&s).read(b"x", "image/png").await.unwrap_err(), ProviderError::Quota);
        let s = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_string("{}").set_delay(Duration::from_millis(800)))
            .mount(&s)
            .await;
        assert_eq!(client(&s).read(b"x", "image/png").await.unwrap_err(), ProviderError::Timeout);
    }
}
