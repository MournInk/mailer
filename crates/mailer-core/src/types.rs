//! Shared data model for the whole application.
//!
//! Every struct here is serialized across the Tauri IPC boundary, so all
//! field names use `camelCase` to match the TypeScript mirror in
//! `src/lib/types.ts`. Keep the two files in sync.

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Accounts
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    Imap,
    Pop3,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TlsMode {
    /// Implicit TLS from the first byte (IMAPS 993 / POP3S 995 / SMTPS 465).
    Tls,
    /// Plaintext connection upgraded via STARTTLS.
    Starttls,
    /// No encryption. Only sensible for localhost bridges (e.g. Proton Bridge).
    None,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SmtpConfig {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: String,
    pub tls: TlsMode,
}

/// A configured mailbox source.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountConfig {
    pub id: String,
    /// Human label shown in the sidebar ("Work", "私人邮箱", ...).
    pub label: String,
    pub email: String,
    pub protocol: Protocol,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: String,
    pub tls: TlsMode,
    /// Optional outgoing server; accounts without SMTP are receive-only.
    #[serde(default)]
    pub smtp: Option<SmtpConfig>,
    /// Background polling interval in seconds. 0 disables auto sync.
    pub sync_interval_secs: u64,
    /// Accent hue used for the account avatar in the UI (0-360).
    pub color_hue: u16,
    pub created_at: i64,
}

/// Account as exposed to the frontend: secrets are redacted.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountPublic {
    pub id: String,
    pub label: String,
    pub email: String,
    pub protocol: Protocol,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub tls: TlsMode,
    pub has_smtp: bool,
    pub smtp_host: Option<String>,
    pub smtp_port: Option<u16>,
    pub smtp_username: Option<String>,
    pub smtp_tls: Option<TlsMode>,
    pub sync_interval_secs: u64,
    pub color_hue: u16,
    pub created_at: i64,
}

impl From<&AccountConfig> for AccountPublic {
    fn from(a: &AccountConfig) -> Self {
        AccountPublic {
            id: a.id.clone(),
            label: a.label.clone(),
            email: a.email.clone(),
            protocol: a.protocol,
            host: a.host.clone(),
            port: a.port,
            username: a.username.clone(),
            tls: a.tls,
            has_smtp: a.smtp.is_some(),
            smtp_host: a.smtp.as_ref().map(|s| s.host.clone()),
            smtp_port: a.smtp.as_ref().map(|s| s.port),
            smtp_username: a.smtp.as_ref().map(|s| s.username.clone()),
            smtp_tls: a.smtp.as_ref().map(|s| s.tls),
            sync_interval_secs: a.sync_interval_secs,
            color_hue: a.color_hue,
            created_at: a.created_at,
        }
    }
}

// ---------------------------------------------------------------------------
// Messages
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Category {
    /// One-time codes / magic links. Surfaced instantly via system popup.
    Verification,
    /// Junk. Kept silent; may be auto-deleted when clearly worthless.
    Spam,
    /// Everything routine. Stored silently.
    Normal,
    /// Bills, invoices, security alerts, personally important mail.
    Important,
}

impl Category {
    pub fn as_str(&self) -> &'static str {
        match self {
            Category::Verification => "verification",
            Category::Spam => "spam",
            Category::Normal => "normal",
            Category::Important => "important",
        }
    }

    pub fn parse(s: &str) -> Option<Category> {
        match s {
            "verification" => Some(Category::Verification),
            "spam" => Some(Category::Spam),
            "normal" => Some(Category::Normal),
            "important" => Some(Category::Important),
            _ => None,
        }
    }
}

/// Result of running the LLM triage over one message.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiAnalysis {
    pub category: Category,
    /// 0.0 - 1.0
    pub confidence: f32,
    /// One-line summary in the user's language.
    pub summary: String,
    /// Extracted OTP / verification code, when `category == Verification`.
    pub verification_code: Option<String>,
    /// True when the model judged a spam message worthless enough to delete.
    pub deletable: bool,
    /// Short model-provided justification (debugging / transparency UI).
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentMeta {
    pub filename: String,
    pub mime: String,
    pub size: u64,
}

