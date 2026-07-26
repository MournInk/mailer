//! Orchestration: fetch → parse → store → classify → act.
//!
//! The engine is UI-agnostic: everything user-visible goes through the
//! [`EventSink`] trait, which the Tauri layer implements (system
//! notifications + window events).

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::error::{Error, Result};
use crate::mail::{imap, parse, pop3, RawMail};
use crate::store::Store;
use crate::types::*;
use crate::{ai, notify};

/// Max messages pulled from the server per account per cycle.
const MAX_FETCH: u32 = 50;
/// Max messages classified per cycle (keeps LLM cost bounded).
const MAX_CLASSIFY: u32 = 20;
/// Scheduler tick.
const TICK: Duration = Duration::from_secs(20);

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// UI bridge implemented by the Tauri layer.
pub trait EventSink: Send + Sync + 'static {
    /// A message deserving a popup (verification code / important mail).
    fn alert(&self, event: &AlertEvent);
    /// Stored data changed for an account; lists should refresh.
    fn mail_changed(&self, account_id: &str);
    /// Live sync state for the UI.
    fn sync_status(&self, status: &SyncStatus);
}

/// No-op sink for tests and headless use.
pub struct NullSink;
impl EventSink for NullSink {
    fn alert(&self, _: &AlertEvent) {}
    fn mail_changed(&self, _: &str) {}
    fn sync_status(&self, _: &SyncStatus) {}
}

pub struct SyncEngine {
    store: Arc<Store>,
    http: reqwest::Client,
    sink: Box<dyn EventSink>,
    /// account_id → latest status.
    states: Mutex<HashMap<String, SyncStatus>>,
    /// accounts currently mid-sync (overlap guard).
    in_flight: Mutex<HashSet<String>>,
    /// account_id → unix millis of last sync attempt (scheduler bookkeeping).
    last_attempt: Mutex<HashMap<String, i64>>,
}

