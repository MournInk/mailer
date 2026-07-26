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
/// Ceiling on one fetch round trip. Neither protocol client sets its own —
/// a server that accepts the connection and then goes silent would otherwise
/// pin the account's `in_flight` slot forever, and that account would never
/// sync again. Generous enough for 50 messages over a slow link.
const FETCH_TIMEOUT: Duration = Duration::from_secs(180);
/// Ceiling on a server-side delete round trip.
const DELETE_TIMEOUT: Duration = Duration::from_secs(60);
/// How long one IDLE waits before re-issuing it. RFC 2177 allows 29 minutes;
/// nine keeps the connection well inside the idle timeout of the NATs and
/// load balancers that sit between a laptop and a mail server.
const IDLE_WINDOW: Duration = Duration::from_secs(9 * 60);
/// How often the watcher looks for accounts it is not watching yet.
const WATCH_RESCAN: Duration = Duration::from_secs(60);
/// Backoff bounds for a connection that keeps failing. The floor is short
/// because the common cause is a network that just came back.
const WATCH_BACKOFF_MIN: Duration = Duration::from_secs(5);
const WATCH_BACKOFF_MAX: Duration = Duration::from_secs(10 * 60);
/// Room on top of `IDLE_WINDOW` for the connect, login and teardown around it.
const WATCH_SLACK: Duration = Duration::from_secs(60);

/// Whether an account can be watched live.
///
/// POP3 has no IDLE and no equivalent — the protocol has no way to tell a client
/// anything it did not ask for. And `sync_interval_secs == 0` is the user
/// switching automatic mail off, which a held-open connection would be exactly
/// the opposite of.
fn watchable(account: &AccountConfig) -> bool {
    account.protocol == Protocol::Imap && account.sync_interval_secs > 0
}

/// Bound one protocol round trip. A timeout surfaces as a normal error so the
/// account's status shows why it stalled instead of sitting silently in sync.
async fn with_timeout<T>(
    limit: Duration,
    what: &str,
    fut: impl std::future::Future<Output = Result<T>>,
) -> Result<Result<T>> {
    match tokio::time::timeout(limit, fut).await {
        Ok(v) => Ok(v),
        Err(_) => Err(Error::Other(format!(
            "{what}超时（{} 秒内服务器无响应）",
            limit.as_secs()
        ))),
    }
}

