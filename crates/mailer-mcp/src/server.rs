//! Method dispatch: one input line in, at most one output line out.
//!
//! The mailbox reaches this module through [`ToolHost`] rather than directly,
//! so the whole protocol surface can be exercised without a database, a
//! network, or an LLM key.

use std::future::Future;

use serde_json::{json, Value};

use crate::protocol::{self, BadRequest};

/// MCP revision this server implements.
pub const PROTOCOL_VERSION: &str = "2024-11-05";
pub const SERVER_NAME: &str = "mailer";
pub const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

/// One tool as MCP advertises it.
#[derive(Debug, Clone)]
pub struct ToolDescriptor {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

/// What dispatch needs from the mailbox.
pub trait ToolHost {
    fn list(&self) -> Vec<ToolDescriptor>;

    /// Run one call. The `Err` string is shown to a human and read by a model,
    /// so it carries a reason and never a payload or a credential.
    fn call(
        &self,
        name: &str,
        args: Value,
    ) -> impl Future<Output = Result<Value, String>>;
}

/// The outcome of handling one input line.
#[derive(Debug, Clone, PartialEq)]
pub struct Handled {
    /// Line to write to stdout, or `None` when the input was a notification.
    pub frame: Option<String>,
    /// Set by `shutdown`: stop reading once this frame is flushed.
    pub stop: bool,
}

impl Handled {
    fn reply(frame: String) -> Handled {
        Handled { frame: Some(frame), stop: false }
    }

    fn silent() -> Handled {
        Handled { frame: None, stop: false }
    }
}

/// Parse, dispatch and frame one line of input.
pub async fn handle<H: ToolHost>(host: &H, line: &str) -> Handled {
    let req = match protocol::parse(line) {
        Ok(req) => req,
        Err(BadRequest { id, code, message }) => {
            tracing::warn!(code, %message, "拒绝一条请求");
            return Handled::reply(protocol::failure(&id, code, &message));
        }
    };

    // A notification carries no id and gets no answer at all — answering one is
    // itself a protocol violation, and clients drop the session over it. This
    // covers `notifications/initialized` and every other `notifications/*`
    // frame a client may decide to send.
    let Some(id) = req.id.clone() else {
        tracing::debug!(method = %req.method, "notification");
        return Handled::silent();
    };

    tracing::debug!(method = %req.method, "request");
    match req.method.as_str() {
        "initialize" => Handled::reply(protocol::success(&id, initialize_result())),
        "tools/list" => Handled::reply(protocol::success(&id, tools_list(host))),
        "tools/call" => match tools_call(host, req.params).await {
            Ok(result) => Handled::reply(protocol::success(&id, result)),
            Err((code, message)) => Handled::reply(protocol::failure(&id, code, &message)),
        },
        "ping" => Handled::reply(protocol::success(&id, json!({}))),
        "shutdown" => Handled { frame: Some(protocol::success(&id, json!({}))), stop: true },
        other => Handled::reply(protocol::failure(
            &id,
            protocol::METHOD_NOT_FOUND,
            &format!("未知方法: {other}"),
        )),
    }
}

fn initialize_result() -> Value {
    json!({
        "protocolVersion": PROTOCOL_VERSION,
        "capabilities": { "tools": {} },
        "serverInfo": { "name": SERVER_NAME, "version": SERVER_VERSION }
    })
}

fn tools_list<H: ToolHost>(host: &H) -> Value {
    let tools: Vec<Value> = host
        .list()
        .into_iter()
        .map(|t| {
            json!({
                "name": t.name,
                "description": t.description,
                // MCP names this field `inputSchema`; `ToolSpec` calls the same
                // thing `json_schema`.
                "inputSchema": t.input_schema,
            })
        })
        .collect();
    json!({ "tools": tools })
}

/// `Err` here means the *call frame* was malformed. A tool that ran and failed
/// comes back as `Ok`, see [`content`].
async fn tools_call<H: ToolHost>(
    host: &H,
    params: Option<Value>,
) -> Result<Value, (i32, String)> {
    let params = params
        .ok_or_else(|| (protocol::INVALID_PARAMS, "tools/call 缺少 params".to_string()))?;
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| (protocol::INVALID_PARAMS, "tools/call 缺少 name 参数".to_string()))?;
    let args = match params.get("arguments") {
        None | Some(Value::Null) => json!({}),
        Some(v) => v.clone(),
    };

    Ok(match host.call(name, args).await {
        Ok(value) => content(&render(&value), false),
        Err(message) => {
            tracing::warn!(tool = %name, %message, "工具调用失败");
            content(&message, true)
        }
    })
}

/// A failed tool is a normal outcome the client feeds back to its model, not a
/// transport fault: MCP wants it as a result with `isError` set, so the model
/// can read why and try something else. A JSON-RPC error would instead surface
/// as a broken connection.
fn content(text: &str, is_error: bool) -> Value {
    json!({
        "content": [{ "type": "text", "text": text }],
        "isError": is_error,
    })
}

