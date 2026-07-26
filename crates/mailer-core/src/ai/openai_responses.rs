//! OpenAI Responses API: `POST {base}/responses` with a bearer token.

use serde::Deserialize;
use serde_json::{json, Value};

use super::{clamp_temperature, decode, require_text, ChatRequest, Wire};
use crate::error::Result;

/// Same ceiling as chat completions.
const MAX_TEMPERATURE: f32 = 2.0;

pub(super) struct OpenaiResponses;

impl Wire for OpenaiResponses {
    fn label(&self) -> &'static str {
        "OpenAI Responses 接口"
    }

    fn endpoint(&self, base: &str, _model: &str) -> String {
        format!("{}/responses", base.trim_end_matches('/'))
    }

    fn authorize(&self, req: reqwest::RequestBuilder, api_key: &str) -> reqwest::RequestBuilder {
        req.bearer_auth(api_key)
    }

    fn body(&self, req: &ChatRequest<'_>) -> Value {
        let mut body = json!({
            "model": req.model,
            "temperature": clamp_temperature(req.temperature, MAX_TEMPERATURE),
            "max_output_tokens": req.max_tokens,
            "input": [
                {
                    "role": "system",
                    "content": [{ "type": "input_text", "text": req.system }],
                },
                {
                    "role": "user",
                    "content": [{ "type": "input_text", "text": req.user }],
                },
            ],
        });
        if req.json_mode {
            body["text"] = json!({ "format": { "type": "json_object" } });
        }
        body
    }

    fn extract(&self, raw: &str) -> Result<String> {
        let parsed: ResponsesReply = decode(raw)?;

        // `output` is a list of turn items: reasoning summaries, tool calls and
        // — somewhere among them — the message we asked for.
        let mut text = String::new();
        if let Some(message) = parsed.output.iter().find(|item| item.kind == "message") {
            for part in &message.content {
                if part.kind == "output_text" {
                    text.push_str(&part.text);
                }
            }
        }

        // Convenience field of the official SDKs; some gateways return only it.
        if text.trim().is_empty() {
            text = match parsed.output_text {
                Some(FlatText::One(s)) => s,
                Some(FlatText::Many(parts)) => parts.concat(),
                None => text,
            };
        }
        require_text(text, raw)
    }
}

#[derive(Debug, Default, Deserialize)]
struct ResponsesReply {
    #[serde(default)]
    output: Vec<OutputItem>,
    #[serde(default)]
    output_text: Option<FlatText>,
}

#[derive(Debug, Default, Deserialize)]
struct OutputItem {
    #[serde(default, rename = "type")]
    kind: String,
    #[serde(default)]
    content: Vec<ContentPart>,
}

#[derive(Debug, Default, Deserialize)]
struct ContentPart {
    #[serde(default, rename = "type")]
    kind: String,
    #[serde(default)]
    text: String,
}

/// `output_text` is a string in the SDKs and a list in a few gateways.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum FlatText {
    One(String),
    Many(Vec<String>),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::sample_request;

    #[test]
    fn builds_a_responses_body() {
        let body = OpenaiResponses.body(&sample_request(false));
        assert_eq!(body["model"], "test-model");
        assert_eq!(body["max_output_tokens"], 400);
        assert_eq!(body["temperature"], 0.25);
        assert!(body.get("max_tokens").is_none(), "wrong token field name");
        assert_eq!(body["input"][0]["role"], "system");
        assert_eq!(body["input"][0]["content"][0]["type"], "input_text");
        assert_eq!(body["input"][0]["content"][0]["text"], "SYSTEM TEXT");
        assert_eq!(body["input"][1]["role"], "user");
        assert_eq!(body["input"][1]["content"][0]["text"], "USER TEXT");
        assert!(body.get("text").is_none());
    }

    #[test]
    fn json_mode_sets_the_text_format() {
        let body = OpenaiResponses.body(&sample_request(true));
        assert_eq!(body["text"]["format"]["type"], "json_object");
    }

    /// The message is not necessarily the first item, and its text may arrive
    /// split across several parts.
    #[test]
    fn extracts_output_text_after_a_reasoning_item() {
        let raw = r#"{"id":"resp_1","output":[
            {"id":"rs_1","type":"reasoning","summary":[]},
            {"id":"msg_1","type":"message","role":"assistant","content":[
                {"type":"output_text","text":"{\"category\":"},
                {"type":"output_text","text":"\"normal\"}"},
                {"type":"refusal","refusal":"ignored"}
            ]}
        ]}"#;
        assert_eq!(
            OpenaiResponses.extract(raw).unwrap(),
            r#"{"category":"normal"}"#
        );
    }

    #[test]
    fn accepts_the_flat_convenience_field() {
        let one = r#"{"output":[],"output_text":"pong"}"#;
        assert_eq!(OpenaiResponses.extract(one).unwrap(), "pong");
        let many = r#"{"output_text":["po","ng"]}"#;
        assert_eq!(OpenaiResponses.extract(many).unwrap(), "pong");
    }

    #[test]
    fn rejects_a_reply_without_any_message() {
        let raw = r#"{"output":[{"type":"reasoning","summary":[]}],"status":"incomplete"}"#;
        assert!(OpenaiResponses.extract(raw).is_err());
        assert!(OpenaiResponses.extract("not json").is_err());
    }
}