/// A fully stored message.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmailMessage {
    pub id: String,
    pub account_id: String,
    /// Mailbox folder, e.g. "INBOX". POP3 accounts always use "INBOX".
    pub folder: String,
    /// IMAP UID or POP3 UIDL token — unique per (account, folder).
    pub uid: String,
    /// RFC 5322 Message-ID header, used for cross-protocol dedup.
    pub message_id: Option<String>,
    pub subject: String,
    pub from_name: String,
    pub from_addr: String,
    pub to_addrs: Vec<String>,
    /// Date header as unix milliseconds.
    pub date: i64,
    /// Plain-text preview (~140 chars).
    pub snippet: String,
    pub body_text: Option<String>,
    pub body_html: Option<String>,
    pub attachments: Vec<AttachmentMeta>,
    pub unread: bool,
    pub starred: bool,
    pub category: Option<Category>,
    pub analysis: Option<AiAnalysis>,
    /// When we ingested it (unix millis).
    pub received_at: i64,
}

/// Lightweight row for the message list (no bodies).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageHeader {
    pub id: String,
    pub account_id: String,
    pub folder: String,
    pub subject: String,
    pub from_name: String,
    pub from_addr: String,
    pub date: i64,
    pub snippet: String,
    pub unread: bool,
    pub starred: bool,
    pub has_attachments: bool,
    pub category: Option<Category>,
    pub verification_code: Option<String>,
    pub summary: Option<String>,
}

/// Query filter for the message list.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct MessageQuery {
    pub account_id: Option<String>,
    pub folder: Option<String>,
    pub category: Option<Category>,
    pub unread_only: bool,
    pub starred_only: bool,
    /// Substring search over subject / sender / snippet.
    pub search: Option<String>,
    pub limit: u32,
    pub offset: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessagePage {
    pub items: Vec<MessageHeader>,
    pub total: u32,
    pub unread: u32,
}

// ---------------------------------------------------------------------------
// AI settings
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AiSettings {
    pub enabled: bool,
    /// Which wire protocol `api_base` speaks.
    pub provider: AiProvider,
    /// Endpoint base, e.g. "https://api.openai.com/v1".
    pub api_base: String,
    pub api_key: String,
    pub model: String,
    pub temperature: f32,
    /// Allow the pipeline to hard-delete spam the model marked `deletable`.
    pub auto_delete_spam: bool,
    /// Extra user instructions appended to the triage prompt.
    pub extra_instructions: String,
}

impl AiSettings {
    /// Whether triage can actually call anything.
    ///
    /// The key is deliberately not part of this. A local endpoint — Ollama,
    /// vLLM, LM Studio, an in-house gateway — takes no credential at all, and
    /// requiring one meant every such user enabled the AI filter, saw no
    /// classification ever happen, and had nothing in the UI to explain why.
    /// A remote endpoint that needs a key answers 401, which surfaces as a
    /// visible per-account sync error instead of silence.
    pub fn is_configured(&self) -> bool {
        self.enabled && !self.api_base.trim().is_empty() && !self.model.trim().is_empty()
    }
}

impl Default for AiSettings {
    fn default() -> Self {
        AiSettings {
            enabled: false,
            provider: AiProvider::OpenaiCompatible,
            api_base: "https://api.openai.com/v1".to_string(),
            api_key: String::new(),
            model: "gpt-4o-mini".to_string(),
            temperature: 0.1,
            auto_delete_spam: false,
            extra_instructions: String::new(),
        }
    }
}

/// AI settings with the key redacted for the frontend.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiSettingsPublic {
    pub enabled: bool,
    pub provider: AiProvider,
    pub api_base: String,
    pub has_api_key: bool,
    pub model: String,
    pub temperature: f32,
    pub auto_delete_spam: bool,
    pub extra_instructions: String,
}

