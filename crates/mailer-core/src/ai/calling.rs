//! Native tool calling, per provider.
//!
//! The assistant used to ask the model to print a JSON action envelope and
//! parsed it out of the prose. Strong models cope; weaker ones — the local
//! 7B models this app is meant to run against — mangle the format often
//! enough that tools simply never fire, which is what a "dumb" assistant
//! looks like from the outside.
//!
//! Every provider here has a real tool protocol, so use it: the model emits a
//! structured call the server validated against our schema, and multi-turn
//! works because the transcript carries tool results as first-class turns
//! rather than as more prose to re-parse.

use serde_json::{json, Value};

use crate::error::{Error, Result};
use crate::types::AiProvider;

/// A tool as the model sees it. Mirrors `tools::ToolSpec`, but owned, because
/// MCP servers contribute tools discovered at run time.
#[derive(Debug, Clone)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    /// JSON Schema for the arguments object.
    pub parameters: Value,
}

/// One call the model asked for.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolInvocation {
    /// Provider-assigned id, echoed back with the result so the model can pair
    /// them. Gemini has no ids, so one is synthesised from the name.
    pub id: String,
    pub name: String,
    pub arguments: Value,
}

/// One entry in the transcript sent back to the model.
#[derive(Debug, Clone)]
pub enum Turn {
    User(String),
    /// What the model said last round, including any calls it made.
    Assistant {
        text: String,
        calls: Vec<ToolInvocation>,
    },
    /// The outcome of one call, paired by `id`.
    ToolResult {
        id: String,
        name: String,
        content: String,
    },
}

/// What one round produced.
#[derive(Debug, Clone, Default)]
pub struct Completion {
    pub text: String,
    /// Chain of thought, when the model emitted one.
    pub reasoning: Option<String>,
    pub calls: Vec<ToolInvocation>,
}

impl Completion {
    pub fn wants_tools(&self) -> bool {
        !self.calls.is_empty()
    }
}

// ---------------------------------------------------------------------------
// Request bodies
// ---------------------------------------------------------------------------

/// Render the whole request for `provider`.
pub fn build_body(
    provider: AiProvider,
    model: &str,
    system: &str,
    turns: &[Turn],
    tools: &[ToolDef],
    temperature: f32,
    max_tokens: u32,
) -> Value {
    match provider {
        AiProvider::OpenaiCompatible => {
            openai_body(model, system, turns, tools, temperature, max_tokens)
        }
        // Same vendor, different API: `/responses` takes `input`, not
        // `messages`, and names the reply budget `max_output_tokens`. Sending it
        // the chat-completions body is a 400 on the first word.
        AiProvider::OpenaiResponses => {
            responses_body(model, system, turns, tools, temperature, max_tokens)
        }
        AiProvider::Anthropic => anthropic_body(model, system, turns, tools, temperature, max_tokens),
        AiProvider::Gemini => gemini_body(system, turns, tools, temperature, max_tokens),
    }
}

fn openai_body(
    model: &str,
    system: &str,
    turns: &[Turn],
    tools: &[ToolDef],
    temperature: f32,
    max_tokens: u32,
) -> Value {
    let mut messages = vec![json!({ "role": "system", "content": system })];
    for t in turns {
        match t {
            Turn::User(text) => messages.push(json!({ "role": "user", "content": text })),
            Turn::Assistant { text, calls } => {
                let mut m = json!({ "role": "assistant" });
                // An assistant turn with calls may legitimately have no text.
                m["content"] = if text.is_empty() { Value::Null } else { json!(text) };
                if !calls.is_empty() {
                    m["tool_calls"] = Value::Array(
                        calls
                            .iter()
                            .map(|c| {
                                json!({
                                    "id": c.id,
                                    "type": "function",
                                    "function": {
                                        "name": c.name,
                                        // Arguments travel as a JSON *string*.
                                        "arguments": c.arguments.to_string(),
                                    }
                                })
                            })
                            .collect(),
                    );
                }
                messages.push(m);
            }
            Turn::ToolResult { id, name, content } => messages.push(json!({
                "role": "tool",
                "tool_call_id": id,
                "name": name,
                "content": content,
            })),
        }
    }

    let mut body = json!({
        "model": model,
        "temperature": temperature,
        "max_tokens": max_tokens,
        "messages": messages,
    });
    if !tools.is_empty() {
        body["tools"] = Value::Array(
            tools
                .iter()
                .map(|t| {
                    json!({
                        "type": "function",
                        "function": {
                            "name": t.name,
                            "description": t.description,
                            "parameters": t.parameters,
                        }
                    })
                })
                .collect(),
        );
        body["tool_choice"] = json!("auto");
    }
    body
}

