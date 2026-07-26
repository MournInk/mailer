//! What the assistant knows about the user, and how that survives being wrong.
//!
//! CONTRACT:
//! - [`remember`] takes one statement and decides how it relates to what is
//!   already stored: ADD, UPDATE, SUPERSEDE or NOOP. Exactly one of those
//!   happens, in one transaction, with a row in the audit trail.
//! - [`for_question`] returns what belongs in front of the model for one
//!   question: standing preferences plus whatever the question retrieved.
//! - [`vacuum`] keeps the table bounded.
//!
//! WHY THIS IS NOT A `memories` TABLE WITH AN INSERT
//!
//! The previous version stored a row per `remember` call and de-duplicated on an
//! exact case-insensitive match. That fails on the two things that actually
//! happen. First, the same preference arrives worded differently every session
//! ("回信简短一点" / "回复的时候别写太长"), so the table fills with near-twins
//! and the real preferences get crowded out of the prompt. Second, and worse,
//! people change their minds. "我用 gmail 那个地址收账单" followed three months
//! later by "账单都转到 outlook 了" left both rows active, and the model was as
//! likely to quote the dead one.
//!
//! So: a candidate is compared against what is stored, and a model decides
//! whether this is the same thing said better (UPDATE), a thing that makes an
//! older statement false (SUPERSEDE), a genuinely new thing (ADD), or nothing new
//! (NOOP). The shape is Mem0's two-phase write; the refusal to delete is Zep's,
//! and it matters more here than in a chatbot: an email client has to be able to
//! show the user why it believed something.
//!
//! WHAT THIS DELIBERATELY DOES NOT DO
//!
//! - **No entity graph.** Mem0's own ablation buys about 1.5 points overall for
//!   twice the tokens and *loses* multi-hop, the one thing a graph should win.
//! - **No extraction pass over every turn.** That is an LLM call per message for
//!   a gain the published evaluations do not support, and it would send mail
//!   content to the endpoint without the user asking for anything.
//! - **No approximate index.** A few hundred vectors is a linear scan measured in
//!   microseconds.
//!
//! Everything here degrades rather than fails: with no embedding model the
//! candidate search is substring-only, and with no chat model configured the
//! reconciler falls back to exact-match dedup and an ADD. Both paths keep
//! working, which is the same rule `rag::search` follows.

use std::collections::HashSet;

use serde_json::Value;

use crate::error::Result;
use crate::store::Store;
use crate::sync::now_ms;
use crate::types::*;

/// One memory is a sentence, not an essay.
pub const MAX_MEMORY_CHARS: usize = 600;
/// Memories put in front of the model for one question.
const MAX_INJECTED: usize = 12;
/// Preferences injected regardless of the question.
const MAX_STANDING: u32 = 5;
/// Candidates the reconciler is shown. More than this and the prompt starts
/// costing more than the decision is worth.
const MAX_CANDIDATES: usize = 8;
/// Candidates fetched per salient word on the substring path.
const PER_TERM: u32 = 4;
/// Cosine floor for a memory to count as related. Below this the two sentences
/// are about different things and showing them to the reconciler only invites a
/// speculative edit.
const RELATED_FLOOR: f32 = 0.55;
/// Reply budget for the reconciler. The answer is one small JSON object.
const RECONCILE_TOKENS: u32 = 400;
/// Active memories kept. Past this the least-used assistant facts are retired.
pub const MAX_ACTIVE: u32 = 500;
/// Audit-trail rows kept.
const MAX_EVENTS: u32 = 2000;
/// How long a retired memory stays readable before it is deleted, in ms.
const HISTORY_MS: i64 = 90 * 24 * 60 * 60 * 1000;

// ---------------------------------------------------------------------------
// Write path
// ---------------------------------------------------------------------------

/// What happened to one remembered statement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Add,
    Update,
    Supersede,
    Noop,
}

impl Op {
    pub fn as_str(self) -> &'static str {
        match self {
            Op::Add => "add",
            Op::Update => "update",
            Op::Supersede => "supersede",
            Op::Noop => "noop",
        }
    }

    /// Chinese, for the tool-call chip the user reads.
    pub fn label(self) -> &'static str {
        match self {
            Op::Add => "记住了 1 条新内容",
            Op::Update => "补全了 1 条已有记忆",
            Op::Supersede => "更新了 1 条记忆，旧的转入历史",
            Op::Noop => "已经记着了，没有重复添加",
        }
    }
}

pub struct Written {
    pub op: Op,
    /// The row that is now active and describes the user.
    pub entry: MemoryEntry,
    /// The row that was retired, on a SUPERSEDE.
    pub retired: Option<MemoryEntry>,
    /// The reconciler's own justification, when a model made the call.
    pub reason: Option<String>,
}