impl From<&AiSettings> for AiSettingsPublic {
    fn from(s: &AiSettings) -> Self {
        AiSettingsPublic {
            enabled: s.enabled,
            provider: s.provider,
            api_base: s.api_base.clone(),
            has_api_key: !s.api_key.is_empty(),
            model: s.model.clone(),
            temperature: s.temperature,
            auto_delete_spam: s.auto_delete_spam,
            extra_instructions: s.extra_instructions.clone(),
        }
    }
}

// ---------------------------------------------------------------------------
// Notification channels
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChannelKind {
    /// Telegram bot: config { botToken, chatId, apiBase? }
    Telegram,
    /// OneBot v11 HTTP endpoint (go-cqhttp / NapCat / Lagrange):
    /// config { apiBase, accessToken?, targetKind: "private"|"group", targetId }
    Qqbot,
    /// Generic JSON webhook: config { url, headers? (map), bodyTemplate? }
    Webhook,
    /// Bark (iOS push): config { server?, deviceKey }
    Bark,
}

impl ChannelKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            ChannelKind::Telegram => "telegram",
            ChannelKind::Qqbot => "qqbot",
            ChannelKind::Webhook => "webhook",
            ChannelKind::Bark => "bark",
        }
    }

    pub fn parse(s: &str) -> Option<ChannelKind> {
        match s {
            "telegram" => Some(ChannelKind::Telegram),
            "qqbot" => Some(ChannelKind::Qqbot),
            "webhook" => Some(ChannelKind::Webhook),
            "bark" => Some(ChannelKind::Bark),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NotifyChannel {
    pub id: String,
    pub name: String,
    pub kind: ChannelKind,
    pub enabled: bool,
    /// Which categories fan out to this channel (default: [Important]).
    pub notify_categories: Vec<Category>,
    /// Kind-specific configuration blob (see `ChannelKind` docs).
    pub config: serde_json::Value,
}

/// What gets pushed to an external channel.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NotifyPayload {
    pub category: Category,
    pub account_email: String,
    pub from: String,
    pub subject: String,
    pub summary: String,
    pub verification_code: Option<String>,
    /// Unix millis.
    pub date: i64,
}

// ---------------------------------------------------------------------------
// Sync / events
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SyncPhase {
    Idle,
    Connecting,
    Fetching,
    Classifying,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncStatus {
    pub account_id: String,
    pub phase: SyncPhase,
    /// New messages ingested during the current/most recent run.
    pub fetched: u32,
    pub error: Option<String>,
    /// Unix millis of last successful completion.
    pub last_ok_at: Option<i64>,
}

/// Emitted to the UI when a message deserves a popup.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AlertEvent {
    pub message_id: String,
    pub category: Category,
    pub account_email: String,
    pub from: String,
    pub subject: String,
    pub summary: String,
    pub verification_code: Option<String>,
}

/// What a delete attempt actually managed to do.
///
/// Deleting on the server is a network round trip that can fail, and the UI
/// hides the row before it starts. So it has to be told, per message, whether
/// the deletion stuck — a row it hid on a request the server refused has to
/// come back.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteReport {
    /// Gone: hidden locally, and removed on the server when that was asked for.
    pub deleted: Vec<String>,
    /// Left exactly as they were, because the server refused.
    pub failed: Vec<String>,
    /// Why, in one line the user can read. Set iff `failed` is non-empty.
    pub error: Option<String>,
}

impl DeleteReport {
    pub fn ok(&self) -> bool {
        self.failed.is_empty()
    }
}

/// Result of a connectivity test (account or channel).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TestResult {
    pub ok: bool,
    pub message: String,
}

/// Outgoing mail (compose form).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OutgoingMail {
    pub account_id: String,
    pub to: Vec<String>,
    pub subject: String,
    pub body: String,
    /// Message id being replied to, if any (sets In-Reply-To).
    pub in_reply_to: Option<String>,
}

// ---------------------------------------------------------------------------
// AI providers
// ---------------------------------------------------------------------------