/// Tool results travel as text. Pretty-printing is what the model on the other
/// end actually reads, and the indentation costs a handful of tokens.
fn render(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeHost;

    impl ToolHost for FakeHost {
        fn list(&self) -> Vec<ToolDescriptor> {
            vec![ToolDescriptor {
                name: "search_mail".to_string(),
                description: "Search the user's stored mail.".to_string(),
                input_schema: json!({"type": "object", "properties": {}}),
            }]
        }

        async fn call(&self, name: &str, args: Value) -> Result<Value, String> {
            match name {
                "search_mail" => Ok(json!({ "echo": args })),
                _ => Err("未找到: 工具 nope".to_string()),
            }
        }
    }

    /// Every frame must be exactly one line, or the transport desynchronises.
    fn parse_frame(handled: &Handled) -> Value {
        let frame = handled.frame.as_deref().expect("expected a reply");
        assert!(!frame.contains('\n'), "frame must not contain a newline");
        serde_json::from_str(frame).expect("frame is valid JSON")
    }

    #[tokio::test]
    async fn initialize_announces_the_tool_capability() {
        let out = handle(&FakeHost, r#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#).await;
        let v = parse_frame(&out);
        assert_eq!(v["result"]["protocolVersion"], PROTOCOL_VERSION);
        assert_eq!(v["result"]["capabilities"]["tools"], json!({}));
        assert_eq!(v["result"]["serverInfo"]["name"], SERVER_NAME);
        assert_eq!(v["result"]["serverInfo"]["version"], SERVER_VERSION);
        assert!(!out.stop);
    }

    #[tokio::test]
    async fn a_notification_produces_no_output() {
        let out =
            handle(&FakeHost, r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#).await;
        assert_eq!(out.frame, None);
        assert!(!out.stop);
    }

    #[tokio::test]
    async fn an_unknown_notification_is_still_silent() {
        let out = handle(&FakeHost, r#"{"jsonrpc":"2.0","method":"notifications/nonsense"}"#).await;
        assert_eq!(out.frame, None);
    }

    #[tokio::test]
    async fn tools_list_renames_the_schema_field() {
        let out = handle(&FakeHost, r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#).await;
        let v = parse_frame(&out);
        let tools = v["result"]["tools"].as_array().expect("array of tools");
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["name"], "search_mail");
        assert_eq!(tools[0]["inputSchema"]["type"], "object");
        assert!(tools[0].get("jsonSchema").is_none());
    }

    #[tokio::test]
    async fn a_successful_call_wraps_the_json_as_text() {
        let line = r#"{"jsonrpc":"2.0","id":3,"method":"tools/call",
            "params":{"name":"search_mail","arguments":{"query":"发票"}}}"#;
        let out = handle(&FakeHost, line).await;
        let v = parse_frame(&out);
        assert_eq!(v["result"]["isError"], json!(false));
        assert_eq!(v["result"]["content"][0]["type"], "text");
        let text = v["result"]["content"][0]["text"].as_str().expect("text payload");
        let echoed: Value = serde_json::from_str(text).expect("payload is JSON");
        assert_eq!(echoed["echo"]["query"], "发票");
    }

    #[tokio::test]
    async fn missing_arguments_become_an_empty_object() {
        let line = r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"search_mail"}}"#;
        let out = handle(&FakeHost, line).await;
        let v = parse_frame(&out);
        let text = v["result"]["content"][0]["text"].as_str().expect("text payload");
        let echoed: Value = serde_json::from_str(text).expect("payload is JSON");
        assert_eq!(echoed["echo"], json!({}));
    }

    #[tokio::test]
    async fn a_failing_tool_is_a_result_not_a_jsonrpc_error() {
        let line = r#"{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"nope"}}"#;
        let out = handle(&FakeHost, line).await;
        let v = parse_frame(&out);
        assert!(v.get("error").is_none());
        assert_eq!(v["result"]["isError"], json!(true));
        assert_eq!(v["result"]["content"][0]["text"], "未找到: 工具 nope");
    }

    #[tokio::test]
    async fn tools_call_without_params_is_invalid_params() {
        let out = handle(&FakeHost, r#"{"jsonrpc":"2.0","id":6,"method":"tools/call"}"#).await;
        let v = parse_frame(&out);
        assert_eq!(v["error"]["code"], json!(protocol::INVALID_PARAMS));
    }

    #[tokio::test]
    async fn tools_call_without_a_name_is_invalid_params() {
        let line = r#"{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"arguments":{}}}"#;
        let out = handle(&FakeHost, line).await;
        let v = parse_frame(&out);
        assert_eq!(v["error"]["code"], json!(protocol::INVALID_PARAMS));
    }

    #[tokio::test]
    async fn an_unknown_method_is_method_not_found() {
        let out = handle(&FakeHost, r#"{"jsonrpc":"2.0","id":8,"method":"mail/send"}"#).await;
        let v = parse_frame(&out);
        assert_eq!(v["error"]["code"], json!(protocol::METHOD_NOT_FOUND));
        assert_eq!(v["id"], json!(8));
    }

    #[tokio::test]
    async fn malformed_json_is_a_parse_error() {
        let out = handle(&FakeHost, "{oops").await;
        let v = parse_frame(&out);
        assert_eq!(v["error"]["code"], json!(protocol::PARSE_ERROR));
        assert_eq!(v["id"], Value::Null);
    }

    #[tokio::test]
    async fn ping_answers_an_empty_result() {
        let out = handle(&FakeHost, r#"{"jsonrpc":"2.0","id":9,"method":"ping"}"#).await;
        let v = parse_frame(&out);
        assert_eq!(v["result"], json!({}));
    }

    #[tokio::test]
    async fn shutdown_answers_then_stops_the_loop() {
        let out = handle(&FakeHost, r#"{"jsonrpc":"2.0","id":10,"method":"shutdown"}"#).await;
        let v = parse_frame(&out);
        assert_eq!(v["result"], json!({}));
        assert!(out.stop);
    }
}
