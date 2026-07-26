//! Chat completions: `POST {base}/chat/completions` with a bearer token.
//!
//! The lingua franca — OpenAI, DeepSeek, Moonshot, Ollama, vLLM, LiteLLM and
//! every gateway that copies them.

use serde::Deserialize;
use serde_json::{json, Value};

use super::{clamp_temperature, decode, require_text, ChatRequest, Wire};
use crate::error::Result;

/// Chat completions accept up to 2.0.
const MAX_TEMPERATURE: f32 = 2.0;

pub(super) struct OpenaiCompat;

impl Wire for OpenaiCompat {
    fn label(&self) -> &'static str {
        "OpenAI 兼容接口"
    }

    fn endpoint(&self, base: &str, _model: &str) -> String {
        format!("{}/chat/completions", base.trim_end_matches('/'))
    }

    fn authorize(&self, req: reqwest::RequestBuilder, api_key: &str) -> reqwest::RequestBuilder {
        req.bearer_auth(api_key)
    }

    fn body(&self, req: &ChatRequest<'_>) -> Value {
        let mut body = json!({
            "model": req.model,
            "temperature": clamp_temperature(req.temperature, MAX_TEMPERATURE),
            "max_tokens": req.max_tokens,
            "messages": [
                { "role": "system", "content": req.system },
                { "role": "user", "content": req.user },
            ],
        });
        if req.json_mode {
            // Absent unless asked: older gateways reject an unknown key outright.
            body["response_format"] = json!({ "type": "json_object" });
        }
        body
    }

    fn extract(&self, raw: &str) -> Result<String> {
        let parsed: ChatResponse = decode(raw)?;
        let text = parsed
            .choices
            .into_iter()
            .next()
            .and_then(|c| c.message.content)
            .unwrap_or_default();
        require_text(text, raw)
    }
}

/// Chat completions envelope. Everything is optional: gateways differ, and a
/// missing field must surface as our own error, not a serde failure.
#[derive(Debug, Deserialize)]
struct ChatResponse {
    #[serde(default)]
    choices: Vec<ChatChoice>,
}

#[derive(Debug, Default, Deserialize)]
struct ChatChoice {
    #[serde(default)]
    message: ChatMessage,
}

#[derive(Debug, Default, Deserialize)]
struct ChatMessage {
    #[serde(default)]
    content: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::sample_request;

    #[test]
    fn builds_a_chat_completions_body() {
        let body = OpenaiCompat.body(&sample_request(false));
        assert_eq!(body["model"], "test-model");
        assert_eq!(body["max_tokens"], 400);
        assert_eq!(body["temperature"], 0.25);
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][0]["content"], "SYSTEM TEXT");
        assert_eq!(body["messages"][1]["role"], "user");
        assert_eq!(body["messages"][1]["content"], "USER TEXT");
        assert!(body.get("response_format").is_none());
    }

    #[test]
    fn json_mode_sets_response_format() {
        let body = OpenaiCompat.body(&sample_request(true));
        assert_eq!(body["response_format"]["type"], "json_object");
    }

    #[test]
    fn endpoint_ignores_a_trailing_slash() {
        assert_eq!(
            OpenaiCompat.endpoint("http://127.0.0.1:11434/v1/", "llama3"),
            "http://127.0.0.1:11434/v1/chat/completions"
        );
    }

    #[test]
    fn extracts_the_first_choice() {
        let raw = r#"{"id":"x","choices":[{"index":0,"message":{"role":"assistant",
                     "content":"{\"category\":\"spam\"}"},"finish_reason":"stop"}]}"#;
        assert_eq!(OpenaiCompat.extract(raw).unwrap(), r#"{"category":"spam"}"#);
    }

    #[test]
    fn rejects_empty_and_malformed_payloads() {
        assert!(OpenaiCompat.extract(r#"{"choices":[]}"#).is_err());
        assert!(OpenaiCompat
            .extract(r#"{"choices":[{"message":{"content":""}}]}"#)
            .is_err());
        assert!(OpenaiCompat.extract("<html>502</html>").is_err());
    }
}
