//! Tauri IPC commands. Thin plumbing over the core engine; every command
//! returns `Result<T, String>` so the frontend gets readable error text.

use mailer_core::mail::{imap, pop3, smtp};
use mailer_core::sync::now_ms;
use mailer_core::types::*;
use mailer_core::{ai, notify};
use serde::Deserialize;
use tauri::State;

use crate::AppState;

type CmdResult<T> = Result<T, String>;

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
    pub api_base: String,
    /// Empty/None keeps the stored key.
    pub api_key: Option<String>,
    pub model: String,
    pub temperature: f32,
    pub auto_delete_spam: bool,
    pub extra_instructions: String,
}

#[tauri::command]
pub fn get_ai_settings(state: State<'_, AppState>) -> CmdResult<AiSettingsPublic> {
    let s = state.engine.store().ai_settings().map_err(err_str)?;
    Ok(AiSettingsPublic::from(&s))
}

#[tauri::command]
pub fn set_ai_settings(state: State<'_, AppState>, input: AiSettingsInput) -> CmdResult<AiSettingsPublic> {
    let store = state.engine.store();
    let old = store.ai_settings().map_err(err_str)?;
    let s = AiSettings {
        enabled: input.enabled,
        api_base: input.api_base.trim().trim_end_matches('/').to_string(),
        api_key: input
            .api_key
            .filter(|k| !k.is_empty())
            .unwrap_or(old.api_key),
        model: input.model,
        temperature: input.temperature.clamp(0.0, 2.0),
        auto_delete_spam: input.auto_delete_spam,
        extra_instructions: input.extra_instructions,
    };
    store.set_ai_settings(&s).map_err(err_str)?;
    Ok(AiSettingsPublic::from(&s))
}

#[tauri::command]
pub async fn test_ai(state: State<'_, AppState>) -> CmdResult<TestResult> {
    let settings = state.engine.store().ai_settings().map_err(err_str)?;
    if settings.api_key.is_empty() {
        return Ok(TestResult { ok: false, message: "尚未配置 API Key".into() });
    }
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

#[tauri::command]
pub async fn send_mail(state: State<'_, AppState>, mail: OutgoingMail) -> CmdResult<()> {
    let store = state.engine.store();
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