/// OpenAI's Responses API.
///
/// Structurally unlike chat completions in three ways that all have to be right
/// at once: the transcript is `input` rather than `messages`, a tool call and
/// its result are top-level items paired by `call_id` rather than a field on an
/// assistant message and a `tool` role, and the function declaration is flat
/// rather than nested under `function`.
fn responses_body(
    model: &str,
    system: &str,
    turns: &[Turn],
    tools: &[ToolDef],
    temperature: f32,
    max_tokens: u32,
) -> Value {
    // The system text rides as the first input item, the same shape the
    // non-tool path in `ai::openai_responses` uses.
    let mut input = vec![json!({
        "role": "system",
        "content": [{ "type": "input_text", "text": system }],
    })];
    for t in turns {
        match t {
            Turn::User(text) => input.push(json!({
                "role": "user",
                "content": [{ "type": "input_text", "text": text }],
            })),
            Turn::Assistant { text, calls } => {
                // An assistant turn that only called a tool has no text to echo,
                // and an empty output_text block is rejected.
                if !text.is_empty() {
                    input.push(json!({
                        "role": "assistant",
                        "content": [{ "type": "output_text", "text": text }],
                    }));
                }
                for c in calls {
                    input.push(json!({
                        "type": "function_call",
                        "call_id": c.id,
                        "name": c.name,
                        // Arguments travel as a JSON *string*, as in chat completions.
                        "arguments": c.arguments.to_string(),
                    }));
                }
            }
            Turn::ToolResult { id, content, .. } => input.push(json!({
                "type": "function_call_output",
                "call_id": id,
                "output": content,
            })),
        }
    }

    let mut body = json!({
        "model": model,
        "temperature": temperature,
        "max_output_tokens": max_tokens,
        "input": input,
    });
    if !tools.is_empty() {
        body["tools"] = Value::Array(
            tools
                .iter()
                .map(|t| {
                    json!({
                        "type": "function",
                        "name": t.name,
                        "description": t.description,
                        "parameters": t.parameters,
                    })
                })
                .collect(),
        );
        body["tool_choice"] = json!("auto");
    }
    body
}

fn anthropic_body(
    model: &str,
    system: &str,
    turns: &[Turn],
    tools: &[ToolDef],
    temperature: f32,
    max_tokens: u32,
) -> Value {
    let mut messages: Vec<Value> = Vec::new();
    for t in turns {
        match t {
            Turn::User(text) => messages.push(json!({ "role": "user", "content": text })),
            Turn::Assistant { text, calls } => {
                let mut content: Vec<Value> = Vec::new();
                if !text.is_empty() {
                    content.push(json!({ "type": "text", "text": text }));
                }
                for c in calls {
                    content.push(json!({
                        "type": "tool_use",
                        "id": c.id,
                        "name": c.name,
                        "input": c.arguments,
                    }));
                }
                messages.push(json!({ "role": "assistant", "content": content }));
            }
            // Anthropic carries results as a *user* turn holding tool_result
            // blocks; there is no tool role.
            Turn::ToolResult { id, content, .. } => messages.push(json!({
                "role": "user",
                "content": [{
                    "type": "tool_result",
                    "tool_use_id": id,
                    "content": content,
                }],
            })),
        }
    }

    let mut body = json!({
        "model": model,
        "system": system,
        "max_tokens": max_tokens,
        "temperature": temperature.min(1.0),
        "messages": messages,
    });
    if !tools.is_empty() {
        body["tools"] = Value::Array(
            tools
                .iter()
                .map(|t| {
                    json!({
                        "name": t.name,
                        "description": t.description,
                        "input_schema": t.parameters,
                    })
                })
                .collect(),
        );
    }
    body
}

fn gemini_body(
    system: &str,
    turns: &[Turn],
    tools: &[ToolDef],
    temperature: f32,
    max_tokens: u32,
) -> Value {
    let mut contents: Vec<Value> = Vec::new();
    for t in turns {
        match t {
            Turn::User(text) => {
                contents.push(json!({ "role": "user", "parts": [{ "text": text }] }))
            }
            Turn::Assistant { text, calls } => {
                let mut parts: Vec<Value> = Vec::new();
                if !text.is_empty() {
                    parts.push(json!({ "text": text }));
                }
                for c in calls {
                    parts.push(json!({
                        "functionCall": { "name": c.name, "args": c.arguments }
                    }));
                }
                contents.push(json!({ "role": "model", "parts": parts }));
            }
            Turn::ToolResult { name, content, .. } => contents.push(json!({
                "role": "user",
                "parts": [{
                    "functionResponse": {
                        "name": name,
                        // Gemini wants an object, not a bare string.
                        "response": { "result": content }
                    }
                }],
            })),
        }
    }

    let mut body = json!({
        "systemInstruction": { "parts": [{ "text": system }] },
        "contents": contents,
        "generationConfig": {
            "temperature": temperature,
            "maxOutputTokens": max_tokens,
        },
    });
    if !tools.is_empty() {
        body["tools"] = json!([{
            "functionDeclarations": tools.iter().map(|t| json!({
                "name": t.name,
                "description": t.description,
                "parameters": t.parameters,
            })).collect::<Vec<_>>()
        }]);
    }
    body
}

// ---------------------------------------------------------------------------
// Response parsing
// ---------------------------------------------------------------------------

/// Pull text, reasoning and tool calls out of one 2xx response body.
pub fn parse_completion(provider: AiProvider, raw: &str) -> Result<Completion> {
    let v: Value = serde_json::from_str(raw)
        .map_err(|e| Error::Ai(format!("AI 响应不是合法 JSON ({e})")))?;
    let mut c = match provider {
        AiProvider::OpenaiCompatible => parse_openai(&v),
        AiProvider::OpenaiResponses => parse_responses(&v),
        AiProvider::Anthropic => parse_anthropic(&v),
        AiProvider::Gemini => parse_gemini(&v),
    };

    // Reasoning models put the chain of thought in the text; separate it so it
    // can be shown as thinking rather than read as the answer.
    let (reasoning, text) = super::split_reasoning(&c.text);
    c.reasoning = c.reasoning.or(reasoning);
    c.text = text;

    if c.text.is_empty() && c.calls.is_empty() && c.reasoning.is_none() {
        return Err(Error::Ai("AI 未返回任何内容".to_string()));
    }
    Ok(c)
}

