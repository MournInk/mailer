//! JSON-RPC 2.0 framing for the MCP stdio transport.
//!
//! One JSON object per line, in both directions. Everything here is pure
//! string and [`Value`] work, so the wire format is testable without a
//! mailbox behind it.

use serde_json::{json, Value};

// Codes from the JSON-RPC 2.0 specification, §5.1.
pub const PARSE_ERROR: i32 = -32700;
pub const INVALID_REQUEST: i32 = -32600;
pub const METHOD_NOT_FOUND: i32 = -32601;
pub const INVALID_PARAMS: i32 = -32602;

/// A well-formed incoming call.
#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    /// `None` marks a notification, which must never be answered.
    pub id: Option<Value>,
    pub method: String,
    pub params: Option<Value>,
}

/// A line that could not be turned into a [`Request`], carrying the code and
/// the id to answer with already worked out.
#[derive(Debug, Clone, PartialEq)]
pub struct BadRequest {
    pub id: Value,
    pub code: i32,
    pub message: String,
}

/// Turn one input line into a request.
///
/// A parse failure is answered with a null id: the id lives inside the text we
/// just failed to read, so there is nothing else to echo (JSON-RPC 2.0 §5.1).
pub fn parse(line: &str) -> Result<Request, BadRequest> {
    let value: Value = serde_json::from_str(line).map_err(|e| BadRequest {
        id: Value::Null,
        code: PARSE_ERROR,
        message: format!("JSON 解析失败: {e}"),
    })?;

    // Batches are legal JSON-RPC but MCP never sends them, and answering half
    // a batch is worse than refusing it outright.
    let obj = value.as_object().ok_or_else(|| BadRequest {
        id: Value::Null,
        code: INVALID_REQUEST,
        message: "请求必须是单个 JSON 对象".to_string(),
    })?;

    // An explicit `"id": null` is treated as a notification: the spec forbids
    // null ids in requests, and a null-id response could not be routed anyway.
    let id = match obj.get("id") {
        None | Some(Value::Null) => None,
        Some(v) => Some(v.clone()),
    };

    let method = obj
        .get("method")
        .and_then(Value::as_str)
        .ok_or_else(|| BadRequest {
            id: id.clone().unwrap_or(Value::Null),
            code: INVALID_REQUEST,
            message: "请求缺少 method 字段".to_string(),
        })?
        .to_string();

    // `"params": null` and a missing `params` mean the same thing to callers.
    let params = obj.get("params").filter(|p| !p.is_null()).cloned();

    Ok(Request { id, method, params })
}

/// Frame a successful result. The returned string carries no newline; the
/// transport adds exactly one.
pub fn success(id: &Value, result: Value) -> String {
    json!({ "jsonrpc": "2.0", "id": id, "result": result }).to_string()
}

/// Frame a JSON-RPC error. Reserved for transport-level faults — a tool that
/// merely failed is a *successful* call with `isError`, see [`crate::server`].
pub fn failure(id: &Value, code: i32, message: &str) -> String {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_request() {
        let req = parse(r#"{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{"a":1}}"#)
            .expect("well-formed request");
        assert_eq!(req.id, Some(json!(1)));
        assert_eq!(req.method, "tools/list");
        assert_eq!(req.params, Some(json!({"a": 1})));
    }

    #[test]
    fn keeps_string_ids_as_they_came() {
        let req = parse(r#"{"jsonrpc":"2.0","id":"abc","method":"ping"}"#).expect("valid");
        assert_eq!(req.id, Some(json!("abc")));
        assert_eq!(req.params, None);
    }

    #[test]
    fn a_missing_id_marks_a_notification() {
        let req = parse(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#).expect("valid");
        assert_eq!(req.id, None);
    }

    #[test]
    fn an_explicit_null_id_marks_a_notification() {
        let req = parse(r#"{"jsonrpc":"2.0","id":null,"method":"notifications/cancelled"}"#)
            .expect("valid");
        assert_eq!(req.id, None);
    }

    #[test]
    fn null_params_read_as_absent() {
        let req = parse(r#"{"jsonrpc":"2.0","id":7,"method":"ping","params":null}"#).expect("valid");
        assert_eq!(req.params, None);
    }

    #[test]
    fn broken_json_is_a_parse_error_with_a_null_id() {
        let bad = parse("{not json").expect_err("should not parse");
        assert_eq!(bad.code, PARSE_ERROR);
        assert_eq!(bad.id, Value::Null);
    }

    #[test]
    fn a_non_object_is_an_invalid_request() {
        let bad = parse("[1, 2, 3]").expect_err("arrays are refused");
        assert_eq!(bad.code, INVALID_REQUEST);
    }

    #[test]
    fn a_missing_method_echoes_the_id() {
        let bad = parse(r#"{"jsonrpc":"2.0","id":42}"#).expect_err("no method");
        assert_eq!(bad.code, INVALID_REQUEST);
        assert_eq!(bad.id, json!(42));
    }

    #[test]
    fn success_frames_are_one_line() {
        let frame = success(&json!(1), json!({"ok": true}));
        assert!(!frame.contains('\n'));
        let parsed: Value = serde_json::from_str(&frame).expect("valid JSON");
        assert_eq!(parsed["jsonrpc"], "2.0");
        assert_eq!(parsed["id"], json!(1));
        assert_eq!(parsed["result"], json!({"ok": true}));
        assert!(parsed.get("error").is_none());
    }

    #[test]
    fn error_frames_carry_code_and_message() {
        let frame = failure(&json!("x"), METHOD_NOT_FOUND, "未知方法: nope");
        assert!(!frame.contains('\n'));
        let parsed: Value = serde_json::from_str(&frame).expect("valid JSON");
        assert_eq!(parsed["id"], json!("x"));
        assert_eq!(parsed["error"]["code"], json!(METHOD_NOT_FOUND));
        assert_eq!(parsed["error"]["message"], "未知方法: nope");
        assert!(parsed.get("result").is_none());
    }

    #[test]
    fn embedded_newlines_are_escaped_not_emitted() {
        // A tool result containing a line break must not split the frame.
        let frame = success(&json!(1), json!({"text": "第一行\n第二行"}));
        assert!(!frame.contains('\n'));
    }
}
