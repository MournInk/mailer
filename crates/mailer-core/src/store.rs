//! SQLite persistence layer.
//!
//! A single `Store` owns the connection behind a mutex; all methods are
//! synchronous and fast (indexed lookups over local data). Call sites inside
//! async code should treat these as cheap; bulk ingestion happens on the sync
//! worker, never on the UI thread.

use std::path::Path;
use std::sync::Mutex;

use rusqlite::{params, Connection, OptionalExtension, Row};

use crate::error::{Error, Result};
use crate::types::*;

pub struct Store {
    conn: Mutex<Connection>,
}

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS accounts (
    id            TEXT PRIMARY KEY,
    label         TEXT NOT NULL,
    email         TEXT NOT NULL,
    protocol      TEXT NOT NULL,
    host          TEXT NOT NULL,
    port          INTEGER NOT NULL,
    username      TEXT NOT NULL,
    password      TEXT NOT NULL,
    tls           TEXT NOT NULL,
    smtp_json     TEXT,
    sync_interval INTEGER NOT NULL DEFAULT 300,
    color_hue     INTEGER NOT NULL DEFAULT 20,
    created_at    INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS messages (
    id           TEXT PRIMARY KEY,
    account_id   TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    folder       TEXT NOT NULL,
    uid          TEXT NOT NULL,
    message_id   TEXT,
    subject      TEXT NOT NULL DEFAULT '',
    from_name    TEXT NOT NULL DEFAULT '',
    from_addr    TEXT NOT NULL DEFAULT '',
    to_json      TEXT NOT NULL DEFAULT '[]',
    date         INTEGER NOT NULL,
    snippet      TEXT NOT NULL DEFAULT '',
    body_text    TEXT,
    body_html    TEXT,
    atts_json    TEXT NOT NULL DEFAULT '[]',
    unread       INTEGER NOT NULL DEFAULT 1,
    starred      INTEGER NOT NULL DEFAULT 0,
    deleted      INTEGER NOT NULL DEFAULT 0,
    category     TEXT,
    analysis_json TEXT,
    received_at  INTEGER NOT NULL,
    UNIQUE(account_id, folder, uid)
);

CREATE INDEX IF NOT EXISTS idx_messages_list
    ON messages(account_id, deleted, date DESC);
CREATE INDEX IF NOT EXISTS idx_messages_category
    ON messages(category, deleted, date DESC);
CREATE INDEX IF NOT EXISTS idx_messages_msgid
    ON messages(account_id, message_id);

CREATE TABLE IF NOT EXISTS channels (
    id          TEXT PRIMARY KEY,
    name        TEXT NOT NULL,
    kind        TEXT NOT NULL,
    enabled     INTEGER NOT NULL DEFAULT 1,
    categories  TEXT NOT NULL DEFAULT '["important"]',
    config_json TEXT NOT NULL DEFAULT '{}'
);

CREATE TABLE IF NOT EXISTS settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
"#;

impl Store {
    /// Open (and migrate) the database at `path`. Use `:memory:` in tests.
    pub fn open(path: &Path) -> Result<Store> {
        let conn = Connection::open(path)?;
        Self::init(conn)
    }

    pub fn open_in_memory() -> Result<Store> {
        let conn = Connection::open_in_memory()?;
        Self::init(conn)
    }

    fn init(conn: Connection) -> Result<Store> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.execute_batch(SCHEMA)?;
        Ok(Store { conn: Mutex::new(conn) })
    }

    fn with<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        f(&conn)
    }

    // -- accounts -----------------------------------------------------------

    pub fn insert_account(&self, a: &AccountConfig) -> Result<()> {
        self.with(|c| {
            c.execute(
                "INSERT INTO accounts
                 (id,label,email,protocol,host,port,username,password,tls,smtp_json,sync_interval,color_hue,created_at)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
                params![
                    a.id,
                    a.label,
                    a.email,
                    protocol_str(a.protocol),
                    a.host,
                    a.port,
                    a.username,
                    a.password,
                    tls_str(a.tls),
                    a.smtp.as_ref().map(|s| serde_json::to_string(s).unwrap_or_default()),
                    a.sync_interval_secs as i64,
                    a.color_hue as i64,
                    a.created_at,
                ],
            )?;
            Ok(())
        })
    }

    pub fn update_account(&self, a: &AccountConfig) -> Result<()> {
        self.with(|c| {
            let n = c.execute(
                "UPDATE accounts SET
                 label=?2,email=?3,protocol=?4,host=?5,port=?6,username=?7,password=?8,tls=?9,
                 smtp_json=?10,sync_interval=?11,color_hue=?12
                 WHERE id=?1",
                params![
                    a.id,
                    a.label,
                    a.email,
                    protocol_str(a.protocol),
                    a.host,
                    a.port,
                    a.username,
                    a.password,
                    tls_str(a.tls),
                    a.smtp.as_ref().map(|s| serde_json::to_string(s).unwrap_or_default()),
                    a.sync_interval_secs as i64,
                    a.color_hue as i64,
                ],
            )?;
            if n == 0 {
                return Err(Error::NotFound(format!("account {}", a.id)));
            }
            Ok(())
        })
    }

    pub fn delete_account(&self, id: &str) -> Result<()> {
        self.with(|c| {
            c.execute("DELETE FROM messages WHERE account_id=?1", params![id])?;
            let n = c.execute("DELETE FROM accounts WHERE id=?1", params![id])?;
            if n == 0 {
                return Err(Error::NotFound(format!("account {id}")));
            }
            Ok(())
        })
    }

    pub fn get_account(&self, id: &str) -> Result<AccountConfig> {
        self.with(|c| {
            c.query_row("SELECT * FROM accounts WHERE id=?1", params![id], row_to_account)
                .optional()?
                .ok_or_else(|| Error::NotFound(format!("account {id}")))
        })
    }

    pub fn list_accounts(&self) -> Result<Vec<AccountConfig>> {
        self.with(|c| {
            let mut stmt = c.prepare("SELECT * FROM accounts ORDER BY created_at ASC")?;
            let rows = stmt.query_map([], row_to_account)?;
            Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
        })
    }

    // -- messages -----------------------------------------------------------

    /// Insert a message unless a row with the same (account, folder, uid) —
    /// or same non-empty Message-ID — already exists. Returns true if inserted.
    pub fn insert_message(&self, m: &EmailMessage) -> Result<bool> {
        self.with(|c| {
            if let Some(mid) = m.message_id.as_deref().filter(|s| !s.is_empty()) {
                let dup: Option<String> = c
                    .query_row(
                        "SELECT id FROM messages WHERE account_id=?1 AND message_id=?2 LIMIT 1",
                        params![m.account_id, mid],
                        |r| r.get(0),
                    )
                    .optional()?;
                if dup.is_some() {
                    return Ok(false);
                }
            }
            let n = c.execute(
                "INSERT OR IGNORE INTO messages
                 (id,account_id,folder,uid,message_id,subject,from_name,from_addr,to_json,date,
                  snippet,body_text,body_html,atts_json,unread,starred,deleted,category,analysis_json,received_at)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,0,?17,?18,?19)",
                params![
                    m.id,
                    m.account_id,
                    m.folder,
                    m.uid,
                    m.message_id,
                    m.subject,
                    m.from_name,
                    m.from_addr,
                    serde_json::to_string(&m.to_addrs)?,
                    m.date,
                    m.snippet,
                    m.body_text,
                    m.body_html,
                    serde_json::to_string(&m.attachments)?,
                    m.unread as i64,
                    m.starred as i64,
                    m.category.map(|c| c.as_str()),
                    m.analysis.as_ref().map(|a| serde_json::to_string(a).unwrap_or_default()),
                    m.received_at,
                ],
            )?;
            Ok(n > 0)
        })
    }

    pub fn get_message(&self, id: &str) -> Result<EmailMessage> {
        self.with(|c| {
            c.query_row("SELECT * FROM messages WHERE id=?1 AND deleted=0", params![id], row_to_message)
                .optional()?
                .ok_or_else(|| Error::NotFound(format!("message {id}")))
        })
    }

    /// Known UIDs for one folder — used by sync to fetch only new mail.
    pub fn known_uids(&self, account_id: &str, folder: &str) -> Result<Vec<String>> {
        self.with(|c| {
            let mut stmt =
                c.prepare("SELECT uid FROM messages WHERE account_id=?1 AND folder=?2")?;
            let rows = stmt.query_map(params![account_id, folder], |r| r.get::<_, String>(0))?;
            Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
        })
    }

    /// Highest numeric UID seen in a folder (IMAP fast-path); None when empty.
    pub fn max_numeric_uid(&self, account_id: &str, folder: &str) -> Result<Option<u32>> {
        self.with(|c| {
            let v: Option<i64> = c.query_row(
                "SELECT MAX(CAST(uid AS INTEGER)) FROM messages WHERE account_id=?1 AND folder=?2",
                params![account_id, folder],
                |r| r.get(0),
            )?;
            Ok(v.map(|x| x as u32))
        })
    }

    pub fn query_messages(&self, q: &MessageQuery) -> Result<MessagePage> {
        self.with(|c| {
            let mut where_sql = String::from("deleted=0");
            let mut args: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

            if let Some(acc) = &q.account_id {
                args.push(Box::new(acc.clone()));
                where_sql.push_str(&format!(" AND account_id=?{}", args.len()));
            }
            if let Some(folder) = &q.folder {
                args.push(Box::new(folder.clone()));
                where_sql.push_str(&format!(" AND folder=?{}", args.len()));
            }
            if let Some(cat) = q.category {
                args.push(Box::new(cat.as_str().to_string()));
                where_sql.push_str(&format!(" AND category=?{}", args.len()));
            }
            if q.unread_only {
                where_sql.push_str(" AND unread=1");
            }
            if q.starred_only {
                where_sql.push_str(" AND starred=1");
            }
            if let Some(s) = q.search.as_deref().filter(|s| !s.trim().is_empty()) {
                let pat = format!("%{}%", s.trim().replace('%', "\\%").replace('_', "\\_"));
                for _ in 0..3 {
                    args.push(Box::new(pat.clone()));
                }
                let n = args.len();
                where_sql.push_str(&format!(
                    " AND (subject LIKE ?{} ESCAPE '\\' OR from_addr LIKE ?{} ESCAPE '\\' OR snippet LIKE ?{} ESCAPE '\\')",
                    n - 2,
                    n - 1,
                    n
                ));
            }

            let params_ref: Vec<&dyn rusqlite::types::ToSql> =
                args.iter().map(|b| b.as_ref()).collect();

            let total: u32 = c.query_row(
                &format!("SELECT COUNT(*) FROM messages WHERE {where_sql}"),
                params_ref.as_slice(),
                |r| r.get(0),
            )?;
            let unread: u32 = c.query_row(
                &format!("SELECT COUNT(*) FROM messages WHERE {where_sql} AND unread=1"),
                params_ref.as_slice(),
                |r| r.get(0),
            )?;

            let limit = if q.limit == 0 { 50 } else { q.limit.min(200) };
            let sql = format!(
                "SELECT id,account_id,folder,subject,from_name,from_addr,date,snippet,unread,starred,atts_json,category,analysis_json
                 FROM messages WHERE {where_sql}
                 ORDER BY date DESC LIMIT {limit} OFFSET {}",
                q.offset
            );
            let mut stmt = c.prepare(&sql)?;
            let rows = stmt.query_map(params_ref.as_slice(), row_to_header)?;
            let items = rows.collect::<std::result::Result<Vec<_>, _>>()?;
            Ok(MessagePage { items, total, unread })
        })
    }

    pub fn set_read(&self, ids: &[String], read: bool) -> Result<()> {
        self.with(|c| {
            let mut stmt = c.prepare("UPDATE messages SET unread=?2 WHERE id=?1")?;
            for id in ids {
                stmt.execute(params![id, (!read) as i64])?;
            }
            Ok(())
        })
    }

    pub fn set_starred(&self, id: &str, starred: bool) -> Result<()> {
        self.with(|c| {
            c.execute("UPDATE messages SET starred=?2 WHERE id=?1", params![id, starred as i64])?;
            Ok(())
        })
    }

    /// Soft delete locally. Server-side deletion is handled by the sync layer.
    pub fn soft_delete(&self, ids: &[String]) -> Result<()> {
        self.with(|c| {
            let mut stmt = c.prepare("UPDATE messages SET deleted=1 WHERE id=?1")?;
            for id in ids {
                stmt.execute(params![id])?;
            }
            Ok(())
        })
    }

    pub fn set_analysis(&self, id: &str, analysis: &AiAnalysis) -> Result<()> {
        self.with(|c| {
            c.execute(
                "UPDATE messages SET category=?2, analysis_json=?3 WHERE id=?1",
                params![id, analysis.category.as_str(), serde_json::to_string(analysis)?],
            )?;
            Ok(())
        })
    }

    /// Unclassified, non-deleted messages (oldest first) for the AI pipeline.
    pub fn unclassified(&self, limit: u32) -> Result<Vec<EmailMessage>> {
        self.with(|c| {
            let mut stmt = c.prepare(
                "SELECT * FROM messages WHERE category IS NULL AND deleted=0
                 ORDER BY date ASC LIMIT ?1",
            )?;
            let rows = stmt.query_map(params![limit as i64], row_to_message)?;
            Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
        })
    }

    /// Category counts for the sidebar (excluding deleted).
    pub fn category_counts(&self) -> Result<Vec<(String, u32, u32)>> {
        self.with(|c| {
            let mut stmt = c.prepare(
                "SELECT COALESCE(category,'pending'), COUNT(*), SUM(unread)
                 FROM messages WHERE deleted=0 GROUP BY category",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, u32>(1)?, r.get::<_, Option<u32>>(2)?.unwrap_or(0)))
            })?;
            Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
        })
    }

    // -- channels -----------------------------------------------------------

    pub fn upsert_channel(&self, ch: &NotifyChannel) -> Result<()> {
        self.with(|c| {
            c.execute(
                "INSERT INTO channels (id,name,kind,enabled,categories,config_json)
                 VALUES (?1,?2,?3,?4,?5,?6)
                 ON CONFLICT(id) DO UPDATE SET
                   name=excluded.name, kind=excluded.kind, enabled=excluded.enabled,
                   categories=excluded.categories, config_json=excluded.config_json",
                params![
                    ch.id,
                    ch.name,
                    ch.kind.as_str(),
                    ch.enabled as i64,
                    serde_json::to_string(&ch.notify_categories)?,
                    serde_json::to_string(&ch.config)?,
                ],
            )?;
            Ok(())
        })
    }

    pub fn delete_channel(&self, id: &str) -> Result<()> {
        self.with(|c| {
            let n = c.execute("DELETE FROM channels WHERE id=?1", params![id])?;
            if n == 0 {
                return Err(Error::NotFound(format!("channel {id}")));
            }
            Ok(())
        })
    }

    pub fn list_channels(&self) -> Result<Vec<NotifyChannel>> {
        self.with(|c| {
            let mut stmt = c.prepare("SELECT id,name,kind,enabled,categories,config_json FROM channels")?;
            let rows = stmt.query_map([], |r| {
                let kind_s: String = r.get(2)?;
                let cats_s: String = r.get(4)?;
                let cfg_s: String = r.get(5)?;
                Ok(NotifyChannel {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    kind: ChannelKind::parse(&kind_s).unwrap_or(ChannelKind::Webhook),
                    enabled: r.get::<_, i64>(3)? != 0,
                    notify_categories: serde_json::from_str(&cats_s).unwrap_or_default(),
                    config: serde_json::from_str(&cfg_s).unwrap_or(serde_json::Value::Null),
                })
            })?;
            Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
        })
    }

    pub fn get_channel(&self, id: &str) -> Result<NotifyChannel> {
        self.list_channels()?
            .into_iter()
            .find(|c| c.id == id)
            .ok_or_else(|| Error::NotFound(format!("channel {id}")))
    }

    // -- settings -----------------------------------------------------------

    pub fn get_setting(&self, key: &str) -> Result<Option<String>> {
        self.with(|c| {
            Ok(c.query_row("SELECT value FROM settings WHERE key=?1", params![key], |r| r.get(0))
                .optional()?)
        })
    }

    pub fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        self.with(|c| {
            c.execute(
                "INSERT INTO settings (key,value) VALUES (?1,?2)
                 ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                params![key, value],
            )?;
            Ok(())
        })
    }

    pub fn ai_settings(&self) -> Result<AiSettings> {
        match self.get_setting("ai")? {
            Some(json) => Ok(serde_json::from_str(&json).unwrap_or_default()),
            None => Ok(AiSettings::default()),
        }
    }

    pub fn set_ai_settings(&self, s: &AiSettings) -> Result<()> {
        self.set_setting("ai", &serde_json::to_string(s)?)
    }
}