fn parse_openai(v: &Value) -> Completion {
    let msg = &v["choices"][0]["message"];
    let calls = msg["tool_calls"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|c| {
                    let name = c["function"]["name"].as_str()?.to_string();
                    // Arguments arrive as a JSON string; a model that emits
                    // broken JSON there should not kill the whole reply, so an
                    // unparseable payload becomes an empty object and the tool
                    // reports the missing field itself.
                    let raw = c["function"]["arguments"].as_str().unwrap_or("{}");
                    Some(ToolInvocation {
                        id: c["id"].as_str().unwrap_or(&name).to_string(),
                        name,
                        arguments: serde_json::from_str(raw).unwrap_or_else(|_| json!({})),
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    Completion {
        // Some gateways expose a separate reasoning field rather than a tag.
        reasoning: msg["reasoning_content"]
            .as_str()
            .or_else(|| msg["reasoning"].as_str())
            .map(str::to_string)
            .filter(|s| !s.trim().is_empty()),
        text: msg["content"].as_str().unwrap_or_default().to_string(),
        calls,
    }
}

/// The Responses API answers with a flat `output` list: reasoning summaries,
/// the assistant message, and any function calls, in whatever order the model
/// produced them. There is no `choices` array to read.
fn parse_responses(v: &Value) -> Completion {
    let mut text = String::new();
    let mut reasoning = String::new();
    let mut calls = Vec::new();

    for item in v["output"].as_array().into_iter().flatten() {
        match item["type"].as_str() {
            Some("message") => {
                for part in item["content"].as_array().into_iter().flatten() {
                    // Refusals and other part types are not an answer.
                    if part["type"].as_str() == Some("output_text") {
                        text.push_str(part["text"].as_str().unwrap_or_default());
                    }
                }
            }
            Some("function_call") => calls.push(ToolInvocation {
                // `call_id` is what a `function_call_output` must echo back;
                // `id` identifies the item itself and will not pair.
                id: item["call_id"]
                    .as_str()
                    .or_else(|| item["id"].as_str())
                    .unwrap_or_default()
                    .to_string(),
                name: item["name"].as_str().unwrap_or_default().to_string(),
                arguments: item["arguments"]
                    .as_str()
                    .and_then(|raw| serde_json::from_str(raw).ok())
                    .unwrap_or_else(|| json!({})),
            }),
            Some("reasoning") => {
                for part in item["summary"].as_array().into_iter().flatten() {
                    reasoning.push_str(part["text"].as_str().unwrap_or_default());
                }
            }
            _ => {}
        }
    }

    // Some gateways answer with only the SDK convenience field.
    if text.is_empty() {
        if let Some(flat) = v["output_text"].as_str() {
            text.push_str(flat);
        }
    }

    Completion {
        text,
        reasoning: (!reasoning.trim().is_empty()).then_some(reasoning),
        calls,
    }
}

fn parse_anthropic(v: &Value) -> Completion {
    let mut text = String::new();
    let mut reasoning = String::new();
    let mut calls = Vec::new();

    for block in v["content"].as_array().into_iter().flatten() {
        match block["type"].as_str() {
            Some("text") => text.push_str(block["text"].as_str().unwrap_or_default()),
            Some("thinking") => {
                reasoning.push_str(block["thinking"].as_str().unwrap_or_default())
            }
            Some("tool_use") => calls.push(ToolInvocation {
                id: block["id"].as_str().unwrap_or_default().to_string(),
                name: block["name"].as_str().unwrap_or_default().to_string(),
                arguments: block["input"].clone(),
            }),
            _ => {}
        }
    }

    Completion {
        text,
        reasoning: (!reasoning.trim().is_empty()).then_some(reasoning),
        calls,
    }
}

fn parse_gemini(v: &Value) -> Completion {
    let mut text = String::new();
    let mut calls = Vec::new();

    for part in v["candidates"][0]["content"]["parts"].as_array().into_iter().flatten() {
        if let Some(t) = part["text"].as_str() {
            text.push_str(t);
        }
        if let Some(call) = part.get("functionCall") {
            let name = call["name"].as_str().unwrap_or_default().to_string();
            calls.push(ToolInvocation {
                // Gemini assigns no id; pairing is by name, so synthesise one
                // that stays stable within the round.
                id: format!("gemini-{name}-{}", calls.len()),
                name,
                arguments: call["args"].clone(),
            });
        }
    }

    Completion { text, reasoning: None, calls }
}

// ---------------------------------------------------------------------------
// Streaming
// ---------------------------------------------------------------------------

/// Ask for a streamed response, and give back the URL to send it to.
///
/// Three of the four providers take a `stream` flag in the body; Gemini changes
/// the method in the path instead, and needs `alt=sse` or it answers with a JSON
/// array rather than events.
pub fn make_streaming(provider: AiProvider, body: &mut Value, url: &str) -> String {
    match provider {
        AiProvider::Gemini => {
            let (path, query) = url.split_once('?').unwrap_or((url, ""));
            let path = path.replace(":generateContent", ":streamGenerateContent");
            let mut out = format!("{path}?alt=sse");
            if !query.is_empty() {
                out.push('&');
                out.push_str(query);
            }
            out
        }
        _ => {
            body["stream"] = json!(true);
            url.to_string()
        }
    }
}

/// One round, assembled from its stream.
///
/// Every provider fragments a response differently, and two of them fragment
/// tool arguments across events — so the accumulator holds partial JSON strings
/// and only parses them at the end. Text is handed out as it arrives, because
/// that is the entire point; everything else is reconstructed into the same
/// [`Completion`] the non-streaming path returns, so nothing downstream has to
/// know which path it came from.
pub struct StreamState {
    provider: AiProvider,
    text: String,
    reasoning: String,
    /// Tool calls under construction, in the order the provider introduced them.
    /// `arguments` is raw JSON text until [`StreamState::finish`].
    partial: Vec<PartialCall>,
    /// Anthropic and the Responses API address fragments by index rather than by
    /// repeating the id; this maps their index onto ours.
    by_index: std::collections::HashMap<i64, usize>,
    done: bool,
}

struct PartialCall {
    id: String,
    name: String,
    arguments: String,
}

impl StreamState {
    pub fn new(provider: AiProvider) -> StreamState {
        StreamState {
            provider,
            text: String::new(),
            reasoning: String::new(),
            partial: Vec::new(),
            by_index: std::collections::HashMap::new(),
            done: false,
        }
    }

    /// True once the provider has said the response is over. A stream that ends
    /// without this is a dropped connection, which the caller reports.
    pub fn finished(&self) -> bool {
        self.done
    }

    /// Feed one SSE `data:` payload. Returns the text to append to the answer,
    /// which is empty for everything that is not prose.
    pub fn feed(&mut self, payload: &str) -> String {
        let payload = payload.trim();
        if payload.is_empty() {
            return String::new();
        }
        // OpenAI ends with a literal sentinel rather than a JSON event.
        if payload == "[DONE]" {
            self.done = true;
            return String::new();
        }
        let Ok(v) = serde_json::from_str::<Value>(payload) else {
            // A partial or non-JSON line is not worth failing a whole answer
            // over; the stream will either recover or end without `done`.
            return String::new();
        };
        match self.provider {
            AiProvider::OpenaiCompatible => self.feed_openai(&v),
            AiProvider::OpenaiResponses => self.feed_responses(&v),
            AiProvider::Anthropic => self.feed_anthropic(&v),
            AiProvider::Gemini => self.feed_gemini(&v),
        }
    }

    /// `choices[0].delta` carries `content`, sometimes `reasoning_content`, and
    /// `tool_calls` whose `function.arguments` arrive in fragments.
    fn feed_openai(&mut self, v: &Value) -> String {
        let choice = &v["choices"][0];
        if choice["finish_reason"].is_string() {
            self.done = true;
        }
        let delta = &choice["delta"];

        for r in ["reasoning_content", "reasoning"] {
            if let Some(chunk) = delta[r].as_str() {
                self.reasoning.push_str(chunk);
            }
        }

        for call in delta["tool_calls"].as_array().into_iter().flatten() {
            let index = call["index"].as_i64().unwrap_or(0);
            let slot = self.slot_for(index);
            if let Some(id) = call["id"].as_str() {
                if !id.is_empty() {
                    self.partial[slot].id = id.to_string();
                }
            }
            if let Some(name) = call["function"]["name"].as_str() {
                self.partial[slot].name.push_str(name);
            }
            if let Some(args) = call["function"]["arguments"].as_str() {
                self.partial[slot].arguments.push_str(args);
            }
        }

        let chunk = delta["content"].as_str().unwrap_or_default();
        self.text.push_str(chunk);
        chunk.to_string()
    }

    /// The Responses API names every event. Only four of them carry payload we
    /// want; the rest describe structure we rebuild anyway.
    fn feed_responses(&mut self, v: &Value) -> String {
        match v["type"].as_str() {
            Some("response.output_text.delta") => {
                let chunk = v["delta"].as_str().unwrap_or_default();
                self.text.push_str(chunk);
                return chunk.to_string();
            }
            Some("response.reasoning_summary_text.delta") => {
                self.reasoning.push_str(v["delta"].as_str().unwrap_or_default());
            }
            Some("response.output_item.added") => {
                if v["item"]["type"].as_str() == Some("function_call") {
                    let index = v["output_index"].as_i64().unwrap_or(0);
                    let slot = self.slot_for(index);
                    self.partial[slot].id = v["item"]["call_id"]
                        .as_str()
                        .or_else(|| v["item"]["id"].as_str())
                        .unwrap_or_default()
                        .to_string();
                    self.partial[slot].name =
                        v["item"]["name"].as_str().unwrap_or_default().to_string();
                }
            }
            Some("response.function_call_arguments.delta") => {
                let index = v["output_index"].as_i64().unwrap_or(0);
                let slot = self.slot_for(index);
                self.partial[slot].arguments.push_str(v["delta"].as_str().unwrap_or_default());
            }
            Some("response.completed" | "response.incomplete" | "response.failed") => {
                self.done = true;
            }
            _ => {}
        }
        String::new()
    }

    /// Anthropic streams blocks: `content_block_start` says what kind, then
    /// `content_block_delta` fills it in — `text_delta`, `thinking_delta`, or
    /// `input_json_delta` for a tool's arguments.
    fn feed_anthropic(&mut self, v: &Value) -> String {
        match v["type"].as_str() {
            Some("content_block_start") => {
                if v["content_block"]["type"].as_str() == Some("tool_use") {
                    let index = v["index"].as_i64().unwrap_or(0);
                    let slot = self.slot_for(index);
                    self.partial[slot].id =
                        v["content_block"]["id"].as_str().unwrap_or_default().to_string();
                    self.partial[slot].name =
                        v["content_block"]["name"].as_str().unwrap_or_default().to_string();
                }
            }
            Some("content_block_delta") => {
                let delta = &v["delta"];
                match delta["type"].as_str() {
                    Some("text_delta") => {
                        let chunk = delta["text"].as_str().unwrap_or_default();
                        self.text.push_str(chunk);
                        return chunk.to_string();
                    }
                    Some("thinking_delta") => {
                        self.reasoning.push_str(delta["thinking"].as_str().unwrap_or_default());
                    }
                    Some("input_json_delta") => {
                        let index = v["index"].as_i64().unwrap_or(0);
                        let slot = self.slot_for(index);
                        self.partial[slot]
                            .arguments
                            .push_str(delta["partial_json"].as_str().unwrap_or_default());
                    }
                    _ => {}
                }
            }
            Some("message_stop") => self.done = true,
            _ => {}
        }
        String::new()
    }

    /// Gemini streams whole `candidates` objects, so each event is a small
    /// version of the non-streaming body and a function call never fragments.
    fn feed_gemini(&mut self, v: &Value) -> String {
        let mut out = String::new();
        for part in v["candidates"][0]["content"]["parts"].as_array().into_iter().flatten() {
            if let Some(t) = part["text"].as_str() {
                // Gemini marks a reasoning part rather than putting it elsewhere.
                if part["thought"].as_bool() == Some(true) {
                    self.reasoning.push_str(t);
                } else {
                    self.text.push_str(t);
                    out.push_str(t);
                }
            }
            if let Some(call) = part.get("functionCall") {
                let name = call["name"].as_str().unwrap_or_default().to_string();
                self.partial.push(PartialCall {
                    id: format!("gemini-{name}-{}", self.partial.len()),
                    name,
                    // Whole, not fragmented — stored as text for one exit path.
                    arguments: call["args"].to_string(),
                });
            }
        }
        if v["candidates"][0]["finishReason"].is_string() {
            self.done = true;
        }
        out
    }

    fn slot_for(&mut self, index: i64) -> usize {
        if let Some(&slot) = self.by_index.get(&index) {
            return slot;
        }
        let slot = self.partial.len();
        self.partial.push(PartialCall {
            id: String::new(),
            name: String::new(),
            arguments: String::new(),
        });
        self.by_index.insert(index, slot);
        slot
    }

    /// The finished round, in the same shape the non-streaming path produces.
    pub fn finish(self) -> Completion {
        let calls = self
            .partial
            .into_iter()
            .filter(|p| !p.name.is_empty())
            .map(|p| ToolInvocation {
                id: if p.id.is_empty() { p.name.clone() } else { p.id },
                // A model that streamed broken JSON must not lose the whole
                // answer; the tool reports its own missing arguments.
                arguments: serde_json::from_str(p.arguments.trim())
                    .unwrap_or_else(|_| json!({})),
                name: p.name,
            })
            .collect();

        // Same treatment as a whole response: a chain of thought inside the text
        // is thinking, not the answer.
        let (reasoning, text) = super::split_reasoning(&self.text);
        Completion {
            text,
            reasoning: reasoning
                .or_else(|| (!self.reasoning.trim().is_empty()).then_some(self.reasoning)),
            calls,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool() -> ToolDef {
        ToolDef {
            name: "search_mail".into(),
            description: "Search mail".into(),
            parameters: json!({"type":"object","properties":{"query":{"type":"string"}}}),
        }
    }

    #[test]
    fn openai_renders_tools_and_a_tool_result_turn() {
        let turns = vec![
            Turn::User("找账单".into()),
            Turn::Assistant {
                text: String::new(),
                calls: vec![ToolInvocation {
                    id: "call_1".into(),
                    name: "search_mail".into(),
                    arguments: json!({"query": "账单"}),
                }],
            },
            Turn::ToolResult {
                id: "call_1".into(),
                name: "search_mail".into(),
                content: "[]".into(),
            },
        ];
        let b = openai_body("gpt-4o-mini", "sys", &turns, &[tool()], 0.2, 900);
        assert_eq!(b["tools"][0]["function"]["name"], "search_mail");
        assert_eq!(b["tool_choice"], "auto");
        // Arguments must be a string, not an object — a common mistake that
        // 400s the request.
        assert!(b["messages"][2]["tool_calls"][0]["function"]["arguments"].is_string());
        assert_eq!(b["messages"][3]["role"], "tool");
        assert_eq!(b["messages"][3]["tool_call_id"], "call_1");
    }

    /// `/responses` is a different API, not a dialect of chat completions.
    /// Sending it `messages` + `max_tokens` is a 400 before the model is
    /// reached, which is what made the assistant unusable on this provider.
    #[test]
    fn responses_uses_input_items_and_pairs_calls_by_call_id() {
        let turns = vec![
            Turn::User("找账单".into()),
            Turn::Assistant {
                text: String::new(),
                calls: vec![ToolInvocation {
                    id: "call_1".into(),
                    name: "search_mail".into(),
                    arguments: json!({"query": "账单"}),
                }],
            },
            Turn::ToolResult {
                id: "call_1".into(),
                name: "search_mail".into(),
                content: "[]".into(),
            },
        ];
        let b = responses_body("gpt-4o-mini", "sys", &turns, &[tool()], 0.2, 900);

        assert_eq!(b["max_output_tokens"], 900);
        assert!(b.get("messages").is_none(), "wrong transcript field");
        assert!(b.get("max_tokens").is_none(), "wrong token field");

        let input = b["input"].as_array().unwrap();
        assert_eq!(input[0]["role"], "system");
        assert_eq!(input[0]["content"][0]["type"], "input_text");
        assert_eq!(input[1]["role"], "user");
        // A text-less assistant turn contributes only the call item.
        assert_eq!(input[2]["type"], "function_call");
        assert_eq!(input[2]["call_id"], "call_1");
        assert!(input[2]["arguments"].is_string(), "arguments must be a JSON string");
        assert_eq!(input[3]["type"], "function_call_output");
        assert_eq!(input[3]["call_id"], "call_1", "the pairing key must match");
        assert_eq!(input.len(), 4);

        // Function declarations are flat here, not nested under "function".
        assert_eq!(b["tools"][0]["type"], "function");
        assert_eq!(b["tools"][0]["name"], "search_mail");
        assert_eq!(b["tool_choice"], "auto");
    }

    #[test]
    fn responses_keeps_assistant_text_when_there_is_any() {
        let turns = vec![Turn::Assistant { text: "好的".into(), calls: Vec::new() }];
        let b = responses_body("m", "sys", &turns, &[], 0.2, 100);
        assert_eq!(b["input"][1]["role"], "assistant");
        assert_eq!(b["input"][1]["content"][0]["type"], "output_text");
        assert_eq!(b["input"][1]["content"][0]["text"], "好的");
        assert!(b.get("tools").is_none(), "no tools, no field");
    }

    /// The reply is a flat `output` list, not `choices`. Reading it as chat
    /// completions found nothing and reported "AI 未返回任何内容".
    #[test]
    fn responses_output_items_are_parsed() {
        let raw = r#"{"id":"resp_1","output":[
            {"id":"rs_1","type":"reasoning","summary":[{"type":"summary_text","text":"先搜索"}]},
            {"id":"msg_1","type":"message","role":"assistant","content":[
                {"type":"output_text","text":"查到了"},
                {"type":"refusal","refusal":"ignored"}
            ]},
            {"id":"fc_1","type":"function_call","call_id":"call_9","name":"search_mail",
             "arguments":"{\"query\":\"账单\"}"}
        ]}"#;
        let c = parse_completion(AiProvider::OpenaiResponses, raw).unwrap();
        assert_eq!(c.text, "查到了");
        assert_eq!(c.reasoning.unwrap(), "先搜索");
        assert_eq!(c.calls.len(), 1);
        // The id has to be the one a function_call_output echoes back.
        assert_eq!(c.calls[0].id, "call_9");
        assert_eq!(c.calls[0].name, "search_mail");
        assert_eq!(c.calls[0].arguments["query"], "账单");
    }

    #[test]
    fn responses_accepts_the_flat_convenience_field() {
        let c = parse_completion(AiProvider::OpenaiResponses, r#"{"output":[],"output_text":"pong"}"#)
            .unwrap();
        assert_eq!(c.text, "pong");
        assert!(!c.wants_tools());
    }

    #[test]
    fn responses_broken_arguments_degrade_to_an_empty_object() {
        let raw = r#"{"output":[{"type":"function_call","call_id":"c1","name":"search_mail",
                     "arguments":"{oops"}]}"#;
        let c = parse_completion(AiProvider::OpenaiResponses, raw).unwrap();
        assert_eq!(c.calls[0].arguments, json!({}));
    }

    #[test]
    fn responses_an_empty_reply_is_an_error() {
        assert!(parse_completion(AiProvider::OpenaiResponses, r#"{"output":[]}"#).is_err());
    }

    #[test]
    fn anthropic_carries_results_as_a_user_turn() {
        let turns = vec![Turn::ToolResult {
            id: "tu_1".into(),
            name: "search_mail".into(),
            content: "ok".into(),
        }];
        let b = anthropic_body("claude", "sys", &turns, &[tool()], 0.2, 900);
        // There is no tool role in this API; results ride inside a user turn.
        assert_eq!(b["messages"][0]["role"], "user");
        assert_eq!(b["messages"][0]["content"][0]["type"], "tool_result");
        assert_eq!(b["tools"][0]["input_schema"]["type"], "object");
        assert_eq!(b["system"], "sys");
    }

    /// Anthropic rejects temperature above 1.0 outright.
    #[test]
    fn anthropic_temperature_is_clamped() {
        let b = anthropic_body("claude", "s", &[], &[], 1.8, 100);
        assert_eq!(b["temperature"], 1.0);
    }

    #[test]
    fn gemini_wraps_a_result_in_an_object() {
        let turns = vec![Turn::ToolResult {
            id: "x".into(),
            name: "search_mail".into(),
            content: "found".into(),
        }];
        let b = gemini_body("sys", &turns, &[tool()], 0.2, 900);
        let resp = &b["contents"][0]["parts"][0]["functionResponse"]["response"];
        assert_eq!(resp["result"], "found");
        assert_eq!(b["tools"][0]["functionDeclarations"][0]["name"], "search_mail");
    }

    #[test]
    fn openai_tool_calls_are_parsed() {
        let raw = r#"{"choices":[{"message":{"content":null,"tool_calls":[
            {"id":"c1","type":"function","function":{"name":"search_mail","arguments":"{\"query\":\"账单\"}"}}]}}]}"#;
        let c = parse_completion(AiProvider::OpenaiCompatible, raw).unwrap();
        assert_eq!(c.calls.len(), 1);
        assert_eq!(c.calls[0].arguments["query"], "账单");
        assert!(c.wants_tools());
    }

    /// A model that writes broken JSON into `arguments` must not take the whole
    /// reply down with it.
    #[test]
    fn unparseable_arguments_degrade_to_an_empty_object() {
        let raw = r#"{"choices":[{"message":{"content":"","tool_calls":[
            {"id":"c1","function":{"name":"search_mail","arguments":"{oops"}}]}}]}"#;
        let c = parse_completion(AiProvider::OpenaiCompatible, raw).unwrap();
        assert_eq!(c.calls[0].arguments, json!({}));
    }

    #[test]
    fn anthropic_tool_use_and_thinking_are_parsed() {
        let raw = r#"{"content":[
            {"type":"thinking","thinking":"先搜索"},
            {"type":"text","text":"好的"},
            {"type":"tool_use","id":"tu_1","name":"search_mail","input":{"query":"a"}}]}"#;
        let c = parse_completion(AiProvider::Anthropic, raw).unwrap();
        assert_eq!(c.reasoning.unwrap(), "先搜索");
        assert_eq!(c.text, "好的");
        assert_eq!(c.calls[0].name, "search_mail");
    }

    #[test]
    fn gemini_function_calls_are_parsed() {
        let raw = r#"{"candidates":[{"content":{"parts":[
            {"text":"查一下"},{"functionCall":{"name":"search_mail","args":{"query":"b"}}}]}}]}"#;
        let c = parse_completion(AiProvider::Gemini, raw).unwrap();
        assert_eq!(c.text, "查一下");
        assert_eq!(c.calls[0].arguments["query"], "b");
    }

    /// A reasoning model's inline tag is separated even when tools are in play.
    #[test]
    fn inline_reasoning_is_split_out_of_a_tool_round() {
        let raw = r#"{"choices":[{"message":{"content":"<think>要先搜索</think>好的"}}]}"#;
        let c = parse_completion(AiProvider::OpenaiCompatible, raw).unwrap();
        assert_eq!(c.reasoning.unwrap(), "要先搜索");
        assert_eq!(c.text, "好的");
    }

    /// Some gateways surface reasoning as its own field instead of a tag.
    #[test]
    fn a_separate_reasoning_field_is_picked_up() {
        let raw = r#"{"choices":[{"message":{"content":"答案","reasoning_content":"推理过程"}}]}"#;
        let c = parse_completion(AiProvider::OpenaiCompatible, raw).unwrap();
        assert_eq!(c.reasoning.unwrap(), "推理过程");
        assert_eq!(c.text, "答案");
    }

    // -- streaming -----------------------------------------------------------

    /// Feed a state machine a list of payloads and report what the user saw plus
    /// what the round turned out to be.
    fn stream(provider: AiProvider, payloads: &[&str]) -> (String, Completion, bool) {
        let mut st = StreamState::new(provider);
        let mut seen = String::new();
        for p in payloads {
            seen.push_str(&st.feed(p));
        }
        let done = st.finished();
        (seen, st.finish(), done)
    }

    /// The common case, and the one every OpenAI-compatible server speaks: text
    /// in `delta.content`, ending with a literal sentinel rather than JSON.
    #[test]
    fn openai_text_arrives_in_fragments() {
        let (seen, c, done) = stream(
            AiProvider::OpenaiCompatible,
            &[
                r#"{"choices":[{"delta":{"role":"assistant","content":"你有"}}]}"#,
                r#"{"choices":[{"delta":{"content":"两封"}}]}"#,
                r#"{"choices":[{"delta":{"content":"账单。"},"finish_reason":"stop"}]}"#,
                "[DONE]",
            ],
        );
        assert_eq!(seen, "你有两封账单。", "the user sees every fragment, in order");
        assert_eq!(c.text, "你有两封账单。");
        assert!(c.calls.is_empty());
        assert!(done);
    }

    /// Tool arguments arrive as JSON split at arbitrary points — the reason the
    /// accumulator holds text and parses once at the end.
    #[test]
    fn openai_tool_arguments_are_reassembled_from_fragments() {
        let (seen, c, done) = stream(
            AiProvider::OpenaiCompatible,
            &[
                r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"search_mail","arguments":""}}]}}]}"#,
                r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"query\":\"账"}}]}}]}"#,
                r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"单\"}"}}]}}]}"#,
                r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
                "[DONE]",
            ],
        );
        assert!(seen.is_empty(), "a tool call is not prose");
        assert!(done);
        assert_eq!(c.calls.len(), 1);
        assert_eq!(c.calls[0].id, "call_1");
        assert_eq!(c.calls[0].name, "search_mail");
        assert_eq!(c.calls[0].arguments["query"], "账单");
    }

    /// Two tools in one round are addressed by index, and their fragments
    /// interleave.
    #[test]
    fn openai_keeps_two_interleaved_tool_calls_apart() {
        let (_, c, _) = stream(
            AiProvider::OpenaiCompatible,
            &[
                r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"a","function":{"name":"recent_mail","arguments":"{"}}]}}]}"#,
                r#"{"choices":[{"delta":{"tool_calls":[{"index":1,"id":"b","function":{"name":"list_accounts","arguments":"{"}}]}}]}"#,
                r#"{"choices":[{"delta":{"tool_calls":[{"index":1,"function":{"arguments":"}"}}]}}]}"#,
                r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"limit\":3}"}}]}}]}"#,
            ],
        );
        assert_eq!(c.calls.len(), 2);
        assert_eq!(c.calls[0].name, "recent_mail");
        assert_eq!(c.calls[0].arguments["limit"], 3);
        assert_eq!(c.calls[1].name, "list_accounts");
    }

    /// Anthropic streams typed blocks; the tool's arguments come as
    /// `input_json_delta` under the same index the block was opened with.
    #[test]
    fn anthropic_blocks_become_text_thinking_and_tools() {
        let (seen, c, done) = stream(
            AiProvider::Anthropic,
            &[
                r#"{"type":"message_start","message":{"id":"m"}}"#,
                r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking"}}"#,
                r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"先查一下"}}"#,
                r#"{"type":"content_block_start","index":1,"content_block":{"type":"text"}}"#,
                r#"{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"共 2 封"}}"#,
                r#"{"type":"content_block_start","index":2,"content_block":{"type":"tool_use","id":"toolu_1","name":"search_mail"}}"#,
                r#"{"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"{\"query\":"}}"#,
                r#"{"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"\"bill\"}"}}"#,
                r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"}}"#,
                r#"{"type":"message_stop"}"#,
            ],
        );
        assert_eq!(seen, "共 2 封", "only text_delta reaches the user");
        assert!(done);
        assert_eq!(c.reasoning.as_deref(), Some("先查一下"));
        assert_eq!(c.calls.len(), 1);
        assert_eq!(c.calls[0].id, "toolu_1");
        assert_eq!(c.calls[0].arguments["query"], "bill");
    }

    /// The Responses API names its events; only some of them carry payload.
    #[test]
    fn the_responses_api_events_are_read_by_name() {
        let (seen, c, done) = stream(
            AiProvider::OpenaiResponses,
            &[
                r#"{"type":"response.created"}"#,
                r#"{"type":"response.reasoning_summary_text.delta","delta":"想一下"}"#,
                r#"{"type":"response.output_text.delta","delta":"账单"}"#,
                r#"{"type":"response.output_text.delta","delta":"两封"}"#,
                r#"{"type":"response.output_item.added","output_index":1,"item":{"type":"function_call","call_id":"fc_1","name":"read_message"}}"#,
                r#"{"type":"response.function_call_arguments.delta","output_index":1,"delta":"{\"message_id\""}"#,
                r#"{"type":"response.function_call_arguments.delta","output_index":1,"delta":":\"m1\"}"}"#,
                r#"{"type":"response.completed"}"#,
            ],
        );
        assert_eq!(seen, "账单两封");
        assert!(done);
        assert_eq!(c.reasoning.as_deref(), Some("想一下"));
        assert_eq!(c.calls[0].id, "fc_1");
        assert_eq!(c.calls[0].arguments["message_id"], "m1");
    }

    /// Gemini streams a small version of the whole body per event, so a function
    /// call never fragments and thinking is a flag on a text part.
    #[test]
    fn gemini_streams_whole_parts() {
        let (seen, c, done) = stream(
            AiProvider::Gemini,
            &[
                r#"{"candidates":[{"content":{"parts":[{"text":"在想","thought":true}]}}]}"#,
                r#"{"candidates":[{"content":{"parts":[{"text":"有 2 封"}]}}]}"#,
                r#"{"candidates":[{"content":{"parts":[{"functionCall":{"name":"search_mail","args":{"query":"账单"}}}]},"finishReason":"STOP"}]}"#,
            ],
        );
        assert_eq!(seen, "有 2 封");
        assert!(done);
        assert_eq!(c.reasoning.as_deref(), Some("在想"));
        assert_eq!(c.calls[0].name, "search_mail");
        assert_eq!(c.calls[0].arguments["query"], "账单");
    }

    /// Streams carry keep-alives, unparseable fragments and events we do not
    /// use. None of them may take an answer down.
    #[test]
    fn noise_in_the_stream_is_survivable() {
        let (seen, c, done) = stream(
            AiProvider::OpenaiCompatible,
            &[
                "",
                ": ping",
                "{not json",
                r#"{"choices":[{"delta":{"content":"ok"}}]}"#,
            ],
        );
        assert_eq!(seen, "ok");
        assert_eq!(c.text, "ok");
        assert!(!done, "nothing said the response was over");
    }

    /// A model that streams a chain of thought inside the text gets the same
    /// treatment as one that answers all at once.
    #[test]
    fn reasoning_inside_streamed_text_is_still_separated() {
        let (seen, c, _) = stream(
            AiProvider::OpenaiCompatible,
            &[
                r#"{"choices":[{"delta":{"content":"<think>先查"}}]}"#,
                r#"{"choices":[{"delta":{"content":"一下</think>有 2 封"}}]}"#,
            ],
        );
        assert!(seen.contains("<think>"), "the raw fragments are what streamed");
        assert_eq!(c.text, "有 2 封", "but the stored answer is the answer");
        assert_eq!(c.reasoning.as_deref(), Some("先查一下"));
    }

    /// Only Gemini changes the URL; the others take a flag in the body.
    #[test]
    fn asking_for_a_stream_is_per_provider() {
        let mut body = json!({"model": "x"});
        let url = make_streaming(
            AiProvider::OpenaiCompatible,
            &mut body,
            "https://api.openai.com/v1/chat/completions",
        );
        assert_eq!(url, "https://api.openai.com/v1/chat/completions");
        assert_eq!(body["stream"], true);

        let mut body = json!({});
        let url = make_streaming(
            AiProvider::Gemini,
            &mut body,
            "https://x/v1beta/models/gemini-2.5-flash:generateContent?key=K",
        );
        assert_eq!(url, "https://x/v1beta/models/gemini-2.5-flash:streamGenerateContent?alt=sse&key=K");
        assert!(body.get("stream").is_none(), "Gemini has no such field");
    }
}
