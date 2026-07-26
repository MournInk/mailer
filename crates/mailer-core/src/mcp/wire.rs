//! JSON-RPC framing and the pure parts of the MCP wire format.
//!
//! Everything here is pure string and `Value` work so the protocol can be
//! exercised without a server, a subprocess or a network.

use serde_json::{json, Value};

use crate::error::{Error, Result};

/// Protocol revision this client implements.
///
/// The server may answer `initialize` with a different one; whatever comes back
/// in the result is what travels in the `MCP-Protocol-Version` header from then
/// on, because a server is allowed to negotiate down and some do not error on a
/// version they do not know.
pub const PROTOCOL_VERSION: &str = "2025-11-25";

/// What a server told us about itself.
#[derive(Debug, Clone, PartialEq)]
pub struct ServerInfo {
    pub name: String,
    pub version: String,
    /// The revision actually negotiated, read from the result rather than
    /// assumed from the request.
    pub protocol_version: String,
    /// True when the server advertised a `tools` capability. Without it there is
    /// nothing here for us: this client uses tools and nothing else.
    pub has_tools: bool,
    /// Free-text guidance some servers supply for the model.
    pub instructions: Option<String>,
}

/// One tool as a server advertises it.
#[derive(Debug, Clone, PartialEq)]
pub struct RemoteTool {
    pub name: String,
    pub description: String,
    /// JSON Schema for the arguments, verbatim. Not validated locally: servers
    /// legitimately publish draft-07 and 2020-12, and the model is the consumer.
    pub input_schema: Value,
}

/// The outcome of one `tools/call`.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolOutcome {
    /// Every text-ish block, joined. Images and audio are described, not carried.
    pub text: String,
    /// The server ran the tool and it failed. Distinct from a JSON-RPC error:
    /// this one is worth handing back to the model, which can correct itself.
    pub is_error: bool,
}

// ---------------------------------------------------------------------------
// Requests
// ---------------------------------------------------------------------------

pub fn initialize_request(id: u64, client_name: &str, client_version: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "initialize",
        "params": {
            "protocolVersion": PROTOCOL_VERSION,
            // Deliberately empty: advertising `sampling` or `elicitation` would
            // promise the server it may call back into us, and we do not answer.
            "capabilities": {},
            "clientInfo": { "name": client_name, "version": client_version },
        },
    })
}

/// The notification a client must send once `initialize` has been answered.
pub fn initialized_notification() -> Value {
    json!({ "jsonrpc": "2.0", "method": "notifications/initialized" })
}

pub fn tools_list_request(id: u64, cursor: Option<&str>) -> Value {
    let mut req = json!({ "jsonrpc": "2.0", "id": id, "method": "tools/list" });
    if let Some(cursor) = cursor {
        req["params"] = json!({ "cursor": cursor });
    }
    req
}

pub fn tools_call_request(id: u64, name: &str, arguments: &Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "tools/call",
        "params": { "name": name, "arguments": arguments },
    })
}

// ---------------------------------------------------------------------------
// Responses
// ---------------------------------------------------------------------------

/// Pull the `result` out of a response frame, turning a JSON-RPC `error` into
/// our own error type.
pub fn result_of(frame: &Value, what: &str) -> Result<Value> {
    if let Some(err) = frame.get("error") {
        let message = err
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("服务器未说明原因");
        let code = err.get("code").and_then(Value::as_i64).unwrap_or(0);
        return Err(Error::Other(format!("MCP {what} 失败 ({code}): {message}")));
    }
    frame
        .get("result")
        .cloned()
        .ok_or_else(|| Error::Other(format!("MCP {what} 的响应既没有 result 也没有 error")))
}

pub fn parse_initialize(result: &Value) -> ServerInfo {
    let info = &result["serverInfo"];
    ServerInfo {
        name: info["name"].as_str().unwrap_or("unknown").to_string(),
        version: info["version"].as_str().unwrap_or_default().to_string(),
        protocol_version: result["protocolVersion"]
            .as_str()
            .unwrap_or(PROTOCOL_VERSION)
            .to_string(),
        has_tools: result["capabilities"].get("tools").is_some(),
        instructions: result["instructions"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string),
    }
}

