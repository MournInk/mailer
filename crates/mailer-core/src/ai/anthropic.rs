//! Anthropic Messages API: `POST {base}/v1/messages`.

use serde::Deserialize;
use serde_json::{json, Value};

use super::{clamp_temperature, decode, require_text, ChatRequest, Wire};
use crate::error::Result;

/// The Messages API rejects anything above 1.0 outright.
const MAX_TEMPERATURE: f32 = 1.0;
/// Pinned wire version: an unset header is an error, and a newer one could
/// change the response shape this file parses.
const API_VERSION: &str = "2023-06-01";

pub(super) struct Anthropic;

impl Wire for Anthropic {
    fn label(&self) -> &'static str {
        "Anthropic 接口"
    }

    fn endpoint(&self, base: &str, _model: &str) -> String {
        let base = base.trim_end_matches('/');
        // Anthropic documents a bare host, but users carry over the OpenAI habit
        // of pasting a ".../v1" base; the doubled path 404s in a way that reads
        // like a bad key.
        let base = base.strip_suffix("/v1").unwrap_or(base);
        format!("{base}/v1/messages")
    }

    fn authorize(&self, req: reqwest::RequestBuilder, api_key: &str) -> reqwest::RequestBuilder {
        // `content-type: application/json` comes from `.json()` on the builder.
        req.header("x-api-key", api_key)
            .header("anthropic-version", API_VERSION)
    }

    fn body(&self, req: &ChatRequest<'_>) -> Value {
        json!({
            "model": req.model,
            "max_tokens": req.max_tokens,
            "temperature": clamp_temperature(req.temperature, MAX_TEMPERATURE),
            // Top-level, not a message: a system-role message is a 400 here.
            "system": req.system,
            "messages": [
                { "role": "user", "content": req.user },
            ],
        })
    }

    fn extract(&self, raw: &str) -> Result<String> {
        let parsed: MessageReply = decode(raw)?;
        // Thinking and tool_use blocks share the list; only text is an answer.
        let text = parsed
            .content
            .iter()
            .filter(|b| b.kind == "text")
            .map(|b| b.text.as_str())
            .collect::<String>();
        require_text(text, raw)
    }

    fn native_json_mode(&self) -> bool {
        // No response_format switch: the prompt asks, and the balanced-brace
        // extraction cleans up whatever prose slips through.
        false
    }
}

#[derive(Debug, Default, Deserialize)]
struct MessageReply {
    #[serde(default)]
    content: Vec<ContentBlock>,
}

#[derive(Debug, Default, Deserialize)]
struct ContentBlock {
    #[serde(default, rename = "type")]
    kind: String,
    #[serde(default)]
    text: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::sample_request;

    #[test]
    fn builds_a_messages_body_with_a_top_level_system() {
        let body = Anthropic.body(&sample_request(true));
        assert_eq!(body["model"], "test-model");
        assert_eq!(body["max_tokens"], 400);
        assert_eq!(body["temperature"], 0.25);
        assert_eq!(body["system"], "SYSTEM TEXT");
        assert_eq!(body["messages"].as_array().unwrap().len(), 1);
        assert_eq!(body["messages"][0]["role"], "user");
        assert_eq!(body["messages"][0]["content"], "USER TEXT");
        // There is no JSON mode to switch on.
        assert!(body.get("response_format").is_none());
    }

    /// Temperature above 1.0 is a hard 400 here, so it is clamped, not passed.
    #[test]
    fn temperature_is_clamped_to_one() {
        let mut req = sample_request(false);
        req.temperature = 1.8;
        assert_eq!(Anthropic.body(&req)["temperature"], 1.0);
    }

    #[test]
    fn endpoint_does_not_double_a_pasted_v1() {
        assert_eq!(
            Anthropic.endpoint("https://api.anthropic.com", "m"),
            "https://api.anthropic.com/v1/messages"
        );
        assert_eq!(
            Anthropic.endpoint("https://api.anthropic.com/v1/", "m"),
            "https://api.anthropic.com/v1/messages"
        );
    }

    #[test]
    fn extracts_text_blocks_only() {
        let raw = r#"{"id":"msg_1","type":"message","role":"assistant","content":[
            {"type":"thinking","thinking":"hidden"},
            {"type":"text","text":"{\"category\":"},
            {"type":"text","text":"\"important\"}"}
        ],"stop_reason":"end_turn"}"#;
        assert_eq!(Anthropic.extract(raw).unwrap(), r#"{"category":"important"}"#);
    }

    #[test]
    fn rejects_a_reply_without_text() {
        assert!(Anthropic.extract(r#"{"content":[]}"#).is_err());
        assert!(Anthropic
            .extract(r#"{"content":[{"type":"tool_use","name":"x"}]}"#)
            .is_err());
        assert!(Anthropic.extract("upstream timeout").is_err());
    }
}
