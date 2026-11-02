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

/// What the request is about: a chat message, or a member's username/display name.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Ask {
    Message,
    Name,
}

/// The five core questions always; plus `policy` (messages, only when a policy is
/// configured) or `name_bad` (names).
pub fn questions(ask: Ask, policy: Option<&str>) -> Value {
    let criteria: serde_json::Map<String, Value> = CLASSES.iter().map(|(k, d)| (k.to_string(), json!(d))).collect();
    let mut q = json!({
        "class": {"type":"choice","instructions":"Classify the message sent in a Discord server","criteria": criteria},
        "dm_lure": {"type":"noul","instructions":"The message pushes people to DM the author or to join another server or channel"},
        "crypto_giveaway": {"type":"noul","instructions":"The message promotes a giveaway, airdrop, investment, or similar bait"},
        "evasion": {"type":"noul","instructions":"The text is written to dodge a filter, for example odd spacing, lookalike characters, or split links"},
        "bot_like": {"type":"noul","instructions":"The author's recent messages and profile look automated or templated"}
    });
    let extra = match (ask, policy) {
        (Ask::Message, Some(p)) => Some((
            "policy",
            json!({
                "type": "choice",
                "instructions": format!("{p}\n\nDecide whether this message should be blocked or allowed under the policy above."),
                "criteria": {"block": "Violates the policy above", "allow": "Does not violate the policy above"}
            }),
        )),
        (Ask::Message, None) => None,
        (Ask::Name, _) => Some((
            "name_bad",
            json!({
                "type": "choice",
                "instructions": "Decide whether this Discord username or display name should be blocked: it impersonates staff or support, advertises, carries an invite or link, or is a scam or giveaway lure.",
                "criteria": {"block": "Should be blocked", "allow": "Fine"}
            }),
        )),
    };
    if let (Some((k, v)), Some(m)) = (extra, q.as_object_mut()) {
        m.insert(k.into(), v);
    }
    q
}

#[derive(Clone)]
pub struct PpqClient {
    http: reqwest::Client,
    base: String,
    key: Secret,
    model: String,
    policy: Option<String>,
}

impl PpqClient {
    pub fn new(base: &str, key: Secret, model: &str, policy: Option<String>, timeout: Duration) -> Self {
        let policy = policy.filter(|p| !p.trim().is_empty());
        Self { http: http_client(timeout), base: base.trim_end_matches('/').into(), key, model: model.into(), policy }
    }

