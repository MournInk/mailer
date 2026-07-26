//! Tauri IPC commands. Thin plumbing over the core engine; every command
//! returns `Result<T, String>` so the frontend gets readable error text.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use mailer_core::mail::{imap, pop3, smtp};
use mailer_core::sync::{now_ms, SyncEngine};
use mailer_core::types::*;
use mailer_core::{ai, assistant, mcp, memory, notify, rag};
use serde::Deserialize;
use tauri::{AppHandle, Emitter, State};

use crate::AppState;

type CmdResult<T> = Result<T, String>;

/// Messages embedded per backfill round trip. Small enough that a failure costs
/// one request, large enough that a 5000-mail mailbox is not 5000 round trips.
const INDEX_BATCH: u32 = 32;
/// Starred messages deep-indexed per round trip. Smaller than `INDEX_BATCH`
/// because each one is many chunks, not one vector.
const DEEP_BATCH: u32 = 6;
/// Conversations listed when the caller does not ask for a number.
const DEFAULT_CONVERSATIONS: u32 = 100;
const MAX_CONVERSATIONS: u32 = 500;
/// Drafts held in memory awaiting the user's approval.
const MAX_PENDING: usize = 32;
/// Progress of the embedding backfill, pushed as it runs.
const INDEX_EVENT: &str = "mailer://index-status";
/// Fragments of an assistant answer, pushed as the model writes them.
const ASSISTANT_DELTA_EVENT: &str = "mailer://assistant-delta";

/// Which OS the shell is running on, so the frontend knows whether to draw its
/// own window controls. Windows and Linux run undecorated and get ours; macOS
/// keeps its traffic lights, and mobile has no window chrome at all.
#[tauri::command]
pub fn host_platform() -> &'static str {
    if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "android") {
        "android"
    } else if cfg!(target_os = "ios") {
        "ios"
    } else {
        "linux"
    }
}

fn err_str<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

// ---------------------------------------------------------------------------
// Accounts
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SmtpInput {
    pub host: String,
    pub port: u16,
    pub username: String,
    /// Empty/None keeps the stored password on update.
    pub password: Option<String>,
    pub tls: TlsMode,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountInput {
    /// None → create; Some → update.
    pub id: Option<String>,
    pub label: String,
    pub email: String,
    pub protocol: Protocol,
    pub host: String,
    pub port: u16,
    pub username: String,
    /// Empty/None keeps the stored password on update.
    pub password: Option<String>,
    pub tls: TlsMode,
    pub smtp: Option<SmtpInput>,
    pub sync_interval_secs: u64,
    pub color_hue: u16,
}

/// Merge form input with any stored secrets into a full AccountConfig.
fn resolve_account(state: &AppState, input: AccountInput) -> CmdResult<AccountConfig> {
    let existing = match &input.id {
        Some(id) => state.engine.store().get_account(id).ok(),
        None => None,
    };

    let password = match input.password.filter(|p| !p.is_empty()) {
        Some(p) => p,
        None => existing
            .as_ref()
            .map(|a| a.password.clone())
            .ok_or_else(|| "请填写密码 / 授权码".to_string())?,
    };

    let smtp = match input.smtp {
        Some(s) => {
            let smtp_password = match s.password.filter(|p| !p.is_empty()) {
                Some(p) => p,
                None => existing
                    .as_ref()
                    .and_then(|a| a.smtp.as_ref())
                    .map(|old| old.password.clone())
                    .unwrap_or_else(|| password.clone()),
            };
            Some(SmtpConfig {
                host: s.host,
                port: s.port,
                username: s.username,
                password: smtp_password,
                tls: s.tls,
            })
        }
        None => None,
    };

    Ok(AccountConfig {
        id: input
            .id
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
        label: if input.label.trim().is_empty() {
            input.email.clone()
        } else {
            input.label
        },
        email: input.email,
        protocol: input.protocol,
        host: input.host,
        port: input.port,
        username: input.username,
        password,
        tls: input.tls,
        smtp,
        sync_interval_secs: input.sync_interval_secs,
        color_hue: input.color_hue,
        created_at: existing.map(|a| a.created_at).unwrap_or_else(now_ms),
    })
}

#[tauri::command]
pub fn list_accounts(state: State<'_, AppState>) -> CmdResult<Vec<AccountPublic>> {
    let accounts = state.engine.store().list_accounts().map_err(err_str)?;
    Ok(accounts.iter().map(AccountPublic::from).collect())
}

#[tauri::command]
pub async fn save_account(
    state: State<'_, AppState>,
    input: AccountInput,
) -> CmdResult<AccountPublic> {
    let is_update = input.id.is_some()
        && state
            .engine
            .store()
            .get_account(input.id.as_deref().unwrap_or_default())
            .is_ok();
    let account = resolve_account(&state, input)?;
    if is_update {
        state.engine.store().update_account(&account).map_err(err_str)?;
    } else {
        state.engine.store().insert_account(&account).map_err(err_str)?;
        // Kick off the first sync right away so the inbox fills up.
        let engine = state.engine.clone();
        let id = account.id.clone();
        tauri::async_runtime::spawn(async move {
            let _ = engine.sync_account(&id).await;
        });
    }
    Ok(AccountPublic::from(&account))
}

#[tauri::command]
pub fn delete_account(state: State<'_, AppState>, id: String) -> CmdResult<()> {
    state.engine.store().delete_account(&id).map_err(err_str)
}

/// Try to connect + authenticate with the given settings (before saving).
#[tauri::command]
pub async fn test_account(
    state: State<'_, AppState>,
    input: AccountInput,
) -> CmdResult<TestResult> {
    let account = resolve_account(&state, input)?;
    let recv = match account.protocol {
        Protocol::Imap => imap::check(&account).await,
        Protocol::Pop3 => pop3::check(&account).await,
    };
    let mut lines = vec![match &recv {
        Ok(()) => format!("✓ {} 连接与登录成功", protocol_label(account.protocol)),
        Err(e) => format!("✗ {} 失败: {e}", protocol_label(account.protocol)),
    }];
    let mut ok = recv.is_ok();

    if let Some(smtp_cfg) = &account.smtp {
        match smtp::check(smtp_cfg, &account.email).await {
            Ok(r) => {
                if !r.ok {
                    ok = false;
                }
                lines.push(if r.ok {
                    "✓ SMTP 连接与登录成功".to_string()
                } else {
                    format!("✗ SMTP 失败: {}", r.message)
                });
            }
            Err(e) => {
                ok = false;
                lines.push(format!("✗ SMTP 失败: {e}"));
            }
        }
    }

    Ok(TestResult { ok, message: lines.join("\n") })
}

fn protocol_label(p: Protocol) -> &'static str {
    match p {
        Protocol::Imap => "IMAP",
        Protocol::Pop3 => "POP3",
    }
}

// ---------------------------------------------------------------------------
// Messages
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn list_messages(state: State<'_, AppState>, query: MessageQuery) -> CmdResult<MessagePage> {
    let store = state.engine.store();
    // Grouping is the stored preference, not something the caller gets to
    // decide per request. The window reads the same setting to know how to
    // render a row, and if the two could disagree the list would show
    // collapsed counts on rows that are not collapsed.
    let group_threads = store.reading_settings().map(|s| s.group_threads).unwrap_or(true);
    store.query_messages(&MessageQuery { group_threads, ..query }).map_err(err_str)
}

#[tauri::command]
pub fn get_message(state: State<'_, AppState>, id: String) -> CmdResult<EmailMessage> {
    state.engine.store().get_message(&id).map_err(err_str)
}

#[tauri::command]
pub fn mark_read(state: State<'_, AppState>, ids: Vec<String>, read: bool) -> CmdResult<()> {
    state.engine.store().set_read(&ids, read).map_err(err_str)
}

#[tauri::command]
pub fn set_starred(state: State<'_, AppState>, id: String, starred: bool) -> CmdResult<()> {
    state.engine.store().set_starred(&id, starred).map_err(err_str)
}

