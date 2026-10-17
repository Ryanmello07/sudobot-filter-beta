use super::*;
use crate::model::PaxmodResult;
use crate::secret::Secret;
use serde_json::{json, Value};
use std::collections::HashMap;

const KNOWN: &[&str] = &[
    "status", "id", "service", "request", "result", "reason", "degraded",
    "unavailable_categories", "content_moderation",
];

#[derive(Clone)]
pub struct PaxmodClient {
    http: reqwest::Client,
    base: String,
    key: Secret,
}

impl PaxmodClient {
    pub fn new(base: &str, key: Secret, timeout: Duration) -> Self {
        Self { http: http_client(timeout), base: base.trim_end_matches('/').into(), key }
    }

    /// `user_id` and `context_id` are what drive Paxmod's Player Context.
    pub async fn moderate(
        &self,
        message: &str,
        user_id: Option<u64>,
        context_id: Option<u64>,
    ) -> Result<PaxmodResult, ProviderError> {
        let msg: String = message.chars().take(5000).collect();
        let mut body = json!({ "message": msg });
        if let Some(u) = user_id {
            body["user_id"] = json!(u.to_string());
        }
        if let Some(c) = context_id {
            body["context_id"] = json!(c.to_string());
        }
        let resp = self
            .http
            .post(format!("{}/text", self.base))
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

pub fn parse(text: &str) -> Result<PaxmodResult, ProviderError> {
    let v: Value = serde_json::from_str(text).map_err(|e| ProviderError::Malformed(e.to_string()))?;
    let obj = v.as_object().ok_or_else(|| ProviderError::Malformed("not an object".into()))?;
    if obj.get("status").and_then(Value::as_str) == Some("error") {
        let code = v.pointer("/error/code").and_then(Value::as_str).unwrap_or("UNKNOWN");
        return Err(match code {
            "USAGE_LIMIT_EXCEEDED" => ProviderError::Quota,
            "LLM_SERVICE_ERROR" | "INTERNAL_SERVER_ERROR" => ProviderError::Server(503),
            other => ProviderError::Rejected(200, other.into()),
        });
    }
    let cm = obj
        .get("content_moderation")
        .and_then(Value::as_object)
        .ok_or_else(|| ProviderError::Malformed("missing content_moderation".into()))?;
    let flagged = cm.get("flagged").and_then(Value::as_bool).unwrap_or(false);
    let mut categories = HashMap::new();
    if let Some(c) = cm.get("categories").and_then(Value::as_object) {
        for (k, v) in c {
            if let Some(s) = v.get("score").and_then(Value::as_f64) {
                categories.insert(k.clone(), s);
            }
        }
    }
    let reason = obj.get("reason").and_then(Value::as_str).map(str::to_string);
    let ban_hit = flagged
        && categories.is_empty()
        && reason.as_deref().is_some_and(|r| r.to_lowercase().contains("ban"));
    let extra: serde_json::Map<String, Value> =
        obj.iter().filter(|(k, _)| !KNOWN.contains(&k.as_str())).map(|(k, v)| (k.clone(), v.clone())).collect();
    Ok(PaxmodResult {
        id: obj.get("id").and_then(Value::as_str).map(str::to_string),
        categories,
        flagged,
        reason,
        degraded: obj.get("degraded").and_then(Value::as_bool).unwrap_or(false),
        ban_hit,
        extra: Value::Object(extra),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_partial_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn client(s: &MockServer) -> PaxmodClient {
        PaxmodClient::new(&s.uri(), Secret::new("k"), Duration::from_millis(300))
    }

    const OK: &str = r#"{"status":"success","id":"abc","service":"pax","result":"flagged","reason":"Content flagged for Spam",
      "content_moderation":{"flagged":true,"categories":{"Spam":{"flagged":true,"score":0.93,"threshold":0.5},"Harassment":{"flagged":false,"score":0.1,"threshold":0.5}}}}"#;

    #[tokio::test]
    async fn sends_ids_and_parses_scores() {
        let s = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/text"))
            .and(header("authorization", "Bearer k"))
            .and(body_partial_json(json!({"message":"hi","user_id":"7","context_id":"9"})))
            .respond_with(ResponseTemplate::new(200).set_body_string(OK))
            .expect(1)
            .mount(&s)
            .await;
        let r = client(&s).moderate("hi", Some(7), Some(9)).await.unwrap();
        assert_eq!(r.id.as_deref(), Some("abc"));
        assert_eq!(r.categories["Spam"], 0.93);
        assert!(!r.degraded && !r.ban_hit);
    }

    #[tokio::test]
    async fn truncates_to_5000_chars() {
        let s = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(move |req: &wiremock::Request| {
                let v: Value = serde_json::from_slice(&req.body).unwrap();
                assert_eq!(v["message"].as_str().unwrap().chars().count(), 5000);
                ResponseTemplate::new(200).set_body_string(OK)
            })
            .mount(&s)
            .await;
        client(&s).moderate(&"x".repeat(9000), None, None).await.unwrap();
    }

    #[test]
    fn degraded_and_unknown_fields_preserved() {
        let r = parse(r#"{"status":"success","id":"a","degraded":true,"unavailable_categories":["Hate"],
          "player_context":{"risk":0.7},"content_moderation":{"flagged":false,"categories":{}}}"#).unwrap();
        assert!(r.degraded);
        assert_eq!(r.extra["player_context"]["risk"], 0.7);
        assert!(r.extra.get("id").is_none());
    }

    #[test]
    fn ban_hit_detected() {
        let r = parse(r#"{"status":"success","id":"a","reason":"User is banned","content_moderation":{"flagged":true,"categories":{}}}"#).unwrap();
        assert!(r.ban_hit);
        let r = parse(r#"{"status":"success","id":"a","reason":"Whitelisted","content_moderation":{"flagged":true,"categories":{}}}"#).unwrap();
        assert!(!r.ban_hit);
    }

    #[tokio::test]
    async fn error_mapping() {
        for (code, body, want) in [
            (429u16, r#"{"status":"error","error":{"code":"USAGE_LIMIT_EXCEEDED","message":"x"}}"#, ProviderError::Quota),
            (503, r#"{"status":"error","error":{"code":"LLM_SERVICE_ERROR","message":"x"}}"#, ProviderError::Server(503)),
        ] {
            let s = MockServer::start().await;
            Mock::given(method("POST")).respond_with(ResponseTemplate::new(code).set_body_string(body)).mount(&s).await;
            assert_eq!(client(&s).moderate("hi", None, None).await.unwrap_err(), want);
        }
        let s = MockServer::start().await;
        Mock::given(method("POST")).respond_with(ResponseTemplate::new(401).set_body_string("{}")).mount(&s).await;
        let e = client(&s).moderate("hi", None, None).await.unwrap_err();
        assert!(matches!(e, ProviderError::Rejected(401, _)));
        assert!(!e.is_health_failure());
    }

    #[tokio::test]
    async fn timeout_and_malformed() {
        let s = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_string(OK).set_delay(Duration::from_millis(800)))
            .mount(&s)
            .await;
        assert_eq!(client(&s).moderate("hi", None, None).await.unwrap_err(), ProviderError::Timeout);
        let s = MockServer::start().await;
        Mock::given(method("POST")).respond_with(ResponseTemplate::new(200).set_body_string("not json")).mount(&s).await;
        assert!(matches!(client(&s).moderate("hi", None, None).await.unwrap_err(), ProviderError::Malformed(_)));
    }
}