impl SyncEngine {
    pub fn new(store: Arc<Store>, sink: Box<dyn EventSink>) -> Arc<Self> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .build()
            .expect("reqwest client");
        Arc::new(SyncEngine {
            store,
            http,
            sink,
            states: Mutex::new(HashMap::new()),
            in_flight: Mutex::new(HashSet::new()),
            last_attempt: Mutex::new(HashMap::new()),
        })
    }

    pub fn store(&self) -> &Arc<Store> {
        &self.store
    }

    pub fn http(&self) -> &reqwest::Client {
        &self.http
    }

    /// Latest known status for every account.
    pub fn statuses(&self) -> Vec<SyncStatus> {
        self.states.lock().unwrap().values().cloned().collect()
    }

    fn set_status(&self, account_id: &str, f: impl FnOnce(&mut SyncStatus)) {
        let mut states = self.states.lock().unwrap();
        let st = states.entry(account_id.to_string()).or_insert_with(|| SyncStatus {
            account_id: account_id.to_string(),
            phase: SyncPhase::Idle,
            fetched: 0,
            error: None,
            last_ok_at: None,
        });
        f(st);
        self.sink.sync_status(st);
    }

    /// Background scheduler: polls every account at its configured interval.
    pub fn start_scheduler(self: &Arc<Self>) -> tokio::task::JoinHandle<()> {
        let engine = Arc::clone(self);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(TICK).await;
                let accounts = match engine.store.list_accounts() {
                    Ok(a) => a,
                    Err(e) => {
                        tracing::error!("scheduler: list_accounts failed: {e}");
                        continue;
                    }
                };
                let now = now_ms();
                for acc in accounts {
                    if acc.sync_interval_secs == 0 {
                        continue; // auto sync disabled
                    }
                    let due = {
                        let last = engine.last_attempt.lock().unwrap();
                        match last.get(&acc.id) {
                            Some(t) => now - t >= (acc.sync_interval_secs as i64) * 1000,
                            None => true,
                        }
                    };
                    if due {
                        let engine = Arc::clone(&engine);
                        tokio::spawn(async move {
                            if let Err(e) = engine.sync_account(&acc.id).await {
                                tracing::warn!("sync {} failed: {e}", acc.id);
                            }
                        });
                    }
                }
            }
        })
    }

    /// Fetch + classify one account. Returns number of new messages stored.
    pub async fn sync_account(&self, account_id: &str) -> Result<u32> {
        {
            let mut in_flight = self.in_flight.lock().unwrap();
            if !in_flight.insert(account_id.to_string()) {
                return Ok(0); // already syncing
            }
        }
        self.last_attempt.lock().unwrap().insert(account_id.to_string(), now_ms());

        let result = self.sync_account_inner(account_id).await;

        self.in_flight.lock().unwrap().remove(account_id);
        match &result {
            Ok(n) => {
                let n = *n;
                self.set_status(account_id, |st| {
                    st.phase = SyncPhase::Idle;
                    st.fetched = n;
                    st.error = None;
                    st.last_ok_at = Some(now_ms());
                });
            }
            Err(e) => {
                let msg = e.to_string();
                self.set_status(account_id, |st| {
                    st.phase = SyncPhase::Error;
                    st.error = Some(msg);
                });
            }
        }
        result
    }

    async fn sync_account_inner(&self, account_id: &str) -> Result<u32> {
        let account = self.store.get_account(account_id)?;

        self.set_status(account_id, |st| {
            st.phase = SyncPhase::Connecting;
            st.fetched = 0;
            st.error = None;
        });

        let known: HashSet<String> =
            self.store.known_uids(account_id, "INBOX")?.into_iter().collect();

        let raws: Vec<RawMail> = match account.protocol {
            Protocol::Imap => imap::fetch_new(&account, &known, MAX_FETCH).await?,
            Protocol::Pop3 => pop3::fetch_new(&account, &known, MAX_FETCH).await?,
        };

        self.set_status(account_id, |st| st.phase = SyncPhase::Fetching);

        let mut inserted = 0u32;
        for raw in &raws {
            let id = uuid::Uuid::new_v4().to_string();
            match parse::parse_mail(id, account_id, raw, now_ms()) {
                Ok(msg) => {
                    if self.store.insert_message(&msg)? {
                        inserted += 1;
                    }
                }
                Err(e) => {
                    tracing::warn!("parse failed for uid {} on {}: {e}", raw.uid, account_id);
                }
            }
        }

        if inserted > 0 {
            self.sink.mail_changed(account_id);
        }

        // Classification is best-effort: a broken LLM endpoint must not fail
        // the mail sync itself.
        self.set_status(account_id, |st| st.phase = SyncPhase::Classifying);
        if let Err(e) = self.classify_pending().await {
            tracing::warn!("classification cycle failed: {e}");
        }

        Ok(inserted)
    }

    /// Run the AI triage over stored-but-unclassified messages.
    /// Returns how many messages were classified.
    pub async fn classify_pending(&self) -> Result<u32> {
        let settings = self.store.ai_settings()?;
        if !settings.enabled || settings.api_key.is_empty() {
            return Ok(0);
        }
        let pending = self.store.unclassified(MAX_CLASSIFY)?;
        if pending.is_empty() {
            return Ok(0);
        }

        let channels = self.store.list_channels()?;
        let mut done = 0u32;
        let mut consecutive_failures = 0u32;

        for msg in pending {
            match ai::classify(&self.http, &settings, &msg).await {
                Ok(analysis) => {
                    consecutive_failures = 0;
                    self.store.set_analysis(&msg.id, &analysis)?;
                    self.act_on(&msg, &analysis, &settings, &channels).await;
                    self.sink.mail_changed(&msg.account_id);
                    done += 1;
                }
                Err(e) => {
                    tracing::warn!("classify {} failed: {e}", msg.id);
                    consecutive_failures += 1;
                    if consecutive_failures >= 3 {
                        // Endpoint is likely down/misconfigured — stop burning
                        // requests; messages stay pending for the next cycle.
                        return Err(Error::Ai(format!(
                            "连续 {consecutive_failures} 次分类失败，本轮中止: {e}"
                        )));
                    }
                }
            }
        }
        Ok(done)
    }

    /// Apply the per-category policy:
    /// - verification → popup with the code (+ opted-in channels)
    /// - important    → popup + external channels
    /// - spam         → silent; hard-delete when allowed and model says so
    /// - normal       → silent
    async fn act_on(
        &self,
        msg: &EmailMessage,
        analysis: &AiAnalysis,
        settings: &AiSettings,
        channels: &[NotifyChannel],
    ) {
        let account_email = self
            .store
            .get_account(&msg.account_id)
            .map(|a| a.email)
            .unwrap_or_default();

        let from = if msg.from_name.is_empty() {
            msg.from_addr.clone()
        } else {
            format!("{} <{}>", msg.from_name, msg.from_addr)
        };

        match analysis.category {
            Category::Verification | Category::Important => {
                self.sink.alert(&AlertEvent {
                    message_id: msg.id.clone(),
                    category: analysis.category,
                    account_email: account_email.clone(),
                    from: from.clone(),
                    subject: msg.subject.clone(),
                    summary: analysis.summary.clone(),
                    verification_code: analysis.verification_code.clone(),
                });
            }
            Category::Spam => {
                if analysis.deletable && settings.auto_delete_spam {
                    tracing::info!("auto-deleting worthless spam {}", msg.id);
                    self.delete_messages(std::slice::from_ref(&msg.id), true).await;
                }
            }
            Category::Normal => {}
        }

        let payload = NotifyPayload {
            category: analysis.category,
            account_email,
            from,
            subject: msg.subject.clone(),
            summary: analysis.summary.clone(),
            verification_code: analysis.verification_code.clone(),
            date: msg.date,
        };
        for ch in channels {
            if ch.enabled && ch.notify_categories.contains(&analysis.category) {
                if let Err(e) = notify::dispatch(&self.http, ch, &payload).await {
                    tracing::warn!("channel {} dispatch failed: {e}", ch.name);
                }
            }
        }
    }

    /// Delete messages locally (soft) and — when `on_server` — remotely too.
    /// Server-side deletion is best-effort: local state always wins.
    pub async fn delete_messages(&self, ids: &[String], on_server: bool) {
        // Resolve UIDs/accounts before the local delete hides the rows.
        let mut groups: HashMap<(String, String), Vec<String>> = HashMap::new();
        let mut touched_accounts: HashSet<String> = HashSet::new();
        for id in ids {
            if let Ok(msg) = self.store.get_message(id) {
                touched_accounts.insert(msg.account_id.clone());
                groups
                    .entry((msg.account_id.clone(), msg.folder.clone()))
                    .or_default()
                    .push(msg.uid.clone());
            }
        }

        if on_server {
            for ((account_id, folder), uids) in &groups {
                let account = match self.store.get_account(account_id) {
                    Ok(a) => a,
                    Err(_) => continue,
                };
                let res = match account.protocol {
                    Protocol::Imap => imap::delete(&account, folder, uids).await,
                    Protocol::Pop3 => pop3::delete(&account, uids).await,
                };
                if let Err(e) = res {
                    tracing::warn!("server delete on {account_id} failed (kept local delete): {e}");
                }
            }
        }
        if let Err(e) = self.store.soft_delete(ids) {
            tracing::error!("local delete failed: {e}");
        }
        for account_id in touched_accounts {
            self.sink.mail_changed(&account_id);
        }
    }

    /// Re-run classification for a single message (user action).
    pub async fn reclassify(&self, message_id: &str) -> Result<AiAnalysis> {
        let settings = self.store.ai_settings()?;
        if !settings.enabled || settings.api_key.is_empty() {
            return Err(Error::InvalidConfig("AI 过滤器未启用或未配置 API Key".into()));
        }
        let msg = self.store.get_message(message_id)?;
        let analysis = ai::classify(&self.http, &settings, &msg).await?;
        self.store.set_analysis(message_id, &analysis)?;
        let channels = self.store.list_channels()?;
        self.act_on(&msg, &analysis, &settings, &channels).await;
        self.sink.mail_changed(&msg.account_id);
        Ok(analysis)
    }
}
