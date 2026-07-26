//! Streamable HTTP transport.
//!
//! One URL, one POST per request. The reply may be a single JSON object or an
//! SSE stream and the client has to accept both — Exa, for one, answers every
//! call as a stream and returns 406 unless `Accept` names both. Session state,
//! when the server hands one out, is a header we echo back and nothing more.

use std::sync::Mutex;

use serde_json::Value;

use super::wire;
use crate::error::{Error, Result};
use crate::types::{McpAuth, McpServerConfig};

pub struct HttpTransport {
    http: reqwest::Client,
    url: String,
    auth: McpAuth,
    api_key: String,
    /// `Mcp-Session-Id`, once the server has issued one. A server that works
    /// without sessions never sets it and we never send it.
    session: Mutex<Option<String>>,
    /// The negotiated revision, echoed on every request after `initialize`.
    /// Servers are entitled to reject a request that omits it.
    protocol: Mutex<Option<String>>,
}

impl HttpTransport {
    pub fn new(http: reqwest::Client, cfg: &McpServerConfig) -> Result<HttpTransport> {
        let url = cfg.url.trim();
        if url.is_empty() {
            return Err(Error::InvalidConfig(format!("MCP 服务器「{}」没有填写地址", cfg.name)));
        }
        if !url.starts_with("http://") && !url.starts_with("https://") {
            return Err(Error::InvalidConfig(format!(
                "MCP 服务器「{}」的地址必须以 http:// 或 https:// 开头",
                cfg.name
            )));
        }
        if cfg.auth != McpAuth::None && cfg.api_key.trim().is_empty() {
            return Err(Error::InvalidConfig(format!(
                "MCP 服务器「{}」选择了鉴权方式但没有填写密钥",
                cfg.name
            )));
        }
        Ok(HttpTransport {
            http,
            url: url.to_string(),
            auth: cfg.auth,
            api_key: cfg.api_key.trim().to_string(),
            session: Mutex::new(None),
            protocol: Mutex::new(None),
        })
    }

    /// Remember the revision the server answered `initialize` with.
    pub fn negotiated(&self, version: &str) {
        *self.protocol.lock().unwrap() = Some(version.to_string());
    }

    fn request(&self, frame: &Value) -> reqwest::RequestBuilder {
        let mut req = self
            .http
            .post(&self.url)
            // Both, always: a server may answer either way and picking one is
            // how you earn a 406 from half of them.
            .header("Accept", "application/json, text/event-stream")
            .header("Content-Type", "application/json")
            .json(frame);
        match self.auth {
            McpAuth::None => {}
            McpAuth::Bearer => req = req.bearer_auth(&self.api_key),
            McpAuth::ApiKeyHeader => req = req.header("x-api-key", &self.api_key),
        }
        if let Some(v) = self.protocol.lock().unwrap().as_deref() {
            req = req.header("MCP-Protocol-Version", v);
        }
        if let Some(s) = self.session.lock().unwrap().as_deref() {
            req = req.header("Mcp-Session-Id", s);
        }
        req
    }

    pub async fn call(&self, frame: Value, id: u64) -> Result<Value> {
        let res = self.request(&frame).send().await?;
        let status = res.status();

        // A session the server has forgotten. Reported as 404, which means
        // "initialize again", not "this URL is wrong" — so say so instead of
        // handing the user a bare 404 they cannot act on.
        if status.as_u16() == 404 && self.session.lock().unwrap().take().is_some() {
            return Err(Error::Other("MCP 会话已过期，需要重新连接".into()));
        }

        self.remember_session(res.headers());

        let body = res.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(Error::Other(format!(
                "MCP 服务器返回 {}: {}",
                status.as_u16(),
                super::snippet(&body)
            )));
        }
        wire::frame_for(&body, id)
    }

    pub async fn notify(&self, frame: Value) -> Result<()> {
        // A notification has no answer, so a non-2xx here is worth noting and
        // nothing more: the session is usable either way.
        match self.request(&frame).send().await {
            Ok(res) => {
                self.remember_session(res.headers());
                if !res.status().is_success() {
                    tracing::debug!("mcp: 通知被拒绝 ({})", res.status());
                }
            }
            Err(e) => tracing::debug!("mcp: 通知发送失败: {e}"),
        }
        Ok(())
    }

    fn remember_session(&self, headers: &reqwest::header::HeaderMap) {
        if let Some(sid) = headers.get("mcp-session-id").and_then(|v| v.to_str().ok()) {
            *self.session.lock().unwrap() = Some(sid.to_string());
        }
    }
}