/// Tools from one page, plus the cursor for the next one if there is more.
pub fn parse_tools_page(result: &Value) -> (Vec<RemoteTool>, Option<String>) {
    let tools = result["tools"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|t| {
                    let name = t["name"].as_str()?.trim();
                    if name.is_empty() {
                        return None;
                    }
                    Some(RemoteTool {
                        name: name.to_string(),
                        // `title` is the human label; the description is what the
                        // model reads, so fall back to the title and then the name
                        // rather than offering a tool with no explanation.
                        description: t["description"]
                            .as_str()
                            .or_else(|| t["title"].as_str())
                            .unwrap_or(name)
                            .trim()
                            .to_string(),
                        input_schema: match &t["inputSchema"] {
                            Value::Object(_) => t["inputSchema"].clone(),
                            // Must never be null per the spec, but a client that
                            // trusts that crashes on the server that ships it.
                            _ => json!({ "type": "object", "properties": {} }),
                        },
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    let cursor = result["nextCursor"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    (tools, cursor)
}

/// Flatten a `tools/call` result into text the model can read.
pub fn parse_tool_outcome(result: &Value) -> ToolOutcome {
    let mut text = String::new();
    for block in result["content"].as_array().into_iter().flatten() {
        match block["type"].as_str() {
            Some("text") => push_line(&mut text, block["text"].as_str().unwrap_or_default()),
            // A link is useful; the bytes of an image are not, to a text model.
            Some("resource_link") => push_line(
                &mut text,
                &format!(
                    "[{}] {}",
                    block["name"].as_str().unwrap_or("resource"),
                    block["uri"].as_str().unwrap_or_default()
                ),
            ),
            Some("resource") => {
                let r = &block["resource"];
                if let Some(t) = r["text"].as_str() {
                    push_line(&mut text, t);
                } else {
                    push_line(
                        &mut text,
                        &format!("[资源 {}]", r["uri"].as_str().unwrap_or_default()),
                    );
                }
            }
            Some(kind @ ("image" | "audio")) => {
                push_line(&mut text, &format!("[{kind} 内容，已省略]"))
            }
            _ => {}
        }
    }

    // Servers are asked to duplicate structured output as text but not all do;
    // without this a structured-only answer arrives empty.
    if text.trim().is_empty() {
        if let Some(structured) = result.get("structuredContent").filter(|v| !v.is_null()) {
            text = serde_json::to_string(structured).unwrap_or_default();
        }
    }

    ToolOutcome {
        text,
        is_error: result["isError"].as_bool().unwrap_or(false),
    }
}

fn push_line(out: &mut String, line: &str) {
    if line.is_empty() {
        return;
    }
    if !out.is_empty() {
        out.push('\n');
    }
    out.push_str(line);
}

/// True for a frame that is a notification or an unsolicited server request —
/// something with no `id` we asked about. Never answered, always skipped.
pub fn is_ignorable(frame: &Value, waiting_for: u64) -> bool {
    match frame.get("id") {
        None | Some(Value::Null) => true,
        Some(id) => id.as_u64() != Some(waiting_for),
    }
}

// ---------------------------------------------------------------------------
// Server-sent events
// ---------------------------------------------------------------------------

/// Extract the JSON payloads from an SSE body, in order.
///
/// Streamable HTTP lets a server answer a POST with either one JSON object or an
/// SSE stream, and the client has to handle both — Exa, for one, answers every
/// call as a stream. Only `data:` matters: `event:`, `id:`, `retry:` and
/// `:`-comment keep-alives are all noise to a client that just wants its result.
/// Multiple `data:` lines in one frame concatenate with newlines, per the SSE
/// specification.
pub fn sse_payloads(body: &str) -> Vec<Value> {
    let mut out = Vec::new();
    let mut data = String::new();

    let flush = |data: &mut String, out: &mut Vec<Value>| {
        if data.is_empty() {
            return;
        }
        if let Ok(v) = serde_json::from_str::<Value>(data.trim()) {
            out.push(v);
        }
        data.clear();
    };

    for raw in body.lines() {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        if line.is_empty() {
            flush(&mut data, &mut out);
            continue;
        }
        if line.starts_with(':') {
            continue; // keep-alive comment
        }
        let Some(rest) = line.strip_prefix("data:") else {
            continue; // event:, id:, retry:, or a field we do not need
        };
        if !data.is_empty() {
            data.push('\n');
        }
        data.push_str(rest.strip_prefix(' ').unwrap_or(rest));
    }
    flush(&mut data, &mut out);
    out
}

/// The frame answering `id` out of a response body that may be JSON or SSE.
pub fn frame_for(body: &str, id: u64) -> Result<Value> {
    let trimmed = body.trim_start();
    if trimmed.starts_with('{') {
        if let Ok(v) = serde_json::from_str::<Value>(trimmed) {
            return Ok(v);
        }
    }
    sse_payloads(body)
        .into_iter()
        .find(|f| !is_ignorable(f, id))
        .ok_or_else(|| {
            Error::Other(format!(
                "MCP 响应中没有 id={id} 的结果: {}",
                snippet(body)
            ))
        })
}

fn snippet(s: &str) -> String {
    let flat = s.split_whitespace().collect::<Vec<_>>().join(" ");
    match flat.char_indices().nth(200) {
        Some((i, _)) => format!("{}…", &flat[..i]),
        None => flat,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initialize_advertises_no_callbacks_it_cannot_answer() {
        let req = initialize_request(1, "mailer", "0.1.0");
        assert_eq!(req["method"], "initialize");
        assert_eq!(req["params"]["protocolVersion"], PROTOCOL_VERSION);
        assert_eq!(req["params"]["capabilities"], json!({}));
        assert_eq!(req["params"]["clientInfo"]["name"], "mailer");
        assert!(initialized_notification().get("id").is_none(), "a notification has no id");
    }

    /// The negotiated revision comes from the result. A server may answer with a
    /// different one, and at least one answers a nonsense request with its own
    /// version instead of an error.
    #[test]
    fn the_negotiated_version_is_read_from_the_result() {
        let result = json!({
            "protocolVersion": "2025-03-26",
            "capabilities": { "tools": { "listChanged": true } },
            "serverInfo": { "name": "exa-search-server", "version": "3.2.1" },
            "instructions": "  be nice  ",
        });
        let info = parse_initialize(&result);
        assert_eq!(info.protocol_version, "2025-03-26");
        assert_eq!(info.name, "exa-search-server");
        assert!(info.has_tools);
        assert_eq!(info.instructions.unwrap(), "be nice");
    }

    /// A server with no tools capability has nothing this client can use.
    #[test]
    fn a_server_without_tools_is_recognised() {
        let info = parse_initialize(&json!({
            "capabilities": { "resources": {} },
            "serverInfo": { "name": "docs", "version": "1" },
        }));
        assert!(!info.has_tools);
        assert_eq!(info.protocol_version, PROTOCOL_VERSION, "defaults to what we asked for");
    }

    #[test]
    fn tools_pages_are_parsed_and_chained() {
        let page = json!({
            "tools": [
                {
                    "name": "web_search_exa",
                    "description": "Search the web",
                    "inputSchema": {
                        "$schema": "http://json-schema.org/draft-07/schema#",
                        "type": "object",
                        "properties": { "query": { "type": "string" } },
                        "required": ["query"],
                    },
                },
                { "name": "titled", "title": "Only a title", "inputSchema": null },
                { "name": "   " },
            ],
            "nextCursor": "page2",
        });
        let (tools, cursor) = parse_tools_page(&page);
        assert_eq!(cursor.as_deref(), Some("page2"));
        assert_eq!(tools.len(), 2, "the nameless tool is dropped: {tools:?}");
        assert_eq!(tools[0].name, "web_search_exa");
        // draft-07 schemas are passed through untouched.
        assert_eq!(tools[0].input_schema["$schema"], "http://json-schema.org/draft-07/schema#");
        // A tool with no description falls back to its title, never to nothing.
        assert_eq!(tools[1].description, "Only a title");
        // A null schema must not crash a client that trusted the spec.
        assert_eq!(tools[1].input_schema["type"], "object");

        let (_, none) = parse_tools_page(&json!({ "tools": [] }));
        assert!(none.is_none());
    }

    #[test]
    fn tool_outcomes_flatten_every_block_kind() {
        let outcome = parse_tool_outcome(&json!({
            "content": [
                { "type": "text", "text": "first" },
                { "type": "image", "data": "…", "mimeType": "image/png" },
                { "type": "resource_link", "name": "readme", "uri": "https://x/readme" },
                { "type": "resource", "resource": { "uri": "file://a", "text": "inline" } },
                { "type": "text", "text": "last" },
            ],
            "isError": false,
        }));
        assert!(!outcome.is_error);
        assert_eq!(outcome.text, "first\n[image 内容，已省略]\n[readme] https://x/readme\ninline\nlast");
    }

    /// A tool that ran and failed is not a transport failure: the text explains
    /// why and the model gets to try something else.
    #[test]
    fn a_failed_tool_is_an_outcome_not_an_error() {
        let outcome = parse_tool_outcome(&json!({
            "content": [{ "type": "text", "text": "MCP error -32602: Tool no_such_tool not found" }],
            "isError": true,
        }));
        assert!(outcome.is_error);
        assert!(outcome.text.contains("not found"));
    }

    /// Some servers answer only with structured output. Reading just `content`
    /// would hand the model an empty result.
    #[test]
    fn structured_only_results_are_not_lost() {
        let outcome = parse_tool_outcome(&json!({
            "content": [],
            "structuredContent": { "temperature": 22 },
        }));
        assert_eq!(outcome.text, r#"{"temperature":22}"#);
    }

    #[test]
    fn a_jsonrpc_error_names_the_code_and_message() {
        let err = result_of(
            &json!({ "jsonrpc": "2.0", "id": 1, "error": { "code": -32602, "message": "Unsupported protocol version" } }),
            "初始化",
        )
        .unwrap_err();
        let text = err.to_string();
        assert!(text.contains("-32602"), "{text}");
        assert!(text.contains("Unsupported protocol version"), "{text}");
        assert!(result_of(&json!({ "id": 1 }), "初始化").is_err(), "neither result nor error");
    }

    // -- SSE ---------------------------------------------------------------

    /// The shape a real server answers with — verified against Exa, which
    /// answers every POST as a stream and never as plain JSON.
    #[test]
    fn sse_frames_are_extracted() {
        let body = "event: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"ok\":true}}\n\n";
        let frames = sse_payloads(body);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0]["result"]["ok"], true);
    }

    #[test]
    fn sse_ignores_comments_and_joins_split_data() {
        let body = concat!(
            ": keep-alive\n",
            "id: 7\n",
            "retry: 3000\n",
            "event: message\n",
            "data: {\"jsonrpc\":\"2.0\",\n",
            "data: \"id\":2,\"result\":{\"n\":1}}\n",
            "\n",
            "event: message\n",
            "data: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\"}\n",
            "\n"
        );
        let frames = sse_payloads(body);
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0]["result"]["n"], 1);
        assert_eq!(frames[1]["method"], "notifications/progress");
    }

    #[test]
    fn a_body_is_read_whether_it_is_json_or_a_stream() {
        let plain = r#"{"jsonrpc":"2.0","id":5,"result":{"from":"json"}}"#;
        assert_eq!(frame_for(plain, 5).unwrap()["result"]["from"], "json");

        let stream = "data: {\"jsonrpc\":\"2.0\",\"id\":5,\"result\":{\"from\":\"sse\"}}\n\n";
        assert_eq!(frame_for(stream, 5).unwrap()["result"]["from"], "sse");
    }

    /// Notifications and answers to other requests must not be mistaken for the
    /// result being waited on.
    #[test]
    fn interleaved_notifications_are_skipped() {
        let body = concat!(
            "data: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/message\",\"params\":{}}\n\n",
            "data: {\"jsonrpc\":\"2.0\",\"id\":99,\"result\":{\"other\":true}}\n\n",
            "data: {\"jsonrpc\":\"2.0\",\"id\":4,\"result\":{\"mine\":true}}\n\n"
        );
        assert_eq!(frame_for(body, 4).unwrap()["result"]["mine"], true);
        assert!(is_ignorable(&json!({ "method": "notifications/progress" }), 4));
        assert!(is_ignorable(&json!({ "id": 99 }), 4));
        assert!(!is_ignorable(&json!({ "id": 4 }), 4));
    }

    #[test]
    fn a_body_with_no_answer_is_an_error() {
        let err = frame_for("data: {\"method\":\"notifications/x\"}\n\n", 1).unwrap_err();
        assert!(err.to_string().contains("id=1"), "{err}");
        assert!(frame_for("", 1).is_err());
        assert!(frame_for("<html>502</html>", 1).is_err());
    }
}