/// Which wire protocol the configured endpoint speaks. The user picks this;
/// we do not sniff it, because a wrong guess fails in confusing ways.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AiProvider {
    /// `POST {base}/chat/completions` — OpenAI, DeepSeek, Ollama, vLLM, most gateways.
    OpenaiCompatible,
    /// `POST {base}/responses` — OpenAI's Responses API.
    OpenaiResponses,
    /// `POST {base}/v1/messages` with `x-api-key` + `anthropic-version`.
    Anthropic,
    /// `POST {base}/models/{model}:generateContent` with an `x-goog-api-key` header.
    Gemini,
}

impl AiProvider {
    /// Endpoint most users will want when they pick this provider.
    pub fn default_base(&self) -> &'static str {
        match self {
            AiProvider::OpenaiCompatible => "https://api.openai.com/v1",
            AiProvider::OpenaiResponses => "https://api.openai.com/v1",
            AiProvider::Anthropic => "https://api.anthropic.com",
            AiProvider::Gemini => "https://generativelanguage.googleapis.com/v1beta",
        }
    }

    pub fn default_model(&self) -> &'static str {
        match self {
            AiProvider::OpenaiCompatible => "gpt-4o-mini",
            AiProvider::OpenaiResponses => "gpt-4o-mini",
            AiProvider::Anthropic => "claude-sonnet-4-5",
            AiProvider::Gemini => "gemini-2.0-flash",
        }
    }
}

impl Default for AiProvider {
    fn default() -> Self {
        AiProvider::OpenaiCompatible
    }
}

/// Embedding endpoint used to build the RAG index over stored mail.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct EmbeddingSettings {
    pub enabled: bool,
    pub provider: AiProvider,
    pub api_base: String,
    pub api_key: String,
    pub model: String,
    /// Requested vector width; 0 means "whatever the model returns".
    pub dimensions: u32,
}