// ---------------------------------------------------------------------------
// Row mappers
// ---------------------------------------------------------------------------

fn protocol_str(p: Protocol) -> &'static str {
    match p {
        Protocol::Imap => "imap",
        Protocol::Pop3 => "pop3",
    }
}

fn tls_str(t: TlsMode) -> &'static str {
    match t {
        TlsMode::Tls => "tls",
        TlsMode::Starttls => "starttls",
        TlsMode::None => "none",
    }
}

fn parse_tls(s: &str) -> TlsMode {
    match s {
        "starttls" => TlsMode::Starttls,
        "none" => TlsMode::None,
        _ => TlsMode::Tls,
    }
}

fn row_to_account(r: &Row<'_>) -> rusqlite::Result<AccountConfig> {
    let protocol: String = r.get("protocol")?;
    let tls: String = r.get("tls")?;
    let smtp_json: Option<String> = r.get("smtp_json")?;
    Ok(AccountConfig {
        id: r.get("id")?,
        label: r.get("label")?,
        email: r.get("email")?,
        protocol: if protocol == "pop3" { Protocol::Pop3 } else { Protocol::Imap },
        host: r.get("host")?,
        port: r.get::<_, i64>("port")? as u16,
        username: r.get("username")?,
        password: r.get("password")?,
        tls: parse_tls(&tls),
        smtp: smtp_json.and_then(|j| serde_json::from_str(&j).ok()),
        sync_interval_secs: r.get::<_, i64>("sync_interval")? as u64,
        color_hue: r.get::<_, i64>("color_hue")? as u16,
        created_at: r.get("created_at")?,
    })
}