/// Store one statement about the user, reconciled against what is already known.
pub async fn remember(
    store: &Store,
    http: &reqwest::Client,
    ai: &AiSettings,
    embedding: &EmbeddingSettings,
    kind: MemoryKind,
    text: &str,
    source: Option<String>,
    origin: MemoryOrigin,
) -> Result<Written> {
    let text = truncate_chars(&collapse_ws(text), MAX_MEMORY_CHARS);
    if text.is_empty() {
        return Err(crate::error::Error::Other("要记住的内容不能为空".into()));
    }
    let norm = normalize(&text);
    let now = now_ms();

    // Fast path: this exact sentence, already stored. No model call, no vector,
    // nothing to reconcile — just say so and refresh how recently it mattered.
    if let Some(existing) = store.memory_by_norm(&norm)? {
        store.touch_memories(&[existing.id.clone()], now)?;
        log(store, &existing.id, Op::Noop, None, Some(&text), None, now)?;
        return Ok(Written { op: Op::Noop, entry: existing, retired: None, reason: None });
    }

    // The candidate's own vector is worth computing before the decision: it is
    // needed to find neighbours, and needed again to store the row if this turns
    // into an ADD. One embedding call either way.
    let vector = embed_one(http, embedding, &text).await;
    let candidates = related(store, &text, vector.as_deref(), embedding).await?;

    let decision = if candidates.is_empty() {
        // Nothing to reconcile against. Asking a model to compare a statement
        // against an empty list is a round trip that can only answer ADD.
        Decision { op: Op::Add, target: None, text: None, reason: None }
    } else {
        reconcile(http, ai, kind, &text, &candidates).await
    };

    apply(store, http, embedding, decision, kind, &text, &norm, source, origin, vector, now).await
}

/// Turn the reconciler's decision into rows.
#[allow(clippy::too_many_arguments)]
async fn apply(
    store: &Store,
    http: &reqwest::Client,
    embedding: &EmbeddingSettings,
    decision: Decision,
    kind: MemoryKind,
    text: &str,
    norm: &str,
    source: Option<String>,
    origin: MemoryOrigin,
    vector: Option<Vec<f32>>,
    now: i64,
) -> Result<Written> {
    let target = decision.target.as_deref().and_then(|id| store.get_memory(id).ok().flatten());

    // A decision naming a memory that is gone, or one the user typed by hand, is
    // downgraded rather than obeyed: the model does not get to overwrite what a
    // person wrote, and it does not get to retire a row that no longer exists.
    let op = match (decision.op, &target) {
        (Op::Update | Op::Supersede, None) => Op::Add,
        (Op::Update, Some(t)) if t.origin == MemoryOrigin::User => Op::Add,
        (Op::Noop, None) => Op::Add,
        (op, _) => op,
    };

    match op {
        Op::Noop => {
            let entry = target.expect("checked above");
            store.touch_memories(&[entry.id.clone()], now)?;
            log(store, &entry.id, Op::Noop, Some(&entry.text), Some(text), decision.reason.as_deref(), now)?;
            Ok(Written { op: Op::Noop, entry, retired: None, reason: decision.reason })
        }

        Op::Update => {
            let old = target.expect("checked above");
            // The merged wording, when the reconciler supplied one; otherwise the
            // new statement, which is why it decided to update at all.
            let merged = decision
                .text
                .as_deref()
                .map(|t| truncate_chars(&collapse_ws(t), MAX_MEMORY_CHARS))
                .filter(|t| !t.is_empty())
                .unwrap_or_else(|| text.to_string());
            let entry = MemoryEntry {
                text: merged.clone(),
                kind,
                updated_at: now,
                // Keep the id and the original creation time: this is the same
                // belief, better worded, and the audit trail says so.
                ..old.clone()
            };
            store.put_memory(&entry, &normalize(&merged))?;
            store_vector(store, http, embedding, &entry.id, &merged, now).await;
            log(store, &entry.id, Op::Update, Some(&old.text), Some(&merged), decision.reason.as_deref(), now)?;
            Ok(Written { op: Op::Update, entry, retired: None, reason: decision.reason })
        }

        Op::Supersede => {
            let old = target.expect("checked above");
            let entry = fresh(kind, text, source, origin, now);
            store.put_memory(&entry, norm)?;
            store.supersede_memory(&old.id, &entry.id, now)?;
            match vector {
                Some(v) => {
                    let _ = store.put_memory_vector(&entry.id, embedding.model.trim(), &v, now);
                }
                None => store_vector(store, http, embedding, &entry.id, text, now).await,
            }
            log(store, &entry.id, Op::Supersede, Some(&old.text), Some(text), decision.reason.as_deref(), now)?;
            let retired = store.get_memory(&old.id)?;
            Ok(Written { op: Op::Supersede, entry, retired, reason: decision.reason })
        }

        Op::Add => {
            let entry = fresh(kind, text, source, origin, now);
            store.put_memory(&entry, norm)?;
            match vector {
                Some(v) => {
                    let _ = store.put_memory_vector(&entry.id, embedding.model.trim(), &v, now);
                }
                None => store_vector(store, http, embedding, &entry.id, text, now).await,
            }
            log(store, &entry.id, Op::Add, None, Some(text), decision.reason.as_deref(), now)?;
            // One sweep per insert, which is the only moment the table grows.
            let _ = vacuum(store, now);
            Ok(Written { op: Op::Add, entry, retired: None, reason: decision.reason })
        }
    }
}

fn fresh(
    kind: MemoryKind,
    text: &str,
    source: Option<String>,
    origin: MemoryOrigin,
    now: i64,
) -> MemoryEntry {
    MemoryEntry {
        id: uuid::Uuid::new_v4().to_string(),
        kind,
        text: text.to_string(),
        source,
        status: MemoryStatus::Active,
        origin,
        superseded_by: None,
        // What we know now is that it is true now. A model guessing at when a
        // preference started would be inventing a date the user never gave.
        valid_from: Some(now),
        valid_to: None,
        use_count: 0,
        created_at: now,
        updated_at: now,
    }
}

fn log(
    store: &Store,
    memory_id: &str,
    op: Op,
    before: Option<&str>,
    after: Option<&str>,
    reason: Option<&str>,
    now: i64,
) -> Result<()> {
    store.append_memory_event(&MemoryEvent {
        id: uuid::Uuid::new_v4().to_string(),
        memory_id: memory_id.to_string(),
        op: op.as_str().to_string(),
        before_text: before.map(str::to_string),
        after_text: after.map(str::to_string),
        reason: reason.map(str::to_string),
        created_at: now,
    })
}