impl Default for EmbeddingSettings {
    fn default() -> Self {
        EmbeddingSettings {
            enabled: false,
            provider: AiProvider::OpenaiCompatible,
            api_base: "https://api.openai.com/v1".to_string(),
            api_key: String::new(),
            model: "text-embedding-3-small".to_string(),
            dimensions: 0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmbeddingSettingsPublic {
    pub enabled: bool,
    pub provider: AiProvider,
    pub api_base: String,
    pub has_api_key: bool,
    pub model: String,
    pub dimensions: u32,
}

impl From<&EmbeddingSettings> for EmbeddingSettingsPublic {
    fn from(s: &EmbeddingSettings) -> Self {
        EmbeddingSettingsPublic {
            enabled: s.enabled,
            provider: s.provider,
            api_base: s.api_base.clone(),
            has_api_key: !s.api_key.is_empty(),
            model: s.model.clone(),
            dimensions: s.dimensions,
        }
    }
}

/// How retrieved candidates get reordered before they reach the model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RerankerKind {
    /// Keep the embedding similarity order.
    None,
    /// `POST {base}/rerank` with `{model, query, documents}` returning
    /// `results[{index, relevance_score}]` — Jina, Cohere, Xinference, TEI.
    RerankApi,
    /// Ask the chat model to score each candidate. No extra service to run,
    /// but it costs one request per rerank.
    LlmScoring,
}

impl Default for RerankerKind {
    fn default() -> Self {
        RerankerKind::None
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RerankerSettings {
    pub kind: RerankerKind,
    pub api_base: String,
    pub api_key: String,
    pub model: String,
    /// Candidates fetched from the vector index before reranking.
    pub candidates: u32,
    /// Results kept after reranking.
    pub top_n: u32,
}

impl Default for RerankerSettings {
    fn default() -> Self {
        RerankerSettings {
            kind: RerankerKind::None,
            api_base: "https://api.jina.ai/v1".to_string(),
            api_key: String::new(),
            model: "jina-reranker-v2-base-multilingual".to_string(),
            candidates: 40,
            top_n: 8,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RerankerSettingsPublic {
    pub kind: RerankerKind,
    pub api_base: String,
    pub has_api_key: bool,
    pub model: String,
    pub candidates: u32,
    pub top_n: u32,
}

impl From<&RerankerSettings> for RerankerSettingsPublic {
    fn from(s: &RerankerSettings) -> Self {
        RerankerSettingsPublic {
            kind: s.kind,
            api_base: s.api_base.clone(),
            has_api_key: !s.api_key.is_empty(),
            model: s.model.clone(),
            candidates: s.candidates,
            top_n: s.top_n,
        }
    }
}

// ---------------------------------------------------------------------------
// Retrieval
// ---------------------------------------------------------------------------

/// One message the retriever considers relevant to a question.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchHit {
    pub message_id: String,
    pub account_id: String,
    pub subject: String,
    pub from_name: String,
    pub from_addr: String,
    pub date: i64,
    /// Excerpt that matched, already trimmed for display.
    pub excerpt: String,
    /// Higher is better. Comparable only within one result set.
    pub score: f32,
}

/// Progress of the embedding index, for the settings screen.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexStatus {
    pub indexed: u32,
    pub total: u32,
    /// Starred messages whose whole body has been chunked and embedded.
    pub deep_indexed: u32,
    /// Starred messages, i.e. how many the deep index is working toward.
    pub deep_total: u32,
    pub model: String,
    /// Set while a backfill is running.
    pub building: bool,
    pub error: Option<String>,
}

// ---------------------------------------------------------------------------
// Trackers
// ---------------------------------------------------------------------------

/// Why one remote reference in a mail is worth naming.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TrackerKind {
    /// A host whose business is knowing you opened the mail.
    Known,
    /// An image nobody is meant to see: 1×1, zero-sized, or hidden.
    Pixel,
    /// An ordinary remote resource. Still a request that reports the open, which
    /// is why it is blocked — but there is no evidence it was put there to.
    Remote,
}

impl TrackerKind {
    /// True for the two kinds that are actually tracking, as opposed to merely
    /// remote. What the counts and the heatmap are about.
    pub fn is_tracker(self) -> bool {
        matches!(self, TrackerKind::Known | TrackerKind::Pixel)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            TrackerKind::Known => "known",
            TrackerKind::Pixel => "pixel",
            TrackerKind::Remote => "remote",
        }
    }

    pub fn parse(s: &str) -> TrackerKind {
        match s {
            "known" => TrackerKind::Known,
            "pixel" => TrackerKind::Pixel,
            _ => TrackerKind::Remote,
        }
    }
}

/// One host a message wanted to reach, and how many times.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackerHit {
    pub host: String,
    pub kind: TrackerKind,
    pub count: u32,
}

/// One day of the heatmap.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackerDay {
    /// `YYYY-MM-DD`, local time — the day the user would call it.
    pub day: String,
    /// Requests blocked that day, counting only the tracking kinds.
    pub blocked: u32,
    /// Messages that carried at least one.
    pub messages: u32,
}

/// The privacy summary the settings screen shows.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackerStats {
    pub days: Vec<TrackerDay>,
    /// The worst offenders over the same window, most requests first.
    pub top: Vec<TrackerHit>,
    /// Totals over the window.
    pub blocked: u32,
    pub messages: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PrivacySettings {
    /// Refuse remote content in mail until the user asks for it, per message.
    /// On by default: a mail client that phones home for every message it shows
    /// is the behaviour being fixed, not a preference.
    pub block_trackers: bool,
}

impl Default for PrivacySettings {
    fn default() -> Self {
        PrivacySettings { block_trackers: true }
    }
}

// ---------------------------------------------------------------------------
// MCP (outbound: servers this app connects to as a client)
// ---------------------------------------------------------------------------

/// How to reach one MCP server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum McpTransport {
    /// Streamable HTTP: one URL, POST per request, answers in JSON or SSE.
    Http,
    /// A local process speaking newline-delimited JSON over stdin/stdout.
    Stdio,
}

impl Default for McpTransport {
    fn default() -> Self {
        McpTransport::Http
    }
}

