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

-- Per-folder IMAP UIDVALIDITY. When a server rebuilds a mailbox it bumps this
-- and reissues UIDs from 1, which would make our stored UIDs alias unrelated
-- new mail. Tracking it lets sync discard the stale UID set instead of
-- silently skipping messages forever (RFC 3501 §2.3.1.1).
CREATE TABLE IF NOT EXISTS folder_state (
    account_id   TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    folder       TEXT NOT NULL,
    uid_validity INTEGER NOT NULL,
    PRIMARY KEY (account_id, folder)
);

-- Embedding index over stored mail. One row per (message, model): changing the
-- embedding model invalidates nothing, it just adds vectors under a new name,
-- so switching models and switching back does not cost a re-index.
CREATE TABLE IF NOT EXISTS message_vectors (
    message_id TEXT NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
    model      TEXT NOT NULL,
    dim        INTEGER NOT NULL,
    -- Little-endian f32 array. Cosine similarity runs in Rust; at personal
    -- mailbox scale a linear scan beats carrying a vector-database dependency.
    vec        BLOB NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (message_id, model)
);

-- Deep index over starred mail.
--
-- `message_vectors` holds one vector per message, built from the subject, the
-- sender and the opening of the body — enough to find a message, never enough to
-- answer a question from inside a long one. Starring is the user's own statement
-- that a message matters, so those get chunked and embedded whole, and the chunk
-- text is kept alongside its vector so a hit can quote the passage that matched
-- rather than the top of the mail.
--
-- Separate table rather than a `chunk` column on `message_vectors`: that would
-- mean rewriting an existing primary key, and the two indexes answer different
-- questions anyway (which message, versus which passage).
CREATE TABLE IF NOT EXISTS message_chunks (
    message_id TEXT NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
    model      TEXT NOT NULL,
    chunk      INTEGER NOT NULL,
    text       TEXT NOT NULL,
    dim        INTEGER NOT NULL,
    vec        BLOB NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (message_id, model, chunk)
);

-- What the assistant has learned about the user.
--
-- `status` rather than deletion: a memory that stopped being true is history the
-- user is entitled to see, and `superseded_by` is the thread back to what
-- replaced it. `norm_text` is the normalised form, so re-remembering the same
-- sentence is an indexed lookup instead of a model call.
CREATE TABLE IF NOT EXISTS memories (
    id            TEXT PRIMARY KEY,
    kind          TEXT NOT NULL,
    text          TEXT NOT NULL,
    source        TEXT,
    created_at    INTEGER NOT NULL,
    updated_at    INTEGER NOT NULL,
    status        TEXT NOT NULL DEFAULT 'active',
    origin        TEXT NOT NULL DEFAULT 'assistant',
    superseded_by TEXT,
    valid_from    INTEGER,
    valid_to      INTEGER,
    norm_text     TEXT NOT NULL DEFAULT '',
    use_count     INTEGER NOT NULL DEFAULT 0,
    last_used_at  INTEGER
);

CREATE INDEX IF NOT EXISTS idx_memories_live
    ON memories(status, kind, updated_at DESC);
CREATE INDEX IF NOT EXISTS idx_memories_norm
    ON memories(norm_text);

CREATE TABLE IF NOT EXISTS memory_vectors (
    memory_id  TEXT NOT NULL REFERENCES memories(id) ON DELETE CASCADE,
    model      TEXT NOT NULL,
    dim        INTEGER NOT NULL,
    vec        BLOB NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (memory_id, model)
);

