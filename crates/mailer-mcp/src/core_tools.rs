//! Bridge from protocol dispatch to `mailer_core::tools`.
//!
//! The MCP server owns no capabilities of its own: it advertises the same
//! catalogue the in-app assistant uses and calls the same `execute`, so an
//! external client and the chat window cannot drift into two different sets of
//! behaviours — or two different safety rules. `send_mail`, in particular,
//! returns a pending action here exactly as it does in the app; nothing leaves
//! the machine on an MCP client's word.

use std::sync::Arc;

use mailer_core::store::Store;
use mailer_core::tools::{self, ToolContext};
use serde_json::Value;

use crate::server::{ToolDescriptor, ToolHost};

pub struct CoreTools {
    store: Arc<Store>,
    http: reqwest::Client,
}

impl CoreTools {
    pub fn new(store: Arc<Store>, http: reqwest::Client) -> CoreTools {
        CoreTools { store, http }
    }
}

impl ToolHost for CoreTools {
    fn list(&self) -> Vec<ToolDescriptor> {
        tools::specs()
            .into_iter()
            .map(|s| ToolDescriptor {
                name: s.name.to_string(),
                description: s.description.to_string(),
                input_schema: s.json_schema,
            })
            .collect()
    }

    async fn call(&self, name: &str, args: Value) -> Result<Value, String> {
        // The context snapshots settings, which is right for one assistant turn
        // but wrong for a session that stays open for days: rebuilding it per
        // call means switching embedding model or API key in the app takes
        // effect on the next tool use instead of the next restart. Three
        // indexed reads against a local SQLite file — cheaper than the HTTP
        // round-trip that usually follows.
        let ctx = ToolContext::new(Arc::clone(&self.store), self.http.clone())
            .map_err(|e| e.to_string())?;
        tools::execute(&ctx, name, args).await.map_err(|e| e.to_string())
    }
}
