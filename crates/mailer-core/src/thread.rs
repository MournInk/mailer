//! Grouping a reply chain into one conversation.
//!
//! A thread is not a field anyone sends. RFC 5322 gives us `Message-ID`,
//! `In-Reply-To` and `References`, and the chain has to be reconstructed from
//! them — every client does this, and every client does it slightly wrong,
//! because the headers are advisory and half the mail in a real mailbox is
//! missing them.
//!
//! So there are two strategies here, in strict order of trust:
//!
//! 1. **References.** If a mail cites an ID we already store, it belongs to
//!    that mail's thread. This is exact, and it is the only rule that can
//!    join two mails with different subjects.
//! 2. **Subject.** If a mail cites nothing, but its subject is a *reply* to
//!    a subject we already have, join that. This is the guess, and it is
//!    fenced: the incoming mail must carry a reply or forward prefix, and the
//!    match must be recent. Without the prefix rule, two unrelated mails
//!    titled 「发票」 become one conversation.
//!
//! Neither rule ever merges across accounts. Two mailboxes that both received
//! the same list mail are two conversations to the person reading them.

use std::collections::BTreeSet;

/// How far back the subject fallback will reach. A reply six months later to
/// a subject as generic as "Hi" is far more likely to be a coincidence than a
/// continuation, and the reference chain still catches the real ones.
pub const SUBJECT_WINDOW_MS: i64 = 30 * 24 * 60 * 60 * 1000;

/// Longest prefix we will treat as a reply marker. Real ones are short
/// ("Re", "转发"); this stops a subject like "Status update: week 3" from
/// having its first clause examined at all.
const MAX_PREFIX_CHARS: usize = 8;

/// Reply markers, lowercased. Latin ones come from the language the sender's
/// client was localised into — a Chinese team using Outlook in German is not a
/// hypothetical.
/// Deliberately not here: "ref". "Ref: INV-4471" is a subject, not a reply
/// marker, and treating it as one would thread every invoice a vendor sends.
const REPLY: &[&str] =
    &["re", "aw", "sv", "vs", "odp", "ynt", "回复", "回覆", "答复", "答覆", "回信"];

/// Forward markers. Kept separate from `REPLY` because a forward is a weaker
/// signal of continuation — but still a signal, and folding a forwarded copy
/// into its own thread is the behaviour people expect.
const FORWARD: &[&str] = &["fw", "fwd", "wg", "tr", "enc", "转发", "轉發", "转寄", "轉寄"];

/// What a subject's prefixes said about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Subject {
    /// Case-folded, whitespace-collapsed, prefixes and list tags removed.
    /// Empty when the subject was empty or was *only* prefixes.
    pub norm: String,
    /// The subject announced itself as continuing something.
    pub is_reply: bool,
}

/// Strip reply prefixes and list tags down to the subject people actually
/// wrote, so "Re: [rust-dev] 回复: Patch v2" and "Patch v2" compare equal.
///
/// Stripping loops: real subjects accumulate prefixes one client at a time,
/// and "答复: Re: Fwd: ..." is what a mail looks like after crossing three of
/// them.
pub fn normalize_subject(subject: &str) -> Subject {
    let mut rest = subject.trim();
    let mut is_reply = false;

    loop {
        if let Some(stripped) = strip_list_tag(rest) {
            rest = stripped;
            continue;
        }
        match strip_prefix(rest) {
            Some((stripped, reply)) => {
                is_reply |= reply;
                rest = stripped;
            }
            None => break,
        }
    }

    Subject { norm: collapse(rest), is_reply }
}

/// `[rust-dev] Patch v2` → `Patch v2`.
///
/// Only when something survives: a subject that *is* a tag ("[FYI]") keeps it,
/// since the alternative is threading it with every other empty subject.
fn strip_list_tag(s: &str) -> Option<&str> {
    let rest = s.strip_prefix('[')?;
    let end = rest.find(']')?;
    let tail = rest[end + 1..].trim_start();
    (!tail.is_empty()).then_some(tail)
}

/// `Re[2]: x` → `x`. Returns whether the marker was a reply (vs a forward).
///
/// The separator may be a fullwidth colon: Chinese and Japanese clients write
/// "回复：" with one, and a byte-wise `:` check silently misses every one of
/// them.
fn strip_prefix(s: &str) -> Option<(&str, bool)> {
    let sep = s.find([':', '：'])?;
    let head = s[..sep].trim();
    if head.chars().count() > MAX_PREFIX_CHARS {
        return None;
    }
    // "Re[2]" and "Re(2)" are how Outlook and Lotus count round trips.
    let word = head
        .split_once(['[', '('])
        .map(|(w, _)| w)
        .unwrap_or(head)
        .trim()
        .to_lowercase();
    if word.is_empty() {
        return None;
    }
    let reply = REPLY.contains(&word.as_str());
    if !reply && !FORWARD.contains(&word.as_str()) {
        return None;
    }
    let tail = s[sep + separator_len(s, sep)..].trim_start();
    Some((tail, reply))
}

