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
use crate::thread;
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
    -- Threading. `thread_id` is the id of the message that started the
    -- conversation, so a mail with no replies is its own thread and needs no
    -- special case anywhere downstream.
    thread_id    TEXT NOT NULL DEFAULT '',
    refs_json    TEXT NOT NULL DEFAULT '[]',
    subject_norm TEXT NOT NULL DEFAULT '',
    UNIQUE(account_id, folder, uid)
);

CREATE INDEX IF NOT EXISTS idx_messages_list
    ON messages(account_id, deleted, date DESC);
CREATE INDEX IF NOT EXISTS idx_messages_category
    ON messages(category, deleted, date DESC);
CREATE INDEX IF NOT EXISTS idx_messages_msgid
    ON messages(account_id, message_id);
-- The threading indexes over `messages` are in MIGRATIONS, not here: on an
-- upgraded database the CREATE TABLE above is a no-op, so the columns they
-- cover do not exist until the ALTERs have run. This table has no such
-- problem — it is new either way.
CREATE TABLE IF NOT EXISTS message_refs (
    message_id TEXT NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
    ref_id     TEXT NOT NULL,
    PRIMARY KEY (message_id, ref_id)
);
-- The reverse lookup: everyone who cites this Message-ID.
CREATE INDEX IF NOT EXISTS idx_message_refs_ref ON message_refs(ref_id);

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

