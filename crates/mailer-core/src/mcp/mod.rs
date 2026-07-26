//! MCP client: tools the assistant borrows from external servers.
//!
//! The app already *is* an MCP server (`mailer-mcp`, which hands the mailbox to
//! Claude or any other client). This module is the other direction: it connects
//! out to servers the user configured — Exa for the web, GitHub for repositories
//! — and offers their tools to the in-app assistant alongthe built-in ones. A
//! model that can only search stored mail cannot answer "这个报错是什么意思" or
//! "这个 PR 合了吗"; with this it can go and look.
//!
//! Layout:
//! - [`wire`]  pure framing: request builders, response parsers, SSE
//! - `http`    Streamable HTTP transport (remote servers)
//! - `stdio`   child-process transport (local servers)
//!
//! Design notes that matter:
//! - **Sessions are cached for the life of the process.** `initialize` plus
//!   `tools/list` costs two round trips, and a chat turn cannot afford them per
//!   message. A session is dropped and rebuilt when its config changes or the
//!   transport dies.
//! - **One dead server never costs the user their answer.** Every failure is
//!   recorded against that server and the rest of the catalogue is offered
//!   anyway. The assistant's own tools are always available.
//! - **Remote tools are offered to the assistant only, never re-exported by
//!   `mailer-mcp`.** Proxying them would let any client of ours spend the user's
//!   Exa key.

pub mod wire;

mod http;
mod stdio;

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use serde_json::Value;
use tokio::sync::Mutex;

use crate::error::{Error, Result};
use crate::store::Store;
use crate::types::{McpServerConfig, McpServerStatus, McpToolInfo, McpTransport};

/// Namespace every borrowed tool is offered under. Chosen to match what the
/// Claude Code and Cursor ecosystems already show users, so a name in a chat
/// transcript means the same thing here as it does there.
pub const PREFIX: &str = "mcp__";

/// Ceiling on a tool name. OpenAI rejects anything longer, and a name the
/// provider refuses would make every tool on that server unusable rather than
/// just oddly written.
const MAX_TOOL_NAME: usize = 64;
/// Pages of `tools/list` to follow. A server that pages forever is broken, and
/// following it forever would hang the chat.
const MAX_TOOL_PAGES: usize = 20;
/// Text one tool call may return. The assistant truncates again for the model;
/// this stops a runaway server from being held in memory in the first place.
const MAX_RESULT_CHARS: usize = 20_000;
/// Tools one server may contribute. Past this the tool list starts crowding out
/// the mail the assistant is supposed to be reasoning about.
const MAX_TOOLS_PER_SERVER: usize = 40;

/// The two ways to reach a server. Both carry one request and get its answer
/// back, and both fire notifications that have none.
///
/// Interior mutability throughout rather than `&mut self`, so a session can be
/// shared as an `Arc` and the sessions map need not be held across a tool call.
enum Pipe {
    Http(http::HttpTransport),
    Stdio(stdio::StdioTransport),
}

impl Pipe {
    async fn call(&self, frame: Value, id: u64) -> Result<Value> {
        match self {
            Pipe::Http(t) => t.call(frame, id).await,
            Pipe::Stdio(t) => t.call(frame, id).await,
        }
    }

    async fn notify(&self, frame: Value) -> Result<()> {
        match self {
            Pipe::Http(t) => t.notify(frame).await,
            Pipe::Stdio(t) => t.notify(frame).await,
        }
    }

    /// True when the transport is known to be unusable, so the session gets
    /// rebuilt instead of failing the next call. Only a child process can be
    /// dead in a way we can see without asking.
    fn dead(&self) -> bool {
        match self {
            Pipe::Http(_) => false,
            Pipe::Stdio(t) => t.is_dead(),
        }
    }
}

/// One tool the assistant can call, as the assistant sees it.
#[derive(Debug, Clone)]
pub struct Entry {
    /// Fully qualified, e.g. `mcp__exa__web_search_exa`.
    pub name: String,
    /// The name on the server.
    pub remote_name: String,
    pub server_id: String,
    /// Prefixed with the server it came from: the model has to know that a
    /// borrowed tool leaves this machine.
    pub description: String,
    pub schema: Value,
}

// ---------------------------------------------------------------------------
// Session
// ---------------------------------------------------------------------------

struct Session {
    /// Everything about the config that would invalidate the connection.
    fingerprint: String,
    pipe: Pipe,
    info: wire::ServerInfo,
    tools: Vec<Entry>,
    next_id: AtomicU64,
}