fn row_to_message(r: &Row<'_>) -> rusqlite::Result<EmailMessage> {
    let to_json: String = r.get("to_json")?;
    let atts_json: String = r.get("atts_json")?;
    let category: Option<String> = r.get("category")?;
    let analysis_json: Option<String> = r.get("analysis_json")?;
    Ok(EmailMessage {
        id: r.get("id")?,
        account_id: r.get("account_id")?,
        folder: r.get("folder")?,
        uid: r.get("uid")?,
        message_id: r.get("message_id")?,
        subject: r.get("subject")?,
        from_name: r.get("from_name")?,
        from_addr: r.get("from_addr")?,
        to_addrs: serde_json::from_str(&to_json).unwrap_or_default(),
        date: r.get("date")?,
        snippet: r.get("snippet")?,
        body_text: r.get("body_text")?,
        body_html: r.get("body_html")?,
        attachments: serde_json::from_str(&atts_json).unwrap_or_default(),
        unread: r.get::<_, i64>("unread")? != 0,
        starred: r.get::<_, i64>("starred")? != 0,
        category: category.as_deref().and_then(Category::parse),
        analysis: analysis_json.and_then(|j| serde_json::from_str(&j).ok()),
        received_at: r.get("received_at")?,
    })
}

fn row_to_header(r: &Row<'_>) -> rusqlite::Result<MessageHeader> {
    let atts_json: String = r.get(10)?;
    let category: Option<String> = r.get(11)?;
    let analysis_json: Option<String> = r.get(12)?;
    let analysis: Option<AiAnalysis> = analysis_json.and_then(|j| serde_json::from_str(&j).ok());
    Ok(MessageHeader {
        id: r.get(0)?,
        account_id: r.get(1)?,
        folder: r.get(2)?,
        subject: r.get(3)?,
        from_name: r.get(4)?,
        from_addr: r.get(5)?,
        date: r.get(6)?,
        snippet: r.get(7)?,
        unread: r.get::<_, i64>(8)? != 0,
        starred: r.get::<_, i64>(9)? != 0,
        has_attachments: atts_json.len() > 2,
        category: category.as_deref().and_then(Category::parse),
        verification_code: analysis.as_ref().and_then(|a| a.verification_code.clone()),
        summary: analysis.map(|a| a.summary),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_account() -> AccountConfig {
        AccountConfig {
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
        }
    }

    fn sample_message(id: &str, uid: &str) -> EmailMessage {
        EmailMessage {
            id: id.into(),
            account_id: "acc1".into(),
            folder: "INBOX".into(),
            uid: uid.into(),
            message_id: Some(format!("<{id}@example.com>")),
            subject: "Hello".into(),
            from_name: "Alice".into(),
            from_addr: "alice@example.com".into(),
            to_addrs: vec!["me@example.com".into()],
            date: 1000,
            snippet: "Hi there".into(),
            body_text: Some("Hi there".into()),
            body_html: None,
            attachments: vec![],
            unread: true,
            starred: false,
            category: None,
            analysis: None,
            received_at: 1000,
        }
    }

    #[test]
    fn account_roundtrip() {
        let s = Store::open_in_memory().unwrap();
        let a = sample_account();
        s.insert_account(&a).unwrap();
        let got = s.get_account("acc1").unwrap();
        assert_eq!(got.email, a.email);
        assert_eq!(got.tls, TlsMode::Tls);
        assert_eq!(s.list_accounts().unwrap().len(), 1);
    }

    #[test]
    fn message_dedup_by_uid_and_message_id() {
        let s = Store::open_in_memory().unwrap();
        s.insert_account(&sample_account()).unwrap();
        assert!(s.insert_message(&sample_message("m1", "100")).unwrap());
        // same uid → skipped
        assert!(!s.insert_message(&sample_message("m2", "100")).unwrap());
        // different uid but same Message-ID → skipped
        let mut m3 = sample_message("m1", "101");
        m3.id = "m3".into();
        assert!(!s.insert_message(&m3).unwrap());
    }

    #[test]
    fn query_and_flags() {
        let s = Store::open_in_memory().unwrap();
        s.insert_account(&sample_account()).unwrap();
        for i in 0..5 {
            let mut m = sample_message(&format!("m{i}"), &format!("{i}"));
            m.message_id = Some(format!("<m{i}@x>"));
            m.date = 1000 + i;
            s.insert_message(&m).unwrap();
        }
        let page = s.query_messages(&MessageQuery::default()).unwrap();
        assert_eq!(page.total, 5);
        assert_eq!(page.unread, 5);
        assert_eq!(page.items[0].id, "m4"); // newest first

        s.set_read(&["m4".into()], true).unwrap();
        let page = s.query_messages(&MessageQuery::default()).unwrap();
        assert_eq!(page.unread, 4);

        s.soft_delete(&["m4".into()]).unwrap();
        let page = s.query_messages(&MessageQuery::default()).unwrap();
        assert_eq!(page.total, 4);
    }

    #[test]
    fn analysis_and_unclassified() {
        let s = Store::open_in_memory().unwrap();
        s.insert_account(&sample_account()).unwrap();
        s.insert_message(&sample_message("m1", "1")).unwrap();
        assert_eq!(s.unclassified(10).unwrap().len(), 1);
        let a = AiAnalysis {
            category: Category::Verification,
            confidence: 0.98,
            summary: "GitHub 登录验证码".into(),
            verification_code: Some("482913".into()),
            deletable: false,
            reason: "OTP email".into(),
        };
        s.set_analysis("m1", &a).unwrap();
        assert!(s.unclassified(10).unwrap().is_empty());
        let m = s.get_message("m1").unwrap();
        assert_eq!(m.category, Some(Category::Verification));
        assert_eq!(m.analysis.unwrap().verification_code.unwrap(), "482913");
    }

    #[test]
    fn channels_roundtrip() {
        let s = Store::open_in_memory().unwrap();
        let ch = NotifyChannel {
            id: "ch1".into(),
            name: "TG".into(),
            kind: ChannelKind::Telegram,
            enabled: true,
            notify_categories: vec![Category::Important, Category::Verification],
            config: serde_json::json!({"botToken": "t", "chatId": "1"}),
        };
        s.upsert_channel(&ch).unwrap();
        let list = s.list_channels().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].kind, ChannelKind::Telegram);
        assert_eq!(list[0].notify_categories.len(), 2);
        s.delete_channel("ch1").unwrap();
        assert!(s.list_channels().unwrap().is_empty());
    }

    #[test]
    fn settings_roundtrip() {
        let s = Store::open_in_memory().unwrap();
        assert!(!s.ai_settings().unwrap().enabled);
        let mut ai = AiSettings::default();
        ai.enabled = true;
        ai.api_key = "sk-test".into();
        s.set_ai_settings(&ai).unwrap();
        assert!(s.ai_settings().unwrap().enabled);
    }
}