-- Categories the user described in their own words.
--
-- `instruction` is the definition: prose the triage prompt carries, not a rule
-- the app evaluates. That is the point — "求职者投递简历或跟进面试" is not
-- expressible as a filter, and everybody's version of it is different.
CREATE TABLE IF NOT EXISTS labels (
    id          TEXT PRIMARY KEY,
    name        TEXT NOT NULL,
    instruction TEXT NOT NULL,
    color_hue   INTEGER NOT NULL DEFAULT 210,
    enabled     INTEGER NOT NULL DEFAULT 1,
    created_at  INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS message_labels (
    message_id TEXT NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
    label_id   TEXT NOT NULL REFERENCES labels(id) ON DELETE CASCADE,
    PRIMARY KEY (message_id, label_id)
);

CREATE INDEX IF NOT EXISTS idx_message_labels_label
    ON message_labels(label_id);

-- What each message tried to load from somebody else's server.
--
-- Written when the mail arrives, whether or not it is ever opened: "how much of
-- my mail is tracking me" is a question about everything that came in, not about
-- what happened to be read.
CREATE TABLE IF NOT EXISTS message_trackers (
    message_id TEXT NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
    host       TEXT NOT NULL,
    kind       TEXT NOT NULL,
    count      INTEGER NOT NULL,
    -- The message's own date, so the heatmap is about when mail arrived rather
    -- than when this table was written.
    day        TEXT NOT NULL,
    PRIMARY KEY (message_id, host, kind)
);

CREATE INDEX IF NOT EXISTS idx_trackers_day ON message_trackers(day);

-- A message with nothing to report has no rows above, which is
-- indistinguishable from one that was never looked at. This is the difference.
CREATE TABLE IF NOT EXISTS tracker_scans (
    message_id TEXT PRIMARY KEY REFERENCES messages(id) ON DELETE CASCADE
);

-- Full-text index over the whole of every message.
--
-- The list search used to be `LIKE` over subject, sender and the 140-character
-- snippet, so a word in the third paragraph of a mail was unfindable; the vector
-- index only ever saw the first 1200 characters. Both of those are now backed by
-- this.
--
-- `text` is not the mail: it is the mail run through `fts_index_text`, which
-- explodes CJK into overlapping bigrams. FTS5's tokenizers cannot segment
-- Chinese — unicode61 reads a whole run as one token, so 账单 inside 十月账单
-- matches nothing, and trigram cannot match anything shorter than three
-- characters, which rules out most Chinese words. Both sides of the search go
-- through the same transform instead.
CREATE VIRTUAL TABLE IF NOT EXISTS message_fts USING fts5(message_id UNINDEXED, text);

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
    // Threading. Existing rows land with an empty `thread_id`, which
    // `backfill_threads` fills in — until then they read as their own thread,
    // which is exactly how an unthreaded mailbox already behaved.
    "ALTER TABLE messages ADD COLUMN thread_id TEXT NOT NULL DEFAULT ''",
    "ALTER TABLE messages ADD COLUMN refs_json TEXT NOT NULL DEFAULT '[]'",
    "ALTER TABLE messages ADD COLUMN subject_norm TEXT NOT NULL DEFAULT ''",
    // Threading reads two ways: "every message in this conversation" when the
    // reading pane opens one, and "the newest per conversation" for every list
    // page. Both are this index.
    "CREATE INDEX IF NOT EXISTS idx_messages_thread ON messages(thread_id, date DESC)",
    // The subject fallback looks up one normalised subject per arriving
    // message, scoped to the account and bounded by date.
    "CREATE INDEX IF NOT EXISTS idx_messages_subject_norm
       ON messages(account_id, subject_norm, date DESC)",
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
            // A virtual table takes no foreign keys, so the index has to be
            // swept by hand. Orphans could never match anything — every query
            // joins `messages` — but they would keep the mail's text on disk
            // after the account it belonged to was removed.
            c.execute(
                "DELETE FROM message_fts
                  WHERE message_id NOT IN (SELECT id FROM messages)",
                [],
            )?;
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
        self.with_tx(|c| {
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
            let subject = thread::normalize_subject(&m.subject);
            let thread_id = resolve_thread(c, m, &subject)?;
            let n = c.execute(
                "INSERT OR IGNORE INTO messages
                 (id,account_id,folder,uid,message_id,subject,from_name,from_addr,to_json,date,
                  snippet,body_text,body_html,atts_json,unread,starred,deleted,category,analysis_json,received_at,
                  thread_id,refs_json,subject_norm)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,0,?17,?18,?19,?20,?21,?22)",
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
                    thread_id,
                    serde_json::to_string(&m.references)?,
                    subject.norm,
                ],
            )?;
            if n > 0 {
                index_refs(c, m)?;
                // A message can name ancestors that arrived after it — IMAP
                // hands out a folder in UID order, not conversation order.
                // Adopting those now is what keeps an out-of-order backfill
                // from leaving a thread split in two.
                adopt_orphans(c, m, &thread_id)?;
            }
            if n > 0 {
                // In the same transaction as the row. As a second call it was one
                // `if let Err` away from a mailbox whose body text is unsearchable,
                // and there is no reason for a caller to be able to get that wrong.
                index_text(c, m)?;
            }
            Ok(n > 0)
        })
    }

    // -- labels -------------------------------------------------------------

    pub fn list_labels(&self) -> Result<Vec<MailLabel>> {
        self.with(|c| {
            let mut stmt = c.prepare(
                "SELECT id, name, instruction, color_hue, enabled, created_at
                   FROM labels ORDER BY created_at ASC",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok(MailLabel {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    instruction: r.get(2)?,
                    color_hue: r.get::<_, i64>(3)?.clamp(0, 360) as u16,
                    enabled: r.get::<_, i64>(4)? != 0,
                    created_at: r.get(5)?,
                })
            })?;
            Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
        })
    }

    pub fn put_label(&self, l: &MailLabel) -> Result<()> {
        self.with(|c| {
            c.execute(
                "INSERT INTO labels (id, name, instruction, color_hue, enabled, created_at)
                 VALUES (?1,?2,?3,?4,?5,?6)
                 ON CONFLICT(id) DO UPDATE SET
                   name=excluded.name, instruction=excluded.instruction,
                   color_hue=excluded.color_hue, enabled=excluded.enabled",
                params![l.id, l.name, l.instruction, l.color_hue, l.enabled as i64, l.created_at],
            )?;
            Ok(())
        })
    }

    pub fn delete_label(&self, id: &str) -> Result<()> {
        self.with(|c| {
            let n = c.execute("DELETE FROM labels WHERE id=?1", params![id])?;
            if n == 0 {
                return Err(Error::NotFound(format!("label {id}")));
            }
            Ok(())
        })
    }

    /// Per-label totals for the sidebar, over undeleted mail.
    pub fn label_counts(&self) -> Result<Vec<LabelCount>> {
        self.with(|c| {
            let mut stmt = c.prepare(
                "SELECT l.id, COUNT(m.id), COALESCE(SUM(m.unread), 0)
                   FROM labels l
                   LEFT JOIN message_labels ml ON ml.label_id = l.id
                   LEFT JOIN messages m ON m.id = ml.message_id AND m.deleted=0
                  GROUP BY l.id",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok(LabelCount {
                    label_id: r.get(0)?,
                    total: r.get::<_, i64>(1)?.max(0) as u32,
                    unread: r.get::<_, i64>(2)?.max(0) as u32,
                })
            })?;
            Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
        })
    }

    // -- trackers -----------------------------------------------------------

    /// Record what one message wanted to load, replacing any earlier scan.
    ///
    /// `day` comes from the message's own date rather than from now: a mailbox
    /// synced for the first time would otherwise pile a year of newsletters onto
    /// today and make the heatmap a lie.
    pub fn put_trackers(&self, message_id: &str, day: &str, hits: &[TrackerHit]) -> Result<()> {
        self.with_tx(|tx| {
            tx.execute("DELETE FROM message_trackers WHERE message_id=?1", params![message_id])?;
            let mut stmt = tx.prepare(
                "INSERT INTO message_trackers (message_id, host, kind, count, day)
                 VALUES (?1,?2,?3,?4,?5)",
            )?;
            for hit in hits {
                stmt.execute(params![message_id, hit.host, hit.kind.as_str(), hit.count, day])?;
            }
            Ok(())
        })
    }

    /// What one message wanted to load, worst kind first.
    pub fn trackers_for(&self, message_id: &str) -> Result<Vec<TrackerHit>> {
        self.with(|c| {
            let mut stmt = c.prepare(
                "SELECT host, kind, count FROM message_trackers
                  WHERE message_id=?1
                  ORDER BY CASE kind WHEN 'known' THEN 0 WHEN 'pixel' THEN 1 ELSE 2 END,
                           count DESC, host ASC",
            )?;
            let rows = stmt.query_map(params![message_id], |r| {
                let kind: String = r.get(1)?;
                Ok(TrackerHit {
                    host: r.get(0)?,
                    kind: TrackerKind::parse(&kind),
                    count: r.get::<_, i64>(2)?.max(0) as u32,
                })
            })?;
            Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
        })
    }

    /// Per-day counts from `since` (a `YYYY-MM-DD` string) onwards, counting only
    /// the kinds that are actually tracking. Days with nothing are absent; the
    /// caller fills the calendar, because only it knows which days it is drawing.
    pub fn tracker_days(&self, since: &str) -> Result<Vec<TrackerDay>> {
        self.with(|c| {
            let mut stmt = c.prepare(
                "SELECT day, SUM(count), COUNT(DISTINCT message_id)
                   FROM message_trackers
                  WHERE day >= ?1 AND kind IN ('known','pixel')
                  GROUP BY day ORDER BY day ASC",
            )?;
            let rows = stmt.query_map(params![since], |r| {
                Ok(TrackerDay {
                    day: r.get(0)?,
                    blocked: r.get::<_, i64>(1)?.max(0) as u32,
                    messages: r.get::<_, i64>(2)?.max(0) as u32,
                })
            })?;
            Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
        })
    }

    /// The hosts asking most often since `since`, most requests first.
    pub fn tracker_top(&self, since: &str, limit: u32) -> Result<Vec<TrackerHit>> {
        self.with(|c| {
            let mut stmt = c.prepare(
                "SELECT host, MIN(kind), SUM(count) FROM message_trackers
                  WHERE day >= ?1 AND kind IN ('known','pixel')
                  GROUP BY host ORDER BY SUM(count) DESC, host ASC LIMIT ?2",
            )?;
            let rows = stmt.query_map(params![since, limit as i64], |r| {
                let kind: String = r.get(1)?;
                Ok(TrackerHit {
                    host: r.get(0)?,
                    kind: TrackerKind::parse(&kind),
                    count: r.get::<_, i64>(2)?.max(0) as u32,
                })
            })?;
            Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
        })
    }

    /// Messages whose trackers have never been scanned, newest first. The scan
    /// arrived after the mailbox did.
    pub fn messages_missing_trackers(&self, limit: u32) -> Result<Vec<EmailMessage>> {
        self.with(|c| {
            let mut stmt = c.prepare(
                "SELECT * FROM messages m
                  WHERE m.deleted=0 AND m.body_html IS NOT NULL AND m.body_html <> ''
                    AND NOT EXISTS (
                      SELECT 1 FROM tracker_scans s WHERE s.message_id = m.id)
                  ORDER BY m.date DESC LIMIT ?1",
            )?;
            let rows = stmt.query_map(params![limit as i64], row_to_message)?;
            Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
        })
    }

    /// Note that a message has been scanned, whether or not anything was found.
    /// Without this, a clean message would be rescanned on every startup.
    pub fn mark_scanned(&self, message_id: &str) -> Result<()> {
        self.with(|c| {
            c.execute(
                "INSERT OR REPLACE INTO tracker_scans (message_id) VALUES (?1)",
                params![message_id],
            )?;
            Ok(())
        })
    }

    // -- full-text index ----------------------------------------------------

    /// Index one message, replacing whatever was there.
    ///
    /// Called from the sync path right after the row lands. Kept separate from
    /// `insert_message` so a re-parse or a body that arrived late can re-index
    /// without touching the message row.
    pub fn index_message_text(&self, m: &EmailMessage) -> Result<()> {
        self.with_tx(|tx| index_text(tx, m))
    }

    pub fn unindex_message_text(&self, id: &str) -> Result<()> {
        self.with(|c| {
            c.execute("DELETE FROM message_fts WHERE message_id=?1", params![id])?;
            Ok(())
        })
    }

    /// Messages with no row in the index yet, newest first.
    ///
    /// The index arrived after the mailbox did, so an existing database has to be
    /// backfilled. Bounded per call so the caller can drive it from a loop.
    pub fn messages_missing_fts(&self, limit: u32) -> Result<Vec<EmailMessage>> {
        self.with(|c| {
            let mut stmt = c.prepare(
                "SELECT * FROM messages m
                  WHERE m.deleted=0
                    AND NOT EXISTS (SELECT 1 FROM message_fts f WHERE f.message_id = m.id)
                  ORDER BY m.date DESC LIMIT ?1",
            )?;
            let rows = stmt.query_map(params![limit as i64], row_to_message)?;
            Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
        })
    }

    /// (indexed, total) for the full-text index.
    pub fn fts_counts(&self) -> Result<(u32, u32)> {
        self.with(|c| {
            let total: i64 =
                c.query_row("SELECT COUNT(*) FROM messages WHERE deleted=0", [], |r| r.get(0))?;
            let indexed: i64 = c.query_row(
                "SELECT COUNT(*) FROM message_fts f
                  JOIN messages m ON m.id = f.message_id AND m.deleted=0",
                [],
                |r| r.get(0),
            )?;
            Ok((indexed as u32, total as u32))
        })
    }

    /// Message ids matching `query`, best first, by BM25 over the whole body.
    ///
    /// `Ok(None)` means the index cannot answer this query — a lone CJK
    /// character, or nothing searchable at all — and the caller should fall back
    /// to the substring path rather than report no results.
    pub fn fts_search(&self, query: &str, limit: u32) -> Result<Option<Vec<String>>> {
        let Some(expr) = fts_match_query(query) else {
            return Ok(None);
        };
        self.with(|c| {
            let mut stmt = c.prepare(
                "SELECT f.message_id FROM message_fts f
                  JOIN messages m ON m.id = f.message_id AND m.deleted=0
                  WHERE message_fts MATCH ?1
                  ORDER BY bm25(message_fts, 0.0, 1.0)
                  LIMIT ?2",
            )?;
            let rows = stmt.query_map(params![expr, limit as i64], |r| r.get::<_, String>(0));
            match rows {
                Ok(rows) => Ok(Some(rows.collect::<std::result::Result<Vec<_>, _>>()?)),
                // A MATCH expression FTS5 rejects must not take the search down
                // with it: the substring path answers instead.
                Err(e) => {
                    tracing::debug!("fts: 查询被拒绝，改用子串匹配: {e}");
                    Ok(None)
                }
            }
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

    /// Every message in one conversation, oldest first — the order it was
    /// read in.
    ///
    /// Bodies included: the reading pane shows the whole chain, and fetching
    /// each message separately would be one IPC round trip per reply.
    pub fn thread_messages(&self, thread_id: &str) -> Result<Vec<EmailMessage>> {
        if thread_id.is_empty() {
            return Ok(Vec::new());
        }
        self.with(|c| {
            let mut stmt = c.prepare(
                "SELECT * FROM messages WHERE thread_id=?1 AND deleted=0 ORDER BY date ASC, id ASC",
            )?;
            let rows = stmt.query_map(params![thread_id], row_to_message)?;
            Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
        })
    }

    /// Give threads to messages stored before threading existed.
    ///
    /// These rows have no `References`: the header was never stored and the
    /// raw mail is long gone, so the exact rule has nothing to work with and
    /// the subject fallback carries the whole backfill. That recovers most of
    /// it — a reply almost always says "Re:" — and it is the only part that is
    /// lossy. New mail arriving afterwards still links to these by
    /// `Message-ID`, which *was* stored, so the graph repairs itself forward.
    ///
    /// Oldest first, so an ancestor is always in place before the reply that
    /// cites it. Bounded per call: the first launch after an upgrade should
    /// not be one multi-minute transaction.
    pub fn backfill_threads(&self, limit: u32) -> Result<u32> {
        self.with_tx(|c| {
            let mut stmt = c.prepare(
                "SELECT * FROM messages WHERE thread_id='' ORDER BY date ASC, id ASC LIMIT ?1",
            )?;
            let batch = stmt
                .query_map(params![limit], row_to_message)?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            drop(stmt);

            let mut done = 0;
            for m in &batch {
                let subject = thread::normalize_subject(&m.subject);
                // The row is already in the table, so `resolve_thread` can see
                // it — and would happily thread it to itself by subject. Its
                // own empty `thread_id` is excluded from both lookups, which
                // is the one thing keeping that from happening.
                let thread_id = resolve_thread(c, m, &subject)?;
                c.execute(
                    "UPDATE messages SET thread_id=?1, subject_norm=?2 WHERE id=?3",
                    params![thread_id, subject.norm, m.id],
                )?;
                adopt_orphans(c, m, &thread_id)?;
                done += 1;
            }
            Ok(done)
        })
    }

    /// How many messages are still waiting on `backfill_threads`.
    pub fn unthreaded_count(&self) -> Result<u32> {
        self.with(|c| {
            let n: i64 =
                c.query_row("SELECT COUNT(*) FROM messages WHERE thread_id=''", [], |r| r.get(0))?;
            Ok(n as u32)
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
            if let Some(label) = q.label_id.as_deref().filter(|s| !s.is_empty()) {
                args.push(Box::new(label.to_string()));
                where_sql.push_str(&format!(
                    " AND id IN (SELECT message_id FROM message_labels WHERE label_id=?{})",
                    args.len()
                ));
            }
            if q.unread_only {
                where_sql.push_str(" AND unread=1");
            }
            if q.starred_only {
                where_sql.push_str(" AND starred=1");
            }
            if let Some(s) = q.search.as_deref().filter(|s| !s.trim().is_empty()) {
                match fts_match_query(s.trim()) {
                    // The index reaches the whole body; the substring columns
                    // never did. Used as a filter rather than as the ordering, so
                    // the list stays newest-first — a mail list re-sorted by
                    // relevance while you type is not a mail list.
                    Some(expr) => {
                        args.push(Box::new(expr));
                        where_sql.push_str(&format!(
                            " AND id IN (SELECT message_id FROM message_fts WHERE message_fts MATCH ?{})",
                            args.len()
                        ));
                    }
                    // A lone CJK character, which is not a token in a bigram
                    // index. The backslash goes first when escaping: after the
                    // wildcards it would also escape the escapes we just added,
                    // and a search for a literal "\" would match nothing.
                    None => {
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
                }
            }

            let params_ref: Vec<&dyn rusqlite::types::ToSql> =
                args.iter().map(|b| b.as_ref()).collect();

            // Both totals from one scan. As two queries the same rows were
            // visited twice per page — and with a `search` filter that is two
            // full table scans for one keystroke.
            //
            // Grouped, the same two numbers are counted over conversations
            // instead of messages: a thread is one row in the list, and one
            // unread reply makes the whole thread unread.
            let count_sql = if q.group_threads {
                format!(
                    "SELECT COUNT(*), COALESCE(SUM(u),0) FROM
                       (SELECT MAX(unread) u FROM messages WHERE {where_sql} GROUP BY thread_id)"
                )
            } else {
                format!("SELECT COUNT(*), COALESCE(SUM(unread),0) FROM messages WHERE {where_sql}")
            };
            let (total, unread): (u32, u32) =
                c.query_row(&count_sql, params_ref.as_slice(), |r| Ok((r.get(0)?, r.get(1)?)))?;

            let limit = if q.limit == 0 { 50 } else { q.limit.min(200) };
            // The window functions run either way, so a row always knows how
            // big its conversation is — the flat list can still show "3" next
            // to a mail without the caller having to ask a second question.
            // Only the `rn=1` filter is conditional.
            //
            // `unread` and `starred` are aggregated over the thread: a
            // collapsed row stands for all of it, and a conversation with one
            // unread reply reads as unread. Ungrouped, the partition is still
            // per-thread, so those aggregates would be wrong on a flat row —
            // hence the two projections.
            let pick = if q.group_threads {
                "MAX(unread) OVER w AS row_unread, MAX(starred) OVER w AS row_starred"
            } else {
                "unread AS row_unread, starred AS row_starred"
            };
            let having = if q.group_threads { "WHERE rn=1" } else { "" };
            let sql = format!(
                "SELECT id,account_id,folder,subject,from_name,from_addr,date,snippet,
                        row_unread,row_starred,atts_json,category,analysis_json,thread_id,thread_count
                   FROM (
                     SELECT id,account_id,folder,subject,from_name,from_addr,date,snippet,
                            atts_json,category,analysis_json,thread_id,
                            {pick},
                            COUNT(*) OVER w AS thread_count,
                            ROW_NUMBER() OVER (PARTITION BY thread_id ORDER BY date DESC, id DESC) AS rn
                       FROM messages WHERE {where_sql}
                       WINDOW w AS (PARTITION BY thread_id)
                   ) {having}
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

    /// Mark a whole conversation read.
    ///
    /// A collapsed row stands for every message under it, so opening one has
    /// to clear all of them — otherwise an unread reply further up the chain
    /// keeps the row bold no matter how many times the user opens it.
    ///
    /// Returns how many messages changed.
    pub fn set_thread_read(&self, thread_id: &str, read: bool) -> Result<u32> {
        if thread_id.is_empty() {
            return Ok(0);
        }
        self.with(|c| {
            let n = c.execute(
                "UPDATE messages SET unread=?2 WHERE thread_id=?1 AND deleted=0 AND unread<>?2",
                params![thread_id, (!read) as i64],
            )?;
            Ok(n as u32)
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
        self.with_tx(|c| {
            c.execute(
                "UPDATE messages SET category=?2, analysis_json=?3 WHERE id=?1",
                params![id, analysis.category.as_str(), serde_json::to_string(analysis)?],
            )?;
            // The labels land with the verdict that produced them, in the same
            // transaction. As a separate call the sidebar counts and the list
            // rows could disagree about the same message.
            c.execute("DELETE FROM message_labels WHERE message_id=?1", params![id])?;
            if !analysis.labels.is_empty() {
                let mut stmt = c.prepare(
                    "INSERT OR IGNORE INTO message_labels (message_id, label_id)
                     SELECT ?1, id FROM labels WHERE name=?2",
                )?;
                for name in &analysis.labels {
                    stmt.execute(params![id, name])?;
                }
            }
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

    pub fn reading_settings(&self) -> Result<ReadingSettings> {
        match self.get_setting("reading")? {
            Some(json) => Ok(serde_json::from_str(&json).unwrap_or_default()),
            None => Ok(ReadingSettings::default()),
        }
    }

    pub fn set_reading_settings(&self, s: &ReadingSettings) -> Result<()> {
        self.set_setting("reading", &serde_json::to_string(s)?)
    }

    pub fn privacy_settings(&self) -> Result<PrivacySettings> {
        match self.get_setting("privacy")? {
            Some(json) => Ok(serde_json::from_str(&json).unwrap_or_default()),
            None => Ok(PrivacySettings::default()),
        }
    }

    pub fn set_privacy_settings(&self, s: &PrivacySettings) -> Result<()> {
        self.set_setting("privacy", &serde_json::to_string(s)?)
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

// ---------------------------------------------------------------------------
// Full-text search
// ---------------------------------------------------------------------------

/// Longest run of CJK exploded into bigrams. A 200-page contract in Chinese
/// would otherwise produce an index entry per character position; past this the
/// tail of the mail is searchable by the vector index and by nothing else, which
/// is the same deal every other mail client offers.
const MAX_FTS_CHARS: usize = 20_000;

/// Turn text into what the index actually stores.
///
/// Latin words and numbers survive as themselves. A CJK run becomes its
/// overlapping bigrams, in order, so a phrase query of consecutive bigrams is
/// exactly a substring match: 十月账单 indexes as `十月 月账 账单`, and a search
/// for 账单 is one token. A single CJK character on its own is kept, because a
/// one-character run has no bigram.
pub fn fts_index_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 2);
    let mut run: Vec<char> = Vec::new();

    let flush = |run: &mut Vec<char>, out: &mut String| {
        match run.len() {
            0 => {}
            1 => push_token(out, &run[0].to_string()),
            _ => {
                for pair in run.windows(2) {
                    push_token(out, &pair.iter().collect::<String>());
                }
            }
        }
        run.clear();
    };

    for ch in s.chars().take(MAX_FTS_CHARS) {
        if is_cjk(ch) {
            run.push(ch);
        } else if ch.is_alphanumeric() {
            flush(&mut run, &mut out);
            // A Latin word is one token; FTS5's own tokenizer will split it on
            // nothing, which is what we want.
            out.push(ch);
        } else {
            flush(&mut run, &mut out);
            push_space(&mut out);
        }
    }
    flush(&mut run, &mut out);
    out
}

fn push_token(out: &mut String, token: &str) {
    if !out.is_empty() && !out.ends_with(' ') {
        out.push(' ');
    }
    out.push_str(token);
    out.push(' ');
}

/// A separator, unless one is already there. FTS5 does not care about runs of
/// whitespace, but a predictable transform is worth having in tests.
fn push_space(out: &mut String) {
    if !out.is_empty() && !out.ends_with(' ') {
        out.push(' ');
    }
}

/// Turn what the user typed into an FTS5 `MATCH` expression, or `None` when this
/// query cannot be answered by the index.
///
/// Every run becomes a quoted phrase and the phrases are ANDed, so "invoice 账单"
/// wants both without requiring them to be adjacent. Quoting is not optional:
/// FTS5 reads `*`, `:`, `^`, `-`, `(`, `)` and `"` as syntax, and an unquoted
/// `42.00` is a syntax error rather than a search.
///
/// `None` for a query with nothing usable in it, and for a lone CJK character:
/// the index holds bigrams, so a single character is not a token in it. The
/// substring path still answers those.
pub fn fts_match_query(query: &str) -> Option<String> {
    let mut phrases: Vec<String> = Vec::new();
    let mut run: Vec<char> = Vec::new();
    let mut word = String::new();
    let mut usable = true;

    let flush_cjk = |run: &mut Vec<char>, phrases: &mut Vec<String>, usable: &mut bool| {
        match run.len() {
            0 => {}
            1 => *usable = false, // one character: not a token in the index
            _ => {
                let bigrams: Vec<String> =
                    run.windows(2).map(|p| p.iter().collect::<String>()).collect();
                phrases.push(format!("\"{}\"", bigrams.join(" ")));
            }
        }
        run.clear();
    };

    for ch in query.chars().take(MAX_FTS_CHARS) {
        if is_cjk(ch) {
            if !word.is_empty() {
                phrases.push(format!("\"{word}\""));
                word.clear();
            }
            run.push(ch);
        } else if ch.is_alphanumeric() {
            flush_cjk(&mut run, &mut phrases, &mut usable);
            word.push(ch);
        } else {
            flush_cjk(&mut run, &mut phrases, &mut usable);
            if !word.is_empty() {
                phrases.push(format!("\"{word}\""));
                word.clear();
            }
        }
    }
    flush_cjk(&mut run, &mut phrases, &mut usable);
    if !word.is_empty() {
        phrases.push(format!("\"{word}\""));
    }

    // A query that was partly unusable would silently search for less than the
    // user asked, and quietly returning the wrong results is worse than handing
    // the query to the substring path.
    if phrases.is_empty() || !usable {
        return None;
    }

    // The last phrase matches as a prefix, because the search box searches while
    // the user is still typing: "stri" has to find Stripe, which a whole-token
    // match never would. Harmless on a finished query — it also matches
    // "invoices" — and a no-op on CJK, where every token is two characters.
    if let Some(last) = phrases.last_mut() {
        last.push('*');
    }
    Some(phrases.join(" AND "))
}

/// CJK ideographs plus the Japanese syllabaries — everything a tokenizer built
/// for spaces cannot split.
fn is_cjk(c: char) -> bool {
    matches!(c as u32,
        0x3040..=0x30FF     // hiragana + katakana
        | 0x3400..=0x4DBF   // CJK ext A
        | 0x4E00..=0x9FFF   // CJK
        | 0xF900..=0xFAFF   // compatibility ideographs
        | 0x20000..=0x2FA1F // ext B and beyond
    )
}

/// Which conversation an arriving message belongs to.
///
/// Tried in order of how much the answer can be trusted; see `crate::thread`
/// for why the subject rule is fenced the way it is. Falling all the way
/// through means the message starts a thread of its own, which is why the
/// last line is its own id rather than a null.
///
/// A message that cites an ancestor arriving *later* is not handled here — it
/// cannot be, the ancestor is not in the table yet. `adopt_orphans` closes
/// that case from the other side.
fn resolve_thread(c: &Connection, m: &EmailMessage, subject: &thread::Subject) -> Result<String> {
    // Closest ancestor first: `references` is ordered oldest → newest, and the
    // nearest one we hold is the most specific answer available.
    if !m.references.is_empty() {
        let mut stmt = c.prepare_cached(
            "SELECT thread_id FROM messages
              WHERE account_id=?1 AND message_id=?2 AND thread_id<>'' LIMIT 1",
        )?;
        for cited in m.references.iter().rev() {
            let found: Option<String> = stmt
                .query_row(params![m.account_id, cited], |r| r.get(0))
                .optional()?;
            if let Some(t) = found {
                return Ok(t);
            }
        }
    }

    // No usable chain. If the subject says this continues something, and we
    // have that something recently enough, take it.
    if subject.is_reply && !subject.norm.is_empty() {
        let found: Option<String> = c
            .query_row(
                "SELECT thread_id FROM messages
                  WHERE account_id=?1 AND subject_norm=?2 AND thread_id<>'' AND date>=?3
                  ORDER BY date DESC LIMIT 1",
                params![m.account_id, subject.norm, m.date - thread::SUBJECT_WINDOW_MS],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(t) = found {
            return Ok(t);
        }
    }

    Ok(m.id.clone())
}

/// Pull in threads that were waiting on this message.
///
/// Mail does not arrive in conversation order: a reply can be in INBOX while
/// the mail it answers is still an unsynced item in Sent, and a fetch window
/// that starts mid-thread leaves every earlier message to arrive afterwards.
/// Those replies each started a thread of their own; now that their ancestor
/// exists, those threads are this one.
///
/// The older thread absorbs the newer so the id stays anchored to whichever
/// message actually came first — otherwise a late-arriving ancestor would
/// renumber a conversation the user is already reading.
fn adopt_orphans(c: &rusqlite::Transaction<'_>, m: &EmailMessage, mine: &str) -> Result<()> {
    let Some(mid) = m.message_id.as_deref().filter(|s| !s.is_empty()) else {
        return Ok(());
    };
    let mut stmt = c.prepare(
        "SELECT DISTINCT x.thread_id FROM message_refs r
           JOIN messages x ON x.id = r.message_id
          WHERE r.ref_id=?1 AND x.account_id=?2 AND x.thread_id<>'' AND x.thread_id<>?3",
    )?;
    let others = stmt
        .query_map(params![mid, m.account_id, mine], |r| r.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    drop(stmt);

    let mut winner = mine.to_string();
    for other in others {
        let (keep, drop_) = if thread_start(c, &other)? < thread_start(c, &winner)? {
            (other, winner)
        } else {
            (winner, other)
        };
        c.execute("UPDATE messages SET thread_id=?1 WHERE thread_id=?2", params![keep, drop_])?;
        winner = keep;
    }
    Ok(())
}

/// When the earliest message in a thread arrived. `i64::MAX` for a thread with
/// no rows, so an empty one never wins a merge.
fn thread_start(c: &Connection, thread_id: &str) -> Result<i64> {
    let v: Option<i64> = c.query_row(
        "SELECT MIN(date) FROM messages WHERE thread_id=?1",
        params![thread_id],
        |r| r.get(0),
    )?;
    Ok(v.unwrap_or(i64::MAX))
}

/// Record what a message cites, so the *reverse* lookup is an index hit.
///
/// This duplicates `refs_json`, deliberately: the column answers "what did
/// this message say" when reading one row back, and the table answers "who
/// cites this id" across all of them. One JSON column cannot do the second
/// without a full scan on every insert.
fn index_refs(c: &Connection, m: &EmailMessage) -> Result<()> {
    if m.references.is_empty() {
        return Ok(());
    }
    let mut stmt = c.prepare_cached(
        "INSERT OR IGNORE INTO message_refs (message_id, ref_id) VALUES (?1, ?2)",
    )?;
    for cited in &m.references {
        stmt.execute(params![m.id, cited])?;
    }
    Ok(())
}

/// Write one message's index row, replacing whatever was there.
fn index_text(c: &Connection, m: &EmailMessage) -> Result<()> {
    // Deleting by an UNINDEXED column is a scan of the FTS table rather than a
    // lookup. The alternative is keying on `messages.rowid`, which `VACUUM` is
    // entitled to renumber — a wrong row is worse than a slow delete on a path
    // that runs once per message.
    c.execute("DELETE FROM message_fts WHERE message_id=?1", params![m.id])?;
    c.execute(
        "INSERT INTO message_fts (message_id, text) VALUES (?1, ?2)",
        params![m.id, fts_index_text(&searchable_text(m))],
    )?;
    Ok(())
}

/// Everything about a message worth searching, in one string.
fn searchable_text(m: &EmailMessage) -> String {
    let mut s = String::new();
    for part in [
        m.subject.as_str(),
        m.from_name.as_str(),
        m.from_addr.as_str(),
        m.body_text.as_deref().unwrap_or(""),
    ] {
        if !part.is_empty() {
            s.push_str(part);
            s.push('\n');
        }
    }
    // The snippet is derived from the body for text mail, but for an HTML-only
    // mail it is the only plain text there is.
    if m.body_text.as_deref().unwrap_or("").is_empty() {
        s.push_str(&m.snippet);
    }
    s
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
        references: r
            .get::<_, String>("refs_json")
            .ok()
            .and_then(|j| serde_json::from_str(&j).ok())
            .unwrap_or_default(),
        thread_id: r.get("thread_id").unwrap_or_default(),
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
        thread_id: r.get(13)?,
        thread_count: r.get::<_, i64>(14)?.max(1) as u32,
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
            references: Vec::new(),
            thread_id: String::new(),
            id: id.into(),
            account_id: "acc1".into(),
            folder: "INBOX".into(),
            uid: uid.into(),
            // Unwrapped, the way `parse_mail` stores it — the angle brackets
            // are RFC syntax, not part of the identifier, and threading
            // compares these against unwrapped `References` entries.
            message_id: Some(format!("{id}@example.com")),
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

    /// A message in a conversation: cites `refs`, dated `date`.
    fn reply(id: &str, uid: &str, subject: &str, date: i64, refs: &[&str]) -> EmailMessage {
        EmailMessage {
            subject: subject.into(),
            date,
            received_at: date,
            references: refs.iter().map(|r| (*r).to_string()).collect(),
            ..sample_message(id, uid)
        }
    }

    fn threaded_store() -> Store {
        let s = Store::open_in_memory().unwrap();
        s.insert_account(&sample_account()).unwrap();
        s
    }

    fn thread_of(s: &Store, id: &str) -> String {
        s.get_message(id).unwrap().thread_id
    }

    #[test]
    fn a_reply_joins_the_thread_it_cites() {
        let s = threaded_store();
        s.insert_message(&reply("m1", "1", "Patch v2", 1000, &[])).unwrap();
        s.insert_message(&reply("m2", "2", "Re: Patch v2", 2000, &["m1@example.com"])).unwrap();

        assert_eq!(thread_of(&s, "m2"), "m1");
        assert_eq!(s.thread_messages("m1").unwrap().len(), 2);
    }

    /// A mail nobody answers is a conversation of one — no null, no branch.
    #[test]
    fn an_unanswered_mail_is_its_own_thread() {
        let s = threaded_store();
        s.insert_message(&reply("m1", "1", "Patch v2", 1000, &[])).unwrap();
        assert_eq!(thread_of(&s, "m1"), "m1");
    }

    /// The exact rule beats the subject rule: a reply that cites its parent
    /// belongs to it even when someone renamed the thread halfway through.
    #[test]
    fn references_win_over_a_changed_subject() {
        let s = threaded_store();
        s.insert_message(&reply("m1", "1", "Patch v2", 1000, &[])).unwrap();
        s.insert_message(&reply("m2", "2", "Re: 换个标题", 2000, &["m1@example.com"])).unwrap();
        assert_eq!(thread_of(&s, "m2"), "m1");
    }

    /// The fallback, for the many clients that drop References entirely.
    #[test]
    fn a_reply_subject_joins_by_subject_when_nothing_is_cited() {
        let s = threaded_store();
        s.insert_message(&reply("m1", "1", "发票", 1000, &[])).unwrap();
        s.insert_message(&reply("m2", "2", "回复: 发票", 2000, &[])).unwrap();
        assert_eq!(thread_of(&s, "m2"), "m1");
    }

    /// The fence on the subject rule. Two people sending an unrelated 「发票」
    /// are two conversations, and without the reply-prefix requirement every
    /// mail with a common subject would collapse into one row.
    #[test]
    fn two_originals_with_the_same_subject_stay_apart() {
        let s = threaded_store();
        s.insert_message(&reply("m1", "1", "发票", 1000, &[])).unwrap();
        s.insert_message(&reply("m2", "2", "发票", 2000, &[])).unwrap();
        assert_ne!(thread_of(&s, "m1"), thread_of(&s, "m2"));
    }

    #[test]
    fn the_subject_rule_gives_up_after_a_month() {
        let s = threaded_store();
        s.insert_message(&reply("m1", "1", "发票", 1000, &[])).unwrap();
        let late = 1000 + thread::SUBJECT_WINDOW_MS + 1;
        s.insert_message(&reply("m2", "2", "回复: 发票", late, &[])).unwrap();
        assert_ne!(thread_of(&s, "m2"), "m1");
    }

    /// Sent and INBOX sync at different times, so the ancestor routinely lands
    /// after the reply that names it.
    #[test]
    fn an_ancestor_arriving_late_absorbs_the_thread_its_children_started() {
        let s = threaded_store();
        s.insert_message(&reply("m2", "2", "Re: Patch", 2000, &["m1@example.com"])).unwrap();
        // m2 could not find m1, so it opened its own thread.
        assert_eq!(thread_of(&s, "m2"), "m2");

        s.insert_message(&reply("m1", "1", "Patch", 1000, &[])).unwrap();
        // The older message wins, so the id stays anchored to the real root.
        assert_eq!(thread_of(&s, "m1"), "m1");
        assert_eq!(thread_of(&s, "m2"), "m1");
        assert_eq!(s.thread_messages("m1").unwrap().len(), 2);
    }

    /// The whole subtree moves, not just the message that cited the newcomer.
    #[test]
    fn a_late_ancestor_pulls_the_replies_of_its_replies_too() {
        let s = threaded_store();
        s.insert_message(&reply("m2", "2", "Re: Patch", 2000, &["m1@example.com"])).unwrap();
        s.insert_message(&reply("m3", "3", "Re: Patch", 3000, &["m2@example.com"])).unwrap();
        assert_eq!(thread_of(&s, "m3"), "m2");

        s.insert_message(&reply("m1", "1", "Patch", 1000, &[])).unwrap();
        assert_eq!(s.thread_messages("m1").unwrap().len(), 3);
    }

    #[test]
    fn threads_never_cross_accounts() {
        let s = threaded_store();
        s.insert_account(&AccountConfig { id: "acc2".into(), ..sample_account() }).unwrap();
        s.insert_message(&reply("m1", "1", "Patch", 1000, &[])).unwrap();
        let other = EmailMessage {
            account_id: "acc2".into(),
            ..reply("m2", "2", "Re: Patch", 2000, &["m1@example.com"])
        };
        s.insert_message(&other).unwrap();
        assert_ne!(thread_of(&s, "m2"), "m1");
    }

    #[test]
    fn grouping_collapses_a_thread_to_its_newest_message() {
        let s = threaded_store();
        s.insert_message(&reply("m1", "1", "Patch", 1000, &[])).unwrap();
        s.insert_message(&reply("m2", "2", "Re: Patch", 2000, &["m1@example.com"])).unwrap();
        s.insert_message(&reply("m3", "3", "别的事", 3000, &[])).unwrap();

        let page = s
            .query_messages(&MessageQuery { group_threads: true, ..Default::default() })
            .unwrap();
        assert_eq!(page.total, 2, "two conversations, three messages");
        assert_eq!(page.items.len(), 2);
        // Newest first: the standalone mail, then the thread's latest reply.
        assert_eq!(page.items[0].id, "m3");
        assert_eq!(page.items[1].id, "m2");
        assert_eq!(page.items[1].thread_count, 2);
        assert_eq!(page.items[0].thread_count, 1);
    }

    /// Ungrouped, every message is a row again — and still knows its thread.
    #[test]
    fn not_grouping_returns_every_message() {
        let s = threaded_store();
        s.insert_message(&reply("m1", "1", "Patch", 1000, &[])).unwrap();
        s.insert_message(&reply("m2", "2", "Re: Patch", 2000, &["m1@example.com"])).unwrap();

        let page = s.query_messages(&MessageQuery::default()).unwrap();
        assert_eq!(page.total, 2);
        assert_eq!(page.items.len(), 2);
        assert_eq!(page.items[0].thread_id, "m1");
        assert_eq!(page.items[0].thread_count, 2);
    }

    /// One unread reply makes the collapsed row unread — the row stands for
    /// the whole conversation, so its badge has to as well.
    #[test]
    fn a_grouped_row_is_unread_when_any_message_in_it_is() {
        let s = threaded_store();
        s.insert_message(&reply("m1", "1", "Patch", 1000, &[])).unwrap();
        s.insert_message(&reply("m2", "2", "Re: Patch", 2000, &["m1@example.com"])).unwrap();
        // The newest is read; the older one is not.
        s.set_read(&["m2".to_string()], true).unwrap();

        let page = s
            .query_messages(&MessageQuery { group_threads: true, ..Default::default() })
            .unwrap();
        assert_eq!(page.total, 1);
        assert_eq!(page.unread, 1, "the thread still holds something unread");
        assert!(page.items[0].unread);
    }

    /// Threading is a view over the filter, not over the mailbox: a thread
    /// whose replies are in another category counts what the filter left.
    #[test]
    fn thread_count_reflects_the_active_filter() {
        let s = threaded_store();
        s.insert_message(&reply("m1", "1", "Patch", 1000, &[])).unwrap();
        s.insert_message(&reply("m2", "2", "Re: Patch", 2000, &["m1@example.com"])).unwrap();
        s.set_starred("m2", true).unwrap();

        let page = s
            .query_messages(&MessageQuery {
                group_threads: true,
                starred_only: true,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].thread_count, 1, "only the starred reply matched");
    }

    /// Rows stored before threading existed. The subject rule is all the
    /// backfill has, and it is enough for the ordinary case.
    #[test]
    fn backfill_threads_rebuilds_conversations_from_subjects() {
        let s = threaded_store();
        s.insert_message(&reply("m1", "1", "Patch", 1000, &[])).unwrap();
        s.insert_message(&reply("m2", "2", "Re: Patch", 2000, &[])).unwrap();
        // Put them back the way an upgraded database would look.
        s.with(|c| {
            c.execute("UPDATE messages SET thread_id='', subject_norm=''", []).unwrap();
            Ok(())
        })
        .unwrap();

        assert_eq!(s.unthreaded_count().unwrap(), 2);
        assert_eq!(s.backfill_threads(100).unwrap(), 2);
        assert_eq!(s.unthreaded_count().unwrap(), 0);
        assert_eq!(thread_of(&s, "m2"), "m1");
    }

    #[test]
    fn backfill_threads_stops_at_its_limit_and_resumes() {
        let s = threaded_store();
        for i in 0..5 {
            s.insert_message(&reply(&format!("m{i}"), &i.to_string(), "Patch", 1000 + i, &[]))
                .unwrap();
        }
        s.with(|c| {
            c.execute("UPDATE messages SET thread_id=''", []).unwrap();
            Ok(())
        })
        .unwrap();

        assert_eq!(s.backfill_threads(2).unwrap(), 2);
        assert_eq!(s.unthreaded_count().unwrap(), 3);
        assert_eq!(s.backfill_threads(100).unwrap(), 3);
        assert_eq!(s.unthreaded_count().unwrap(), 0);
    }

    /// Opening a collapsed row clears the whole conversation — including the
    /// reply the user could not see from the list.
    #[test]
    fn marking_a_thread_read_clears_every_message_in_it() {
        let s = threaded_store();
        s.insert_message(&reply("m1", "1", "Patch", 1000, &[])).unwrap();
        s.insert_message(&reply("m2", "2", "Re: Patch", 2000, &["m1@example.com"])).unwrap();

        assert_eq!(s.set_thread_read("m1", true).unwrap(), 2);
        let page = s
            .query_messages(&MessageQuery { group_threads: true, ..Default::default() })
            .unwrap();
        assert_eq!(page.unread, 0);
        assert!(!page.items[0].unread);
        // Idempotent: nothing left to change on a second open.
        assert_eq!(s.set_thread_read("m1", true).unwrap(), 0);
    }

    #[test]
    fn reading_settings_default_to_grouping() {
        let s = Store::open_in_memory().unwrap();
        assert!(s.reading_settings().unwrap().group_threads);
        s.set_reading_settings(&ReadingSettings { group_threads: false }).unwrap();
        assert!(!s.reading_settings().unwrap().group_threads);
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
                    labels: Vec::new(),
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

    /// A label filters the list, and its count follows the mail rather than the
    /// verdict text — the join table is what the sidebar reads.
    #[test]
    fn labels_attach_to_mail_and_filter_it() {
        let s = Store::open_in_memory().unwrap();
        s.insert_account(&sample_account()).unwrap();
        let now = 1_700_000_000_000;
        s.put_label(&MailLabel {
            id: "l1".into(),
            name: "候选人简历".into(),
            instruction: "求职者投递简历".into(),
            created_at: now,
            ..Default::default()
        })
        .unwrap();

        for i in 0..3 {
            let mut m = sample_message(&format!("m{i}"), &format!("{i}"));
            m.message_id = Some(format!("<m{i}@x>"));
            m.unread = i == 0;
            s.insert_message(&m).unwrap();
        }
        let labelled = |names: Vec<String>| AiAnalysis {
            category: Category::Normal,
            confidence: 0.9,
            summary: "x".into(),
            verification_code: None,
            deletable: false,
            reason: "r".into(),
            labels: names,
        };
        s.set_analysis("m0", &labelled(vec!["候选人简历".into()])).unwrap();
        s.set_analysis("m1", &labelled(vec!["候选人简历".into()])).unwrap();
        // A name nobody defined attaches to nothing, rather than erroring.
        s.set_analysis("m2", &labelled(vec!["不存在的标签".into()])).unwrap();

        let counts = s.label_counts().unwrap();
        assert_eq!(counts.len(), 1);
        assert_eq!(counts[0].total, 2);
        assert_eq!(counts[0].unread, 1);

        let page = s
            .query_messages(&MessageQuery { label_id: Some("l1".into()), ..Default::default() })
            .unwrap();
        assert_eq!(page.total, 2);
        assert!(page.items.iter().all(|m| m.id != "m2"));

        // Re-classifying replaces rather than accumulating.
        s.set_analysis("m0", &labelled(Vec::new())).unwrap();
        assert_eq!(s.label_counts().unwrap()[0].total, 1);

        // Deleting the label takes its attachments with it, and leaves the mail.
        s.delete_label("l1").unwrap();
        assert!(s.label_counts().unwrap().is_empty());
        assert_eq!(s.query_messages(&MessageQuery::default()).unwrap().total, 3);
    }

    /// The index format. FTS5 cannot segment Chinese, so both sides of the
    /// search go through this: 十月账单 becomes `十月 月账 账单`, and a phrase
    /// query of consecutive bigrams is then exactly a substring match.
    #[test]
    fn cjk_is_indexed_as_bigrams_and_latin_as_words() {
        assert_eq!(fts_index_text("十月账单").trim(), "十月 月账 账单");
        assert_eq!(fts_index_text("Stripe invoice").trim(), "Stripe invoice");
        assert_eq!(fts_index_text("发票 42.00 元").trim(), "发票 42 00 元");
        // A one-character run has no bigram, so it is kept as itself.
        assert_eq!(fts_index_text("A 型").trim(), "A 型");
        assert_eq!(fts_index_text("").trim(), "");
    }

    /// FTS5 reads `*`, `:`, `^`, `-` and `"` as syntax, so an unquoted query is a
    /// syntax error rather than a search. Everything is quoted, and a query the
    /// index cannot answer says so instead of matching nothing.
    #[test]
    fn a_query_is_quoted_or_refused() {
        // The trailing `*` is what keeps search-as-you-type working.
        assert_eq!(fts_match_query("十月账单").unwrap(), "\"十月 月账 账单\"*");
        assert_eq!(fts_match_query("invoice").unwrap(), "\"invoice\"*");
        // Two runs want both, without demanding they sit next to each other.
        assert_eq!(fts_match_query("账单 invoice").unwrap(), "\"账单\" AND \"invoice\"*");
        // Punctuation is a separator, never syntax.
        assert_eq!(fts_match_query("42.00").unwrap(), "\"42\" AND \"00\"*");
        for hostile in ["\"", "*", "a OR b -c", "NEAR(x y)"] {
            let expr = fts_match_query(hostile);
            if let Some(expr) = &expr {
                // Every part is a quoted phrase, so nothing the user typed can
                // reach FTS5 as an operator.
                assert!(
                    expr.split(" AND ")
                        .all(|p| p.starts_with('"') && p.trim_end_matches('*').ends_with('"')),
                    "{hostile:?} produced {expr}"
                );
            }
        }
        // A lone CJK character is not a token in a bigram index.
        assert!(fts_match_query("账").is_none());
        assert!(fts_match_query("查 一").is_none());
        assert!(fts_match_query("   ").is_none());
        assert!(fts_match_query("!!!").is_none());
    }

    /// The point of the whole thing: a word buried in a long body is findable.
    /// The old search covered subject, sender and a 140-character snippet.
    #[test]
    fn the_full_text_index_reaches_the_middle_of_a_long_body() {
        let s = Store::open_in_memory().unwrap();
        s.insert_account(&sample_account()).unwrap();

        let mut m = sample_message("m1", "1");
        m.subject = "月度对账单".into();
        m.snippet = "开头的一段话".into();
        m.body_text = Some(format!(
            "{}\n订单号 SO-99182 的退款已经处理\n{}",
            "无关的内容。".repeat(200),
            "后面还有很多。".repeat(200)
        ));
        s.insert_message(&m).unwrap();

        let hits = |q: &str| s.fts_search(q, 10).unwrap().unwrap_or_default();
        assert_eq!(hits("SO-99182"), vec!["m1"], "an id deep in the body");
        assert_eq!(hits("SO-991"), vec!["m1"], "and while it is still being typed");
        assert_eq!(hits("退款"), vec!["m1"], "two Chinese characters mid-body");
        assert_eq!(hits("对账单"), vec!["m1"], "the subject still works");
        assert!(hits("不存在的词").is_empty());

        // And the list view finds it too, which is the user-visible half.
        let page = s
            .query_messages(&MessageQuery {
                search: Some("SO-99182".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.total, 1);

        // A deleted message leaves the index, without the row being swept.
        s.soft_delete(&["m1".to_string()]).unwrap();
        assert!(hits("退款").is_empty(), "a deleted mail must not come back");
    }

    /// A mailbox that predates the index has to be backfillable. New mail is
    /// indexed with its row, so the only way to be in that state is to have been
    /// upgraded — simulated here by emptying the index.
    #[test]
    fn messages_without_an_index_row_are_reported_for_backfill() {
        let s = Store::open_in_memory().unwrap();
        s.insert_account(&sample_account()).unwrap();
        for i in 0..3 {
            let mut m = sample_message(&format!("m{i}"), &format!("{i}"));
            m.message_id = Some(format!("<m{i}@x>"));
            m.body_text = Some(format!("正文 {i} 提到了 widget"));
            s.insert_message(&m).unwrap();
        }
        assert_eq!(s.fts_counts().unwrap(), (3, 3), "new mail is indexed as it lands");

        s.with(|c| {
            c.execute("DELETE FROM message_fts", [])?;
            Ok(())
        })
        .unwrap();
        assert_eq!(s.fts_counts().unwrap(), (0, 3));
        assert_eq!(s.messages_missing_fts(10).unwrap().len(), 3);

        for m in s.messages_missing_fts(2).unwrap() {
            s.index_message_text(&m).unwrap();
        }
        assert_eq!(s.fts_counts().unwrap(), (2, 3));
        assert_eq!(s.messages_missing_fts(10).unwrap().len(), 1);

        // Re-indexing replaces rather than duplicating.
        let m = s.get_message("m0").unwrap();
        s.index_message_text(&m).unwrap();
        assert_eq!(s.fts_counts().unwrap(), (2, 3));
        assert_eq!(s.fts_search("widget", 10).unwrap().unwrap().len(), 2);
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
        fresh.message_id = Some("m2@example.com".into());
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