impl Session {
    async fn connect(cfg: &McpServerConfig, http: &reqwest::Client) -> Result<Session> {
        let pipe = match cfg.transport {
            McpTransport::Http => Pipe::Http(http::HttpTransport::new(http.clone(), cfg)?),
            McpTransport::Stdio => Pipe::Stdio(stdio::StdioTransport::spawn(cfg)?),
        };

        let next_id = AtomicU64::new(1);
        let id = next_id.fetch_add(1, Ordering::Relaxed);
        let frame = pipe
            .call(wire::initialize_request(id, "mailer", env!("CARGO_PKG_VERSION")), id)
            .await?;
        let info = wire::parse_initialize(&wire::result_of(&frame, "初始化")?);

        // HTTP has to echo the negotiated revision on every later request, and
        // servers are entitled to reject one that does not. Set it before
        // `tools/list`, which is the first request that would be rejected.
        if let Pipe::Http(t) = &pipe {
            t.negotiated(&info.protocol_version);
        }

        // Some servers reject everything after `initialize` until they see this,
        // and it has no answer to wait for.
        pipe.notify(wire::initialized_notification()).await?;

        if !info.has_tools {
            return Err(Error::Other(format!(
                "MCP 服务器「{}」没有提供任何工具，无法使用",
                cfg.name
            )));
        }

        let mut session =
            Session { fingerprint: fingerprint(cfg), pipe, info, tools: Vec::new(), next_id };
        session.tools = session.discover(cfg).await?;
        Ok(session)
    }

    async fn discover(&self, cfg: &McpServerConfig) -> Result<Vec<Entry>> {
        let mut remote = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..MAX_TOOL_PAGES {
            let id = self.next_id.fetch_add(1, Ordering::Relaxed);
            let frame =
                self.pipe.call(wire::tools_list_request(id, cursor.as_deref()), id).await?;
            let (mut page, next) = wire::parse_tools_page(&wire::result_of(&frame, "获取工具列表")?);
            remote.append(&mut page);
            match next {
                Some(c) if remote.len() < MAX_TOOLS_PER_SERVER => cursor = Some(c),
                _ => break,
            }
        }
        if remote.len() > MAX_TOOLS_PER_SERVER {
            tracing::warn!(
                "mcp: 服务器「{}」提供了 {} 个工具，只取前 {MAX_TOOLS_PER_SERVER} 个",
                cfg.name,
                remote.len()
            );
            remote.truncate(MAX_TOOLS_PER_SERVER);
        }

        let label = server_slug(cfg);
        let mut used = HashSet::new();
        Ok(remote
            .into_iter()
            .filter_map(|t| {
                let name = qualify(&label, &t.name, &mut used)?;
                Some(Entry {
                    name,
                    remote_name: t.name,
                    server_id: cfg.id.clone(),
                    // The model has to be told this is not local. Without it a
                    // model reasonably assumes every tool is as private as
                    // `search_mail`, and pastes mail into a web search.
                    description: format!(
                        "[外部服务 {}] {}",
                        cfg.name.trim(),
                        t.description.trim()
                    ),
                    schema: t.input_schema,
                })
            })
            .collect())
    }

    async fn call_tool(&self, remote_name: &str, args: Value) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let frame = self.pipe.call(wire::tools_call_request(id, remote_name, &args), id).await?;
        let outcome = wire::parse_tool_outcome(&wire::result_of(&frame, "调用工具")?);

        // A tool that ran and failed is not a transport failure: the loop hands
        // the text back to the model, which can correct itself and try again.
        if outcome.is_error {
            let why = if outcome.text.trim().is_empty() {
                "服务器没有说明原因".to_string()
            } else {
                truncate(&outcome.text)
            };
            return Err(Error::Other(format!("{remote_name} 执行失败: {why}")));
        }
        Ok(serde_json::json!({ "text": truncate(&outcome.text) }))
    }
}

// ---------------------------------------------------------------------------
// Hub
// ---------------------------------------------------------------------------

/// Every live MCP session, keyed by server id.
///
/// Process-global because sessions are process-global resources: a child process
/// and an HTTP session outlive any one chat turn, and threading a handle through
/// every call site would only make that less obvious, not less true.
pub struct Hub {
    sessions: Mutex<HashMap<String, Arc<Session>>>,
    /// Why the last attempt at a server failed. Read by the settings screen; a
    /// silent failure is the one thing worse than a slow tool.
    errors: Mutex<HashMap<String, String>>,
}