fn separator_len(s: &str, at: usize) -> usize {
    s[at..].chars().next().map(char::len_utf8).unwrap_or(1)
}

/// Case-fold and collapse runs of whitespace, so subjects that differ only in
/// how a client re-wrapped them still compare equal.
fn collapse(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut gap = false;
    for ch in s.trim().chars() {
        if ch.is_whitespace() {
            gap = true;
            continue;
        }
        if gap && !out.is_empty() {
            out.push(' ');
        }
        gap = false;
        out.extend(ch.to_lowercase());
    }
    out
}

/// Every ancestor a message claims, oldest first, deduped.
///
/// `References` is the full chain and `In-Reply-To` is the immediate parent,
/// which is usually its last entry — but only usually, so both are read. The
/// order matters: the store walks this list looking for the *closest* ancestor
/// it already has, and closest means last.
pub fn ancestors(references: &[String], in_reply_to: &[String]) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for id in references.iter().chain(in_reply_to.iter()) {
        let id = unwrap_id(id);
        if id.is_empty() || !seen.insert(id.to_string()) {
            continue;
        }
        out.push(id.to_string());
    }
    out
}

/// `<abc@host>` → `abc@host`. Stored IDs are unwrapped, so cited ones must be
/// too or nothing will ever match.
pub fn unwrap_id(raw: &str) -> &str {
    raw.trim().trim_start_matches('<').trim_end_matches('>').trim()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn norm(s: &str) -> String {
        normalize_subject(s).norm
    }

    #[test]
    fn strips_stacked_prefixes_across_languages() {
        assert_eq!(norm("Re: Patch v2"), "patch v2");
        assert_eq!(norm("回复: Patch v2"), "patch v2");
        assert_eq!(norm("答复：Patch v2"), "patch v2");
        assert_eq!(norm("Fwd: Re: 回复: Patch v2"), "patch v2");
        assert_eq!(norm("AW: Patch v2"), "patch v2");
    }

    #[test]
    fn strips_round_trip_counters() {
        assert_eq!(norm("Re[2]: Patch v2"), "patch v2");
        assert_eq!(norm("Re(5): Patch v2"), "patch v2");
    }

    #[test]
    fn strips_one_list_tag_but_keeps_a_bare_one() {
        assert_eq!(norm("[rust-dev] Patch v2"), "patch v2");
        assert_eq!(norm("Re: [rust-dev] Patch v2"), "patch v2");
        // Nothing would survive, so the tag is the subject.
        assert_eq!(norm("[FYI]"), "[fyi]");
    }

    /// The guard that keeps "Invoice: 2024" from being read as a prefix.
    #[test]
    fn leaves_real_subjects_that_contain_a_colon_alone() {
        assert_eq!(norm("Invoice: 2024-03"), "invoice: 2024-03");
        assert_eq!(norm("会议纪要：三月"), "会议纪要：三月");
        // Long enough that the head is never even examined.
        assert_eq!(norm("Status update for the week: shipped"), "status update for the week: shipped");
    }

    #[test]
    fn reply_flag_separates_a_reply_from_an_original() {
        assert!(normalize_subject("Re: Patch").is_reply);
        assert!(normalize_subject("回复: Patch").is_reply);
        assert!(!normalize_subject("Patch").is_reply);
        // A forward is not a reply — it may start its own conversation.
        assert!(!normalize_subject("Fwd: Patch").is_reply);
    }

    /// "Ref:" is how invoices and tickets are titled, not how replies are.
    #[test]
    fn ref_is_not_a_reply_marker() {
        assert_eq!(norm("Ref: INV-4471"), "ref: inv-4471");
        assert!(!normalize_subject("Ref: INV-4471").is_reply);
    }

    #[test]
    fn collapses_rewrapped_whitespace_and_case() {
        assert_eq!(norm("  Patch   v2\r\n  final "), "patch v2 final");
        assert_eq!(norm("PATCH V2"), "patch v2");
    }

    #[test]
    fn empty_and_prefix_only_subjects_normalise_to_nothing() {
        assert_eq!(norm(""), "");
        assert_eq!(norm("Re:"), "");
        assert_eq!(norm("Re: Fwd:  "), "");
    }

    #[test]
    fn ancestors_dedupe_and_keep_the_closest_last() {
        let refs = vec!["<a@x>".to_string(), "<b@x>".to_string()];
        let irt = vec!["<b@x>".to_string()];
        assert_eq!(ancestors(&refs, &irt), vec!["a@x", "b@x"]);
    }

    /// In-Reply-To naming an ancestor References omitted still counts.
    #[test]
    fn ancestors_include_an_in_reply_to_outside_the_chain() {
        let refs = vec!["<a@x>".to_string()];
        let irt = vec!["<c@x>".to_string()];
        assert_eq!(ancestors(&refs, &irt), vec!["a@x", "c@x"]);
    }

    #[test]
    fn ancestors_drop_empties_and_bare_brackets() {
        let refs = vec!["".to_string(), "<>".to_string(), " <a@x> ".to_string()];
        assert_eq!(ancestors(&refs, &[]), vec!["a@x"]);
    }
}