/// Star or unstar a whole selection.
#[tauri::command]
pub fn set_starred_many(
    state: State<'_, AppState>,
    ids: Vec<String>,
    starred: bool,
) -> CmdResult<()> {
    state.engine.store().set_starred_many(&ids, starred).map_err(err_str)
}

/// Delete messages, reporting what actually went.
///
/// The list hides the rows before this is called, so the report is how it learns
/// which ones to put back: a server that refused the delete still has the mail,
/// and pretending otherwise would make it reappear at the next sync as if from
/// nowhere.
#[tauri::command]
pub async fn delete_messages(
    state: State<'_, AppState>,
    ids: Vec<String>,
    on_server: bool,
) -> CmdResult<DeleteReport> {
    Ok(state.engine.delete_messages(&ids, on_server).await)
}

/// Trigger sync for one account, or all accounts when `account_id` is None.
#[tauri::command]
pub async fn sync_now(
    state: State<'_, AppState>,
    account_id: Option<String>,
) -> CmdResult<()> {
    let engine = state.engine.clone();
    let ids: Vec<String> = match account_id {
        Some(id) => vec![id],
        None => engine
            .store()
            .list_accounts()
            .map_err(err_str)?
            .into_iter()
            .map(|a| a.id)
            .collect(),
    };
    for id in ids {
        let engine = engine.clone();
        tauri::async_runtime::spawn(async move {
            let _ = engine.sync_account(&id).await;
        });
    }
    Ok(())
}

