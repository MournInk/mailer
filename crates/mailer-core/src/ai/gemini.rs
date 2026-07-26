//! Google Gemini: `POST {base}/models/{model}:generateContent`.

use serde::Deserialize;
use serde_json::{json, Value};

use super::{clamp_temperature, decode, require_text, ChatRequest, Wire};
use crate::error::Result;

/// Gemini accepts 0.0-2.0.
const MAX_TEMPERATURE: f32 = 2.0;

pub(super) struct Gemini;

impl Wire for Gemini {
    fn label(&self) -> &'static str {
        "Gemini 接口"
    }

    fn endpoint(&self, base: &str, model: &str) -> String {
        let base = base.trim_end_matches('/');
        let model = model.trim().trim_matches('/');
        // The model is a path segment, so a name that already carries its
        // collection ("models/gemini-2.0-flash", "tunedModels/xyz") is a full
        // resource name; only a bare name needs the "models/" prefix. Doubling
        // it yields a 404 that reads like a wrong model name.
        if model.contains('/') {
            format!("{base}/{model}:generateContent")
        } else {
            format!("{base}/models/{model}:generateContent")
        }
    }

    fn authorize(&self, req: reqwest::RequestBuilder, api_key: &str) -> reqwest::RequestBuilder {
        // Header rather than the `?key=` query parameter: URLs end up in logs.
        req.header("x-goog-api-key", api_key)
    }

    fn body(&self, req: &ChatRequest<'_>) -> Value {
        let mut generation_config = json!({
            "temperature": clamp_temperature(req.temperature, MAX_TEMPERATURE),
            "maxOutputTokens": req.max_tokens,
        });
        if req.json_mode {
            generation_config["responseMimeType"] = json!("application/json");
        }
        json!({
            "systemInstruction": { "parts": [{ "text": req.system }] },
            "contents": [
                { "role": "user", "parts": [{ "text": req.user }] },
            ],
            "generationConfig": generation_config,
        })
    }

    fn extract(&self, raw: &str) -> Result<String> {
        let parsed: GenerateReply = decode(raw)?;
        let text = parsed
            .candidates
            .into_iter()
            .next()
            .map(|c| {
                c.content
                    .parts
                    .iter()
                    .map(|p| p.text.as_str())
                    .collect::<String>()
            })
            .unwrap_or_default();
        require_text(text, raw)
    }
}

#[derive(Debug, Default, Deserialize)]
struct GenerateReply {
    #[serde(default)]
    candidates: Vec<Candidate>,
}

#[derive(Debug, Default, Deserialize)]
struct Candidate {
    #[serde(default)]
    content: Content,
}

#[derive(Debug, Default, Deserialize)]
struct Content {
    #[serde(default)]
    parts: Vec<Part>,
}

#[derive(Debug, Default, Deserialize)]
struct Part {
    // Function-call parts carry no text; defaulting keeps them harmless.
    #[serde(default)]
    text: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::sample_request;

    #[test]
    fn builds_a_generate_content_body() {
        let body = Gemini.body(&sample_request(false));
        assert_eq!(body["systemInstruction"]["parts"][0]["text"], "SYSTEM TEXT");
        assert_eq!(body["contents"][0]["role"], "user");
        assert_eq!(body["contents"][0]["parts"][0]["text"], "USER TEXT");
        assert_eq!(body["generationConfig"]["maxOutputTokens"], 400);
        assert_eq!(body["generationConfig"]["temperature"], 0.25);
        assert!(body["generationConfig"].get("responseMimeType").is_none());
        // The model travels in the path, never in the body.
        assert!(body.get("model").is_none());
    }

    #[test]
    fn json_mode_sets_the_response_mime_type() {
        let body = Gemini.body(&sample_request(true));
        assert_eq!(
            body["generationConfig"]["responseMimeType"],
            "application/json"
        );
    }

    /// The model name is a path segment; a fully qualified name must not get a
    /// second "models/" in front of it.
    #[test]
    fn endpoint_handles_qualified_model_names() {
        let base = "https://generativelanguage.googleapis.com/v1beta";
        assert_eq!(
            Gemini.endpoint(base, "gemini-2.0-flash"),
            format!("{base}/models/gemini-2.0-flash:generateContent")
        );
        assert_eq!(
            Gemini.endpoint(&format!("{base}/"), " models/gemini-2.0-flash "),
            format!("{base}/models/gemini-2.0-flash:generateContent")
        );
        assert_eq!(
            Gemini.endpoint(base, "tunedModels/my-tune"),
            format!("{base}/tunedModels/my-tune:generateContent")
        );
    }

    #[test]
    fn extracts_the_first_candidates_parts() {
        let raw = r#"{"candidates":[{"content":{"role":"model","parts":[
            {"text":"{\"category\":"},{"text":"\"spam\"}"}
        ]},"finishReason":"STOP"}]}"#;
        assert_eq!(Gemini.extract(raw).unwrap(), r#"{"category":"spam"}"#);
    }

    /// A safety block returns a candidate with no parts at all.
    #[test]
    fn rejects_a_blocked_or_empty_reply() {
        let blocked = r#"{"candidates":[{"finishReason":"SAFETY"}],"promptFeedback":{"blockReason":"SAFETY"}}"#;
        assert!(Gemini.extract(blocked).is_err());
        assert!(Gemini.extract(r#"{"candidates":[]}"#).is_err());
        assert!(Gemini.extract("<html>429</html>").is_err());
    }
}