/// `YYYY-MM-DD` in the user's own timezone.
///
/// Local, not UTC: the heatmap is a row of days as the user experienced them, and
/// a mail that arrived at 08:00 in Shanghai belongs to that morning, not to the
/// previous evening.
pub fn local_day(ms: i64) -> String {
    use chrono::TimeZone;
    chrono::Local
        .timestamp_millis_opt(ms)
        .single()
        .map(|t| t.format("%Y-%m-%d").to_string())
        .unwrap_or_default()
}

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
    /// Held for the duration of one classification cycle. The pending queue is
    /// shared by every account, so the cycle has to be too.
    classifying: tokio::sync::Mutex<()>,
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
            classifying: tokio::sync::Mutex::new(()),
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
    ///
    /// Returns a future the host is expected to spawn on its own runtime
    /// (`tauri::async_runtime::spawn`) — Tauri's `setup` hook runs outside a
    /// tokio runtime context, so this must not call `tokio::spawn` itself.
    pub async fn run_scheduler(self: Arc<Self>) {
        loop {
            tokio::time::sleep(TICK).await;
            let accounts = match self.store.list_accounts() {
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
                    let last = self.last_attempt.lock().unwrap();
                    match last.get(&acc.id) {
                        Some(t) => now - t >= (acc.sync_interval_secs as i64) * 1000,
                        None => true,
                    }
                };
                if due {
                    // Inside the running future, so a runtime is in context.
                    let engine = Arc::clone(&self);
                    tokio::spawn(async move {
                        if let Err(e) = engine.sync_account(&acc.id).await {
                            tracing::warn!("sync {} failed: {e}", acc.id);
                        }
                    });
                }
            }
        }
    }

    /// Hold an IMAP connection open per account so mail arrives instead of being
    /// collected.
    ///
    /// The scheduler above still runs, and still has to: IDLE is not universal
    /// (POP3 has no equivalent, and some IMAP servers do not offer it), a dropped
    /// connection is normal on a laptop that sleeps or a phone that changes
    /// network, and a timer is the thing that notices. This makes the common case
    /// immediate and leaves the timer as the floor.
    ///
    /// One task per account, each reconnecting on its own. Spawned as a future for
    /// the same reason as `run_scheduler`: Tauri's setup hook has no runtime in
    /// context.
    pub async fn run_watchers(self: Arc<Self>) {
        // Accounts already being watched, so a rescan does not start a second
        // connection for one that is merely mid-reconnect.
        let mut watching: HashSet<String> = HashSet::new();
        loop {
            let accounts = self.store.list_accounts().unwrap_or_default();
            for acc in accounts {
                if !watchable(&acc) {
                    continue;
                }
                if !watching.insert(acc.id.clone()) {
                    continue;
                }
                let engine = Arc::clone(&self);
                tokio::spawn(engine.watch_account(acc.id.clone()));
            }
            // Long enough that adding an account is picked up without the loop
            // being a poll in its own right.
            tokio::time::sleep(WATCH_RESCAN).await;
            // Forget accounts that went away, so a re-added one is watched again.
            let live: HashSet<String> = self
                .store
                .list_accounts()
                .unwrap_or_default()
                .into_iter()
                .map(|a| a.id)
                .collect();
            watching.retain(|id| live.contains(id));
        }
    }

    /// One account's live connection, for as long as the account exists.
    ///
    /// Failure is expected here — a laptop closes, a network changes, a server
    /// recycles the connection — so the loop backs off rather than giving up, and
    /// gives up only on the two things that will not fix themselves: the account
    /// being gone, and a server that does not do IDLE.
    async fn watch_account(self: Arc<Self>, account_id: String) {
        let mut backoff = WATCH_BACKOFF_MIN;
        loop {
            let Ok(account) = self.store.get_account(&account_id) else {
                tracing::debug!("watch: {account_id} 已不存在，停止监听");
                return;
            };
            if account.sync_interval_secs == 0 {
                return;
            }

            // The wait bounds itself, but only once it is waiting: a server that
            // accepts the connection and then goes silent mid-login would hold
            // this task forever, and this account would never be live again.
            let waited = with_timeout(
                IDLE_WINDOW + WATCH_SLACK,
                "等待新邮件",
                imap::wait_for_mail(&account, IDLE_WINDOW),
            )
            .await
            .and_then(|r| r);

            match waited {
                Ok(imap::Watch::Changed) => {
                    backoff = WATCH_BACKOFF_MIN;
                    tracing::debug!("watch: {} 有新动静，立即同步", account.email);
                    if let Err(e) = self.sync_account(&account_id).await {
                        tracing::warn!("watch: {} 同步失败: {e}", account.email);
                    }
                }
                Ok(imap::Watch::Quiet) => {
                    // Go straight back to waiting. Re-issuing IDLE is what the
                    // RFC asks for anyway.
                    backoff = WATCH_BACKOFF_MIN;
                }
                Ok(imap::Watch::Unsupported) => {
                    tracing::info!("watch: {} 不支持 IDLE，改由定时同步负责", account.email);
                    return;
                }
                Err(e) => {
                    // Includes a rejected login: retrying with a backoff is
                    // right, because the user may be about to fix the password.
                    tracing::debug!(
                        "watch: {} 连接中断，{} 秒后重试: {e}",
                        account.email,
                        backoff.as_secs()
                    );
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(WATCH_BACKOFF_MAX);
                }
            }
        }
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
            Protocol::Imap => {
                let expected = self.store.uid_validity(account_id, "INBOX")?;
                let fetched =
                    with_timeout(FETCH_TIMEOUT, "收信", imap::fetch_new(&account, &known, MAX_FETCH, expected))
                        .await??;
                if fetched.uids_reset {
                    // The mailbox was rebuilt server-side: retire the stale UIDs
                    // so future syncs re-diff cleanly. Messages survive, and
                    // Message-ID dedup keeps re-fetched mail from duplicating.
                    let n = self.store.clear_uids(account_id, "INBOX")?;
                    tracing::warn!("UIDVALIDITY reset on {account_id}: retired {n} stale UIDs");
                }
                self.store
                    .set_uid_validity(account_id, "INBOX", fetched.uid_validity)?;
                fetched.mails
            }
            Protocol::Pop3 => {
                with_timeout(FETCH_TIMEOUT, "收信", pop3::fetch_new(&account, &known, MAX_FETCH))
                    .await??
            }
        };

        self.set_status(account_id, |st| st.phase = SyncPhase::Fetching);

        let mut inserted = 0u32;
        for raw in &raws {
            let id = uuid::Uuid::new_v4().to_string();
            match parse::parse_mail(id, account_id, raw, now_ms()) {
                Ok(msg) => {
                    if self.store.insert_message(&msg)? {
                        inserted += 1;
                        // What this mail wanted to load, recorded now so the
                        // privacy figures cover everything that arrived rather
                        // than only what somebody opened.
                        self.scan_trackers(&msg);
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

        // Leave import mode only once the backlog is genuinely drained: the
        // server had nothing new AND nothing is still waiting for triage.
        // Flipping earlier would let the tail of the first import arrive as
        // popups, which is exactly what import mode exists to prevent.
        if !self.store.initial_import_done(account_id)? {
            let pending = self.store.unclassified_count(account_id)?;
            if inserted == 0 && pending == 0 {
                self.store.set_initial_import_done(account_id, true)?;
                tracing::info!("initial import finished for {account_id}; alerts are live");
            }
        }

        Ok(inserted)
    }

    /// Record what one message wanted to load from elsewhere.
    ///
    /// Best-effort and non-fatal: the scan is a report about the mail, and a
    /// mailbox that refused to accept mail because a report failed would be a
    /// worse trade than a missing report. Text-only mail is marked scanned
    /// without looking, because there is nothing in it to find.
    pub fn scan_trackers(&self, msg: &EmailMessage) {
        let hits = match msg.body_html.as_deref().filter(|h| !h.is_empty()) {
            Some(html) => crate::trackers::scan(html),
            None => Vec::new(),
        };
        let day = local_day(msg.date);
        let outcome = self
            .store
            .put_trackers(&msg.id, &day, &hits)
            .and_then(|()| self.store.mark_scanned(&msg.id));
        if let Err(e) = outcome {
            tracing::warn!("tracker scan not recorded for {}: {e}", msg.id);
        }
    }

    /// Run the AI triage over stored-but-unclassified messages.
    /// Returns how many messages were classified.
    pub async fn classify_pending(&self) -> Result<u32> {
        let settings = self.store.ai_settings()?;
        if !settings.is_configured() {
            return Ok(0);
        }

        // `unclassified` is global, not per account, so two accounts syncing at
        // once would both pick up the same rows: the same message classified
        // twice, billed twice, and — worse — announced twice, pushed to every
        // channel twice, and auto-deleted by whichever cycle got there second.
        // Whoever holds this runs the cycle; the other returns and the rows are
        // still pending on the next tick.
        let Ok(_cycle) = self.classifying.try_lock() else {
            tracing::debug!("classification already in flight; leaving the backlog for next tick");
            return Ok(0);
        };

        let pending = self.store.unclassified(MAX_CLASSIFY)?;
        if pending.is_empty() {
            return Ok(0);
        }

        let channels = self.store.list_channels()?;
        let mut done = 0u32;
        let mut consecutive_failures = 0u32;
        // One lookup per account per cycle rather than per message.
        let mut import_mode: HashMap<String, bool> = HashMap::new();

        for msg in pending {
            let importing = match import_mode.get(&msg.account_id) {
                Some(v) => *v,
                None => {
                    let v = !self.store.initial_import_done(&msg.account_id).unwrap_or(true);
                    import_mode.insert(msg.account_id.clone(), v);
                    v
                }
            };
            match ai::classify(&self.http, &settings, &msg).await {
                Ok(analysis) => {
                    consecutive_failures = 0;
                    self.store.set_analysis(&msg.id, &analysis)?;
                    self.act_on(&msg, &analysis, &settings, &channels, importing).await;
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

    /// Apply the per-category policy.
    ///
    /// Live mail:
    /// - verification → popup with the code (+ opted-in channels)
    /// - important    → popup + external channels
    /// - spam         → silent; hard-delete when allowed and model says so
    /// - normal       → silent
    ///
    /// Import mode (`import` = true) covers the first pass over a mailbox the
    /// user just connected. That backlog was already dealt with in whatever
    /// client they used before, so announcing it is noise — a thousand-message
    /// inbox would otherwise produce a thousand popups. Nothing alerts, nothing
    /// goes to external channels, spam is filtered out, and verification mail
    /// is deleted outright: a code from last month is worthless and the codes
    /// themselves are the one thing worth not leaving lying around.
    async fn act_on(
        &self,
        msg: &EmailMessage,
        analysis: &AiAnalysis,
        settings: &AiSettings,
        channels: &[NotifyChannel],
        import: bool,
    ) {
        if import {
            match analysis.category {
                Category::Spam => {
                    // The user asked for spam to be cleared out of the backlog.
                    // Server-side deletion still needs their explicit opt-in.
                    self.delete_messages(
                        std::slice::from_ref(&msg.id),
                        settings.auto_delete_spam && analysis.deletable,
                    )
                    .await;
                }
                Category::Verification => {
                    self.delete_messages(std::slice::from_ref(&msg.id), false).await;
                }
                Category::Important | Category::Normal => {}
            }
            return;
        }

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
    ///
    /// A server delete that fails leaves its messages **untouched**: they stay
    /// visible, and the returned report names them. The UI hides a row the
    /// moment the user asks, so a silent failure here would look exactly like a
    /// success and the mail would reappear at the next sync with no explanation.
    /// Local-only deletion cannot fail this way and always reports success.
    pub async fn delete_messages(&self, ids: &[String], on_server: bool) -> DeleteReport {
        // Resolve UIDs/accounts before the local delete hides the rows. A
        // message that no longer resolves is already gone, which is the outcome
        // being asked for, so it counts as deleted.
        let mut groups: HashMap<(String, String), Vec<(String, String)>> = HashMap::new();
        let mut touched_accounts: HashSet<String> = HashSet::new();
        let mut report = DeleteReport::default();

        for id in ids {
            match self.store.get_message(id) {
                Ok(msg) => {
                    touched_accounts.insert(msg.account_id.clone());
                    groups
                        .entry((msg.account_id.clone(), msg.folder.clone()))
                        .or_default()
                        .push((id.clone(), msg.uid.clone()));
                }
                Err(_) => report.deleted.push(id.clone()),
            }
        }

        for ((account_id, folder), targets) in &groups {
            let (ids_here, uids): (Vec<String>, Vec<String>) = targets.iter().cloned().unzip();
            if !on_server {
                report.deleted.extend(ids_here);
                continue;
            }

            let outcome = match self.store.get_account(account_id) {
                Ok(account) => match account.protocol {
                    Protocol::Imap => {
                        with_timeout(DELETE_TIMEOUT, "删除", imap::delete(&account, folder, &uids))
                            .await
                            .and_then(|r| r)
                    }
                    Protocol::Pop3 => {
                        with_timeout(DELETE_TIMEOUT, "删除", pop3::delete(&account, &uids))
                            .await
                            .and_then(|r| r)
                    }
                },
                Err(e) => Err(e),
            };

            match outcome {
                Ok(()) => report.deleted.extend(ids_here),
                Err(e) => {
                    tracing::warn!("server delete on {account_id} failed, keeping the mail: {e}");
                    report.failed.extend(ids_here);
                    // The first failure is the one worth showing; a second
                    // account's timeout says nothing new.
                    report.error.get_or_insert_with(|| e.to_string());
                }
            }
        }

        if let Err(e) = self.store.soft_delete(&report.deleted) {
            tracing::error!("local delete failed: {e}");
        }
        for account_id in touched_accounts {
            self.sink.mail_changed(&account_id);
        }
        report
    }

    /// Re-run classification for a single message (user action).
    pub async fn reclassify(&self, message_id: &str) -> Result<AiAnalysis> {
        let settings = self.store.ai_settings()?;
        if !settings.is_configured() {
            return Err(Error::InvalidConfig(
                "AI 过滤器未启用，或尚未填写接口地址与模型名称".into(),
            ));
        }
        let msg = self.store.get_message(message_id)?;
        let analysis = ai::classify(&self.http, &settings, &msg).await?;
        self.store.set_analysis(message_id, &analysis)?;
        let channels = self.store.list_channels()?;
        self.act_on(&msg, &analysis, &settings, &channels, false).await;
        self.sink.mail_changed(&msg.account_id);
        Ok(analysis)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Records what the engine pushed to the UI.
    #[derive(Default)]
    struct RecordingSink {
        changed: Mutex<Vec<String>>,
    }

    impl EventSink for Arc<RecordingSink> {
        fn alert(&self, _: &AlertEvent) {}
        fn mail_changed(&self, account_id: &str) {
            self.changed.lock().unwrap().push(account_id.to_string());
        }
        fn sync_status(&self, _: &SyncStatus) {}
    }

    fn seeded_store() -> Arc<Store> {
        seeded_store_with(1)
    }

    /// An account plus `count` unclassified messages.
    fn seeded_store_with(count: usize) -> Arc<Store> {
        let store = Store::open_in_memory().unwrap();
        store
            .insert_account(&AccountConfig {
                id: "acc1".into(),
                label: "Test".into(),
                email: "me@example.com".into(),
                protocol: Protocol::Imap,
                host: "imap.example.com".into(),
                port: 993,
                username: "me@example.com".into(),
                password: "secret".into(),
                tls: TlsMode::Tls,
                smtp: None,
                sync_interval_secs: 0,
                color_hue: 20,
                created_at: 1,
            })
            .unwrap();
        for i in 0..count {
            store
                .insert_message(&EmailMessage {
                    id: format!("m{}", i + 1),
                    account_id: "acc1".into(),
                    folder: "INBOX".into(),
                    uid: format!("{}", i + 1),
                    message_id: Some(format!("<m{}@example.com>", i + 1)),
                    subject: "Hello".into(),
                    from_name: "Alice".into(),
                    from_addr: "alice@example.com".into(),
                    to_addrs: vec!["me@example.com".into()],
                    date: 1000 + i as i64,
                    snippet: "Hi".into(),
                    body_text: Some("Hi".into()),
                    body_html: None,
                    attachments: vec![],
                    unread: true,
                    starred: false,
                    category: None,
                    analysis: None,
                    received_at: 1000,
                })
                .unwrap();
        }
        Arc::new(store)
    }

    /// An unconfigured AI filter must be a no-op, not an error — the mail sync
    /// has to keep working before the user configures an LLM.
    /// Only some accounts can be live, and getting that wrong means either a
    /// pointless reconnect loop against POP3 or a connection the user switched off.
    #[test]
    fn only_imap_accounts_with_auto_sync_are_watched() {
        let acc = AccountConfig {
            id: "acc1".into(),
            label: "Test".into(),
            email: "me@example.com".into(),
            protocol: Protocol::Imap,
            host: "imap.example.com".into(),
            port: 993,
            username: "me@example.com".into(),
            password: "secret".into(),
            tls: TlsMode::Tls,
            smtp: None,
            sync_interval_secs: 300,
            color_hue: 20,
            created_at: 1,
        };
        assert!(watchable(&acc));

        // POP3 cannot be told anything it did not ask for.
        let mut pop = acc.clone();
        pop.protocol = Protocol::Pop3;
        assert!(!watchable(&pop));

        // Automatic mail switched off.
        let mut manual = acc.clone();
        manual.sync_interval_secs = 0;
        assert!(!watchable(&manual));
    }

    #[tokio::test]
    async fn classify_pending_is_noop_without_ai() {
        let store = seeded_store();
        let sink = Arc::new(RecordingSink::default());
        let engine = SyncEngine::new(Arc::clone(&store), Box::new(Arc::clone(&sink)));

        assert_eq!(engine.classify_pending().await.unwrap(), 0);

        // Enabled but with nothing to call: still a no-op, not a failed request.
        let ai = AiSettings {
            enabled: true,
            api_base: String::new(),
            model: String::new(),
            ..AiSettings::default()
        };
        store.set_ai_settings(&ai).unwrap();
        assert_eq!(engine.classify_pending().await.unwrap(), 0);

        // The message stays pending for a later, configured run.
        assert_eq!(store.unclassified(10).unwrap().len(), 1);
    }

    /// A local model has no API key, and treating that as "unconfigured" made
    /// the whole AI filter a silent no-op for anyone running Ollama. The cycle
    /// must now actually attempt the endpoint the user configured.
    ///
    /// The base URL here is rejected by `ai::validate` before any socket is
    /// opened, so what the test observes is the attempt itself: a skipped cycle
    /// answers `Ok(0)`, and only a cycle that really called out can accumulate
    /// the three consecutive failures that abort the round.
    #[tokio::test]
    async fn a_keyless_endpoint_is_still_attempted() {
        let store = seeded_store_with(3);
        let sink = Arc::new(RecordingSink::default());
        let engine = SyncEngine::new(Arc::clone(&store), Box::new(Arc::clone(&sink)));

        store
            .set_ai_settings(&AiSettings {
                enabled: true,
                api_key: String::new(),
                api_base: "127.0.0.1:11434/v1".into(), // no scheme: refused locally
                model: "qwen2.5:7b".into(),
                ..AiSettings::default()
            })
            .unwrap();

        let err = engine.classify_pending().await.unwrap_err();
        assert!(matches!(err, Error::Ai(_)), "got {err:?}");
        // Nothing was classified on a failed round, so nothing is lost.
        assert_eq!(store.unclassified(10).unwrap().len(), 3);
    }

    /// Two accounts syncing at once must not both run the cycle: `unclassified`
    /// is global, so the second would re-classify the first's messages — billed
    /// twice, announced twice, and auto-deleted by whichever finished second.
    #[tokio::test]
    async fn only_one_classification_cycle_runs_at_a_time() {
        let store = seeded_store();
        let sink = Arc::new(RecordingSink::default());
        let engine = SyncEngine::new(Arc::clone(&store), Box::new(Arc::clone(&sink)));

        store
            .set_ai_settings(&AiSettings {
                enabled: true,
                api_base: "127.0.0.1:11434/v1".into(),
                model: "qwen2.5:7b".into(),
                ..AiSettings::default()
            })
            .unwrap();

        // Held by hand: the second caller has to find the door shut and leave
        // the backlog alone rather than duplicate the work.
        let held = engine.classifying.lock().await;
        assert_eq!(engine.classify_pending().await.unwrap(), 0);
        assert_eq!(store.unclassified(10).unwrap().len(), 1);
        drop(held);
    }

    /// Local-only delete must hide the message and tell the UI which account
    /// changed — resolved before the row is hidden, not after.
    #[tokio::test]
    async fn local_delete_hides_message_and_reports_account() {
        let store = seeded_store();
        let sink = Arc::new(RecordingSink::default());
        let engine = SyncEngine::new(Arc::clone(&store), Box::new(Arc::clone(&sink)));

        let report = engine.delete_messages(&["m1".to_string()], false).await;

        assert!(report.ok(), "a local delete cannot fail: {report:?}");
        assert_eq!(report.deleted, vec!["m1".to_string()]);
        assert!(store.get_message("m1").is_err());
        assert_eq!(store.query_messages(&MessageQuery::default()).unwrap().total, 0);
        assert_eq!(sink.changed.lock().unwrap().as_slice(), ["acc1"]);
    }

    /// A server that refuses the delete still has the mail. Hiding it locally
    /// anyway would look like success and then undo itself at the next sync, so
    /// the message stays and the report names it.
    #[tokio::test]
    async fn a_refused_server_delete_keeps_the_message() {
        let store = seeded_store();
        let sink = Arc::new(RecordingSink::default());
        let engine = SyncEngine::new(Arc::clone(&store), Box::new(Arc::clone(&sink)));

        // The seeded account points at a host that does not resolve, so the
        // IMAP round trip fails without a server to talk to.
        let report = engine.delete_messages(&["m1".to_string()], true).await;

        assert!(!report.ok(), "the delete did not happen");
        assert_eq!(report.failed, vec!["m1".to_string()]);
        assert!(report.deleted.is_empty());
        assert!(report.error.is_some(), "a failure needs a reason to show");
        // Still there, still readable.
        assert!(store.get_message("m1").is_ok());
        assert_eq!(store.query_messages(&MessageQuery::default()).unwrap().total, 1);
    }

    /// An id that no longer resolves is already in the state the caller asked
    /// for, so it counts as deleted rather than as a failure to report.
    #[tokio::test]
    async fn deleting_a_message_that_is_already_gone_succeeds() {
        let store = seeded_store();
        let sink = Arc::new(RecordingSink::default());
        let engine = SyncEngine::new(Arc::clone(&store), Box::new(Arc::clone(&sink)));

        let report = engine.delete_messages(&["ghost".to_string()], true).await;

        assert!(report.ok(), "{report:?}");
        assert_eq!(report.deleted, vec!["ghost".to_string()]);
    }

    /// A server that accepts the connection and then goes silent must not pin
    /// the sync slot: the wait has to end, and it has to end as an error the
    /// account status can show.
    #[tokio::test(start_paused = true)]
    async fn a_silent_server_times_out_instead_of_hanging() {
        let never = async {
            // Longer than any real round trip; only the timeout can end this.
            tokio::time::sleep(Duration::from_secs(86_400)).await;
            Ok::<(), Error>(())
        };
        let outcome = with_timeout(FETCH_TIMEOUT, "收信", never).await;
        match outcome {
            Err(Error::Other(msg)) => assert!(msg.contains("超时"), "got {msg}"),
            other => panic!("expected a timeout error, got {other:?}"),
        }
    }

    /// A slow-but-alive server keeps its result — the guard must not truncate
    /// a transfer that was going to finish.
    #[tokio::test(start_paused = true)]
    async fn a_slow_but_responsive_server_still_succeeds() {
        let slow = async {
            tokio::time::sleep(FETCH_TIMEOUT - Duration::from_secs(1)).await;
            Ok::<u8, Error>(7)
        };
        assert_eq!(with_timeout(FETCH_TIMEOUT, "收信", slow).await.unwrap().unwrap(), 7);
    }

    /// Reclassify surfaces a clear configuration error instead of calling out
    /// to an unconfigured endpoint.
    #[tokio::test]
    async fn reclassify_requires_configured_ai() {
        let store = seeded_store();
        let sink = Arc::new(RecordingSink::default());
        let engine = SyncEngine::new(store, Box::new(Arc::clone(&sink)));

        let err = engine.reclassify("m1").await.unwrap_err();
        assert!(matches!(err, Error::InvalidConfig(_)), "got {err:?}");
    }
}