#[tauri::command]
pub fn sync_statuses(state: State<'_, AppState>) -> CmdResult<Vec<SyncStatus>> {
    Ok(state.engine.statuses())
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CategoryCount {
    pub category: String,
    pub total: u32,
    pub unread: u32,
}

#[tauri::command]
pub fn category_counts(state: State<'_, AppState>) -> CmdResult<Vec<CategoryCount>> {
    let rows = state.engine.store().category_counts().map_err(err_str)?;
    Ok(rows
        .into_iter()
        .map(|(category, total, unread)| CategoryCount { category, total, unread })
        .collect())
}

// ---------------------------------------------------------------------------
// AI settings
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiSettingsInput {
    pub enabled: bool,
    pub provider: AiProvider,
    pub api_base: String,
    /// Empty/None keeps the stored key.
    pub api_key: Option<String>,
    pub model: String,
    pub temperature: f32,
    pub auto_delete_spam: bool,
    pub extra_instructions: String,
}

/// Endpoints are stored without a trailing slash: every caller appends its own
/// path, and `.../v1//chat/completions` is rejected by some gateways.
fn clean_base(base: &str) -> String {
    base.trim().trim_end_matches('/').to_string()
}

/// A blank key field means "keep what is stored" — the frontend never receives
/// the key back, so it has nothing to send us on a plain edit of the model name.
fn keep_secret(input: Option<String>, stored: String) -> String {
    input.filter(|k| !k.is_empty()).unwrap_or(stored)
}

fn merge_ai(old: AiSettings, input: AiSettingsInput) -> AiSettings {
    AiSettings {
        enabled: input.enabled,
        provider: input.provider,
        api_base: clean_base(&input.api_base),
        api_key: keep_secret(input.api_key, old.api_key),
        model: input.model.trim().to_string(),
        temperature: input.temperature.clamp(0.0, 2.0),
        auto_delete_spam: input.auto_delete_spam,
        extra_instructions: input.extra_instructions,
    }
}

#[tauri::command]
pub fn get_ai_settings(state: State<'_, AppState>) -> CmdResult<AiSettingsPublic> {
    let s = state.engine.store().ai_settings().map_err(err_str)?;
    Ok(AiSettingsPublic::from(&s))
}

#[tauri::command]
pub fn set_ai_settings(state: State<'_, AppState>, input: AiSettingsInput) -> CmdResult<AiSettingsPublic> {
    let store = state.engine.store();
    let s = merge_ai(store.ai_settings().map_err(err_str)?, input);
    store.set_ai_settings(&s).map_err(err_str)?;
    Ok(AiSettingsPublic::from(&s))
}

#[tauri::command]
pub async fn test_ai(state: State<'_, AppState>) -> CmdResult<TestResult> {
    // No key check: a local endpoint (Ollama, vLLM, LM Studio) takes none, and
    // refusing to probe one left those users with no way to test their setup.
    // `ai::test` validates the base URL and model, and a remote endpoint that
    // does need a key answers 401 with a message worth reading.
    let settings = state.engine.store().ai_settings().map_err(err_str)?;
    Ok(ai::test(state.engine.http(), &settings).await)
}

#[tauri::command]
pub async fn reclassify(state: State<'_, AppState>, message_id: String) -> CmdResult<AiAnalysis> {
    state.engine.reclassify(&message_id).await.map_err(err_str)
}

// ---------------------------------------------------------------------------
// Notification channels
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn list_channels(state: State<'_, AppState>) -> CmdResult<Vec<NotifyChannel>> {
    state.engine.store().list_channels().map_err(err_str)
}

#[tauri::command]
pub fn save_channel(state: State<'_, AppState>, mut channel: NotifyChannel) -> CmdResult<NotifyChannel> {
    if channel.id.is_empty() {
        channel.id = uuid::Uuid::new_v4().to_string();
    }
    if channel.notify_categories.is_empty() {
        channel.notify_categories = vec![Category::Important];
    }
    state.engine.store().upsert_channel(&channel).map_err(err_str)?;
    Ok(channel)
}

#[tauri::command]
pub fn delete_channel(state: State<'_, AppState>, id: String) -> CmdResult<()> {
    state.engine.store().delete_channel(&id).map_err(err_str)
}

#[tauri::command]
pub async fn test_channel(state: State<'_, AppState>, id: String) -> CmdResult<TestResult> {
    let channel = state.engine.store().get_channel(&id).map_err(err_str)?;
    Ok(notify::test(state.engine.http(), &channel).await)
}

// ---------------------------------------------------------------------------
// Sending
// ---------------------------------------------------------------------------

/// The one path mail takes out of this machine. Shared with
/// [`confirm_pending_action`], so an assistant draft the user approved is sent
/// exactly the way the compose window sends.
async fn send_outgoing(engine: &SyncEngine, mail: &OutgoingMail) -> CmdResult<()> {
    let store = engine.store();
    let account = store.get_account(&mail.account_id).map_err(err_str)?;
    // Resolve the RFC Message-ID of the message being replied to, if any.
    let in_reply_to = mail
        .in_reply_to
        .as_deref()
        .and_then(|mid| store.get_message(mid).ok())
        .and_then(|m| m.message_id);
    smtp::send(
        &account,
        &mail.to,
        &mail.cc,
        &mail.bcc,
        &mail.subject,
        &mail.body,
        in_reply_to.as_deref(),
    )
    .await
    .map_err(err_str)
}

#[tauri::command]
pub async fn send_mail(state: State<'_, AppState>, mail: OutgoingMail) -> CmdResult<()> {
    let engine = state.engine.clone();
    send_outgoing(&engine, &mail).await
}

// ---------------------------------------------------------------------------
// Embedding settings
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmbeddingSettingsInput {
    pub enabled: bool,
    pub provider: AiProvider,
    pub api_base: String,
    /// Empty/None keeps the stored key.
    pub api_key: Option<String>,
    pub model: String,
    pub dimensions: u32,
}

fn merge_embedding(old: EmbeddingSettings, input: EmbeddingSettingsInput) -> EmbeddingSettings {
    EmbeddingSettings {
        enabled: input.enabled,
        provider: input.provider,
        api_base: clean_base(&input.api_base),
        api_key: keep_secret(input.api_key, old.api_key),
        model: input.model.trim().to_string(),
        dimensions: input.dimensions,
    }
}

#[tauri::command]
pub fn get_embedding_settings(state: State<'_, AppState>) -> CmdResult<EmbeddingSettingsPublic> {
    let s = state.engine.store().embedding_settings().map_err(err_str)?;
    Ok(EmbeddingSettingsPublic::from(&s))
}

#[tauri::command]
pub fn set_embedding_settings(
    state: State<'_, AppState>,
    input: EmbeddingSettingsInput,
) -> CmdResult<EmbeddingSettingsPublic> {
    let store = state.engine.store();
    let s = merge_embedding(store.embedding_settings().map_err(err_str)?, input);
    store.set_embedding_settings(&s).map_err(err_str)?;
    Ok(EmbeddingSettingsPublic::from(&s))
}

/// Probe the embedding endpoint with one throwaway input.
///
/// Runs against the *stored* settings whether or not indexing is enabled: the
/// user tests a configuration in order to decide about switching it on.
#[tauri::command]
pub async fn test_embedding(state: State<'_, AppState>) -> CmdResult<TestResult> {
    let engine = state.engine.clone();
    let settings = engine.store().embedding_settings().map_err(err_str)?;
    let probe = ["连接测试".to_string()];
    // `rag` scrubs the key out of provider responses before they reach an error
    // string, so the message below is safe to show and to screenshot.
    match rag::embed(engine.http(), &settings, &probe).await {
        Ok(vectors) => {
            let dim = vectors.first().map(Vec::len).unwrap_or(0);
            Ok(if dim == 0 {
                TestResult { ok: false, message: "嵌入接口没有返回向量".into() }
            } else {
                TestResult {
                    ok: true,
                    message: format!(
                        "连接成功，模型 {} 返回 {dim} 维向量",
                        settings.model.trim()
                    ),
                }
            })
        }
        Err(e) => Ok(TestResult { ok: false, message: format!("连接失败: {e}") }),
    }
}

// ---------------------------------------------------------------------------
// Reranker settings
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RerankerSettingsInput {
    pub kind: RerankerKind,
    pub api_base: String,
    /// Empty/None keeps the stored key.
    pub api_key: Option<String>,
    pub model: String,
    pub candidates: u32,
    pub top_n: u32,
}

fn merge_reranker(old: RerankerSettings, input: RerankerSettingsInput) -> RerankerSettings {
    RerankerSettings {
        kind: input.kind,
        api_base: clean_base(&input.api_base),
        api_key: keep_secret(input.api_key, old.api_key),
        model: input.model.trim().to_string(),
        candidates: input.candidates,
        top_n: input.top_n,
    }
}

#[tauri::command]
pub fn get_reranker_settings(state: State<'_, AppState>) -> CmdResult<RerankerSettingsPublic> {
    let s = state.engine.store().reranker_settings().map_err(err_str)?;
    Ok(RerankerSettingsPublic::from(&s))
}

#[tauri::command]
pub fn set_reranker_settings(
    state: State<'_, AppState>,
    input: RerankerSettingsInput,
) -> CmdResult<RerankerSettingsPublic> {
    let store = state.engine.store();
    let s = merge_reranker(store.reranker_settings().map_err(err_str)?, input);
    store.set_reranker_settings(&s).map_err(err_str)?;
    Ok(RerankerSettingsPublic::from(&s))
}

// ---------------------------------------------------------------------------
// User-defined labels
// ---------------------------------------------------------------------------

/// A label is a name plus a sentence; the sentence is the whole feature, so it
/// is the one field with a real minimum.
const MIN_INSTRUCTION_CHARS: usize = 4;
/// Labels one mailbox may define. Each one is prompt text on every message that
/// arrives, so the ceiling is a cost ceiling, not a storage one.
const MAX_LABELS: usize = 20;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LabelInput {
    /// None → create.
    pub id: Option<String>,
    pub name: String,
    pub instruction: String,
    pub color_hue: u16,
    pub enabled: bool,
}

#[tauri::command]
pub fn list_labels(state: State<'_, AppState>) -> CmdResult<Vec<MailLabel>> {
    state.engine.store().list_labels().map_err(err_str)
}

#[tauri::command]
pub fn label_counts(state: State<'_, AppState>) -> CmdResult<Vec<LabelCount>> {
    state.engine.store().label_counts().map_err(err_str)
}

#[tauri::command]
pub fn save_label(state: State<'_, AppState>, input: LabelInput) -> CmdResult<Vec<MailLabel>> {
    let store = state.engine.store();
    let name = input.name.trim().to_string();
    let instruction = input.instruction.trim().to_string();
    if name.is_empty() {
        return Err("请给标签起一个名字".into());
    }
    if instruction.chars().count() < MIN_INSTRUCTION_CHARS {
        return Err("请描述一下什么样的邮件属于这个标签，模型要靠这句话判断".into());
    }

    let mut labels = store.list_labels().map_err(err_str)?;
    let existing = input
        .id
        .as_deref()
        .filter(|id| !id.is_empty())
        .and_then(|id| labels.iter().position(|l| l.id == id));
    if existing.is_none() && labels.len() >= MAX_LABELS {
        return Err(format!("最多 {MAX_LABELS} 个标签"));
    }
    // The name is what the model answers with, so two labels sharing one would
    // make the answer ambiguous and attach mail to both.
    if labels
        .iter()
        .enumerate()
        .any(|(i, l)| Some(i) != existing && l.name.trim().eq_ignore_ascii_case(&name))
    {
        return Err(format!("已经有一个叫「{name}」的标签了"));
    }

    let label = MailLabel {
        id: existing
            .map(|i| labels[i].id.clone())
            .unwrap_or_else(new_id),
        name,
        instruction,
        color_hue: input.color_hue.min(360),
        enabled: input.enabled,
        created_at: existing.map(|i| labels[i].created_at).unwrap_or_else(now_ms),
    };
    store.put_label(&label).map_err(err_str)?;
    match existing {
        Some(i) => labels[i] = label,
        None => labels.push(label),
    }
    Ok(labels)
}

#[tauri::command]
pub fn delete_label(state: State<'_, AppState>, id: String) -> CmdResult<Vec<MailLabel>> {
    let store = state.engine.store();
    store.delete_label(&id).map_err(err_str)?;
    store.list_labels().map_err(err_str)
}

// ---------------------------------------------------------------------------
// Trackers
// ---------------------------------------------------------------------------

/// Days the privacy screen draws. Ten weeks is enough for a pattern to be visible
/// and short enough to fit a settings card without scrolling sideways.
const TRACKER_DAYS: i64 = 70;
/// Hosts named in the "worst offenders" list.
const TRACKER_TOP: u32 = 8;
/// Messages scanned per pass during the backfill.
const TRACKER_BATCH: u32 = 200;

#[tauri::command]
pub fn get_privacy_settings(state: State<'_, AppState>) -> CmdResult<PrivacySettings> {
    state.engine.store().privacy_settings().map_err(err_str)
}

#[tauri::command]
pub fn set_privacy_settings(
    state: State<'_, AppState>,
    input: PrivacySettings,
) -> CmdResult<PrivacySettings> {
    let store = state.engine.store();
    store.set_privacy_settings(&input).map_err(err_str)?;
    Ok(input)
}

#[tauri::command]
pub fn get_reading_settings(state: State<'_, AppState>) -> CmdResult<ReadingSettings> {
    state.engine.store().reading_settings().map_err(err_str)
}

#[tauri::command]
pub fn set_reading_settings(
    state: State<'_, AppState>,
    input: ReadingSettings,
) -> CmdResult<ReadingSettings> {
    state.engine.store().set_reading_settings(&input).map_err(err_str)?;
    Ok(input)
}

/// How the compose window should be opened for a reply or a forward.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DraftPrefill {
    pub account_id: String,
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub subject: String,
    pub body: String,
    pub in_reply_to: Option<String>,
}