    pub async fn assess(&self, state: &JevState, ask: Ask) -> Result<JevResult, ProviderError> {
        let body = json!({"state": state, "model": self.model, "questions": questions(ask, self.policy.as_deref())});
        let resp = self
            .http
            .post(format!("{}/decisions", self.base))
            .bearer_auth(self.key.expose())
            .json(&body)
            .send()
            .await
            .map_err(map_reqwest)?;
        let status = resp.status().as_u16();
        let text = resp.text().await.map_err(map_reqwest)?;
        match map_status(status, &text) {
            // ppq's 429 is a per-minute rate limit on the key, not a spent daily quota:
            // report it as a retryable server-side failure so the short breaker handles it
            // instead of switching the model off until midnight UTC.
            Some(ProviderError::Quota) => return Err(ProviderError::Server(429)),
            Some(e) => return Err(e),
            None => {}
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

/// p(block) from the optional `policy` / `name_bad` choice answer. Absent or unreadable: None.
fn block_probability(a: &Value) -> Option<f64> {
    let ans = a.get("policy").or_else(|| a.get("name_bad"))?;
    if let Some(p) = ans.pointer("/probabilities/block").and_then(Value::as_f64) {
        return Some(p.clamp(0.0, 1.0));
    }
    match ans.get("choice").and_then(Value::as_str) {
        Some("block") => Some(1.0),
        Some("allow") => Some(0.0),
        _ => None,
    }
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
        policy_block: block_probability(a),
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

    fn client_with(s: &MockServer, policy: Option<&str>) -> PpqClient {
        PpqClient::new(&s.uri(), Secret::new("k"), "m", policy.map(String::from), Duration::from_millis(300))
    }

    fn client(s: &MockServer) -> PpqClient {
        client_with(s, None)
    }

    #[tokio::test]
    async fn request_shape_and_parse() {
        let s = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/decisions"))
            .and(header("authorization", "Bearer k"))
            .and(body_partial_json(json!({"model":"m","state":{"message":"free nitro"},"questions":{"dm_lure":{"type":"noul"},"class":{"type":"choice"}}})))
            .respond_with(ResponseTemplate::new(200).set_body_string(OK))
            .expect(1)
            .mount(&s)
            .await;
        let st = JevState { message: "free nitro".into(), ..Default::default() };
        let r = client(&s).assess(&st, Ask::Message).await.unwrap();
        assert_eq!(r.class, "scam_phishing");
        assert_eq!(r.class_probs["legitimate"], 0.02);
        assert_eq!(r.crypto_giveaway, 0.95);
        assert_eq!(r.policy_block, None);
    }

    async fn sent_questions(s: &MockServer) -> Value {
        let reqs = s.received_requests().await.unwrap();
        assert_eq!(reqs.len(), 1);
        let body: Value = serde_json::from_slice(&reqs[0].body).unwrap();
        body["questions"].clone()
    }

    #[tokio::test]
    async fn message_request_includes_policy_question() {
        let s = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/decisions"))
            .and(body_partial_json(json!({"model":"m","questions":{"policy":{"type":"choice"},"dm_lure":{"type":"noul"}}})))
            .respond_with(ResponseTemplate::new(200).set_body_string(OK))
            .expect(1)
            .mount(&s)
            .await;
        client_with(&s, Some("POLICY TEXT")).assess(&Default::default(), Ask::Message).await.unwrap();
        let q = sent_questions(&s).await;
        assert!(q["policy"]["instructions"].as_str().unwrap().contains("POLICY TEXT"));
        assert_eq!(q["policy"]["criteria"]["block"], "Violates the policy above");
        assert_eq!(q["policy"]["criteria"]["allow"], "Does not violate the policy above");
        assert!(q.get("name_bad").is_none());
    }

    #[tokio::test]
    async fn no_policy_omits_question() {
        let s = MockServer::start().await;
        Mock::given(method("POST")).respond_with(ResponseTemplate::new(200).set_body_string(OK)).mount(&s).await;
        client_with(&s, None).assess(&Default::default(), Ask::Message).await.unwrap();
        let q = sent_questions(&s).await;
        assert!(q.get("policy").is_none());
        assert!(q.get("name_bad").is_none());
        assert_eq!(q.as_object().unwrap().len(), 5);
    }

    #[tokio::test]
    async fn blank_policy_is_treated_as_none() {
        for blank in ["", "   ", " \n\t"] {
            let s = MockServer::start().await;
            Mock::given(method("POST")).respond_with(ResponseTemplate::new(200).set_body_string(OK)).mount(&s).await;
            client_with(&s, Some(blank)).assess(&Default::default(), Ask::Message).await.unwrap();
            assert!(sent_questions(&s).await.get("policy").is_none(), "blank policy {blank:?} must be omitted");
        }
    }

    #[tokio::test]
    async fn name_request_uses_name_bad_not_policy() {
        let s = MockServer::start().await;
        Mock::given(method("POST")).respond_with(ResponseTemplate::new(200).set_body_string(OK)).mount(&s).await;
        client_with(&s, Some("POLICY TEXT")).assess(&Default::default(), Ask::Name).await.unwrap();
        let q = sent_questions(&s).await;
        assert!(q.get("policy").is_none());
        assert_eq!(q["name_bad"]["type"], "choice");
        assert_eq!(q["name_bad"]["criteria"]["block"], "Should be blocked");
        assert_eq!(q["name_bad"]["criteria"]["allow"], "Fine");
        assert!(q["name_bad"]["instructions"].as_str().unwrap().starts_with("Decide whether this Discord username or display name"));
    }

    fn with_answer(key: &str, answer: &str) -> String {
        OK.replacen("\"answers\":{", &format!("\"answers\":{{\"{key}\":{answer},"), 1)
    }

    #[test]
    fn parse_reads_policy_block() {
        let r = parse(&with_answer("policy", r#"{"type":"choice","choice":"block","probabilities":{"block":0.93,"allow":0.07}}"#)).unwrap();
        assert_eq!(r.policy_block, Some(0.93));
        let r = parse(&with_answer("policy", r#"{"type":"choice","choice":"block"}"#)).unwrap();
        assert_eq!(r.policy_block, Some(1.0));
        let r = parse(&with_answer("policy", r#"{"type":"choice","choice":"allow"}"#)).unwrap();
        assert_eq!(r.policy_block, Some(0.0));
        let r = parse(&with_answer("name_bad", r#"{"type":"choice","choice":"block","probabilities":{"block":0.81,"allow":0.19}}"#)).unwrap();
        assert_eq!(r.policy_block, Some(0.81));
        assert_eq!(parse(OK).unwrap().policy_block, None);
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
        let e = client(&s).assess(&Default::default(), Ask::Message).await.unwrap_err();
        assert_eq!(e, ProviderError::Server(429));
        assert!(e.is_health_failure() && e.is_retryable());
        let s = MockServer::start().await;
        Mock::given(method("POST")).respond_with(ResponseTemplate::new(500)).mount(&s).await;
        assert_eq!(client(&s).assess(&Default::default(), Ask::Message).await.unwrap_err(), ProviderError::Server(500));
    }

    #[tokio::test]
    async fn timeout() {
        let s = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_string(OK).set_delay(Duration::from_millis(800)))
            .mount(&s)
            .await;
        assert_eq!(client(&s).assess(&Default::default(), Ask::Message).await.unwrap_err(), ProviderError::Timeout);
    }
}