/// How a secret reaches an HTTP MCP server. There is no standard: GitHub wants
/// a bearer token, Exa wants `x-api-key`, and a server behind a gateway may want
/// neither.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum McpAuth {
    None,
    /// `Authorization: Bearer <key>`
    Bearer,
    /// `x-api-key: <key>`
    ApiKeyHeader,
}

impl Default for McpAuth {
    fn default() -> Self {
        McpAuth::None
    }
}

/// One external MCP server the user configured.
///
/// The `name` is not decoration: it is the namespace the server's tools are
/// offered to the model under, so two servers with a `search` tool stay
/// distinguishable.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct McpServerConfig {
    pub id: String,
    pub name: String,
    pub transport: McpTransport,
    /// HTTP only. The full endpoint, e.g. `https://mcp.exa.ai/mcp`.
    pub url: String,
    /// HTTP only.
    pub auth: McpAuth,
    /// HTTP only. Never leaves the machine except as the header `auth` names.
    pub api_key: String,
    /// stdio only. The executable to run.
    pub command: String,
    /// stdio only.
    pub args: Vec<String>,
    /// stdio only. Added to the inherited environment.
    pub env: std::collections::BTreeMap<String, String>,
    pub enabled: bool,
}

impl Default for McpServerConfig {
    fn default() -> Self {
        McpServerConfig {
            id: String::new(),
            name: String::new(),
            transport: McpTransport::Http,
            url: String::new(),
            auth: McpAuth::None,
            api_key: String::new(),
            command: String::new(),
            args: Vec::new(),
            env: std::collections::BTreeMap::new(),
            enabled: true,
        }
    }
}

/// A server config as the UI sees it: the key is replaced by whether there is
/// one, exactly as the AI and reranker settings do.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServerPublic {
    pub id: String,
    pub name: String,
    pub transport: McpTransport,
    pub url: String,
    pub auth: McpAuth,
    pub has_api_key: bool,
    pub command: String,
    pub args: Vec<String>,
    pub env: std::collections::BTreeMap<String, String>,
    pub enabled: bool,
}

impl From<&McpServerConfig> for McpServerPublic {
    fn from(s: &McpServerConfig) -> Self {
        McpServerPublic {
            id: s.id.clone(),
            name: s.name.clone(),
            transport: s.transport,
            url: s.url.clone(),
            auth: s.auth,
            has_api_key: !s.api_key.is_empty(),
            command: s.command.clone(),
            args: s.args.clone(),
            env: s.env.clone(),
            enabled: s.enabled,
        }
    }
}

/// What one server is currently good for, for the settings screen.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServerStatus {
    pub id: String,
    /// The name the server calls itself, which is often not the one the user
    /// typed.
    pub server_name: String,
    pub server_version: String,
    pub protocol_version: String,
    /// Tools the assistant can now call, fully qualified.
    pub tools: Vec<McpToolInfo>,
    /// Why the last attempt failed, if it did.
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpToolInfo {
    /// The name the model calls, e.g. `mcp__exa__web_search_exa`.
    pub name: String,
    /// The name on the server.
    pub remote_name: String,
    pub description: String,
}

// ---------------------------------------------------------------------------
// Memory
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MemoryKind {
    /// How the user wants the assistant to behave.
    Preference,
    /// Something durable about the user or their mail.
    Fact,
    /// Who someone is, so "给老王发封邮件" resolves to an address.
    Contact,
}

/// Whether a memory still describes the user.
///
/// A retired memory is kept, not deleted: an email client has to be able to show
/// why it believed something, and the user has to be able to see that a
/// preference changed rather than finding it silently gone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MemoryStatus {
    /// Injected into prompts and searched.
    Active,
    /// Replaced by something newer. History only.
    Superseded,
}

impl Default for MemoryStatus {
    fn default() -> Self {
        MemoryStatus::Active
    }
}

/// Who wrote a memory. The reconciler may retire what it wrote itself, but it
/// may never overwrite what the user typed by hand.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MemoryOrigin {
    User,
    Assistant,
}