/// What replying to (or forwarding) one message should put in the composer.
///
/// Built here rather than in the window because the recipient set is the part
/// that can be silently wrong — see `mailer_core::reply`. `kind` is one of
/// `reply`, `reply_all`, `forward`.
#[tauri::command]
pub fn prepare_draft(
    state: State<'_, AppState>,
    id: String,
    kind: String,
    when: String,
) -> CmdResult<DraftPrefill> {
    let store = state.engine.store();
    let msg = store.get_message(&id).map_err(err_str)?;

    // Every address this user owns, across all accounts — a reply must not go
    // to another of their own mailboxes either.
    let mine: Vec<String> = store
        .list_accounts()
        .map_err(err_str)?
        .into_iter()
        .flat_map(|a| [a.email, a.username])
        .filter(|s| !s.trim().is_empty())
        .collect();

    let forwarding = kind == "forward";
    let recipients = if forwarding {
        mailer_core::reply::Recipients::default()
    } else {
        mailer_core::reply::reply_recipients(&msg, &mine, kind == "reply_all")
    };

    Ok(DraftPrefill {
        account_id: msg.account_id.clone(),
        to: recipients.to,
        cc: recipients.cc,
        subject: if forwarding {
            mailer_core::reply::forward_subject(&msg.subject)
        } else {
            mailer_core::reply::reply_subject(&msg.subject)
        },
        body: if forwarding {
            mailer_core::reply::forward_body(&msg, &when)
        } else {
            mailer_core::reply::reply_body(&msg, &when)
        },
        // A forward starts a new conversation; a reply continues one.
        in_reply_to: (!forwarding).then(|| msg.id.clone()),
    })
}

/// Mark a whole conversation read — what opening a collapsed row means.
#[tauri::command]
pub fn mark_thread_read(
    state: State<'_, AppState>,
    thread_id: String,
    read: bool,
) -> CmdResult<u32> {
    state.engine.store().set_thread_read(&thread_id, read).map_err(err_str)
}

/// Every message in one conversation, oldest first.
#[tauri::command]
pub fn thread_messages(state: State<'_, AppState>, thread_id: String) -> CmdResult<Vec<EmailMessage>> {
    state.engine.store().thread_messages(&thread_id).map_err(err_str)
}

/// What one message wanted to load from somebody else's server.
#[tauri::command]
pub fn message_trackers(state: State<'_, AppState>, id: String) -> CmdResult<Vec<TrackerHit>> {
    state.engine.store().trackers_for(&id).map_err(err_str)
}

/// The heatmap, the worst offenders, and the totals behind them.
///
/// Every day in the window is present whether or not anything happened on it: a
/// calendar with holes in it is not a calendar, and the store only returns the
/// days it has rows for.
#[tauri::command]
pub fn tracker_stats(state: State<'_, AppState>) -> CmdResult<TrackerStats> {
    let store = state.engine.store();
    let today = now_ms();
    let day_ms = 86_400_000i64;
    let since = mailer_core::sync::local_day(today - (TRACKER_DAYS - 1) * day_ms);

    let found = store.tracker_days(&since).map_err(err_str)?;
    let by_day: std::collections::HashMap<String, &TrackerDay> =
        found.iter().map(|d| (d.day.clone(), d)).collect();

    let mut days = Vec::with_capacity(TRACKER_DAYS as usize);
    for back in (0..TRACKER_DAYS).rev() {
        let day = mailer_core::sync::local_day(today - back * day_ms);
        days.push(match by_day.get(&day) {
            Some(d) => (*d).clone(),
            None => TrackerDay { day, blocked: 0, messages: 0 },
        });
    }

    Ok(TrackerStats {
        blocked: days.iter().map(|d| d.blocked).sum(),
        messages: days.iter().map(|d| d.messages).sum(),
        top: store.tracker_top(&since, TRACKER_TOP).map_err(err_str)?,
        days,
    })
}

/// Scan mail that arrived before the scanner did.
///
/// Same shape as the text-index backfill: local, bounded, yielding, and measured
/// rather than assumed so a message that cannot be scanned cannot loop.
pub async fn backfill_trackers(engine: std::sync::Arc<SyncEngine>) {
    let store = engine.store();
    loop {
        let batch = match store.messages_missing_trackers(TRACKER_BATCH) {
            Ok(batch) => batch,
            Err(e) => {
                tracing::warn!("tracker scan: 读取待扫描邮件失败: {e}");
                return;
            }
        };
        if batch.is_empty() {
            return;
        }
        tracing::info!("tracker scan: 正在扫描 {} 封旧邮件", batch.len());
        for msg in &batch {
            engine.scan_trackers(msg);
        }
        // `scan_trackers` swallows its own failures, so progress is checked here:
        // a batch that comes back unchanged would otherwise repeat forever.
        match store.messages_missing_trackers(1) {
            Ok(next) if next.first().map(|m| m.id.as_str()) == batch.first().map(|m| m.id.as_str()) => {
                tracing::warn!("tracker scan: 无法扫描 {}，已停止", batch[0].id);
                return;
            }
            Ok(_) => {}
            Err(_) => return,
        }
        tokio::task::yield_now().await;
    }
}

// ---------------------------------------------------------------------------
// Threading
// ---------------------------------------------------------------------------

/// Messages threaded per pass. Each one is a couple of indexed lookups, but the
/// batch holds the store mutex, so it stays short enough not to be felt.
const THREAD_BATCH: u32 = 300;

/// Thread whatever arrived before threading existed, then stop.
///
/// New mail is threaded on insert, so this only has work to do once — on the
/// first launch after the upgrade that added the columns. Bounded and measured
/// like the other backfills: a batch that fails to shrink means something in it
/// cannot be threaded, and repeating it forever would be worse than stopping.
pub async fn backfill_threads(engine: std::sync::Arc<SyncEngine>) {
    let store = engine.store();
    let mut left = match store.unthreaded_count() {
        Ok(0) => return,
        Ok(n) => n,
        Err(e) => {
            tracing::warn!("threading: 读取待处理邮件数失败: {e}");
            return;
        }
    };
    tracing::info!("threading: 正在整理 {left} 封旧邮件的会话");
    loop {
        match store.backfill_threads(THREAD_BATCH) {
            Ok(0) => return,
            Ok(_) => {}
            Err(e) => {
                tracing::warn!("threading: 整理失败: {e}");
                return;
            }
        }
        let now = match store.unthreaded_count() {
            Ok(0) => return,
            Ok(n) => n,
            Err(_) => return,
        };
        if now >= left {
            tracing::warn!("threading: 还剩 {now} 封无法整理，已停止");
            return;
        }
        left = now;
        tokio::task::yield_now().await;
    }
}

// ---------------------------------------------------------------------------
// Full-text index
// ---------------------------------------------------------------------------

/// Messages indexed per pass. The work is local and fast, but it holds the store
/// mutex for the batch, so it is kept short enough not to be felt in the UI.
const TEXT_BATCH: u32 = 200;

/// Index whatever the full-text index has not seen yet, then stop.
///
/// New mail is indexed as it lands, so this only ever has work to do on a
/// database written by a build that predates the index. Yields between batches:
/// a 20 000-message mailbox is a few seconds of work and the window has to stay
/// responsive through it.
pub async fn backfill_text_index(engine: std::sync::Arc<SyncEngine>) {
    let store = engine.store();
    let (indexed, total) = match store.fts_counts() {
        Ok(counts) => counts,
        Err(e) => {
            tracing::warn!("text index: 无法读取索引状态: {e}");
            return;
        }
    };
    if indexed >= total {
        return;
    }
    tracing::info!("text index: 正在补全 {} 封旧邮件的全文索引", total - indexed);

    let mut done = indexed;
    loop {
        let batch = match store.messages_missing_fts(TEXT_BATCH) {
            Ok(batch) => batch,
            Err(e) => {
                tracing::warn!("text index: 读取待索引邮件失败: {e}");
                return;
            }
        };
        if batch.is_empty() {
            break;
        }
        for msg in &batch {
            if let Err(e) = store.index_message_text(msg) {
                // One unindexable message must not take the pass down with it.
                tracing::warn!("text index: {} 索引失败: {e}", msg.id);
            }
        }

        // A message that fails stays "missing", so it comes back in the next
        // batch — without this the pass would retry it forever. Progress is
        // measured, not assumed.
        let progressed = match store.fts_counts() {
            Ok((now, _)) => {
                let moved = now > done;
                done = now;
                moved
            }
            Err(e) => {
                tracing::warn!("text index: 无法确认进度: {e}");
                false
            }
        };
        if !progressed {
            tracing::warn!("text index: 有 {} 封邮件无法索引，已跳过", batch.len());
            break;
        }
        tokio::task::yield_now().await;
    }
    tracing::info!("text index: 完成，{done}/{total}");
}