// ---------------------------------------------------------------------------
// Candidates
// ---------------------------------------------------------------------------

/// Stored memories that might be about the same thing as `text`.
///
/// Two paths, unioned: cosine over the memory vectors when embeddings are
/// configured, and the substring search over salient words either way. The
/// substring path is not a fallback — an exact name or address is precisely what
/// a vector is worst at, and a contact memory is mostly names and addresses.
async fn related(
    store: &Store,
    text: &str,
    vector: Option<&[f32]>,
    embedding: &EmbeddingSettings,
) -> Result<Vec<MemoryEntry>> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut out: Vec<MemoryEntry> = Vec::new();

    if let Some(query) = vector {
        let mut scored: Vec<(f32, String)> = store
            .active_memory_vectors(embedding.model.trim())?
            .into_iter()
            .filter_map(|(id, v)| crate::rag::cosine(query, &v).map(|s| (s, id)))
            .filter(|(s, _)| *s >= RELATED_FLOOR)
            .collect();
        scored.sort_by(|a, b| b.0.total_cmp(&a.0));
        for (_, id) in scored.into_iter().take(MAX_CANDIDATES) {
            if let Some(m) = store.get_memory(&id)? {
                if m.status == MemoryStatus::Active && seen.insert(m.id.clone()) {
                    out.push(m);
                }
            }
        }
    }

    for term in salient_terms(text) {
        if out.len() >= MAX_CANDIDATES {
            break;
        }
        for m in store.search_memories(&term, PER_TERM)? {
            if out.len() >= MAX_CANDIDATES {
                break;
            }
            if seen.insert(m.id.clone()) {
                out.push(m);
            }
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// The reconciler
// ---------------------------------------------------------------------------

struct Decision {
    op: Op,
    target: Option<String>,
    text: Option<String>,
    reason: Option<String>,
}

/// The prompt is the whole design of this module, so it is worth reading.
///
/// The failure it is written against is over-merging. A model asked "are these
/// the same?" will happily fold "账单发到 a@x" and "发票发到 b@y" into one
/// sentence, and the user loses a fact they never contradicted. Hence the rule
/// about material details, the insistence that two things can be true at once,
/// and the instruction to prefer NOOP over a speculative edit.
const RECONCILE_SYSTEM: &str = r#"You are the memory reconciler for a personal email assistant. Decide how one NEW
statement about the user relates to what is already stored.

Reply with ONLY this JSON object and nothing else:
{"op":"ADD"|"UPDATE"|"SUPERSEDE"|"NOOP","target":<id or null>,"text":<string or null>,"reason":"<12 words or fewer>"}

ADD        Nothing stored covers this. "target" is null, "text" is null.
UPDATE     A stored statement means the same thing, and the new one is more
           complete. Keep that id in "target"; "text" is the merged statement.
SUPERSEDE  A stored statement is now WRONG, because the user changed their mind
           or their circumstances changed. "target" is the id being retired,
           "text" is the replacement. The old one is kept as history.
NOOP       Something stored already says this. "text" is null.

Rules:
- Statements differing in ANY material detail — a different person, address,
  amount, date, product or account — are DIFFERENT statements. ADD them. Never
  merge them.
- Two things being true at once is not a contradiction. Choose SUPERSEDE only
  when the new statement cannot be true at the same time as the old one.
- Preferences supersede. Facts about separate things accumulate.
- Never add detail that is not in the NEW statement.
- Prefer NOOP over a speculative edit, and ADD over a merge you are unsure of.
- "reason" is for the user's audit log. Say what you decided and why, briefly."#;

async fn reconcile(
    http: &reqwest::Client,
    ai: &AiSettings,
    kind: MemoryKind,
    text: &str,
    candidates: &[MemoryEntry],
) -> Decision {
    let fallback = Decision { op: Op::Add, target: None, text: None, reason: None };
    if !ai.is_configured() {
        // No model to ask. An ADD is the honest answer: the exact-match path has
        // already run, so this is at worst a near-duplicate, and a wrong merge
        // loses information a wrong duplicate does not.
        return fallback;
    }

    let mut user = format!("NEW (kind={}): {text}\n\nSTORED:\n", kind_str(kind));
    for m in candidates {
        user.push_str(&format!(
            "{} | {} | since {} | {}\n",
            m.id,
            kind_str(m.kind),
            m.valid_from.or(Some(m.created_at)).map(iso_day).unwrap_or_default(),
            m.text
        ));
    }

    let raw = match crate::ai::chat_json(http, ai, RECONCILE_SYSTEM, &user, RECONCILE_TOKENS).await
    {
        Ok(raw) => raw,
        Err(e) => {
            // A memory the user asked for must not be lost to a flaky endpoint.
            tracing::warn!("memory: 归并调用失败，按新增处理: {e}");
            return fallback;
        }
    };

    parse_decision(&raw, candidates).unwrap_or_else(|| {
        tracing::debug!("memory: 无法解析归并结果，按新增处理: {}", first_line(&raw));
        Decision { op: Op::Add, target: None, text: None, reason: None }
    })
}

/// Read the reconciler's answer, refusing anything that does not name a
/// candidate it was actually shown.
fn parse_decision(raw: &str, candidates: &[MemoryEntry]) -> Option<Decision> {
    let value: Value = serde_json::from_str(json_object(raw)?).ok()?;
    let op = match value["op"].as_str()?.trim().to_ascii_uppercase().as_str() {
        "ADD" => Op::Add,
        "UPDATE" => Op::Update,
        "SUPERSEDE" => Op::Supersede,
        "NOOP" => Op::Noop,
        _ => return None,
    };
    let target = value["target"]
        .as_str()
        .map(str::trim)
        .filter(|id| candidates.iter().any(|c| c.id == *id))
        .map(str::to_string);
    // An op about a stored memory that names none of them is not actionable, and
    // guessing which one was meant is how the wrong memory gets rewritten.
    if matches!(op, Op::Update | Op::Supersede | Op::Noop) && target.is_none() {
        return None;
    }
    Some(Decision {
        op,
        target,
        text: value["text"].as_str().map(str::to_string).filter(|t| !t.trim().is_empty()),
        reason: value["reason"]
            .as_str()
            .map(|r| truncate_chars(&collapse_ws(r), 120))
            .filter(|r| !r.is_empty()),
    })
}

/// The first `{…}` in a reply, so a model that wraps its JSON in a fence or a
/// sentence is still understood.
fn json_object(raw: &str) -> Option<&str> {
    let start = raw.find('{')?;
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (i, ch) in raw[start..].char_indices() {
        if in_string {
            match ch {
                _ if escaped => escaped = false,
                '\\' => escaped = true,
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match ch {
            '"' => in_string = true,
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&raw[start..start + i + ch.len_utf8()]);
                }
            }
            _ => {}
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Read path
// ---------------------------------------------------------------------------

/// What the model should be told about this user before it answers `question`.
///
/// Standing preferences first — they are about every answer, not this one — then
/// whatever the question itself retrieved, by vector and by substring. Injected
/// memories are marked as used, which is the signal eviction ranks by.
pub async fn for_question(
    store: &Store,
    http: &reqwest::Client,
    embedding: &EmbeddingSettings,
    question: &str,
) -> Result<Vec<MemoryEntry>> {
    let mut out: Vec<MemoryEntry> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    for m in store.standing_preferences(MAX_STANDING)? {
        if seen.insert(m.id.clone()) {
            out.push(m);
        }
    }

    let vector = embed_one(http, embedding, question).await;
    for m in related(store, question, vector.as_deref(), embedding).await? {
        if out.len() >= MAX_INJECTED {
            break;
        }
        if seen.insert(m.id.clone()) {
            out.push(m);
        }
    }
    out.truncate(MAX_INJECTED);

    let ids: Vec<String> = out.iter().map(|m| m.id.clone()).collect();
    // Best-effort: failing to record a use must not cost the user their answer.
    if let Err(e) = store.touch_memories(&ids, now_ms()) {
        tracing::debug!("memory: 记录使用次数失败: {e}");
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Housekeeping
// ---------------------------------------------------------------------------

/// Keep the table bounded: retire the least-used assistant facts past the cap,
/// then delete history older than 90 days and trim the audit trail.
pub fn vacuum(store: &Store, now: i64) -> Result<u32> {
    let mut retired = 0;
    if store.count_active_memories()? > MAX_ACTIVE {
        retired = store.evict_memories(MAX_ACTIVE, now)?;
        if retired > 0 {
            tracing::info!("memory: 记忆超过 {MAX_ACTIVE} 条，{retired} 条最少用到的转入历史");
        }
    }
    store.prune_memory_history(now - HISTORY_MS, MAX_EVENTS)?;
    Ok(retired)
}

// ---------------------------------------------------------------------------
// Text
// ---------------------------------------------------------------------------

/// The form two sentences are compared by. Case, spacing and a trailing stop
/// are not differences in meaning, and treating them as such is what let the old
/// table fill with twins.
pub fn normalize(text: &str) -> String {
    let lowered = collapse_ws(text).to_lowercase();
    lowered.trim_end_matches(['。', '.', '！', '!', '，', ',', '、', ';', '；']).trim().to_string()
}

/// Words worth looking memories up by.
///
/// `Store::search_memories` is a substring match, so this has to produce
/// substrings. Latin words come out whole; Chinese has no spaces, so a CJK run is
/// emitted as overlapping two-character windows — enough to match 老王 inside
/// "老王的邮箱" without a segmentation dictionary. Filler characters are dropped:
/// a term like 一下 matches everything and ranks nothing.
pub fn salient_terms(text: &str) -> Vec<String> {
    const MAX_TERMS: usize = 8;
    const LATIN_STOP: &[&str] = &[
        "the", "and", "for", "you", "your", "with", "what", "when", "where", "which", "this",
        "that", "from", "have", "has", "was", "are", "can", "did", "does", "about", "please",
        "tell", "show", "give", "mail", "email",
    ];
    const CJK_STOP: &str = "的了吗呢吧啊哦呀我你您他她它们这那些什么怎么样请帮把和跟与在是有个么下上再还就都也很最不没有过要会能给让说看多少一二三四五六七八九十封件事情时候";

    let mut terms: Vec<String> = Vec::new();
    let mut push = |t: String| {
        if terms.len() < MAX_TERMS && !terms.contains(&t) {
            terms.push(t);
        }
    };

    for run in text.split(|c: char| !is_word_char(c)) {
        if run.is_empty() {
            continue;
        }
        if run.chars().all(|c| c.is_ascii_alphanumeric()) {
            let word = run.to_ascii_lowercase();
            if word.chars().count() >= 2 && !LATIN_STOP.contains(&word.as_str()) {
                push(word);
            }
            continue;
        }
        let chars: Vec<char> = run.chars().collect();
        // A one- or two-character run is already the term; windowing it would
        // produce nothing for 老王 on its own.
        if chars.len() <= 2 {
            if !chars.iter().all(|c| CJK_STOP.contains(*c)) {
                push(run.to_string());
            }
            continue;
        }
        for pair in chars.windows(2) {
            if !pair.iter().any(|c| CJK_STOP.contains(*c)) {
                push(pair.iter().collect::<String>());
            }
        }
    }
    terms
}

/// Letters, digits and CJK — everything else is a separator. An address splits
/// into its words on purpose: `wang` and `acme` each match, where the whole
/// string would only match itself.
fn is_word_char(c: char) -> bool {
    c.is_alphanumeric()
}

fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn truncate_chars(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((i, _)) => s[..i].to_string(),
        None => s.to_string(),
    }
}

fn first_line(s: &str) -> String {
    truncate_chars(&collapse_ws(s), 160)
}

pub fn kind_str(k: MemoryKind) -> &'static str {
    match k {
        MemoryKind::Preference => "preference",
        MemoryKind::Fact => "fact",
        MemoryKind::Contact => "contact",
    }
}

/// `2026-03-02` from unix millis. The reconciler and the prompt both want a day,
/// not a timestamp: "since 2026-03" is the kind of thing that decides whether a
/// statement superseded another, and an epoch number is not.
fn iso_day(ms: i64) -> String {
    use chrono::TimeZone;
    chrono::Local.timestamp_millis_opt(ms).single().map(|t| t.format("%Y-%m-%d").to_string()).unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Embedding
// ---------------------------------------------------------------------------

/// One vector, or `None` when embeddings are off, unconfigured or failing.
/// Every caller treats `None` as "rank by substring instead".
async fn embed_one(
    http: &reqwest::Client,
    embedding: &EmbeddingSettings,
    text: &str,
) -> Option<Vec<f32>> {
    if !embedding.enabled {
        return None;
    }
    match crate::rag::embed(http, embedding, &[text.to_string()]).await {
        Ok(mut v) if !v.is_empty() => Some(v.remove(0)),
        Ok(_) => None,
        Err(e) => {
            tracing::debug!("memory: 嵌入失败，改用关键词匹配: {e}");
            None
        }
    }
}

/// Store a memory's vector, ignoring failure: an unvectorised memory is still
/// found by substring, and losing the memory instead would be worse.
async fn store_vector(
    store: &Store,
    http: &reqwest::Client,
    embedding: &EmbeddingSettings,
    id: &str,
    text: &str,
    now: i64,
) {
    if let Some(v) = embed_one(http, embedding, text).await {
        if let Err(e) = store.put_memory_vector(id, embedding.model.trim(), &v, now) {
            tracing::debug!("memory: 写入记忆向量失败: {e}");
        }
    }
}

// ---------------------------------------------------------------------------
// Public helper for the settings screen
// ---------------------------------------------------------------------------

/// Store a memory the user typed. No reconciliation: a person editing their own
/// preferences means exactly what they wrote, and a model second-guessing that
/// would be the worst possible behaviour here.
pub async fn write_by_hand(
    store: &Store,
    http: &reqwest::Client,
    embedding: &EmbeddingSettings,
    entry: &MemoryEntry,
) -> Result<MemoryEntry> {
    let text = truncate_chars(&collapse_ws(&entry.text), MAX_MEMORY_CHARS);
    if text.is_empty() {
        return Err(crate::error::Error::Other("记忆内容不能为空".into()));
    }
    let now = now_ms();
    let previous = store.get_memory(&entry.id)?;
    let stored = MemoryEntry {
        text: text.clone(),
        origin: MemoryOrigin::User,
        status: MemoryStatus::Active,
        valid_from: previous.as_ref().and_then(|p| p.valid_from).or(Some(now)),
        valid_to: None,
        superseded_by: None,
        created_at: previous.as_ref().map(|p| p.created_at).unwrap_or(now),
        updated_at: now,
        use_count: previous.as_ref().map(|p| p.use_count).unwrap_or(0),
        ..entry.clone()
    };
    store.put_memory(&stored, &normalize(&text))?;
    store_vector(store, http, embedding, &stored.id, &text, now).await;
    log(
        store,
        &stored.id,
        if previous.is_some() { Op::Update } else { Op::Add },
        previous.as_ref().map(|p| p.text.as_str()),
        Some(&text),
        Some("用户手动编辑"),
        now,
    )?;
    Ok(stored)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> Store {
        Store::open_in_memory().unwrap()
    }

    fn off() -> (reqwest::Client, AiSettings, EmbeddingSettings) {
        // Nothing configured: the paths that must work without a model.
        (reqwest::Client::new(), AiSettings::default(), EmbeddingSettings::default())
    }

    async fn add(s: &Store, kind: MemoryKind, text: &str) -> Written {
        let (http, ai, emb) = off();
        remember(s, &http, &ai, &emb, kind, text, None, MemoryOrigin::Assistant).await.unwrap()
    }

    /// Whitespace, case and a trailing stop are not differences in meaning. The
    /// old table de-duplicated on `eq_ignore_ascii_case`, which meant "回信简短。"
    /// and "回信简短" were two memories.
    #[test]
    fn normalisation_ignores_what_is_not_meaning() {
        assert_eq!(normalize("  回信要简短。 "), "回信要简短");
        assert_eq!(normalize("Reply  BRIEFLY!"), "reply briefly");
        assert_eq!(normalize("老王 = wang@acme.com,"), "老王 = wang@acme.com");
        assert_ne!(normalize("账单发到 a@x"), normalize("账单发到 b@y"), "addresses differ");
    }

    #[tokio::test]
    async fn the_same_sentence_twice_is_one_memory_and_no_model_call() {
        let s = store();
        let first = add(&s, MemoryKind::Preference, "回信要简短").await;
        assert_eq!(first.op, Op::Add);

        // Punctuation and spacing differ; the meaning does not.
        let again = add(&s, MemoryKind::Preference, " 回信要简短。 ").await;
        assert_eq!(again.op, Op::Noop);
        assert_eq!(again.entry.id, first.entry.id);
        assert_eq!(s.list_memories().unwrap().len(), 1);

        // The NOOP still counts as a use, which is what eviction ranks by.
        assert_eq!(s.get_memory(&first.entry.id).unwrap().unwrap().use_count, 1);
    }

    /// With no chat model there is nothing to reconcile with, and a wrong merge
    /// loses information a duplicate does not. So: two rows, both readable.
    #[tokio::test]
    async fn without_a_model_a_near_duplicate_is_added_not_merged() {
        let s = store();
        add(&s, MemoryKind::Preference, "回信要简短").await;
        let second = add(&s, MemoryKind::Preference, "回复的时候别写太长").await;
        assert_eq!(second.op, Op::Add);
        assert_eq!(s.list_memories().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn every_write_leaves_a_trail() {
        let s = store();
        let w = add(&s, MemoryKind::Contact, "老王是 wang@acme.com").await;
        let events = s.memory_events(Some(&w.entry.id), 10).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].op, "add");
        assert_eq!(events[0].after_text.as_deref(), Some("老王是 wang@acme.com"));
        assert!(events[0].before_text.is_none());
    }

    /// A superseded memory is history: still readable, never injected, and it
    /// points at what replaced it.
    #[tokio::test]
    async fn superseding_keeps_the_old_row_as_history() {
        let s = store();
        let old = add(&s, MemoryKind::Fact, "账单发到 gmail 那个地址").await;
        let new = add(&s, MemoryKind::Fact, "账单都转到 outlook 了").await;

        let now = now_ms();
        s.supersede_memory(&old.entry.id, &new.entry.id, now).unwrap();

        let active = s.list_memories().unwrap();
        assert_eq!(active.len(), 1, "only the current statement is active");
        assert_eq!(active[0].id, new.entry.id);

        let retired = s.get_memory(&old.entry.id).unwrap().unwrap();
        assert_eq!(retired.status, MemoryStatus::Superseded);
        assert_eq!(retired.superseded_by.as_deref(), Some(new.entry.id.as_str()));
        assert_eq!(retired.valid_to, Some(now));

        let history = s.superseded_memories(10).unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].id, old.entry.id);

        // And it must not come back through search.
        assert!(s.search_memories("gmail", 10).unwrap().is_empty());
    }

    /// The reconciler decides what happens to a *stored* memory, so a decision
    /// naming a memory it was not shown cannot be trusted — it is a hallucinated
    /// id, and acting on it would rewrite an unrelated row.
    #[test]
    fn a_decision_must_name_a_candidate_it_was_shown() {
        let shown = vec![MemoryEntry {
            id: "m1".into(),
            text: "回信要简短".into(),
            ..Default::default()
        }];

        let good = parse_decision(r#"{"op":"UPDATE","target":"m1","text":"回信简短且不客套"}"#, &shown)
            .expect("names a shown candidate");
        assert_eq!(good.op, Op::Update);
        assert_eq!(good.target.as_deref(), Some("m1"));

        assert!(
            parse_decision(r#"{"op":"UPDATE","target":"invented","text":"x"}"#, &shown).is_none(),
            "an invented id is refused"
        );
        assert!(
            parse_decision(r#"{"op":"SUPERSEDE","target":null,"text":"x"}"#, &shown).is_none(),
            "superseding nothing is not actionable"
        );
        // ADD needs no target, and is the shape most answers take.
        let added = parse_decision(r#"{"op":"ADD","target":null,"text":null,"reason":"新事实"}"#, &shown)
            .unwrap();
        assert_eq!(added.op, Op::Add);
        assert_eq!(added.reason.as_deref(), Some("新事实"));
    }

    /// Models wrap JSON in fences and prose. Reading the first balanced object
    /// is the difference between a working reconciler and one that always ADDs.
    #[test]
    fn the_decision_object_is_found_inside_whatever_the_model_wrapped_it_in() {
        let shown =
            vec![MemoryEntry { id: "m1".into(), text: "x".into(), ..Default::default() }];
        for raw in [
            "```json\n{\"op\":\"NOOP\",\"target\":\"m1\"}\n```",
            "Sure! {\"op\":\"NOOP\",\"target\":\"m1\"} — hope that helps",
            "{\"op\":\"NOOP\",\"target\":\"m1\",\"text\":null,\"reason\":\"already stored\"}",
            // A brace inside a string must not close the object early.
            "{\"op\":\"NOOP\",\"target\":\"m1\",\"reason\":\"has a } in it\"}",
        ] {
            assert_eq!(parse_decision(raw, &shown).map(|d| d.op), Some(Op::Noop), "{raw}");
        }
        assert!(parse_decision("no json here", &shown).is_none());
        assert!(parse_decision(r#"{"op":"REWRITE","target":"m1"}"#, &shown).is_none());
    }

    /// A preference applies to a question that shares no words with it — that is
    /// what makes it a preference — so it is fetched by use, not relevance.
    #[tokio::test]
    async fn preferences_are_injected_whatever_the_question_is() {
        let s = store();
        let (http, _, emb) = off();
        add(&s, MemoryKind::Preference, "回答尽量简短").await;
        add(&s, MemoryKind::Contact, "老王是 wang@acme.com").await;

        let picked = for_question(&s, &http, &emb, "上个月的电费是多少").await.unwrap();
        assert!(
            picked.iter().any(|m| m.text.contains("简短")),
            "the preference has to travel: {picked:?}"
        );

        // Injection counts as a use.
        let pref = s.standing_preferences(5).unwrap();
        assert_eq!(pref[0].use_count, 1);
    }

    #[tokio::test]
    async fn a_question_retrieves_the_memory_that_answers_it() {
        let s = store();
        let (http, _, emb) = off();
        add(&s, MemoryKind::Contact, "老王是 wang@acme.com").await;
        add(&s, MemoryKind::Fact, "房租每月 3200").await;

        let picked = for_question(&s, &http, &emb, "老王的邮箱是什么").await.unwrap();
        assert!(picked.iter().any(|m| m.text.contains("wang@acme.com")), "{picked:?}");
    }

    /// The three model-driven ops, end to end, without a model: what the
    /// reconciler decides is only useful if `apply` turns it into the right rows.
    #[tokio::test]
    async fn each_decision_produces_the_rows_it_promises() {
        let s = store();
        let (http, _, emb) = off();
        let old = add(&s, MemoryKind::Fact, "账单发到 gmail 那个地址").await;

        let decide = |op: Op, target: &str, text: &str| Decision {
            op,
            target: Some(target.to_string()),
            text: Some(text.to_string()),
            reason: Some("测试".into()),
        };
        let run = |d: Decision, text: String| {
            let (http, emb) = (http.clone(), emb.clone());
            let s = &s;
            async move {
                apply(
                    s,
                    &http,
                    &emb,
                    d,
                    MemoryKind::Fact,
                    &text,
                    &normalize(&text),
                    None,
                    MemoryOrigin::Assistant,
                    None,
                    now_ms(),
                )
                .await
                .unwrap()
            }
        };

        // UPDATE keeps the id and the creation time: the same belief, reworded.
        let merged = run(
            decide(Op::Update, &old.entry.id, "账单发到 gmail，每月 1 号"),
            "账单发到 gmail 那个地址（每月 1 号）".into(),
        )
        .await;
        assert_eq!(merged.op, Op::Update);
        assert_eq!(merged.entry.id, old.entry.id);
        assert_eq!(merged.entry.created_at, old.entry.created_at);
        assert_eq!(merged.entry.text, "账单发到 gmail，每月 1 号");
        assert_eq!(s.list_memories().unwrap().len(), 1, "no second row");

        // SUPERSEDE writes a new row and retires the old one, keeping both.
        let replaced =
            run(decide(Op::Supersede, &old.entry.id, ""), "账单都转到 outlook 了".into()).await;
        assert_eq!(replaced.op, Op::Supersede);
        assert_ne!(replaced.entry.id, old.entry.id);
        assert_eq!(replaced.retired.as_ref().unwrap().status, MemoryStatus::Superseded);
        assert_eq!(
            replaced.retired.as_ref().unwrap().superseded_by.as_deref(),
            Some(replaced.entry.id.as_str())
        );
        let active = s.list_memories().unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].text, "账单都转到 outlook 了");

        // The trail carries the reason, which is the whole point of keeping it.
        let trail = s.memory_events(Some(&replaced.entry.id), 10).unwrap();
        assert_eq!(trail[0].op, "supersede");
        assert_eq!(trail[0].before_text.as_deref(), Some("账单发到 gmail，每月 1 号"));
        assert_eq!(trail[0].reason.as_deref(), Some("测试"));
    }

    /// A decision about a memory that has since been deleted must not create a
    /// row pointing at nothing, and must not silently drop the statement either.
    #[tokio::test]
    async fn a_decision_about_a_vanished_memory_becomes_an_add() {
        let s = store();
        let (http, _, emb) = off();
        let out = apply(
            &s,
            &http,
            &emb,
            Decision {
                op: Op::Supersede,
                target: Some("gone".into()),
                text: None,
                reason: None,
            },
            MemoryKind::Fact,
            "新的事实",
            &normalize("新的事实"),
            None,
            MemoryOrigin::Assistant,
            None,
            now_ms(),
        )
        .await
        .unwrap();
        assert_eq!(out.op, Op::Add);
        assert_eq!(out.entry.text, "新的事实");
        assert!(out.retired.is_none());
    }

    /// The user's own words are not the model's to overwrite.
    #[tokio::test]
    async fn a_hand_written_memory_is_never_merged_away() {
        let s = store();
        let (http, _, emb) = off();
        let mine = write_by_hand(
            &s,
            &http,
            &emb,
            &MemoryEntry {
                id: uuid::Uuid::new_v4().to_string(),
                kind: MemoryKind::Preference,
                text: "  永远用中文回答  ".into(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(mine.text, "永远用中文回答", "collapsed, not mangled");
        assert_eq!(mine.origin, MemoryOrigin::User);

        // An UPDATE aimed at it is downgraded to an ADD rather than obeyed.
        let out = apply(
            &s,
            &http,
            &emb,
            Decision {
                op: Op::Update,
                target: Some(mine.id.clone()),
                text: Some("用英文回答".into()),
                reason: None,
            },
            MemoryKind::Preference,
            "用英文回答",
            &normalize("用英文回答"),
            None,
            MemoryOrigin::Assistant,
            None,
            now_ms(),
        )
        .await
        .unwrap();
        assert_eq!(out.op, Op::Add);
        assert_eq!(s.get_memory(&mine.id).unwrap().unwrap().text, "永远用中文回答");
    }

    /// Past the cap the least-used assistant facts are retired — but never a
    /// preference and never anything the user typed.
    #[tokio::test]
    async fn eviction_spares_preferences_and_hand_written_memories() {
        let s = store();
        let now = now_ms();
        let mut ids = Vec::new();
        for i in 0..6 {
            let e = MemoryEntry {
                id: format!("fact{i}"),
                kind: MemoryKind::Fact,
                text: format!("事实 {i}"),
                origin: MemoryOrigin::Assistant,
                use_count: i,
                created_at: now,
                updated_at: now + i as i64,
                ..Default::default()
            };
            s.put_memory(&e, &normalize(&e.text)).unwrap();
            ids.push(e.id.clone());
        }
        let pref = MemoryEntry {
            id: "pref".into(),
            kind: MemoryKind::Preference,
            text: "简短".into(),
            created_at: now,
            updated_at: now,
            ..Default::default()
        };
        s.put_memory(&pref, "简短").unwrap();
        let mine = MemoryEntry {
            id: "mine".into(),
            kind: MemoryKind::Fact,
            text: "我自己写的".into(),
            origin: MemoryOrigin::User,
            created_at: now,
            updated_at: now,
            ..Default::default()
        };
        s.put_memory(&mine, "我自己写的").unwrap();

        // Keep three of the eight.
        let retired = s.evict_memories(3, now + 100).unwrap();
        assert_eq!(retired, 5);
        let left: HashSet<String> = s.list_memories().unwrap().into_iter().map(|m| m.id).collect();
        assert!(left.contains("pref"), "a preference is never evicted");
        assert!(left.contains("mine"), "the user's own line is never evicted");
        assert!(left.contains("fact5"), "the most-used fact survives: {left:?}");
        assert!(!left.contains("fact0"), "the least-used goes first");
    }

    #[tokio::test]
    async fn history_is_dropped_only_after_ninety_days() {
        let s = store();
        let now = now_ms();
        let old = add(&s, MemoryKind::Fact, "去年的事").await;
        let recent = add(&s, MemoryKind::Fact, "上周的事").await;
        s.supersede_memory(&old.entry.id, "x", now - HISTORY_MS - 1).unwrap();
        s.supersede_memory(&recent.entry.id, "y", now - 1000).unwrap();

        let gone = s.prune_memory_history(now - HISTORY_MS, MAX_EVENTS).unwrap();
        assert_eq!(gone, 1);
        assert!(s.get_memory(&old.entry.id).unwrap().is_none());
        assert!(s.get_memory(&recent.entry.id).unwrap().is_some(), "recent history stays");
    }

    #[tokio::test]
    async fn the_table_stays_bounded() {
        let s = store();
        let now = now_ms();
        for i in 0..(MAX_ACTIVE + 20) {
            let e = MemoryEntry {
                id: format!("m{i}"),
                kind: MemoryKind::Fact,
                text: format!("事实 {i}"),
                use_count: i,
                created_at: now,
                updated_at: now,
                ..Default::default()
            };
            s.put_memory(&e, &normalize(&e.text)).unwrap();
        }
        vacuum(&s, now).unwrap();
        assert_eq!(s.count_active_memories().unwrap(), MAX_ACTIVE);
    }

    #[test]
    fn salient_terms_drop_filler() {
        let terms = salient_terms("帮我查一下老王的账单邮件");
        assert!(terms.contains(&"老王".to_string()));
        assert!(terms.contains(&"账单".to_string()));
        assert!(!terms.iter().any(|t| t.contains('的')));

        let latin = salient_terms("What did Stripe send about invoice 4471?");
        assert!(latin.contains(&"stripe".to_string()));
        assert!(latin.contains(&"invoice".to_string()));
        assert!(!latin.contains(&"what".to_string()));

        assert!(salient_terms("").is_empty());
        assert!(salient_terms("???").is_empty());
    }

    /// The question decides which facts travel; an unrelated contact must not.
    #[tokio::test]
    async fn an_unrelated_memory_stays_out_of_the_prompt() {
        let s = store();
        let (http, _, emb) = off();
        add(&s, MemoryKind::Contact, "老王是 wang@example.com").await;
        add(&s, MemoryKind::Contact, "小李是 li@example.com").await;
        add(&s, MemoryKind::Preference, "回答尽量简短").await;

        let picked = for_question(&s, &http, &emb, "老王的邮箱是多少").await.unwrap();
        let texts: Vec<&str> = picked.iter().map(|m| m.text.as_str()).collect();
        assert!(texts.iter().any(|t| t.contains("wang@example.com")), "{texts:?}");
        assert!(!texts.iter().any(|t| t.contains("li@example.com")), "{texts:?}");
        assert!(texts.iter().any(|t| t.contains("简短")), "偏好始终适用: {texts:?}");
    }

    #[test]
    fn a_long_memory_is_bounded() {
        let long = "字".repeat(MAX_MEMORY_CHARS + 100);
        assert_eq!(truncate_chars(&long, MAX_MEMORY_CHARS).chars().count(), MAX_MEMORY_CHARS);
    }

    #[tokio::test]
    async fn an_empty_memory_is_refused() {
        let s = store();
        let (http, ai, emb) = off();
        assert!(
            remember(&s, &http, &ai, &emb, MemoryKind::Fact, "   ", None, MemoryOrigin::Assistant)
                .await
                .is_err()
        );
    }
}
