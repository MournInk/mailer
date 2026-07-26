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
    pub model: String,
    /// Set while a backfill is running.
    pub building: bool,
    pub error: Option<String>,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryEntry {
    pub id: String,
    pub kind: MemoryKind,
    pub text: String,
    /// Where it came from — a message id, or "assistant" when inferred.
    pub source: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
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