// ---------------------------------------------------------------------------
// MCP servers (outbound: what the assistant may borrow)
// ---------------------------------------------------------------------------

/// One server as the settings screen submits it.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServerInput {
    /// Empty for a new server.
    pub id: Option<String>,
    pub name: String,
    pub transport: McpTransport,
    pub url: String,
    pub auth: McpAuth,
    /// Empty/None keeps the stored key, exactly as the AI settings do.
    pub api_key: Option<String>,
    pub command: String,
    pub args: Vec<String>,
    pub env: std::collections::BTreeMap<String, String>,
    pub enabled: bool,
}

/// Refuse to send credentials over a cleartext link.
///
/// An authenticated `http://` endpoint hands the key to anyone on the path.
/// Loopback is exempt because there is no path — a local bridge is the normal
/// way to run one of these, and there is nothing to intercept.
fn insecure_endpoint(input: &McpServerInput) -> Option<String> {
    if input.transport != McpTransport::Http || input.auth == McpAuth::None {
        return None;
    }
    let url = input.url.trim().to_ascii_lowercase();
    let rest = url.strip_prefix("http://")?;
    // A bracketed IPv6 literal carries its own colons, so the port separator
    // cannot simply be the first one.
    let host = match rest.strip_prefix('[') {
        Some(v6) => v6.split(']').next().map(|h| format!("[{h}]")).unwrap_or_default(),
        None => rest.split(['/', ':', '?']).next().unwrap_or("").to_string(),
    };
    let host = host.as_str();
    let local = host == "localhost"
        || host == "127.0.0.1"
        || host == "[::1]"
        || host == "::1"
        || host.ends_with(".localhost");
    if local {
        return None;
    }
    Some(format!(
        "「{}」用 http:// 连接并且带了密钥，密钥会以明文经过网络。请改用 https://（本机地址除外）。",
        input.name.trim()
    ))
}

fn merge_mcp(old: Option<&McpServerConfig>, input: McpServerInput) -> McpServerConfig {
    McpServerConfig {
        id: input.id.filter(|id| !id.is_empty()).unwrap_or_else(new_id),
        name: input.name.trim().to_string(),
        transport: input.transport,
        url: input.url.trim().to_string(),
        auth: input.auth,
        api_key: keep_secret(
            input.api_key,
            old.map(|o| o.api_key.clone()).unwrap_or_default(),
        ),
        command: input.command.trim().to_string(),
        args: input.args.into_iter().map(|a| a.trim().to_string()).filter(|a| !a.is_empty()).collect(),
        // Same rule as `api_key`: a blank value means "keep what is stored",
        // because that is all the form was ever shown. A name the user removed
        // is still removed — only values present-but-empty are restored.
        env: {
            let stored = old.map(|o| o.env.clone()).unwrap_or_default();
            input
                .env
                .into_iter()
                .map(|(k, v)| {
                    if v.is_empty() {
                        let kept = stored.get(&k).cloned().unwrap_or_default();
                        (k, kept)
                    } else {
                        (k, v)
                    }
                })
                .collect()
        },
        enabled: input.enabled,
    }
}

#[tauri::command]
pub fn get_mcp_servers(state: State<'_, AppState>) -> CmdResult<Vec<McpServerPublic>> {
    let servers = state.engine.store().mcp_servers().map_err(err_str)?;
    Ok(servers.iter().map(McpServerPublic::from).collect())
}

/// Add or update one server, then forget its session so the next question
/// connects with what was just saved.
#[tauri::command]
pub async fn save_mcp_server(
    state: State<'_, AppState>,
    input: McpServerInput,
) -> CmdResult<Vec<McpServerPublic>> {
    if input.name.trim().is_empty() {
        return Err("请给这个服务器起一个名字".into());
    }
    if let Some(why) = insecure_endpoint(&input) {
        return Err(why);
    }
    let store = state.engine.store();
    let mut servers = store.mcp_servers().map_err(err_str)?;
    let existing = input
        .id
        .as_deref()
        .filter(|id| !id.is_empty())
        .and_then(|id| servers.iter().position(|s| s.id == id));
    let merged = merge_mcp(existing.map(|i| &servers[i]), input);

    let id = merged.id.clone();
    match existing {
        Some(i) => servers[i] = merged,
        None => servers.push(merged),
    }
    store.set_mcp_servers(&servers).map_err(err_str)?;
    // The old session still speaks to the old URL with the old key.
    mcp::hub().forget(&id).await;
    Ok(servers.iter().map(McpServerPublic::from).collect())
}

#[tauri::command]
pub async fn delete_mcp_server(
    state: State<'_, AppState>,
    id: String,
) -> CmdResult<Vec<McpServerPublic>> {
    let store = state.engine.store();
    let mut servers = store.mcp_servers().map_err(err_str)?;
    servers.retain(|s| s.id != id);
    store.set_mcp_servers(&servers).map_err(err_str)?;
    mcp::hub().forget(&id).await;
    Ok(servers.iter().map(McpServerPublic::from).collect())
}

/// Connect to every enabled server and report what each one offers.
///
/// This is the settings screen's "test" button and its status list at once:
/// there is nothing to test about an MCP server except whether it connects and
/// what tools it has.
#[tauri::command]
pub async fn mcp_status(state: State<'_, AppState>) -> CmdResult<Vec<McpServerStatus>> {
    let engine = state.engine.clone();
    Ok(mcp::hub().status(engine.store(), engine.http()).await)
}

/// Drop every cached session, so the next call reconnects from scratch.
#[tauri::command]
pub async fn reconnect_mcp(state: State<'_, AppState>) -> CmdResult<Vec<McpServerStatus>> {
    let engine = state.engine.clone();
    mcp::hub().forget_all().await;
    Ok(mcp::hub().status(engine.store(), engine.http()).await)
}

// ---------------------------------------------------------------------------
// Embedding index
// ---------------------------------------------------------------------------

/// Backfill bookkeeping. `rag::status` reports what the database holds; whether
/// a run is in flight is only knowable here, where the task is owned.
#[derive(Default)]
pub struct IndexTask {
    building: AtomicBool,
    /// Why the last run stopped early. Cleared when a new one starts.
    last_error: Mutex<Option<String>>,
}

impl IndexTask {
    fn last_error(&self) -> Option<String> {
        self.last_error.lock().expect("index mutex poisoned").clone()
    }

    fn set_last_error(&self, error: Option<String>) {
        *self.last_error.lock().expect("index mutex poisoned") = error;
    }
}

/// A run that died halfway is more useful to the user than the configuration
/// hint `rag::status` supplies, so it wins when both are present.
fn overlay(mut status: IndexStatus, building: bool, last_error: Option<String>) -> IndexStatus {
    status.building = building;
    if last_error.is_some() {
        status.error = last_error;
    }
    status
}

fn current_status(engine: &SyncEngine, index: &IndexTask) -> CmdResult<IndexStatus> {
    let settings = engine.store().embedding_settings().map_err(err_str)?;
    let status = rag::status(engine.store(), &settings).map_err(err_str)?;
    Ok(overlay(
        status,
        index.building.load(Ordering::SeqCst),
        index.last_error(),
    ))
}

#[tauri::command]
pub fn index_status(state: State<'_, AppState>) -> CmdResult<IndexStatus> {
    current_status(&state.engine, &state.index)
}

