//! Tauri shell: wires the core engine to windows, events and system
//! notifications. Works on desktop (Win/macOS/Linux) and mobile (iOS/Android).

mod commands;

use std::fs;
use std::sync::Arc;

use mailer_core::store::Store;
use mailer_core::sync::{EventSink, SyncEngine};
use mailer_core::types::{AlertEvent, Category, SyncStatus};
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_notification::NotificationExt;

pub struct AppState {
    pub engine: Arc<SyncEngine>,
    /// The embedding backfill: whether one is running, and why the last one
    /// stopped. Nothing the store can answer.
    pub index: Arc<commands::IndexTask>,
    /// Assistant drafts waiting for the user to approve them.
    pub pending: Arc<commands::PendingActions>,
}

/// Bridges core events to the UI: window events + OS notifications.
struct TauriSink {
    app: AppHandle,
}

impl EventSink for TauriSink {
    fn alert(&self, event: &AlertEvent) {
        // In-app popup (modal / toast) for the running window.
        let _ = self.app.emit("mailer://alert", event);

        // System notification even when the window is hidden.
        let (title, body) = match event.category {
            Category::Verification => {
                let code = event.verification_code.as_deref().unwrap_or("——");
                (
                    format!("验证码 {code}"),
                    format!("{} · {}", event.from, event.subject),
                )
            }
            _ => (
                format!("重要邮件 · {}", event.account_email),
                format!("{}\n{}", event.subject, event.summary),
            ),
        };
        if let Err(e) = self
            .app
            .notification()
            .builder()
            .title(&title)
            .body(&body)
            .show()
        {
            tracing::warn!("system notification failed: {e}");
        }
    }

    fn mail_changed(&self, account_id: &str) {
        let _ = self.app.emit("mailer://mail-changed", account_id);
    }

    fn sync_status(&self, status: &SyncStatus) {
        let _ = self.app.emit("mailer://sync-status", status);
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,mailer=debug,mailer_core=debug".into()),
        )
        .init();

    tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let data_dir = app.path().app_data_dir()?;
            fs::create_dir_all(&data_dir)?;
            let store = Arc::new(
                Store::open(&data_dir.join("mailer.db")).map_err(|e| e.to_string())?,
            );
            let sink = Box::new(TauriSink { app: app.handle().clone() });
            let engine = SyncEngine::new(store, sink);
            tauri::async_runtime::spawn(engine.clone().run_scheduler());
            // A held-open IMAP connection per account, so mail arrives rather
            // than being collected on the next tick. The scheduler above is still
            // the floor: IDLE is not universal and connections drop.
            tauri::async_runtime::spawn(engine.clone().run_watchers());
            // The full-text index arrived after the mailbox did, so a database
            // from an earlier build has mail in it that nothing has indexed.
            // Purely local work — no endpoint, no key, no cost — so it runs
            // itself rather than waiting to be asked.
            tauri::async_runtime::spawn(commands::backfill_text_index(engine.clone()));
            // Same deal for the tracker report: local, and about mail that
            // arrived before there was anything looking.
            tauri::async_runtime::spawn(commands::backfill_trackers(engine.clone()));
            app.manage(AppState {
                engine,
                index: Arc::default(),
                pending: Arc::default(),
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::host_platform,
            commands::list_accounts,
            commands::save_account,
            commands::delete_account,
            commands::test_account,
            commands::list_messages,
            commands::get_message,
            commands::mark_read,
            commands::set_starred,
            commands::delete_messages,
            commands::sync_now,
            commands::sync_statuses,
            commands::category_counts,
            commands::get_ai_settings,
            commands::set_ai_settings,
            commands::test_ai,
            commands::reclassify,
            commands::list_channels,
            commands::save_channel,
            commands::delete_channel,
            commands::test_channel,
            commands::send_mail,
            commands::get_embedding_settings,
            commands::set_embedding_settings,
            commands::test_embedding,
            commands::get_reranker_settings,
            commands::set_reranker_settings,
            commands::get_privacy_settings,
            commands::set_privacy_settings,
            commands::message_trackers,
            commands::tracker_stats,
            commands::get_mcp_servers,
            commands::save_mcp_server,
            commands::delete_mcp_server,
            commands::mcp_status,
            commands::reconnect_mcp,
            commands::index_status,
            commands::index_pending,
            commands::clear_index,
            commands::search_mail,
            commands::list_memories,
            commands::list_memory_history,
            commands::memory_events,
            commands::save_memory,
            commands::delete_memory,
            commands::list_conversations,
            commands::conversation_turns,
            commands::delete_conversation,
            commands::assistant_ask,
            commands::confirm_pending_action,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