static HUB: OnceLock<Arc<Hub>> = OnceLock::new();

pub fn hub() -> Arc<Hub> {
    HUB.get_or_init(|| {
        Arc::new(Hub { sessions: Mutex::new(HashMap::new()), errors: Mutex::new(HashMap::new()) })
    })
    .clone()
}

impl Hub {
    /// Every tool the assistant may borrow this turn.
    ///
    /// Connects whatever is not connected yet, in parallel, and skips whatever
    /// will not connect. Never returns an error: a broken MCP server is a
    /// missing capability, not a failed question.
    pub async fn catalogue(&self, store: &Store, http: &reqwest::Client) -> Vec<Entry> {
        let configs = match store.mcp_servers() {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("mcp: 读取服务器配置失败: {e}");
                return Vec::new();
            }
        };
        let wanted: Vec<McpServerConfig> = configs.into_iter().filter(|c| c.enabled).collect();
        if wanted.is_empty() {
            return Vec::new();
        }

        let ready = self.ensure(&wanted, http).await;

        // Deterministic order, and one qualified name means one tool: two
        // servers the user gave the same name would otherwise shadow each other
        // depending on which connected first.
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for cfg in &wanted {
            let Some(session) = ready.get(&cfg.id) else { continue };
            for entry in &session.tools {
                if seen.insert(entry.name.clone()) {
                    out.push(entry.clone());
                } else {
                    tracing::warn!("mcp: 工具名 {} 重复，已跳过", entry.name);
                }
            }
        }
        out
    }

    /// Connect what is missing and return everything usable, keyed by id.
    async fn ensure(
        &self,
        wanted: &[McpServerConfig],
        http: &reqwest::Client,
    ) -> HashMap<String, Arc<Session>> {
        let mut sessions = self.sessions.lock().await;

        // Configs that vanished, were disabled, or were edited: their session is
        // no longer the session the user asked for.
        let live: HashSet<&str> = wanted.iter().map(|c| c.id.as_str()).collect();
        sessions.retain(|id, s| live.contains(id.as_str()) && !s.pipe.dead());
        for cfg in wanted {
            let stale = sessions.get(&cfg.id).is_some_and(|s| s.fingerprint != fingerprint(cfg));
            if stale {
                sessions.remove(&cfg.id);
            }
        }

        let missing: Vec<&McpServerConfig> =
            wanted.iter().filter(|c| !sessions.contains_key(&c.id)).collect();
        if !missing.is_empty() {
            // In parallel: one server behind a slow network must not decide how
            // long the whole chat waits.
            let attempts = futures::future::join_all(
                missing.iter().map(|cfg| async move { (cfg, Session::connect(cfg, http).await) }),
            )
            .await;
            let mut errors = self.errors.lock().await;
            for (cfg, result) in attempts {
                match result {
                    Ok(session) => {
                        tracing::info!(
                            "mcp: 已连接「{}」({} {}, 协议 {}), {} 个工具",
                            cfg.name,
                            session.info.name,
                            session.info.version,
                            session.info.protocol_version,
                            session.tools.len()
                        );
                        errors.remove(&cfg.id);
                        sessions.insert(cfg.id.clone(), Arc::new(session));
                    }
                    Err(e) => {
                        tracing::warn!("mcp: 连接「{}」失败: {e}", cfg.name);
                        errors.insert(cfg.id.clone(), e.to_string());
                    }
                }
            }
        }

        sessions.clone()
    }

    /// Run one borrowed tool. `name` is the qualified name the model called.
    pub async fn call(
        &self,
        store: &Store,
        http: &reqwest::Client,
        name: &str,
        args: Value,
    ) -> Result<Value> {
        if let Some((session, remote)) = self.resolve(name).await {
            return session.call_tool(&remote, args).await;
        }
        // The model called something we have not looked up yet — a session that
        // died between rounds, or a first call with no catalogue built.
        self.catalogue(store, http).await;
        let Some((session, remote)) = self.resolve(name).await else {
            return Err(Error::NotFound(format!("外部工具 {name} 不可用")));
        };
        session.call_tool(&remote, args).await
    }

    async fn resolve(&self, name: &str) -> Option<(Arc<Session>, String)> {
        let sessions = self.sessions.lock().await;
        for session in sessions.values() {
            if let Some(entry) = session.tools.iter().find(|t| t.name == name) {
                return Some((session.clone(), entry.remote_name.clone()));
            }
        }
        None
    }

    /// One line per configured server for the settings screen: what it calls
    /// itself, what it offers, and why it is not working if it is not.
    pub async fn status(&self, store: &Store, http: &reqwest::Client) -> Vec<McpServerStatus> {
        let configs = store.mcp_servers().unwrap_or_default();
        let enabled: Vec<McpServerConfig> =
            configs.iter().filter(|c| c.enabled).cloned().collect();
        let ready = self.ensure(&enabled, http).await;
        let errors = self.errors.lock().await.clone();

        configs
            .iter()
            .map(|cfg| match ready.get(&cfg.id) {
                Some(session) => McpServerStatus {
                    id: cfg.id.clone(),
                    server_name: session.info.name.clone(),
                    server_version: session.info.version.clone(),
                    protocol_version: session.info.protocol_version.clone(),
                    tools: session
                        .tools
                        .iter()
                        .map(|t| McpToolInfo {
                            name: t.name.clone(),
                            remote_name: t.remote_name.clone(),
                            description: t.description.clone(),
                        })
                        .collect(),
                    error: None,
                },
                None => McpServerStatus {
                    id: cfg.id.clone(),
                    server_name: String::new(),
                    server_version: String::new(),
                    protocol_version: String::new(),
                    tools: Vec::new(),
                    error: if cfg.enabled {
                        Some(errors.get(&cfg.id).cloned().unwrap_or_else(|| "尚未连接".into()))
                    } else {
                        None
                    },
                },
            })
            .collect()
    }

    /// Forget a server's session so the next use reconnects. Called after the
    /// user edits a config, and by the settings screen's retry button.
    pub async fn forget(&self, id: &str) {
        self.sessions.lock().await.remove(id);
        self.errors.lock().await.remove(id);
    }

    /// Forget everything. Used when the whole server list is replaced.
    pub async fn forget_all(&self) {
        self.sessions.lock().await.clear();
        self.errors.lock().await.clear();
    }
}

