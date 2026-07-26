//! Tauri IPC commands. Thin plumbing over the core engine; every command
//! returns `Result<T, String>` so the frontend gets readable error text.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use mailer_core::mail::{imap, pop3, smtp};
use mailer_core::sync::{now_ms, SyncEngine};
use mailer_core::types::*;
use mailer_core::{ai, assistant, notify, rag};
use serde::Deserialize;
use tauri::{AppHandle, Emitter, State};

use crate::AppState;

type CmdResult<T> = Result<T, String>;

/// Messages embedded per backfill round trip. Small enough that a failure costs
/// one request, large enough that a 5000-mail mailbox is not 5000 round trips.
const INDEX_BATCH: u32 = 32;
/// Conversations listed when the caller does not ask for a number.
const DEFAULT_CONVERSATIONS: u32 = 100;
const MAX_CONVERSATIONS: u32 = 500;
/// Drafts held in memory awaiting the user's approval.
const MAX_PENDING: usize = 32;
/// Progress of the embedding backfill, pushed as it runs.
const INDEX_EVENT: &str = "mailer://index-status";

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
    state.engine.store().query_messages(&query).map_err(err_str)
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

#[tauri::command]
pub async fn delete_messages(
    state: State<'_, AppState>,
    ids: Vec<String>,
    on_server: bool,
) -> CmdResult<()> {
    state.engine.delete_messages(&ids, on_server).await;
    Ok(())
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

fn memory_from(
    input: MemoryInput,
    existing: Option<&MemoryEntry>,
    now: i64,
) -> CmdResult<MemoryEntry> {
    let text = input.text.trim().to_string();
    if text.is_empty() {
        return Err("记忆内容不能为空".to_string());
    }
    Ok(MemoryEntry {
        id: input
            .id
            .filter(|id| !id.is_empty())
            .unwrap_or_else(new_id),
        kind: input.kind,
        text,
        source: input.source.filter(|s| !s.trim().is_empty()),
        // Editing a memory does not make it new; only `updated_at` moves.
        created_at: existing.map(|m| m.created_at).unwrap_or(now),
        updated_at: now,
    })
}

#[tauri::command]
pub fn list_memories(state: State<'_, AppState>) -> CmdResult<Vec<MemoryEntry>> {
    state.engine.store().list_memories().map_err(err_str)
}

#[tauri::command]
pub fn save_memory(state: State<'_, AppState>, input: MemoryInput) -> CmdResult<MemoryEntry> {
    let store = state.engine.store();
    // One indexed lookup: reading the whole table to find a single row got
    // slower with every memory the assistant ever stored.
    let existing = match input.id.as_deref().filter(|id| !id.is_empty()) {
        Some(id) => store.get_memory(id).map_err(err_str)?,
        None => None,
    };
    let entry = memory_from(input, existing.as_ref(), now_ms())?;
    store.upsert_memory(&entry).map_err(err_str)?;
    Ok(entry)
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

/// Ask the assistant one question.
///
/// `conversation_id` of `None` starts a new conversation; the id it was given is
/// on the returned turn, so the caller can keep asking into the same thread.
#[tauri::command]
pub async fn assistant_ask(
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

    let reply = assistant::ask(engine.store(), engine.http(), &conversation_id, &text)
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
        let entry = memory_from(memory_input(None, "  回信要简短  "), None, 1_700).unwrap();
        assert!(!entry.id.is_empty());
        assert_eq!(entry.text, "回信要简短");
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
            source: None,
            created_at: 1_000,
            updated_at: 1_000,
        };
        let entry = memory_from(memory_input(Some("m1"), "新内容"), Some(&existing), 2_000).unwrap();

        assert_eq!(entry.id, "m1");
        assert_eq!(entry.created_at, 1_000);
        assert_eq!(entry.updated_at, 2_000);
        assert_eq!(entry.kind, MemoryKind::Preference);
    }

    #[test]
    fn an_empty_memory_is_rejected() {
        let err = memory_from(memory_input(None, "   \n "), None, 1).unwrap_err();
        assert!(err.contains("不能为空"), "unexpected message: {err}");
    }
}