-- No foreign key on purpose: the trail of what was believed and when has to
-- outlive the row it describes.
CREATE TABLE IF NOT EXISTS memory_events (
    id          TEXT PRIMARY KEY,
    memory_id   TEXT NOT NULL,
    op          TEXT NOT NULL,
    before_text TEXT,
    after_text  TEXT,
    reason      TEXT,
    created_at  INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_memory_events_mem
    ON memory_events(memory_id, created_at DESC);

CREATE TABLE IF NOT EXISTS conversations (
    id         TEXT PRIMARY KEY,
    title      TEXT NOT NULL DEFAULT '',
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS chat_turns (
    id              TEXT PRIMARY KEY,
    conversation_id TEXT NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
    role            TEXT NOT NULL,
    content         TEXT NOT NULL,
    tool_calls_json TEXT NOT NULL DEFAULT '[]',
    reasoning       TEXT,
    citations_json  TEXT NOT NULL DEFAULT '[]',
    created_at      INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_chat_turns_conv
    ON chat_turns(conversation_id, created_at ASC);
"#;

/// Columns added after the first release. `ALTER TABLE ... ADD COLUMN` on an
/// existing column is an error, not a no-op, so each one is attempted and its
/// duplicate-column failure ignored.
const MIGRATIONS: &[&str] = &[
    // 0 until the first full pass over a newly added mailbox finishes. While
    // it is 0, triage runs in import mode: no popups for a backlog the user
    // has already dealt with elsewhere.
    "ALTER TABLE accounts ADD COLUMN initial_import_done INTEGER NOT NULL DEFAULT 0",
    // Reasoning models emit a chain of thought worth keeping with the answer.
    "ALTER TABLE chat_turns ADD COLUMN reasoning TEXT",
    // The memory table grew a lifecycle: superseded rows stay as history, and a
    // normalised form makes re-remembering the same sentence a lookup.
    "ALTER TABLE memories ADD COLUMN status TEXT NOT NULL DEFAULT 'active'",
    "ALTER TABLE memories ADD COLUMN origin TEXT NOT NULL DEFAULT 'assistant'",
    "ALTER TABLE memories ADD COLUMN superseded_by TEXT",
    "ALTER TABLE memories ADD COLUMN valid_from INTEGER",
    "ALTER TABLE memories ADD COLUMN valid_to INTEGER",
    "ALTER TABLE memories ADD COLUMN norm_text TEXT NOT NULL DEFAULT ''",
    "ALTER TABLE memories ADD COLUMN use_count INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE memories ADD COLUMN last_used_at INTEGER",
];

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
        for stmt in MIGRATIONS {
            // Already applied on an existing database; anything else is real.
            match conn.execute(stmt, []) {
                Ok(_) => {}
                Err(e) if e.to_string().contains("duplicate column name") => {}
                Err(e) => return Err(e.into()),
            }
        }
        Ok(Store { conn: Mutex::new(conn) })
    }

    fn with<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        f(&conn)
    }

    /// Run `f` inside one transaction, committing only if it succeeds.
    ///
    /// Every bare `execute` is its own implicit transaction, which in WAL mode
    /// means a commit — and a disk write — per statement. For the loops below
    /// (marking a multi-selection read, deleting a batch) that turned one user
    /// action into one commit per message.
    fn with_tx<T>(&self, f: impl FnOnce(&rusqlite::Transaction<'_>) -> Result<T>) -> Result<T> {
        let mut conn = self.conn.lock().expect("store mutex poisoned");
        let tx = conn.transaction()?;
        let out = f(&tx)?;
        tx.commit()?;
        Ok(out)
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

    /// Several messages by id, in the order the ids were given.
    ///
    /// Ids that no longer resolve — deleted between a vector scan and here — are
    /// simply absent from the result, exactly as a per-id lookup would find.
    /// One prepared statement for the batch: the retriever asks for up to two
    /// hundred candidates at a time, and as individual queries that was two
    /// hundred prepares and two hundred trips through the connection mutex.
    pub fn get_messages(&self, ids: &[String]) -> Result<Vec<EmailMessage>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        self.with(|c| {
            let mut stmt = c.prepare("SELECT * FROM messages WHERE id=?1 AND deleted=0")?;
            let mut out = Vec::with_capacity(ids.len());
            for id in ids {
                if let Some(msg) = stmt.query_row(params![id], row_to_message).optional()? {
                    out.push(msg);
                }
            }
            Ok(out)
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

    /// Last known IMAP UIDVALIDITY for a folder, if we have ever synced it.
    pub fn uid_validity(&self, account_id: &str, folder: &str) -> Result<Option<u32>> {
        self.with(|c| {
            Ok(c.query_row(
                "SELECT uid_validity FROM folder_state WHERE account_id=?1 AND folder=?2",
                params![account_id, folder],
                |r| r.get::<_, i64>(0),
            )
            .optional()?
            .map(|v| v as u32))
        })
    }

    pub fn set_uid_validity(&self, account_id: &str, folder: &str, value: u32) -> Result<()> {
        self.with(|c| {
            c.execute(
                "INSERT INTO folder_state (account_id, folder, uid_validity) VALUES (?1,?2,?3)
                 ON CONFLICT(account_id, folder) DO UPDATE SET uid_validity=excluded.uid_validity",
                params![account_id, folder, value as i64],
            )?;
            Ok(())
        })
    }

    /// Forget every stored UID for a folder after a UIDVALIDITY change. The
    /// messages stay — only their now-meaningless server UIDs are cleared, so
    /// the next sync re-diffs from scratch and `message_id` dedup prevents
    /// duplicates.
    pub fn clear_uids(&self, account_id: &str, folder: &str) -> Result<usize> {
        self.with(|c| {
            // A UID must stay unique per (account, folder); prefix the row id so
            // the cleared values cannot collide with real server UIDs.
            Ok(c.execute(
                "UPDATE messages SET uid = 'stale:' || id
                 WHERE account_id=?1 AND folder=?2 AND uid NOT LIKE 'stale:%'",
                params![account_id, folder],
            )?)
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
                // The backslash goes first: escaping it after the wildcards
                // would also escape the escapes we just added, and a search for
                // a literal "\" would silently match nothing.
                let pat = format!("%{}%", escape_like(s.trim()));
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

            // Both totals from one scan. As two queries the same rows were
            // visited twice per page — and with a `search` filter that is two
            // full table scans for one keystroke.
            let (total, unread): (u32, u32) = c.query_row(
                &format!(
                    "SELECT COUNT(*), COALESCE(SUM(unread),0) FROM messages WHERE {where_sql}"
                ),
                params_ref.as_slice(),
                |r| Ok((r.get(0)?, r.get(1)?)),
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
        if ids.is_empty() {
            return Ok(());
        }
        self.with_tx(|c| {
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
        if ids.is_empty() {
            return Ok(());
        }
        self.with_tx(|c| {
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

    // -- import state -------------------------------------------------------

    /// False while the first full pass over a newly added mailbox is still
    /// pending. Triage consults this to decide whether to alert.
    pub fn initial_import_done(&self, account_id: &str) -> Result<bool> {
        self.with(|c| {
            let v: Option<i64> = c
                .query_row(
                    "SELECT initial_import_done FROM accounts WHERE id=?1",
                    params![account_id],
                    |r| r.get(0),
                )
                .optional()?;
            Ok(v.unwrap_or(1) != 0)
        })
    }

    pub fn set_initial_import_done(&self, account_id: &str, done: bool) -> Result<()> {
        self.with(|c| {
            c.execute(
                "UPDATE accounts SET initial_import_done=?2 WHERE id=?1",
                params![account_id, done as i64],
            )?;
            Ok(())
        })
    }

    /// Messages on this account still waiting for their first classification.
    pub fn unclassified_count(&self, account_id: &str) -> Result<u32> {
        self.with(|c| {
            Ok(c.query_row(
                "SELECT COUNT(*) FROM messages
                 WHERE account_id=?1 AND category IS NULL AND deleted=0",
                params![account_id],
                |r| r.get(0),
            )?)
        })
    }

    // -- embedding index ----------------------------------------------------

    /// Store one message vector. Replaces any previous vector for this model.
    pub fn put_vector(&self, message_id: &str, model: &str, vec: &[f32], now: i64) -> Result<()> {
        let bytes = encode_vector(vec);
        self.with(|c| {
            c.execute(
                "INSERT INTO message_vectors (message_id, model, dim, vec, created_at)
                 VALUES (?1,?2,?3,?4,?5)
                 ON CONFLICT(message_id, model) DO UPDATE SET
                   dim=excluded.dim, vec=excluded.vec, created_at=excluded.created_at",
                params![message_id, model, vec.len() as i64, bytes, now],
            )?;
            Ok(())
        })
    }

    /// Every stored vector for `model`, as (message_id, vector).
    pub fn all_vectors(&self, model: &str) -> Result<Vec<(String, Vec<f32>)>> {
        self.with(|c| {
            let mut stmt = c.prepare(
                "SELECT v.message_id, v.vec FROM message_vectors v
                 JOIN messages m ON m.id = v.message_id
                 WHERE v.model = ?1 AND m.deleted = 0",
            )?;
            let rows = stmt.query_map(params![model], |r| {
                let id: String = r.get(0)?;
                let bytes: Vec<u8> = r.get(1)?;
                Ok((id, decode_vector(&bytes)))
            })?;
            Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
        })
    }

    /// Non-deleted messages with no vector under `model`, oldest first.
    pub fn messages_missing_vectors(&self, model: &str, limit: u32) -> Result<Vec<EmailMessage>> {
        self.with(|c| {
            let mut stmt = c.prepare(
                "SELECT m.* FROM messages m
                 LEFT JOIN message_vectors v
                   ON v.message_id = m.id AND v.model = ?1
                 WHERE m.deleted = 0 AND v.message_id IS NULL
                 ORDER BY m.date ASC LIMIT ?2",
            )?;
            let rows = stmt.query_map(params![model, limit as i64], row_to_message)?;
            Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
        })
    }

    /// (indexed, total) for `model`, counting only live messages.
    pub fn vector_counts(&self, model: &str) -> Result<(u32, u32)> {
        self.with(|c| {
            let total: u32 =
                c.query_row("SELECT COUNT(*) FROM messages WHERE deleted=0", [], |r| r.get(0))?;
            let indexed: u32 = c.query_row(
                "SELECT COUNT(*) FROM message_vectors v
                 JOIN messages m ON m.id = v.message_id
                 WHERE v.model=?1 AND m.deleted=0",
                params![model],
                |r| r.get(0),
            )?;
            Ok((indexed, total))
        })
    }

    pub fn clear_vectors(&self, model: &str) -> Result<usize> {
        self.with(|c| {
            let chunks = c.execute("DELETE FROM message_chunks WHERE model=?1", params![model])?;
            let whole = c.execute("DELETE FROM message_vectors WHERE model=?1", params![model])?;
            Ok(chunks + whole)
        })
    }

    // -- deep index over starred mail ---------------------------------------

    /// Replace every chunk of one message under `model`.
    ///
    /// All or nothing: a half-written message would answer questions from the
    /// paragraphs that happened to make it in, and look complete doing it.
    pub fn put_chunks(
        &self,
        message_id: &str,
        model: &str,
        chunks: &[(String, Vec<f32>)],
        now: i64,
    ) -> Result<()> {
        self.with_tx(|c| {
            c.execute(
                "DELETE FROM message_chunks WHERE message_id=?1 AND model=?2",
                params![message_id, model],
            )?;
            let mut stmt = c.prepare(
                "INSERT INTO message_chunks (message_id, model, chunk, text, dim, vec, created_at)
                 VALUES (?1,?2,?3,?4,?5,?6,?7)",
            )?;
            for (i, (text, vec)) in chunks.iter().enumerate() {
                stmt.execute(params![
                    message_id,
                    model,
                    i as i64,
                    text,
                    vec.len() as i64,
                    encode_vector(vec),
                    now
                ])?;
            }
            Ok(())
        })
    }

    /// Every stored chunk vector for `model`, as (message_id, chunk, vector).
    ///
    /// Without the text: a scan needs only the vectors, and the passages of every
    /// starred message would be megabytes to carry through it. The text of the
    /// few chunks that survive ranking is fetched by [`Store::chunk_text`].
    pub fn all_chunk_vectors(&self, model: &str) -> Result<Vec<(String, i64, Vec<f32>)>> {
        self.with(|c| {
            let mut stmt = c.prepare(
                "SELECT k.message_id, k.chunk, k.vec FROM message_chunks k
                 JOIN messages m ON m.id = k.message_id
                 WHERE k.model = ?1 AND m.deleted = 0",
            )?;
            let rows = stmt.query_map(params![model], |r| {
                let bytes: Vec<u8> = r.get(2)?;
                Ok((r.get(0)?, r.get(1)?, decode_vector(&bytes)))
            })?;
            Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
        })
    }

    pub fn chunk_text(&self, message_id: &str, model: &str, chunk: i64) -> Result<Option<String>> {
        self.with(|c| {
            Ok(c.query_row(
                "SELECT text FROM message_chunks
                 WHERE message_id=?1 AND model=?2 AND chunk=?3",
                params![message_id, model, chunk],
                |r| r.get(0),
            )
            .optional()?)
        })
    }

    /// Starred, non-deleted messages with no chunks under `model`, newest first.
    ///
    /// Newest first, unlike the whole-message backfill: a message starred a
    /// minute ago is the one about to be asked about.
    pub fn starred_missing_chunks(&self, model: &str, limit: u32) -> Result<Vec<EmailMessage>> {
        self.with(|c| {
            let mut stmt = c.prepare(
                "SELECT m.* FROM messages m
                 WHERE m.starred = 1 AND m.deleted = 0
                   AND NOT EXISTS (
                     SELECT 1 FROM message_chunks k
                     WHERE k.message_id = m.id AND k.model = ?1
                   )
                 ORDER BY m.date DESC LIMIT ?2",
            )?;
            let rows = stmt.query_map(params![model, limit as i64], row_to_message)?;
            Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
        })
    }

    /// (starred messages with chunks, starred messages) for `model`.
    pub fn chunk_counts(&self, model: &str) -> Result<(u32, u32)> {
        self.with(|c| {
            let total: u32 = c.query_row(
                "SELECT COUNT(*) FROM messages WHERE starred=1 AND deleted=0",
                [],
                |r| r.get(0),
            )?;
            let indexed: u32 = c.query_row(
                "SELECT COUNT(DISTINCT k.message_id) FROM message_chunks k
                 JOIN messages m ON m.id = k.message_id
                 WHERE k.model=?1 AND m.starred=1 AND m.deleted=0",
                params![model],
                |r| r.get(0),
            )?;
            Ok((indexed, total))
        })
    }

    /// Drop the deep index for messages that are no longer starred.
    ///
    /// Un-starring is a statement too. Keeping the chunks would leave the
    /// retriever quietly favouring passages the user has since dismissed.
    pub fn prune_unstarred_chunks(&self) -> Result<usize> {
        self.with(|c| {
            Ok(c.execute(
                "DELETE FROM message_chunks WHERE message_id IN (
                   SELECT k.message_id FROM message_chunks k
                   LEFT JOIN messages m ON m.id = k.message_id
                   WHERE m.id IS NULL OR m.starred = 0 OR m.deleted = 1
                 )",
                [],
            )?)
        })
    }

    // -- memory -------------------------------------------------------------

    /// Write one memory, creating it or replacing it in place.
    ///
    /// `norm_text` is supplied by the caller rather than derived here: the
    /// normalisation rules belong with the reconciler that also uses them to
    /// find duplicates, and having two implementations would eventually mean two
    /// different answers to "is this the same sentence".
    pub fn put_memory(&self, m: &MemoryEntry, norm_text: &str) -> Result<()> {
        self.with(|c| {
            c.execute(
                "INSERT INTO memories
                   (id, kind, text, source, created_at, updated_at,
                    status, origin, superseded_by, valid_from, valid_to, norm_text, use_count)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)
                 ON CONFLICT(id) DO UPDATE SET
                   kind=excluded.kind, text=excluded.text,
                   source=excluded.source, updated_at=excluded.updated_at,
                   status=excluded.status, origin=excluded.origin,
                   superseded_by=excluded.superseded_by,
                   valid_from=excluded.valid_from, valid_to=excluded.valid_to,
                   norm_text=excluded.norm_text",
                params![
                    m.id,
                    memory_kind_str(m.kind),
                    m.text,
                    m.source,
                    m.created_at,
                    m.updated_at,
                    memory_status_str(m.status),
                    memory_origin_str(m.origin),
                    m.superseded_by,
                    m.valid_from,
                    m.valid_to,
                    norm_text,
                    m.use_count,
                ],
            )?;
            Ok(())
        })
    }

    /// Every memory still believed, newest first.
    pub fn list_memories(&self) -> Result<Vec<MemoryEntry>> {
        self.with(|c| {
            let mut stmt = c.prepare(&format!(
                "{MEMORY_COLS} WHERE status='active' ORDER BY updated_at DESC"
            ))?;
            let rows = stmt.query_map([], row_to_memory)?;
            Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
        })
    }

    /// Retired memories, newest first. History for the settings screen; never
    /// injected into a prompt.
    pub fn superseded_memories(&self, limit: u32) -> Result<Vec<MemoryEntry>> {
        self.with(|c| {
            let mut stmt = c.prepare(&format!(
                "{MEMORY_COLS} WHERE status='superseded' ORDER BY updated_at DESC LIMIT ?1"
            ))?;
            let rows = stmt.query_map(params![limit as i64], row_to_memory)?;
            Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
        })
    }

    /// One memory by id whatever its status, or `None` when it is gone.
    pub fn get_memory(&self, id: &str) -> Result<Option<MemoryEntry>> {
        self.with(|c| {
            Ok(c.query_row(&format!("{MEMORY_COLS} WHERE id=?1"), params![id], row_to_memory)
                .optional()?)
        })
    }

    /// The active memory whose normalised text is exactly this, if there is one.
    ///
    /// The fast path of the write side: a model that re-remembers the same
    /// preference every session costs one indexed lookup instead of a
    /// reconciliation call.
    pub fn memory_by_norm(&self, norm_text: &str) -> Result<Option<MemoryEntry>> {
        self.with(|c| {
            Ok(c.query_row(
                &format!("{MEMORY_COLS} WHERE status='active' AND norm_text=?1 LIMIT 1"),
                params![norm_text],
                row_to_memory,
            )
            .optional()?)
        })
    }

    /// Active preferences, most recently useful first.
    ///
    /// Preferences are about *how* to answer, so they apply to a question whose
    /// words match nothing — which is why they are fetched by recency of use
    /// rather than by relevance.
    pub fn standing_preferences(&self, limit: u32) -> Result<Vec<MemoryEntry>> {
        self.with(|c| {
            let mut stmt = c.prepare(&format!(
                "{MEMORY_COLS} WHERE status='active' AND kind='preference'
                 ORDER BY COALESCE(last_used_at, updated_at) DESC LIMIT ?1"
            ))?;
            let rows = stmt.query_map(params![limit as i64], row_to_memory)?;
            Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
        })
    }

    pub fn delete_memory(&self, id: &str) -> Result<()> {
        self.with(|c| {
            let n = c.execute("DELETE FROM memories WHERE id=?1", params![id])?;
            if n == 0 {
                return Err(Error::NotFound(format!("memory {id}")));
            }
            Ok(())
        })
    }

    /// Retire `old_id` in favour of `new_id`, in one transaction with nothing
    /// deleted. `at` becomes the moment the old statement stopped being true.
    pub fn supersede_memory(&self, old_id: &str, new_id: &str, at: i64) -> Result<()> {
        self.with_tx(|tx| {
            tx.execute(
                "UPDATE memories
                    SET status='superseded', superseded_by=?2, valid_to=?3, updated_at=?3
                  WHERE id=?1",
                params![old_id, new_id, at],
            )?;
            // A retired memory must not keep matching a semantic search.
            tx.execute("DELETE FROM memory_vectors WHERE memory_id=?1", params![old_id])?;
            Ok(())
        })
    }

    /// Record that these memories were put in front of the model. This is the
    /// signal eviction ranks by, so it is worth one write per answer.
    pub fn touch_memories(&self, ids: &[String], at: i64) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        self.with_tx(|tx| {
            let mut stmt = tx.prepare(
                "UPDATE memories SET use_count=use_count+1, last_used_at=?2 WHERE id=?1",
            )?;
            for id in ids {
                stmt.execute(params![id, at])?;
            }
            Ok(())
        })
    }

    /// Free-text lookup over active memories, so the assistant can pull only
    /// what is relevant instead of pasting the whole table into every prompt.
    pub fn search_memories(&self, needle: &str, limit: u32) -> Result<Vec<MemoryEntry>> {
        let needle = needle.trim();
        if needle.is_empty() {
            let mut all = self.list_memories()?;
            all.truncate(limit as usize);
            return Ok(all);
        }
        self.with(|c| {
            let pat = format!("%{}%", escape_like(needle));
            let mut stmt = c.prepare(&format!(
                "{MEMORY_COLS} WHERE status='active' AND text LIKE ?1 ESCAPE '\\'
                 ORDER BY updated_at DESC LIMIT ?2"
            ))?;
            let rows = stmt.query_map(params![pat, limit as i64], row_to_memory)?;
            Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
        })
    }

    // -- memory vectors -----------------------------------------------------

    pub fn put_memory_vector(
        &self,
        memory_id: &str,
        model: &str,
        vec: &[f32],
        now: i64,
    ) -> Result<()> {
        self.with(|c| {
            c.execute(
                "INSERT INTO memory_vectors (memory_id, model, dim, vec, created_at)
                 VALUES (?1,?2,?3,?4,?5)
                 ON CONFLICT(memory_id, model) DO UPDATE SET
                   dim=excluded.dim, vec=excluded.vec, created_at=excluded.created_at",
                params![memory_id, model, vec.len() as i64, encode_vector(vec), now],
            )?;
            Ok(())
        })
    }

    /// Vectors of every active memory under one model. Small by construction —
    /// the table is capped — so a linear scan is the whole search.
    pub fn active_memory_vectors(&self, model: &str) -> Result<Vec<(String, Vec<f32>)>> {
        self.with(|c| {
            let mut stmt = c.prepare(
                "SELECT v.memory_id, v.vec FROM memory_vectors v
                 JOIN memories m ON m.id = v.memory_id
                 WHERE v.model=?1 AND m.status='active'",
            )?;
            let rows = stmt.query_map(params![model], |r| {
                let id: String = r.get(0)?;
                let blob: Vec<u8> = r.get(1)?;
                Ok((id, decode_vector(&blob)))
            })?;
            Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
        })
    }

    // -- memory audit trail --------------------------------------------------

    pub fn append_memory_event(&self, e: &MemoryEvent) -> Result<()> {
        self.with(|c| {
            c.execute(
                "INSERT INTO memory_events
                   (id, memory_id, op, before_text, after_text, reason, created_at)
                 VALUES (?1,?2,?3,?4,?5,?6,?7)",
                params![e.id, e.memory_id, e.op, e.before_text, e.after_text, e.reason, e.created_at],
            )?;
            Ok(())
        })
    }

    /// The trail for one memory, or for everything when `memory_id` is `None`.
    pub fn memory_events(&self, memory_id: Option<&str>, limit: u32) -> Result<Vec<MemoryEvent>> {
        self.with(|c| {
            let to_event = |r: &Row<'_>| {
                Ok(MemoryEvent {
                    id: r.get(0)?,
                    memory_id: r.get(1)?,
                    op: r.get(2)?,
                    before_text: r.get(3)?,
                    after_text: r.get(4)?,
                    reason: r.get(5)?,
                    created_at: r.get(6)?,
                })
            };
            const COLS: &str =
                "SELECT id, memory_id, op, before_text, after_text, reason, created_at
                 FROM memory_events";
            // One `?` bound either way, so the two branches differ only in SQL —
            // and the rows have to be collected while the statement is alive.
            let (sql, bind) = match memory_id {
                Some(id) => (
                    format!("{COLS} WHERE memory_id=?2 ORDER BY created_at DESC LIMIT ?1"),
                    Some(id.to_string()),
                ),
                None => (format!("{COLS} ORDER BY created_at DESC LIMIT ?1"), None),
            };
            let mut stmt = c.prepare(&sql)?;
            let rows = match &bind {
                Some(id) => stmt
                    .query_map(params![limit as i64, id], to_event)?
                    .collect::<std::result::Result<Vec<_>, _>>()?,
                None => stmt
                    .query_map(params![limit as i64], to_event)?
                    .collect::<std::result::Result<Vec<_>, _>>()?,
            };
            Ok(rows)
        })
    }

    // -- memory housekeeping -------------------------------------------------

    pub fn count_active_memories(&self) -> Result<u32> {
        self.with(|c| {
            let n: i64 =
                c.query_row("SELECT COUNT(*) FROM memories WHERE status='active'", [], |r| {
                    r.get(0)
                })?;
            Ok(n as u32)
        })
    }

    /// Retire the least useful assistant-written facts until `keep` remain.
    ///
    /// Preferences and anything the user typed are never evicted: they are small
    /// in number, they are what personalisation actually is, and losing a
    /// hand-written line to an automatic sweep would be indefensible. Ranking is
    /// by use, then by how long ago that use was.
    pub fn evict_memories(&self, keep: u32, at: i64) -> Result<u32> {
        self.with_tx(|tx| {
            let mut stmt = tx.prepare(
                "SELECT id FROM memories
                  WHERE status='active' AND origin='assistant' AND kind<>'preference'
                  ORDER BY use_count ASC, COALESCE(last_used_at, updated_at) ASC",
            )?;
            let ranked: Vec<String> = stmt
                .query_map([], |r| r.get::<_, String>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            drop(stmt);

            let total: i64 =
                tx.query_row("SELECT COUNT(*) FROM memories WHERE status='active'", [], |r| {
                    r.get(0)
                })?;
            let excess = (total - keep as i64).max(0) as usize;
            let doomed = &ranked[..excess.min(ranked.len())];

            for id in doomed {
                tx.execute(
                    "UPDATE memories SET status='superseded', valid_to=?2, updated_at=?2
                      WHERE id=?1",
                    params![id, at],
                )?;
                tx.execute("DELETE FROM memory_vectors WHERE memory_id=?1", params![id])?;
            }
            Ok(doomed.len() as u32)
        })
    }

    /// Delete superseded rows retired before `before`, and trim the trail to its
    /// newest `keep_events` entries.
    pub fn prune_memory_history(&self, before: i64, keep_events: u32) -> Result<u32> {
        self.with_tx(|tx| {
            let gone = tx.execute(
                "DELETE FROM memories WHERE status='superseded' AND updated_at < ?1",
                params![before],
            )?;
            tx.execute(
                "DELETE FROM memory_events WHERE id NOT IN
                   (SELECT id FROM memory_events ORDER BY created_at DESC LIMIT ?1)",
                params![keep_events as i64],
            )?;
            Ok(gone as u32)
        })
    }

    // -- conversations ------------------------------------------------------

    pub fn upsert_conversation(&self, c0: &Conversation) -> Result<()> {
        self.with(|c| {
            c.execute(
                "INSERT INTO conversations (id, title, created_at, updated_at)
                 VALUES (?1,?2,?3,?4)
                 ON CONFLICT(id) DO UPDATE SET
                   title=excluded.title, updated_at=excluded.updated_at",
                params![c0.id, c0.title, c0.created_at, c0.updated_at],
            )?;
            Ok(())
        })
    }

    /// Whether a conversation row exists.
    ///
    /// `chat_turns` has a foreign key onto `conversations`, so every message has
    /// to answer this first. Answering it by listing conversations and scanning
    /// them meant reading the whole table to look at one primary key.
    pub fn conversation_exists(&self, id: &str) -> Result<bool> {
        self.with(|c| {
            Ok(c.query_row("SELECT 1 FROM conversations WHERE id=?1", params![id], |_| Ok(()))
                .optional()?
                .is_some())
        })
    }

    pub fn list_conversations(&self, limit: u32) -> Result<Vec<Conversation>> {
        self.with(|c| {
            let mut stmt = c.prepare(
                "SELECT id, title, created_at, updated_at FROM conversations
                 ORDER BY updated_at DESC LIMIT ?1",
            )?;
            let rows = stmt.query_map(params![limit as i64], |r| {
                Ok(Conversation {
                    id: r.get(0)?,
                    title: r.get(1)?,
                    created_at: r.get(2)?,
                    updated_at: r.get(3)?,
                })
            })?;
            Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
        })
    }

    pub fn delete_conversation(&self, id: &str) -> Result<()> {
        self.with(|c| {
            c.execute("DELETE FROM chat_turns WHERE conversation_id=?1", params![id])?;
            c.execute("DELETE FROM conversations WHERE id=?1", params![id])?;
            Ok(())
        })
    }

    pub fn append_turn(&self, t: &ChatTurn) -> Result<()> {
        self.with(|c| {
            c.execute(
                "INSERT INTO chat_turns
                 (id, conversation_id, role, content, tool_calls_json, citations_json, created_at, reasoning)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
                params![
                    t.id,
                    t.conversation_id,
                    chat_role_str(t.role),
                    t.content,
                    serde_json::to_string(&t.tool_calls)?,
                    serde_json::to_string(&t.citations)?,
                    t.created_at,
                    t.reasoning,
                ],
            )?;
            c.execute(
                "UPDATE conversations SET updated_at=?2 WHERE id=?1",
                params![t.conversation_id, t.created_at],
            )?;
            Ok(())
        })
    }

    pub fn conversation_turns(&self, conversation_id: &str) -> Result<Vec<ChatTurn>> {
        self.with(|c| {
            let mut stmt = c.prepare(
                "SELECT id, conversation_id, role, content, tool_calls_json, citations_json, created_at, reasoning
                 FROM chat_turns WHERE conversation_id=?1 ORDER BY created_at ASC",
            )?;
            let rows = stmt.query_map(params![conversation_id], |r| {
                let role: String = r.get(2)?;
                let tools: String = r.get(4)?;
                let cites: String = r.get(5)?;
                Ok(ChatTurn {
                    id: r.get(0)?,
                    conversation_id: r.get(1)?,
                    role: parse_chat_role(&role),
                    content: r.get(3)?,
                    tool_calls: serde_json::from_str(&tools).unwrap_or_default(),
                    citations: serde_json::from_str(&cites).unwrap_or_default(),
                    created_at: r.get(6)?,
                    reasoning: r.get(7)?,
                })
            })?;
            Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
        })
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

    pub fn embedding_settings(&self) -> Result<EmbeddingSettings> {
        match self.get_setting("embedding")? {
            Some(json) => Ok(serde_json::from_str(&json).unwrap_or_default()),
            None => Ok(EmbeddingSettings::default()),
        }
    }

    pub fn set_embedding_settings(&self, s: &EmbeddingSettings) -> Result<()> {
        self.set_setting("embedding", &serde_json::to_string(s)?)
    }

    pub fn reranker_settings(&self) -> Result<RerankerSettings> {
        match self.get_setting("reranker")? {
            Some(json) => Ok(serde_json::from_str(&json).unwrap_or_default()),
            None => Ok(RerankerSettings::default()),
        }
    }

    pub fn set_reranker_settings(&self, s: &RerankerSettings) -> Result<()> {
        self.set_setting("reranker", &serde_json::to_string(s)?)
    }

    /// The external MCP servers, in the order the user added them.
    ///
    /// A row that will not deserialise is dropped rather than failing the whole
    /// list: one malformed entry from an older build must not cost the user
    /// every server they configured.
    pub fn mcp_servers(&self) -> Result<Vec<McpServerConfig>> {
        let Some(json) = self.get_setting("mcp_servers")? else {
            return Ok(Vec::new());
        };
        let raw: Vec<serde_json::Value> = serde_json::from_str(&json).unwrap_or_default();
        Ok(raw
            .into_iter()
            .filter_map(|v| serde_json::from_value::<McpServerConfig>(v).ok())
            .filter(|s| !s.id.is_empty())
            .collect())
    }

    pub fn set_mcp_servers(&self, servers: &[McpServerConfig]) -> Result<()> {
        self.set_setting("mcp_servers", &serde_json::to_string(servers)?)
    }
}

/// Escape the three characters `LIKE ... ESCAPE '\'` treats specially.
///
/// The backslash has to go first. Escaping it last would double the escapes
/// added for `%` and `_`, and a user searching for a literal backslash — a
/// Windows path in a mail, say — would match nothing at all.
fn escape_like(needle: &str) -> String {
    needle
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

/// f32 array as a little-endian blob.
fn encode_vector(vec: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(vec.len() * 4);
    for v in vec {
        bytes.extend_from_slice(&v.to_le_bytes());
    }
    bytes
}

/// Little-endian f32 array as written by `encode_vector`.
fn decode_vector(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

fn memory_kind_str(k: MemoryKind) -> &'static str {
    match k {
        MemoryKind::Preference => "preference",
        MemoryKind::Fact => "fact",
        MemoryKind::Contact => "contact",
    }
}

fn parse_memory_kind(s: &str) -> MemoryKind {
    match s {
        "contact" => MemoryKind::Contact,
        "fact" => MemoryKind::Fact,
        _ => MemoryKind::Preference,
    }
}

fn chat_role_str(r: ChatRole) -> &'static str {
    match r {
        ChatRole::User => "user",
        ChatRole::Assistant => "assistant",
        ChatRole::Tool => "tool",
    }
}

fn parse_chat_role(s: &str) -> ChatRole {
    match s {
        "user" => ChatRole::User,
        "tool" => ChatRole::Tool,
        _ => ChatRole::Assistant,
    }
}

/// The column list every memory query selects, so `row_to_memory` can read one
/// fixed shape instead of one per call site.
const MEMORY_COLS: &str = "SELECT id, kind, text, source, created_at, updated_at,
        status, origin, superseded_by, valid_from, valid_to, use_count
   FROM memories";

fn memory_status_str(s: MemoryStatus) -> &'static str {
    match s {
        MemoryStatus::Active => "active",
        MemoryStatus::Superseded => "superseded",
    }
}

fn parse_memory_status(s: &str) -> MemoryStatus {
    match s {
        "superseded" => MemoryStatus::Superseded,
        _ => MemoryStatus::Active,
    }
}

fn memory_origin_str(o: MemoryOrigin) -> &'static str {
    match o {
        MemoryOrigin::User => "user",
        MemoryOrigin::Assistant => "assistant",
    }
}

fn parse_memory_origin(s: &str) -> MemoryOrigin {
    match s {
        "user" => MemoryOrigin::User,
        _ => MemoryOrigin::Assistant,
    }
}

fn row_to_memory(r: &Row<'_>) -> rusqlite::Result<MemoryEntry> {
    let kind: String = r.get(1)?;
    let status: String = r.get(6)?;
    let origin: String = r.get(7)?;
    Ok(MemoryEntry {
        id: r.get(0)?,
        kind: parse_memory_kind(&kind),
        text: r.get(2)?,
        source: r.get(3)?,
        created_at: r.get(4)?,
        updated_at: r.get(5)?,
        status: parse_memory_status(&status),
        origin: parse_memory_origin(&origin),
        superseded_by: r.get(8)?,
        valid_from: r.get(9)?,
        valid_to: r.get(10)?,
        use_count: r.get::<_, i64>(11)?.max(0) as u32,
    })
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

    /// The batch lookup answers in the order asked and quietly omits rows that
    /// are gone — a vector index outlives the message it points at.
    #[test]
    fn get_messages_keeps_the_requested_order_and_skips_the_missing() {
        let s = Store::open_in_memory().unwrap();
        s.insert_account(&sample_account()).unwrap();
        for i in 0..3 {
            let mut m = sample_message(&format!("m{i}"), &format!("{i}"));
            m.message_id = Some(format!("<m{i}@x>"));
            s.insert_message(&m).unwrap();
        }
        s.soft_delete(&["m1".into()]).unwrap();

        let ids = vec!["m2".to_string(), "gone".to_string(), "m1".to_string(), "m0".to_string()];
        let got: Vec<String> = s.get_messages(&ids).unwrap().into_iter().map(|m| m.id).collect();
        assert_eq!(got, vec!["m2".to_string(), "m0".to_string()]);
        assert!(s.get_messages(&[]).unwrap().is_empty());
    }

    /// A wildcard or an escape character typed into the search box is a literal,
    /// not a pattern: "50%" must not match every subject in the mailbox.
    #[test]
    fn search_treats_like_metacharacters_as_literals() {
        let s = Store::open_in_memory().unwrap();
        s.insert_account(&sample_account()).unwrap();
        for (id, subject) in [("m1", "省 50% 的运费"), ("m2", "普通邮件"), ("m3", r"路径 C:\temp")] {
            let mut m = sample_message(id, id);
            m.message_id = Some(format!("<{id}@x>"));
            m.subject = subject.into();
            s.insert_message(&m).unwrap();
        }
        let find = |needle: &str| {
            s.query_messages(&MessageQuery {
                search: Some(needle.to_string()),
                ..Default::default()
            })
            .unwrap()
        };

        assert_eq!(find("50%").total, 1);
        assert_eq!(find("%").total, 1, "a bare %% must not match everything");
        assert_eq!(find("_").total, 0);
        // A backslash is the escape character; unescaped it swallowed the "t".
        assert_eq!(find(r"C:\temp").total, 1);
        assert_eq!(find("\\").total, 1);
    }

    /// Both counters come from one scan now; they still have to agree with the
    /// filtered set rather than the whole table.
    #[test]
    fn counts_follow_the_filter() {
        let s = Store::open_in_memory().unwrap();
        s.insert_account(&sample_account()).unwrap();
        for i in 0..4 {
            let mut m = sample_message(&format!("m{i}"), &format!("{i}"));
            m.message_id = Some(format!("<m{i}@x>"));
            m.subject = if i < 2 { "账单".into() } else { "其他".into() };
            s.insert_message(&m).unwrap();
        }
        s.set_read(&["m0".into()], true).unwrap();

        let page = s.query_messages(&MessageQuery {
            search: Some("账单".into()),
            ..Default::default()
        })
        .unwrap();
        assert_eq!((page.total, page.unread), (2, 1));

        let all = s.query_messages(&MessageQuery::default()).unwrap();
        assert_eq!((all.total, all.unread), (4, 3));
    }

    #[test]
    fn a_conversation_is_looked_up_by_key() {
        let s = Store::open_in_memory().unwrap();
        assert!(!s.conversation_exists("c1").unwrap());
        s.upsert_conversation(&Conversation {
            id: "c1".into(),
            title: "标题".into(),
            created_at: 1,
            updated_at: 1,
        })
        .unwrap();
        assert!(s.conversation_exists("c1").unwrap());
        assert!(!s.conversation_exists("c2").unwrap());
    }

    #[test]
    fn a_memory_is_looked_up_by_key_or_by_its_normalised_text() {
        let s = Store::open_in_memory().unwrap();
        assert!(s.get_memory("mem1").unwrap().is_none());
        s.put_memory(
            &MemoryEntry {
                id: "mem1".into(),
                kind: MemoryKind::Contact,
                text: "老王是 wang@example.com".into(),
                created_at: 7,
                updated_at: 7,
                ..Default::default()
            },
            "老王是 wang@example.com",
        )
        .unwrap();
        let got = s.get_memory("mem1").unwrap().expect("stored");
        assert_eq!(got.kind, MemoryKind::Contact);
        assert_eq!(got.created_at, 7);
        assert_eq!(got.status, MemoryStatus::Active);
        assert_eq!(got.origin, MemoryOrigin::Assistant, "a stored row defaults to inferred");

        // The write path's fast lane: the same sentence again is an indexed hit.
        assert_eq!(
            s.memory_by_norm("老王是 wang@example.com").unwrap().map(|m| m.id).as_deref(),
            Some("mem1")
        );
        assert!(s.memory_by_norm("别的内容").unwrap().is_none());
    }

    /// A batch is one transaction. The visible contract is all-or-nothing plus
    /// "an empty batch is a no-op", which is what an empty selection produces.
    #[test]
    fn batched_flag_updates_apply_together() {
        let s = Store::open_in_memory().unwrap();
        s.insert_account(&sample_account()).unwrap();
        for i in 0..3 {
            let mut m = sample_message(&format!("m{i}"), &format!("{i}"));
            m.message_id = Some(format!("<m{i}@x>"));
            s.insert_message(&m).unwrap();
        }
        s.set_read(&["m0".into(), "m1".into(), "m2".into()], true).unwrap();
        assert_eq!(s.query_messages(&MessageQuery::default()).unwrap().unread, 0);

        s.set_read(&[], false).unwrap();
        s.soft_delete(&[]).unwrap();
        assert_eq!(s.query_messages(&MessageQuery::default()).unwrap().total, 3);

        s.soft_delete(&["m0".into(), "m1".into()]).unwrap();
        assert_eq!(s.query_messages(&MessageQuery::default()).unwrap().total, 1);
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
    fn uid_validity_reset_retires_stale_uids() {
        let s = Store::open_in_memory().unwrap();
        s.insert_account(&sample_account()).unwrap();
        s.insert_message(&sample_message("m1", "7")).unwrap();
        assert_eq!(s.uid_validity("acc1", "INBOX").unwrap(), None);

        s.set_uid_validity("acc1", "INBOX", 42).unwrap();
        assert_eq!(s.uid_validity("acc1", "INBOX").unwrap(), Some(42));
        assert_eq!(s.known_uids("acc1", "INBOX").unwrap(), vec!["7".to_string()]);

        // After a server-side rebuild the old UID must stop matching, so a new
        // message that happens to reuse UID 7 is not mistaken for the old one.
        assert_eq!(s.clear_uids("acc1", "INBOX").unwrap(), 1);
        assert!(!s.known_uids("acc1", "INBOX").unwrap().contains(&"7".to_string()));
        // The message itself survives the reset.
        assert_eq!(s.query_messages(&MessageQuery::default()).unwrap().total, 1);

        // A different message may now claim UID 7 without a uniqueness clash.
        let mut fresh = sample_message("m2", "7");
        fresh.message_id = Some("<m2@example.com>".into());
        assert!(s.insert_message(&fresh).unwrap());

        // Clearing twice must not re-mangle already-retired rows.
        assert_eq!(s.clear_uids("acc1", "INBOX").unwrap(), 1);
        s.set_uid_validity("acc1", "INBOX", 43).unwrap();
        assert_eq!(s.uid_validity("acc1", "INBOX").unwrap(), Some(43));
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