// ---------------------------------------------------------------------------
// Names
// ---------------------------------------------------------------------------

/// True for a tool name this module owns.
pub fn is_remote(name: &str) -> bool {
    name.starts_with(PREFIX)
}

/// The server part of a qualified name, for display. Best effort: a server
/// slugged to nothing has no name to show.
pub fn server_of(name: &str) -> Option<&str> {
    name.strip_prefix(PREFIX)?.split("__").next().filter(|s| !s.is_empty())
}

/// The tool part of a qualified name, for display.
pub fn tool_of(name: &str) -> &str {
    name.strip_prefix(PREFIX)
        .and_then(|rest| rest.split_once("__"))
        .map(|(_, tool)| tool)
        .unwrap_or(name)
}

/// Everything about a config that changes what the connection *is*. A relabelled
/// server with the same URL still gets a new session, because the label is part
/// of every tool name the model has been shown.
fn fingerprint(cfg: &McpServerConfig) -> String {
    serde_json::to_string(cfg).unwrap_or_default()
}

/// The server's namespace: its name, reduced to what a tool name may contain.
fn server_slug(cfg: &McpServerConfig) -> String {
    let slug = slug(&cfg.name);
    if slug.is_empty() {
        // A server the user did not name still needs a stable namespace, and
        // the id is the only thing guaranteed unique.
        slug_or(&cfg.id, "server")
    } else {
        slug
    }
}

/// `mcp__<server>__<tool>`, kept unique within one server and inside the length
/// every provider accepts.
fn qualify(server: &str, tool: &str, used: &mut HashSet<String>) -> Option<String> {
    let tool_slug = slug(tool);
    if tool_slug.is_empty() {
        return None;
    }
    let head = format!("{PREFIX}{server}__");
    // Truncating the tool rather than the server keeps names attributable: the
    // user can always see which server a call went to.
    let room = MAX_TOOL_NAME.saturating_sub(head.chars().count());
    if room < 4 {
        tracing::warn!("mcp: 服务器名「{server}」太长，工具名无法容纳");
        return None;
    }

    let mut candidate = format!("{head}{}", take_chars(&tool_slug, room));
    // Truncation can collide. A numeric suffix is ugly but it is better than
    // two different tools answering to one name.
    for n in 2..100 {
        if used.insert(candidate.clone()) {
            return Some(candidate);
        }
        let suffix = format!("_{n}");
        let keep = room.saturating_sub(suffix.chars().count());
        candidate = format!("{head}{}{suffix}", take_chars(&tool_slug, keep));
    }
    None
}