/// Start the embedding backfill, or report the run already under way.
///
/// Returns immediately: embedding a large mailbox takes minutes, and the UI
/// follows along through `mailer://index-status` instead of waiting on a call.
#[tauri::command]
pub fn index_pending(app: AppHandle, state: State<'_, AppState>) -> CmdResult<IndexStatus> {
    let engine = state.engine.clone();
    let index = state.index.clone();
    let settings = engine.store().embedding_settings().map_err(err_str)?;
    if !settings.enabled {
        return Err("尚未启用邮件索引，请先在设置中配置嵌入模型".to_string());
    }

    // A second click while the first run is in flight would re-embed the same
    // rows and bill the user for them twice.
    if index
        .building
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return current_status(&engine, &index);
    }
    index.set_last_error(None);

    tauri::async_runtime::spawn(async move {
        // The settings are the ones read above, on purpose: changing the model
        // mid-run would mix two vector spaces into one index.
        loop {
            match rag::index_pending(engine.store(), engine.http(), &settings, INDEX_BATCH).await {
                // Nothing left to embed — or nothing embeddable, which the next
                // run will find again rather than spinning on it here.
                Ok(0) => break,
                Ok(n) => {
                    tracing::debug!("index: embedded {n} message(s)");
                    emit_index_status(&app, &engine, &index);
                }
                Err(e) => {
                    let message = e.to_string();
                    tracing::warn!("index: backfill stopped: {message}");
                    index.set_last_error(Some(message));
                    break;
                }
            }
        }

        // Then the deep pass over starred mail. It runs second because the
        // whole-message index is what makes search work at all; chunking the
        // starred few only makes it sharper.
        loop {
            match rag::index_starred_pending(engine.store(), engine.http(), &settings, DEEP_BATCH)
                .await
            {
                Ok(0) => break,
                Ok(n) => {
                    tracing::debug!("index: deep-indexed {n} starred message(s)");
                    emit_index_status(&app, &engine, &index);
                }
                Err(e) => {
                    let message = e.to_string();
                    tracing::warn!("index: deep pass stopped: {message}");
                    index.set_last_error(Some(message));
                    break;
                }
            }
        }

        index.building.store(false, Ordering::SeqCst);
        emit_index_status(&app, &engine, &index);
    });

    current_status(&state.engine, &state.index)
}

fn emit_index_status(app: &AppHandle, engine: &SyncEngine, index: &IndexTask) {
    match current_status(engine, index) {
        Ok(status) => {
            let _ = app.emit(INDEX_EVENT, status);
        }
        Err(e) => tracing::warn!("index: could not report progress: {e}"),
    }
}

/// Drop every vector for the current embedding model.
#[tauri::command]
pub fn clear_index(state: State<'_, AppState>) -> CmdResult<IndexStatus> {
    // Deleting rows a running backfill is still writing would leave the counter
    // reporting a fraction of an index nobody asked for.
    if state.index.building.load(Ordering::SeqCst) {
        return Err("正在建立索引，请等待完成后再清空".to_string());
    }
    let store = state.engine.store();
    let settings = store.embedding_settings().map_err(err_str)?;
    let removed = store.clear_vectors(settings.model.trim()).map_err(err_str)?;
    tracing::info!("index: cleared {removed} vector(s)");
    state.index.set_last_error(None);
    current_status(&state.engine, &state.index)
}

// ---------------------------------------------------------------------------
// Retrieval
// ---------------------------------------------------------------------------

/// Semantic search over stored mail, falling back to substring search when the
/// index is empty or embeddings are switched off — `rag` decides which.
///
/// `limit` of `None` means "as many as the reranker settings allow".
#[tauri::command]
pub async fn search_mail(
    state: State<'_, AppState>,
    query: String,
    limit: Option<u32>,
) -> CmdResult<Vec<SearchHit>> {
    let engine = state.engine.clone();
    let store = engine.store();
    let ai_settings = store.ai_settings().map_err(err_str)?;
    let embedding = store.embedding_settings().map_err(err_str)?;
    let reranker = store.reranker_settings().map_err(err_str)?;
    rag::search(
        store,
        engine.http(),
        &ai_settings,
        &embedding,
        &reranker,
        &query,
        limit.unwrap_or(0),
    )
    .await
    .map_err(err_str)
}

// ---------------------------------------------------------------------------
// Memory
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryInput {
    /// None → create; Some → update.
    pub id: Option<String>,
    pub kind: MemoryKind,
    pub text: String,
    pub source: Option<String>,
}

/// History rows shown on the knowledge screen.
const MEMORY_HISTORY: u32 = 50;
/// Audit-trail rows shown for one memory.
const MEMORY_EVENTS: u32 = 20;

fn memory_from(input: MemoryInput, existing: Option<&MemoryEntry>, now: i64) -> MemoryEntry {
    MemoryEntry {
        id: input.id.filter(|id| !id.is_empty()).unwrap_or_else(new_id),
        kind: input.kind,
        text: input.text,
        source: input.source.filter(|s| !s.trim().is_empty()),
        // Editing a memory does not make it new; only `updated_at` moves.
        created_at: existing.map(|m| m.created_at).unwrap_or(now),
        updated_at: now,
        ..Default::default()
    }
}

#[tauri::command]
pub fn list_memories(state: State<'_, AppState>) -> CmdResult<Vec<MemoryEntry>> {
    state.engine.store().list_memories().map_err(err_str)
}

/// Memories that stopped being true, newest first. Nothing here reaches a
/// prompt; it is the record of what the assistant used to believe.
#[tauri::command]
pub fn list_memory_history(state: State<'_, AppState>) -> CmdResult<Vec<MemoryEntry>> {
    state.engine.store().superseded_memories(MEMORY_HISTORY).map_err(err_str)
}

/// What happened to one memory, or to the memory as a whole when `id` is absent.
#[tauri::command]
pub fn memory_events(
    state: State<'_, AppState>,
    id: Option<String>,
) -> CmdResult<Vec<MemoryEvent>> {
    state
        .engine
        .store()
        .memory_events(id.as_deref(), MEMORY_EVENTS)
        .map_err(err_str)
}

/// Add or edit a memory by hand.
///
/// Goes through `memory::write_by_hand`, which marks it as the user's own and
/// therefore off-limits to the reconciler: a model rewriting a line a person
/// typed would be the worst behaviour this feature could have.
#[tauri::command]
pub async fn save_memory(
    state: State<'_, AppState>,
    input: MemoryInput,
) -> CmdResult<MemoryEntry> {
    if input.text.trim().is_empty() {
        return Err("记忆内容不能为空".to_string());
    }
    let engine = state.engine.clone();
    let store = engine.store();
    // One indexed lookup: reading the whole table to find a single row got
    // slower with every memory the assistant ever stored.
    let existing = match input.id.as_deref().filter(|id| !id.is_empty()) {
        Some(id) => store.get_memory(id).map_err(err_str)?,
        None => None,
    };
    let entry = memory_from(input, existing.as_ref(), now_ms());
    let embedding = store.embedding_settings().map_err(err_str)?;
    memory::write_by_hand(store, engine.http(), &embedding, &entry)
        .await
        .map_err(err_str)
}

#[tauri::command]
pub fn delete_memory(state: State<'_, AppState>, id: String) -> CmdResult<()> {
    state.engine.store().delete_memory(&id).map_err(err_str)
}

// ---------------------------------------------------------------------------
// Assistant
// ---------------------------------------------------------------------------

/// A draft the assistant produced, with the conversation that asked for it.
#[derive(Clone)]
struct PendingEntry {
    conversation_id: String,
    action: PendingAction,
}

/// Actions waiting for the user's approval.
///
/// In memory only, deliberately: approving is a live decision about a draft the
/// user has just read, and a restart must not leave a "send this" from last week
/// sitting in the database waiting to be clicked.
#[derive(Default)]
pub struct PendingActions {
    entries: Mutex<Vec<PendingEntry>>,
}

impl PendingActions {
    fn remember(&self, conversation_id: &str, action: PendingAction) {
        let mut entries = self.entries.lock().expect("pending mutex poisoned");
        entries.retain(|e| e.action.id != action.id);
        entries.push(PendingEntry {
            conversation_id: conversation_id.to_string(),
            action,
        });
        // The oldest drafts fall off: nobody comes back to approve the first of
        // twenty they walked away from.
        let overflow = entries.len().saturating_sub(MAX_PENDING);
        entries.drain(..overflow);
    }

    fn get(&self, id: &str) -> Option<PendingEntry> {
        let entries = self.entries.lock().expect("pending mutex poisoned");
        entries.iter().find(|e| e.action.id == id).cloned()
    }

    fn forget(&self, id: &str) {
        let mut entries = self.entries.lock().expect("pending mutex poisoned");
        entries.retain(|e| e.action.id != id);
    }
}

