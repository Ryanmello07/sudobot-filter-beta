use super::*;
use crate::model::JevResult;
use crate::secret::Secret;
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::HashMap;

pub const CLASSES: &[(&str, &str)] = &[
    ("legitimate", "Ordinary conversation, questions, jokes, or on-topic sharing"),
    ("spam", "Unsolicited repetitive or promotional content"),
    ("scam_phishing", "Attempts to steal accounts, money or credentials, fake giveaways, malicious links"),
    ("promo_solicitation", "Advertising a product, service, server or channel"),
    ("nsfw_lure", "Pushes people toward adult content or adult servers"),
    ("other_abuse", "Harassment, hate or threats"),
];

/// What Jev is asked to judge. Message text is truncated by the caller.
#[derive(Debug, Clone, Serialize, Default)]
pub struct JevState {
    pub message: String,
    pub image_text: Option<String>,
    pub prior_messages: Vec<String>,
    pub account_age_days: i64,
    pub minutes_in_guild: Option<i64>,
    pub has_default_avatar: bool,
    pub role_count: usize,
}

pub fn questions() -> Value {
    let criteria: serde_json::Map<String, Value> = CLASSES.iter().map(|(k, d)| (k.to_string(), json!(d))).collect();
    json!({
        "class": {"type":"choice","instructions":"Classify the message sent in a Discord server","criteria": criteria},
        "dm_lure": {"type":"noul","instructions":"The message pushes people to DM the author or to join another server or channel"},
        "crypto_giveaway": {"type":"noul","instructions":"The message promotes a giveaway, airdrop, investment, or similar bait"},
        "evasion": {"type":"noul","instructions":"The text is written to dodge a filter, for example odd spacing, lookalike characters, or split links"},
        "bot_like": {"type":"noul","instructions":"The author's recent messages and profile look automated or templated"}
    })
}

#[derive(Clone)]
pub struct JevClient {
    http: reqwest::Client,
    base: String,
    key: Secret,
    model: String,
}

impl JevClient {
    pub fn new(base: &str, key: Secret, model: &str, timeout: Duration) -> Self {
        Self { http: http_client(timeout), base: base.trim_end_matches('/').into(), key, model: model.into() }
    }

    pub async fn assess(&self, state: &JevState) -> Result<JevResult, ProviderError> {
        let body = json!({"state": state, "model": self.model, "questions": questions()});
        let resp = self
            .http
            .post(format!("{}/systemone", self.base))
            .bearer_auth(self.key.expose())
            .json(&body)
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

fn malformed(s: &str) -> ProviderError {
    ProviderError::Malformed(s.into())
}

fn noul(a: &Value, id: &str) -> Result<f64, ProviderError> {
    let v = a
        .pointer(&format!("/{id}/noul"))
        .and_then(Value::as_f64)
        .ok_or_else(|| malformed(&format!("missing noul answer {id}")))?;
    if !(0.0..=1.0).contains(&v) {
        return Err(malformed(&format!("noul {id} out of range")));
    }
    Ok(v)
}

/// Strict: the response must contain every question with the expected shape.
pub fn parse(text: &str) -> Result<JevResult, ProviderError> {
    let v: Value = serde_json::from_str(text).map_err(|e| malformed(&e.to_string()))?;
    let a = v.get("answers").ok_or_else(|| malformed("missing answers"))?;
    let class = a.pointer("/class/choice").and_then(Value::as_str).ok_or_else(|| malformed("missing class choice"))?;
    if !CLASSES.iter().any(|(k, _)| *k == class) {
        return Err(malformed("unknown class label"));
    }
    let probs: HashMap<String, f64> = a
        .pointer("/class/probabilities")
        .and_then(Value::as_object)
        .map(|m| m.iter().filter_map(|(k, v)| v.as_f64().map(|f| (k.clone(), f))).collect())
        .unwrap_or_default();
    Ok(JevResult {
        class: class.into(),
        class_probs: probs,
        class_confidence: a.pointer("/class/confidence").and_then(Value::as_f64).unwrap_or(0.0),
        dm_lure: noul(a, "dm_lure")?,
        crypto_giveaway: noul(a, "crypto_giveaway")?,
        evasion: noul(a, "evasion")?,
        bot_like: noul(a, "bot_like")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_partial_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const OK: &str = r#"{"model":"jev-1.13.0","answers":{
      "class":{"type":"choice","choice":"scam_phishing","confidence":0.97,"probabilities":{"scam_phishing":0.9,"legitimate":0.02,"spam":0.08}},
      "dm_lure":{"type":"noul","noul":0.8},"crypto_giveaway":{"type":"noul","noul":0.95},
      "evasion":{"type":"noul","noul":0.1},"bot_like":{"type":"noul","noul":0.6}},"usage":{"input_tokens":1,"output_tokens":1}}"#;

    fn client(s: &MockServer) -> JevClient {
        JevClient::new(&s.uri(), Secret::new("k"), "jev-latest", Duration::from_millis(300))
    }

    #[tokio::test]
    async fn request_shape_and_parse() {
        let s = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/systemone"))
            .and(header("authorization", "Bearer k"))
            .and(body_partial_json(json!({"model":"jev-latest","state":{"message":"free nitro"},"questions":{"dm_lure":{"type":"noul"},"class":{"type":"choice"}}})))
            .respond_with(ResponseTemplate::new(200).set_body_string(OK))
            .expect(1)
            .mount(&s)
            .await;
        let st = JevState { message: "free nitro".into(), ..Default::default() };
        let r = client(&s).assess(&st).await.unwrap();
        assert_eq!(r.class, "scam_phishing");
        assert_eq!(r.class_probs["legitimate"], 0.02);
        assert_eq!(r.crypto_giveaway, 0.95);
    }

    #[test]
    fn schema_mismatches_are_malformed() {
        for bad in [
            "{}",
            r#"{"answers":{}}"#,
            r#"{"answers":{"class":{"choice":"banana"},"dm_lure":{"noul":0.1},"crypto_giveaway":{"noul":0.1},"evasion":{"noul":0.1},"bot_like":{"noul":0.1}}}"#,
            r#"{"answers":{"class":{"choice":"spam"},"dm_lure":{"noul":1.5},"crypto_giveaway":{"noul":0.1},"evasion":{"noul":0.1},"bot_like":{"noul":0.1}}}"#,
            r#"{"answers":{"class":{"choice":"spam"},"dm_lure":{"noul":0.1}}}"#,
        ] {
            let e = parse(bad).unwrap_err();
            assert!(matches!(e, ProviderError::Malformed(_)), "{bad}");
            assert!(e.is_health_failure());
        }
    }

    #[tokio::test]
    async fn http_errors() {
        let s = MockServer::start().await;
        Mock::given(method("POST")).respond_with(ResponseTemplate::new(429)).mount(&s).await;
        assert_eq!(client(&s).assess(&Default::default()).await.unwrap_err(), ProviderError::Quota);
        let s = MockServer::start().await;
        Mock::given(method("POST")).respond_with(ResponseTemplate::new(500)).mount(&s).await;
        assert_eq!(client(&s).assess(&Default::default()).await.unwrap_err(), ProviderError::Server(500));
    }

    #[tokio::test]
    async fn timeout() {
        let s = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_string(OK).set_delay(Duration::from_millis(800)))
            .mount(&s)
            .await;
        assert_eq!(client(&s).assess(&Default::default()).await.unwrap_err(), ProviderError::Timeout);
    }
}