/// Anything a tool name may not contain becomes `_`. Providers validate names
/// against `^[a-zA-Z0-9_-]+$` and reject the whole request otherwise, so a
/// server with a dotted tool name would break every tool in the turn.
fn slug(s: &str) -> String {
    let mut out = String::new();
    let mut last_underscore = false;
    for ch in s.trim().chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
            out.push(ch);
            last_underscore = ch == '_';
        } else if !last_underscore && !out.is_empty() {
            out.push('_');
            last_underscore = true;
        }
    }
    while out.ends_with('_') {
        out.pop();
    }
    out
}

fn slug_or(s: &str, fallback: &str) -> String {
    let slug = slug(s);
    if slug.is_empty() {
        fallback.to_string()
    } else {
        slug
    }
}

fn take_chars(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

fn truncate(s: &str) -> String {
    if s.chars().count() <= MAX_RESULT_CHARS {
        return s.to_string();
    }
    format!("{}…（外部工具返回过长，已截断）", take_chars(s, MAX_RESULT_CHARS))
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

    fn cfg(name: &str) -> McpServerConfig {
        McpServerConfig {
            id: "s1".into(),
            name: name.into(),
            url: "https://mcp.exa.ai/mcp".into(),
            ..Default::default()
        }
    }

    #[test]
    fn a_qualified_name_says_which_server_it_came_from() {
        let mut used = HashSet::new();
        let name = qualify("exa", "web_search_exa", &mut used).unwrap();
        assert_eq!(name, "mcp__exa__web_search_exa");
        assert!(is_remote(&name));
        assert_eq!(server_of(&name), Some("exa"));
        assert_eq!(tool_of(&name), "web_search_exa");
        assert!(!is_remote("search_mail"), "a built-in tool is not remote");
    }

    /// Providers validate tool names against `[a-zA-Z0-9_-]` and reject the
    /// whole request on a bad one — which would break every tool in the turn,
    /// not just the offending one.
    #[test]
    fn names_are_reduced_to_what_a_provider_accepts() {
        assert_eq!(slug("Exa Search"), "Exa_Search");
        assert_eq!(slug("github.copilot/mcp"), "github_copilot_mcp");
        assert_eq!(slug("  spaced  out  "), "spaced_out");
        assert_eq!(slug("中文名"), "", "nothing usable survives, so the id is used instead");
        assert_eq!(slug("keep-dashes_and_1"), "keep-dashes_and_1");

        let mut used = HashSet::new();
        let name = qualify("my_server", "do.a/thing", &mut used).unwrap();
        assert_eq!(name, "mcp__my_server__do_a_thing");
        assert!(
            name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'),
            "{name}"
        );
    }

    /// A server the user named in Chinese still needs a namespace, or every one
    /// of its tools would be dropped.
    #[test]
    fn an_unnameable_server_falls_back_to_its_id() {
        let mut c = cfg("网页搜索");
        c.id = "srv-42".into();
        assert_eq!(server_slug(&c), "srv-42");
        c.id = "中文".into();
        assert_eq!(server_slug(&c), "server");
    }

    #[test]
    fn names_stay_inside_the_provider_limit_and_stay_unique() {
        let mut used = HashSet::new();
        let long = "a".repeat(120);
        let first = qualify("exa", &long, &mut used).unwrap();
        assert!(first.chars().count() <= MAX_TOOL_NAME, "{}", first.chars().count());

        // Two different tools whose names only differ past the cut must not
        // collapse into one.
        let second = qualify("exa", &format!("{long}b"), &mut used).unwrap();
        assert_ne!(first, second);
        assert!(second.chars().count() <= MAX_TOOL_NAME);

        // A server name that leaves no room is refused rather than producing a
        // name the provider will reject.
        let mut used = HashSet::new();
        assert!(qualify(&"s".repeat(70), "tool", &mut used).is_none());
    }

    /// The fingerprint decides when a session is thrown away. Missing an edit
    /// would leave the user talking to the old server with the new settings on
    /// screen.
    #[test]
    fn every_edit_invalidates_the_session() {
        let base = cfg("exa");
        for mutate in [
            (|c: &mut McpServerConfig| c.name = "exa2".into()) as fn(&mut McpServerConfig),
            |c| c.url = "https://other/mcp".into(),
            |c| c.api_key = "k".into(),
            |c| c.auth = crate::types::McpAuth::Bearer,
            |c| c.transport = McpTransport::Stdio,
            |c| c.command = "npx".into(),
            |c| c.args = vec!["-y".into()],
            |c| {
                c.env.insert("K".into(), "V".into());
            },
        ] {
            let mut edited = base.clone();
            mutate(&mut edited);
            assert_ne!(fingerprint(&base), fingerprint(&edited), "{edited:?}");
        }
        assert_eq!(fingerprint(&base), fingerprint(&base.clone()));
    }

    /// End-to-end against a real server, over the real transport. Ignored
    /// because it needs the network, which a test suite must not: run it with
    /// `cargo test -p mailer-core -- --ignored exa` after changing anything in
    /// this module. Exa's default tools need no key, which is what makes it
    /// usable as a check; it also answers every POST as SSE, so this exercises
    /// the stream path rather than the easy one.
    #[tokio::test]
    #[ignore = "needs network"]
    async fn a_real_server_connects_and_answers() {
        let mut c = cfg("exa");
        c.id = "live".into();
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(60))
            .build()
            .unwrap();

        let session = Session::connect(&c, &http).await.expect("connect");
        assert!(!session.info.name.is_empty(), "{:?}", session.info);
        assert!(!session.tools.is_empty(), "no tools advertised");
        assert!(
            session.tools.iter().all(|t| t.name.starts_with("mcp__exa__")),
            "{:?}",
            session.tools
        );

        let tool = session
            .tools
            .iter()
            .find(|t| t.remote_name.contains("search"))
            .expect("a search tool");
        let out = session
            .call_tool(&tool.remote_name, serde_json::json!({ "query": "MCP protocol" }))
            .await
            .expect("call");
        assert!(!out["text"].as_str().unwrap_or_default().is_empty(), "{out}");
    }

    /// The stdio path, against a server small enough to read. Ignored because it
    /// needs `python3`, which the suite must not assume: run it with
    /// `cargo test -p mailer-core -- --ignored stdio`.
    ///
    /// The script deliberately prints a banner line first and a notification
    /// between the answers, because real servers do both and a client that
    /// cannot skip them looks broken against half the ecosystem.
    #[tokio::test]
    #[ignore = "needs python3"]
    async fn a_stdio_server_connects_and_answers() {
        const SERVER: &str = r#"
import json, sys
print("starting up", flush=True)            # a banner, not protocol
for line in sys.stdin:
    req = json.loads(line)
    m, i = req.get("method"), req.get("id")
    if m == "initialize":
        r = {"protocolVersion": "2025-11-25", "capabilities": {"tools": {}},
             "serverInfo": {"name": "toy", "version": "0.1"}}
    elif m == "tools/list":
        r = {"tools": [{"name": "echo", "description": "Echo back",
                        "inputSchema": {"type": "object", "properties": {"s": {"type": "string"}}}}]}
    elif m == "tools/call":
        args = req["params"].get("arguments") or {}
        r = {"content": [{"type": "text", "text": "echo: " + str(args.get("s", ""))}]}
    else:
        continue                             # notifications get no answer
    print(json.dumps({"jsonrpc": "2.0", "method": "notifications/progress"}), flush=True)
    print(json.dumps({"jsonrpc": "2.0", "id": i, "result": r}), flush=True)
"#;
        let mut c = cfg("玩具服务器");
        c.id = "toy".into();
        c.transport = McpTransport::Stdio;
        c.command = "python3".into();
        c.args = vec!["-u".into(), "-c".into(), SERVER.into()];

        let session = Session::connect(&c, &reqwest::Client::new()).await.expect("connect");
        assert_eq!(session.info.name, "toy");
        // The server name is not expressible in a tool name, so the id is.
        assert_eq!(session.tools[0].name, "mcp__toy__echo");
        assert!(session.tools[0].description.contains("外部服务 玩具服务器"));

        let out = session.call_tool("echo", serde_json::json!({ "s": "喂" })).await.unwrap();
        assert_eq!(out["text"], "echo: 喂");

        // A second call reuses the same process rather than paying for a spawn.
        let again = session.call_tool("echo", serde_json::json!({ "s": "again" })).await.unwrap();
        assert_eq!(again["text"], "echo: again");
    }

    #[test]
    fn a_long_result_is_bounded() {
        let text = "字".repeat(MAX_RESULT_CHARS + 500);
        let out = truncate(&text);
        assert!(out.contains("已截断"));
        assert!(out.chars().count() < MAX_RESULT_CHARS + 40);
        assert_eq!(truncate("short"), "short");
    }
}