/// One fragment of an answer being written.
#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantDelta {
    pub conversation_id: String,
    pub text: String,
}

/// Ask the assistant one question.
///
/// `conversation_id` of `None` starts a new conversation; the id it was given is
/// on the returned turn, so the caller can keep asking into the same thread.
#[tauri::command]
pub async fn assistant_ask(
    app: AppHandle,
    state: State<'_, AppState>,
    conversation_id: Option<String>,
    text: String,
) -> CmdResult<AssistantReply> {
    let engine = state.engine.clone();
    let pending = state.pending.clone();
    let conversation_id = conversation_id
        .map(|id| id.trim().to_string())
        .filter(|id| !id.is_empty())
        .unwrap_or_else(new_id);

    // Each fragment of prose, as the model writes it. Tagged with the
    // conversation so a panel that has moved on ignores what it did not ask for.
    let sink = {
        let app = app.clone();
        let id = conversation_id.clone();
        move |chunk: &str| {
            let _ = app.emit(
                ASSISTANT_DELTA_EVENT,
                AssistantDelta { conversation_id: id.clone(), text: chunk.to_string() },
            );
        }
    };

    let reply =
        assistant::ask_streaming(engine.store(), engine.http(), &conversation_id, &text, &sink)
            .await
            .map_err(err_str)?;
    // The action is only ever executed from here, against this snapshot — the
    // model cannot hand `confirm_pending_action` an id it made up later.
    if let Some(action) = &reply.pending_confirmation {
        pending.remember(&conversation_id, action.clone());
    }
    Ok(reply)
}

/// Carry out an action the user approved. Nothing else executes one.
#[tauri::command]
pub async fn confirm_pending_action(state: State<'_, AppState>, id: String) -> CmdResult<()> {
    let engine = state.engine.clone();
    let pending = state.pending.clone();
    let entry = pending
        .get(&id)
        .ok_or_else(|| "该操作已失效，请让助手重新起草".to_string())?;

    match entry.action.kind.as_str() {
        "send_mail" => {
            let mail: OutgoingMail = serde_json::from_value(entry.action.payload)
                .map_err(|e| format!("草稿内容无法解析: {e}"))?;
            send_outgoing(&engine, &mail).await?;
            // Forgotten only once the mail is really gone, so a send that failed
            // on a bad SMTP password stays confirmable after the user fixes it.
            pending.forget(&id);
            record_sent(&engine, &entry.conversation_id, &mail);
            Ok(())
        }
        other => Err(format!("不支持的操作类型: {other}")),
    }
}

/// Fold the send back into the transcript. The assistant's last word was "等你确认";
/// without this the conversation never says what the user decided.
fn record_sent(engine: &SyncEngine, conversation_id: &str, mail: &OutgoingMail) {
    let subject = if mail.subject.trim().is_empty() {
        "（无主题）"
    } else {
        &mail.subject
    };
    let turn = ChatTurn {
        reasoning: None,
        id: new_id(),
        conversation_id: conversation_id.to_string(),
        role: ChatRole::Tool,
        content: format!("已按你的确认发送邮件给 {}，主题：{subject}", mail.to.join("、")),
        tool_calls: vec![ToolCallRecord {
            name: "send_mail".to_string(),
            // Headers only: the body is in the draft the user just approved.
            arguments: serde_json::json!({ "to": mail.to, "subject": mail.subject }),
            summary: "邮件已发送".to_string(),
            ok: true,
        }],
        citations: Vec::new(),
        created_at: now_ms(),
    };
    if let Err(e) = engine.store().append_turn(&turn) {
        // The mail is already gone; an incomplete transcript is not worth
        // telling the user their send failed.
        tracing::warn!("could not record the sent mail in the conversation: {e}");
    }
}

// ---------------------------------------------------------------------------
// Conversations
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn list_conversations(
    state: State<'_, AppState>,
    limit: Option<u32>,
) -> CmdResult<Vec<Conversation>> {
    let limit = limit
        .unwrap_or(DEFAULT_CONVERSATIONS)
        .clamp(1, MAX_CONVERSATIONS);
    state.engine.store().list_conversations(limit).map_err(err_str)
}

#[tauri::command]
pub fn conversation_turns(
    state: State<'_, AppState>,
    conversation_id: String,
) -> CmdResult<Vec<ChatTurn>> {
    state
        .engine
        .store()
        .conversation_turns(&conversation_id)
        .map_err(err_str)
}

#[tauri::command]
pub fn delete_conversation(state: State<'_, AppState>, id: String) -> CmdResult<()> {
    state.engine.store().delete_conversation(&id).map_err(err_str)
}

fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn mcp_input(url: &str, auth: McpAuth, transport: McpTransport) -> McpServerInput {
        McpServerInput {
            id: None,
            name: "exa".into(),
            transport,
            url: url.into(),
            auth,
            api_key: Some("secret".into()),
            command: String::new(),
            args: Vec::new(),
            env: Default::default(),
            enabled: true,
        }
    }

    /// A key sent over cleartext is a key handed to whoever is on the path.
    #[test]
    fn an_authenticated_http_endpoint_is_refused() {
        let bad = mcp_input("http://mcp.example.com/v1", McpAuth::Bearer, McpTransport::Http);
        assert!(insecure_endpoint(&bad).is_some());
    }

    /// Loopback has no path to intercept, and a local bridge is the normal way
    /// to run one of these.
    #[test]
    fn loopback_is_allowed_to_stay_plain() {
        for url in [
            "http://localhost:8080/mcp",
            "http://127.0.0.1:3000",
            "http://[::1]:9000/mcp",
        ] {
            let input = mcp_input(url, McpAuth::Bearer, McpTransport::Http);
            assert!(insecure_endpoint(&input).is_none(), "{url}");
        }
    }

    #[test]
    fn https_and_unauthenticated_and_stdio_are_all_fine() {
        assert!(insecure_endpoint(&mcp_input(
            "https://mcp.example.com",
            McpAuth::Bearer,
            McpTransport::Http
        ))
        .is_none());
        // Nothing secret to leak.
        assert!(insecure_endpoint(&mcp_input(
            "http://mcp.example.com",
            McpAuth::None,
            McpTransport::Http
        ))
        .is_none());
        // stdio never touches the network.
        assert!(insecure_endpoint(&mcp_input("", McpAuth::Bearer, McpTransport::Stdio)).is_none());
    }

    /// `env` on a stdio server is where tokens live, so the public DTO must
    /// not carry the values — only which names are set.
    #[test]
    fn stdio_env_values_never_reach_the_frontend() {
        let cfg = McpServerConfig {
            id: "s1".into(),
            name: "github".into(),
            transport: McpTransport::Stdio,
            url: String::new(),
            auth: McpAuth::None,
            api_key: String::new(),
            command: "npx".into(),
            args: vec!["-y".into()],
            env: [("GITHUB_TOKEN".to_string(), "ghp_verysecret".to_string())]
                .into_iter()
                .collect(),
            enabled: true,
        };
        let public = McpServerPublic::from(&cfg);
        assert_eq!(public.env.get("GITHUB_TOKEN").map(String::as_str), Some(""));
        assert!(!format!("{:?}", public).contains("verysecret"));
    }

    /// And a blank value coming back from that form keeps what was stored,
    /// exactly as a blank api_key does — otherwise saving any other field
    /// would wipe the token.
    #[test]
    fn a_blank_env_value_keeps_the_stored_secret() {
        let old = McpServerConfig {
            id: "s1".into(),
            name: "github".into(),
            transport: McpTransport::Stdio,
            url: String::new(),
            auth: McpAuth::None,
            api_key: String::new(),
            command: "npx".into(),
            args: vec![],
            env: [
                ("GITHUB_TOKEN".to_string(), "ghp_verysecret".to_string()),
                ("GONE".to_string(), "x".to_string()),
            ]
            .into_iter()
            .collect(),
            enabled: true,
        };
        let mut input = mcp_input("", McpAuth::None, McpTransport::Stdio);
        input.name = "github".into();
        // The form round-trips the blanked value, and drops "GONE" entirely.
        input.env = [("GITHUB_TOKEN".to_string(), String::new())].into_iter().collect();

        let merged = merge_mcp(Some(&old), input);
        assert_eq!(merged.env.get("GITHUB_TOKEN").map(String::as_str), Some("ghp_verysecret"));
        assert!(!merged.env.contains_key("GONE"), "a removed name stays removed");
    }

    use super::*;

    fn stored_ai() -> AiSettings {
        AiSettings {
            enabled: true,
            provider: AiProvider::OpenaiCompatible,
            api_base: "https://api.openai.com/v1".into(),
            api_key: "sk-stored".into(),
            model: "gpt-4o-mini".into(),
            ..AiSettings::default()
        }
    }

    fn ai_input(api_key: Option<&str>) -> AiSettingsInput {
        AiSettingsInput {
            enabled: true,
            provider: AiProvider::Anthropic,
            api_base: "https://api.anthropic.com/".into(),
            api_key: api_key.map(str::to_string),
            model: "  claude-sonnet-4-5  ".into(),
            temperature: 5.0,
            auto_delete_spam: true,
            extra_instructions: "只保留重要邮件".into(),
        }
    }

    #[test]
    fn a_blank_ai_key_keeps_the_stored_one() {
        for blank in [None, Some("")] {
            let merged = merge_ai(stored_ai(), ai_input(blank));
            assert_eq!(merged.api_key, "sk-stored");
        }
        let merged = merge_ai(stored_ai(), ai_input(Some("sk-new")));
        assert_eq!(merged.api_key, "sk-new");
    }

    #[test]
    fn ai_provider_round_trips_and_the_base_is_normalised() {
        let merged = merge_ai(stored_ai(), ai_input(None));
        assert_eq!(merged.provider, AiProvider::Anthropic);
        assert_eq!(merged.api_base, "https://api.anthropic.com");
        assert_eq!(merged.model, "claude-sonnet-4-5");
        // Out-of-range temperatures come from a text field, not a slider.
        assert_eq!(merged.temperature, 2.0);
        assert_eq!(AiSettingsPublic::from(&merged).provider, AiProvider::Anthropic);
    }

    #[test]
    fn the_public_ai_view_reports_the_key_without_carrying_it() {
        let public = AiSettingsPublic::from(&merge_ai(stored_ai(), ai_input(None)));
        assert!(public.has_api_key);
        let json = serde_json::to_string(&public).unwrap();
        assert!(!json.contains("sk-stored"), "key leaked into {json}");
    }

    #[test]
    fn a_blank_embedding_key_keeps_the_stored_one() {
        let stored = EmbeddingSettings { api_key: "emb-stored".into(), ..Default::default() };
        let input = EmbeddingSettingsInput {
            enabled: true,
            provider: AiProvider::Gemini,
            api_base: "https://generativelanguage.googleapis.com/v1beta///".into(),
            api_key: Some(String::new()),
            model: " text-embedding-004 ".into(),
            dimensions: 768,
        };
        let merged = merge_embedding(stored, input);

        assert_eq!(merged.api_key, "emb-stored");
        assert_eq!(merged.provider, AiProvider::Gemini);
        assert_eq!(merged.api_base, "https://generativelanguage.googleapis.com/v1beta");
        assert_eq!(merged.model, "text-embedding-004");
        assert_eq!(merged.dimensions, 768);
        assert!(EmbeddingSettingsPublic::from(&merged).has_api_key);
    }

    #[test]
    fn a_blank_reranker_key_keeps_the_stored_one() {
        let stored = RerankerSettings { api_key: "rr-stored".into(), ..Default::default() };
        let input = RerankerSettingsInput {
            kind: RerankerKind::RerankApi,
            api_base: "https://api.jina.ai/v1/".into(),
            api_key: None,
            model: "jina-reranker-v2-base-multilingual".into(),
            candidates: 60,
            top_n: 10,
        };
        let merged = merge_reranker(stored, input);

        assert_eq!(merged.api_key, "rr-stored");
        assert_eq!(merged.kind, RerankerKind::RerankApi);
        assert_eq!(merged.api_base, "https://api.jina.ai/v1");
        assert_eq!(merged.candidates, 60);
        assert_eq!(merged.top_n, 10);
        let json = serde_json::to_string(&RerankerSettingsPublic::from(&merged)).unwrap();
        assert!(!json.contains("rr-stored"), "key leaked into {json}");
    }

    fn status() -> IndexStatus {
        IndexStatus {
            indexed: 3,
            total: 10,
            deep_indexed: 1,
            deep_total: 2,
            model: "text-embedding-3-small".into(),
            building: false,
            error: Some("尚未配置嵌入接口地址".into()),
        }
    }

    #[test]
    fn a_failed_run_replaces_the_configuration_hint() {
        let out = overlay(status(), true, Some("嵌入接口返回 401".into()));
        assert!(out.building);
        assert_eq!(out.error.as_deref(), Some("嵌入接口返回 401"));
    }

    #[test]
    fn without_a_run_error_the_configuration_hint_stands() {
        let out = overlay(status(), false, None);
        assert!(!out.building);
        assert_eq!(out.error.as_deref(), Some("尚未配置嵌入接口地址"));
        assert_eq!(out.indexed, 3);
    }

    fn action(id: &str) -> PendingAction {
        PendingAction {
            id: id.to_string(),
            kind: "send_mail".to_string(),
            description: "发送邮件给 someone@example.com".to_string(),
            payload: serde_json::json!({ "accountId": "a1" }),
        }
    }

    #[test]
    fn a_pending_action_survives_until_it_is_used() {
        let pending = PendingActions::default();
        pending.remember("conv-1", action("act-1"));

        let entry = pending.get("act-1").expect("remembered");
        assert_eq!(entry.conversation_id, "conv-1");
        assert_eq!(entry.action.kind, "send_mail");

        pending.forget("act-1");
        assert!(pending.get("act-1").is_none());
    }

    #[test]
    fn re_proposing_the_same_action_does_not_duplicate_it() {
        let pending = PendingActions::default();
        pending.remember("conv-1", action("act-1"));
        pending.remember("conv-2", action("act-1"));

        assert_eq!(pending.entries.lock().unwrap().len(), 1);
        assert_eq!(pending.get("act-1").unwrap().conversation_id, "conv-2");
    }

    #[test]
    fn old_drafts_fall_off_instead_of_growing_forever() {
        let pending = PendingActions::default();
        for i in 0..MAX_PENDING + 5 {
            pending.remember("conv-1", action(&format!("act-{i}")));
        }

        assert_eq!(pending.entries.lock().unwrap().len(), MAX_PENDING);
        assert!(pending.get("act-0").is_none(), "the oldest should have been dropped");
        assert!(pending.get(&format!("act-{}", MAX_PENDING + 4)).is_some());
    }

    fn memory_input(id: Option<&str>, text: &str) -> MemoryInput {
        MemoryInput {
            id: id.map(str::to_string),
            kind: MemoryKind::Preference,
            text: text.to_string(),
            source: Some("  ".to_string()),
        }
    }

    #[test]
    fn a_new_memory_gets_an_id_and_both_timestamps() {
        let entry = memory_from(memory_input(None, "回信要简短"), None, 1_700);
        assert!(!entry.id.is_empty());
        // A whitespace-only source is no source at all.
        assert_eq!(entry.source, None);
        assert_eq!(entry.created_at, 1_700);
        assert_eq!(entry.updated_at, 1_700);
    }

    #[test]
    fn editing_a_memory_keeps_the_original_creation_time() {
        let existing = MemoryEntry {
            id: "m1".into(),
            kind: MemoryKind::Fact,
            text: "旧内容".into(),
            created_at: 1_000,
            updated_at: 1_000,
            ..Default::default()
        };
        let entry = memory_from(memory_input(Some("m1"), "新内容"), Some(&existing), 2_000);

        assert_eq!(entry.id, "m1");
        assert_eq!(entry.created_at, 1_000);
        assert_eq!(entry.updated_at, 2_000);
        assert_eq!(entry.kind, MemoryKind::Preference);
    }
}