impl Default for MemoryOrigin {
    fn default() -> Self {
        MemoryOrigin::Assistant
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct MemoryEntry {
    pub id: String,
    pub kind: MemoryKind,
    pub text: String,
    /// Where it came from — a message id, or "assistant" when inferred.
    pub source: Option<String>,
    pub status: MemoryStatus,
    pub origin: MemoryOrigin,
    /// The id that replaced this one, when it was superseded.
    pub superseded_by: Option<String>,
    /// When what this says started being true, as far as we know. Distinct from
    /// `created_at`, which is when we came to believe it.
    pub valid_from: Option<i64>,
    /// When it stopped being true. `None` on an active memory.
    pub valid_to: Option<i64>,
    /// How many answers this has been injected into. The eviction signal.
    pub use_count: u32,
    pub created_at: i64,
    pub updated_at: i64,
}

impl Default for MemoryEntry {
    fn default() -> Self {
        MemoryEntry {
            id: String::new(),
            kind: MemoryKind::Fact,
            text: String::new(),
            source: None,
            status: MemoryStatus::Active,
            origin: MemoryOrigin::Assistant,
            superseded_by: None,
            valid_from: None,
            valid_to: None,
            use_count: 0,
            created_at: 0,
            updated_at: 0,
        }
    }
}

/// One thing that happened to one memory, for the audit trail.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryEvent {
    pub id: String,
    pub memory_id: String,
    /// `add` / `update` / `supersede` / `noop` / `delete`.
    pub op: String,
    pub before_text: Option<String>,
    pub after_text: Option<String>,
    /// The reconciler's own short justification, when a model made the call.
    pub reason: Option<String>,
    pub created_at: i64,
}

// ---------------------------------------------------------------------------
// Assistant
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChatRole {
    User,
    Assistant,
    /// A tool result folded back into the transcript.
    Tool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatTurn {
    pub id: String,
    pub conversation_id: String,
    pub role: ChatRole,
    pub content: String,
    /// The model's chain of thought, when it emitted one. Shown collapsed:
    /// useful for judging an answer, not something to read every time.
    #[serde(default)]
    pub reasoning: Option<String>,
    /// Tool invocations this turn made, for the UI to show its work.
    #[serde(default)]
    pub tool_calls: Vec<ToolCallRecord>,
    /// Messages the answer drew on.
    #[serde(default)]
    pub citations: Vec<SearchHit>,
    pub created_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallRecord {
    pub name: String,
    /// Arguments as JSON, for display and debugging.
    pub arguments: serde_json::Value,
    /// Short human-readable outcome, never the full payload.
    pub summary: String,
    pub ok: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Conversation {
    pub id: String,
    pub title: String,
    pub created_at: i64,
    pub updated_at: i64,
}

/// What the assistant returns for one user message.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantReply {
    pub turn: ChatTurn,
    /// True when the model asked to send mail and we are waiting for the user
    /// to confirm — sending is never done on the model's word alone.
    pub pending_confirmation: Option<PendingAction>,
}

/// An action the assistant proposes but will not perform unconfirmed.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingAction {
    pub id: String,
    pub kind: String,
    /// Rendered for the user to read before approving.
    pub description: String,
    pub payload: serde_json::Value,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A local model takes no API key. Treating a missing key as "not
    /// configured" is what made the AI filter a silent no-op against Ollama.
    #[test]
    fn a_local_endpoint_without_a_key_counts_as_configured() {
        let mut s = AiSettings {
            enabled: true,
            api_base: "http://127.0.0.1:11434/v1".into(),
            model: "qwen2.5:7b".into(),
            api_key: String::new(),
            ..AiSettings::default()
        };
        assert!(s.is_configured());

        s.enabled = false;
        assert!(!s.is_configured(), "the switch still decides");

        s.enabled = true;
        s.model = "  ".into();
        assert!(!s.is_configured(), "no model to call");

        s.model = "qwen2.5:7b".into();
        s.api_base = String::new();
        assert!(!s.is_configured(), "no endpoint to call");
    }

    #[test]
    fn the_default_settings_are_not_configured() {
        assert!(!AiSettings::default().is_configured());
    }
}
