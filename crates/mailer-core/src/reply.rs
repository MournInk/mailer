//! Who a reply goes to, and what a forward looks like.
//!
//! Answering one person is easy. Answering a thread is where clients get it
//! wrong, and the wrong answer is silent: the mail sends, it looks fine, and
//! four people never learn what was decided. So the recipient set is built
//! here, as a pure function over the message, and tested — rather than
//! assembled in the compose window where nothing can check it.
//!
//! Two rules do most of the work:
//!
//! - **You are never a recipient of your own reply.** Every address the
//!   account owns comes out, or every reply-all CCs you a copy of your own
//!   mail — and on a busy thread that compounds with each round.
//! - **Nobody appears twice.** Comparison is case-insensitive, because
//!   `Alice@Example.com` and `alice@example.com` are the same mailbox and
//!   only one of them should be written down.

use crate::types::EmailMessage;

/// Where a reply is addressed.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Recipients {
    pub to: Vec<String>,
    pub cc: Vec<String>,
}

/// Build the recipient set for a reply.
///
/// `me` is every address this account answers to; all of them are excluded.
/// With `all` false this is just the sender — the safe default, and what the
/// button says. With `all` true the thread is preserved: the sender and the
/// original To line become To, and the original Cc stays Cc.
pub fn reply_recipients(msg: &EmailMessage, me: &[String], all: bool) -> Recipients {
    if !all {
        return Recipients { to: dedup(&[msg.from_addr.clone()], &[]), cc: Vec::new() };
    }

    let mut to_pool = vec![msg.from_addr.clone()];
    to_pool.extend(msg.to_addrs.iter().cloned());
    let mut to = dedup(&to_pool, me);
    let mut cc = dedup(&msg.cc_addrs, me);
    // Anyone already on the To line does not also belong on Cc.
    cc.retain(|addr| !to.iter().any(|t| same_addr(t, addr)));

    // Everyone left was us. Rather than send a mail addressed to nobody,
    // promote the copies — and failing that, answer the sender, even if the
    // sender is this account replying to its own message.
    if to.is_empty() {
        to = std::mem::take(&mut cc);
    }
    if to.is_empty() {
        to = vec![msg.from_addr.clone()];
    }
    Recipients { to, cc }
}

/// `Re: ...`, without stacking a second one on a subject that has it.
pub fn reply_subject(subject: &str) -> String {
    prefixed(subject, "Re", crate::thread::normalize_subject(subject).is_reply)
}

/// `Fwd: ...`. A forward of a reply keeps the `Re:` — the subject people
/// recognise is the one that has been on the thread all along.
pub fn forward_subject(subject: &str) -> String {
    let already = subject.trim().to_lowercase();
    let already = already.starts_with("fwd:")
        || already.starts_with("fw:")
        || already.starts_with("转发:")
        || already.starts_with("转发：");
    prefixed(subject, "Fwd", already)
}

fn prefixed(subject: &str, tag: &str, already: bool) -> String {
    let s = subject.trim();
    if s.is_empty() {
        return format!("{tag}: (无主题)");
    }
    if already {
        return s.to_string();
    }
    format!("{tag}: {s}")
}

/// The quoted original that goes under a forward.
///
/// Plain text on purpose: the compose window sends `text/plain`, so an HTML
/// mail is flattened rather than pasted as markup the recipient would see raw.
/// The header block is the conventional one every client prints, because a
/// forward whose provenance is missing is just an unattributed wall of text.
pub fn forward_body(msg: &EmailMessage, when: &str) -> String {
    let mut out = String::from("\n\n---------- 转发的邮件 ----------\n");
    out.push_str(&format!(
        "发件人: {} <{}>\n",
        if msg.from_name.is_empty() { &msg.from_addr } else { &msg.from_name },
        msg.from_addr
    ));
    out.push_str(&format!("日期: {when}\n"));
    out.push_str(&format!(
        "主题: {}\n",
        if msg.subject.is_empty() { "(无主题)" } else { &msg.subject }
    ));
    if !msg.to_addrs.is_empty() {
        out.push_str(&format!("收件人: {}\n", msg.to_addrs.join(", ")));
    }
    if !msg.cc_addrs.is_empty() {
        out.push_str(&format!("抄送: {}\n", msg.cc_addrs.join(", ")));
    }
    out.push('\n');
    out.push_str(&body_text(msg));
    out
}

/// The quoted original that goes under a reply — the same block, marked as a
/// quote so a reader can tell where the new text stops.
pub fn reply_body(msg: &EmailMessage, when: &str) -> String {
    let who = if msg.from_name.is_empty() { &msg.from_addr } else { &msg.from_name };
    let mut out = format!("\n\n在 {when}，{who} <{}> 写道：\n", msg.from_addr);
    for line in body_text(msg).lines() {
        out.push_str("> ");
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// The message as plain text, however it arrived.
fn body_text(msg: &EmailMessage) -> String {
    if let Some(text) = msg.body_text.as_deref().filter(|t| !t.trim().is_empty()) {
        return text.to_string();
    }
    if let Some(html) = msg.body_html.as_deref() {
        return crate::mail::parse::html_to_text(html);
    }
    msg.snippet.clone()
}

/// Case-insensitive mailbox comparison.
fn same_addr(a: &str, b: &str) -> bool {
    a.trim().eq_ignore_ascii_case(b.trim())
}

/// Keep the first spelling of each address, drop blanks and anything in `skip`.
fn dedup(addrs: &[String], skip: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(addrs.len());
    for addr in addrs {
        let addr = addr.trim();
        if addr.is_empty()
            || skip.iter().any(|s| same_addr(s, addr))
            || out.iter().any(|o| same_addr(o, addr))
        {
            continue;
        }
        out.push(addr.to_string());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg() -> EmailMessage {
        EmailMessage {
            id: "m1".into(),
            account_id: "acc1".into(),
            folder: "INBOX".into(),
            uid: "1".into(),
            message_id: Some("m1@example.com".into()),
            references: Vec::new(),
            thread_id: "m1".into(),
            subject: "Q3 预算".into(),
            from_name: "李敏".into(),
            from_addr: "limin@example.com".into(),
            to_addrs: vec!["me@example.com".into(), "bob@example.com".into()],
            cc_addrs: vec!["carol@example.com".into()],
            date: 1_700_000_000_000,
            snippet: "预算表".into(),
            body_text: Some("第 9 行要不要重算？".into()),
            body_html: None,
            attachments: Vec::new(),
            unread: true,
            starred: false,
            category: None,
            analysis: None,
            received_at: 1_700_000_000_000,
        }
    }

    const ME: &[&str] = &["me@example.com"];

    fn me() -> Vec<String> {
        ME.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_plain_reply_goes_to_the_sender_only() {
        let r = reply_recipients(&msg(), &me(), false);
        assert_eq!(r.to, ["limin@example.com"]);
        assert!(r.cc.is_empty());
    }

    /// The bug this module exists to prevent: four people dropped silently.
    #[test]
    fn reply_all_keeps_everyone_on_the_thread() {
        let r = reply_recipients(&msg(), &me(), true);
        assert_eq!(r.to, ["limin@example.com", "bob@example.com"]);
        assert_eq!(r.cc, ["carol@example.com"]);
    }

    #[test]
    fn reply_all_never_addresses_the_account_itself() {
        let r = reply_recipients(&msg(), &me(), true);
        assert!(!r.to.iter().chain(r.cc.iter()).any(|a| a.contains("me@")));
    }

    /// An account with several addresses drops all of them, not just the one
    /// the mail happened to be delivered to.
    #[test]
    fn every_address_the_account_owns_is_excluded() {
        let m = EmailMessage {
            to_addrs: vec!["me@example.com".into(), "bob@example.com".into()],
            cc_addrs: vec!["alias@example.com".into()],
            ..msg()
        };
        let mine = vec!["me@example.com".to_string(), "alias@example.com".to_string()];
        let r = reply_recipients(&m, &mine, true);
        assert_eq!(r.to, ["limin@example.com", "bob@example.com"]);
        assert!(r.cc.is_empty());
    }

    #[test]
    fn a_mailbox_spelled_two_ways_is_one_recipient() {
        let m = EmailMessage {
            from_addr: "Limin@Example.com".into(),
            to_addrs: vec!["limin@example.com".into(), "BOB@example.com".into()],
            cc_addrs: vec!["bob@example.com".into()],
            ..msg()
        };
        let r = reply_recipients(&m, &me(), true);
        assert_eq!(r.to, ["Limin@Example.com", "BOB@example.com"]);
        assert!(r.cc.is_empty(), "bob is already on To");
    }

    /// Replying to a mail you sent to one person: everyone but you is the
    /// recipient, and you are the sender.
    #[test]
    fn replying_to_your_own_message_addresses_the_people_you_wrote_to() {
        let m = EmailMessage {
            from_addr: "me@example.com".into(),
            from_name: "我".into(),
            to_addrs: vec!["limin@example.com".into()],
            cc_addrs: Vec::new(),
            ..msg()
        };
        let r = reply_recipients(&m, &me(), true);
        assert_eq!(r.to, ["limin@example.com"]);
    }

    /// A note to yourself has nobody else on it. Answering it must still
    /// produce a sendable mail rather than an empty envelope.
    #[test]
    fn a_message_only_from_you_to_you_still_gets_a_recipient() {
        let m = EmailMessage {
            from_addr: "me@example.com".into(),
            to_addrs: vec!["me@example.com".into()],
            cc_addrs: Vec::new(),
            ..msg()
        };
        let r = reply_recipients(&m, &me(), true);
        assert_eq!(r.to, ["me@example.com"]);
        assert!(r.cc.is_empty());
    }

    /// Everyone on To was us, but there are others on Cc: they become the
    /// recipients rather than the mail going out Cc-only.
    #[test]
    fn copies_are_promoted_when_the_to_line_empties_out() {
        let m = EmailMessage {
            from_addr: "me@example.com".into(),
            to_addrs: vec!["me@example.com".into()],
            cc_addrs: vec!["carol@example.com".into()],
            ..msg()
        };
        let r = reply_recipients(&m, &me(), true);
        assert_eq!(r.to, ["carol@example.com"]);
        assert!(r.cc.is_empty());
    }

    #[test]
    fn blank_addresses_are_dropped() {
        let m = EmailMessage {
            to_addrs: vec!["  ".into(), "bob@example.com".into()],
            cc_addrs: vec!["".into()],
            ..msg()
        };
        let r = reply_recipients(&m, &me(), true);
        assert_eq!(r.to, ["limin@example.com", "bob@example.com"]);
        assert!(r.cc.is_empty());
    }

    #[test]
    fn reply_subject_does_not_stack_prefixes() {
        assert_eq!(reply_subject("Q3 预算"), "Re: Q3 预算");
        assert_eq!(reply_subject("Re: Q3 预算"), "Re: Q3 预算");
        assert_eq!(reply_subject("回复: Q3 预算"), "回复: Q3 预算");
        assert_eq!(reply_subject("  "), "Re: (无主题)");
    }

    #[test]
    fn forward_subject_does_not_stack_but_keeps_re() {
        assert_eq!(forward_subject("Q3 预算"), "Fwd: Q3 预算");
        assert_eq!(forward_subject("Fwd: Q3 预算"), "Fwd: Q3 预算");
        assert_eq!(forward_subject("转发: Q3 预算"), "转发: Q3 预算");
        // A forwarded reply keeps the Re: — that is the thread's name.
        assert_eq!(forward_subject("Re: Q3 预算"), "Fwd: Re: Q3 预算");
    }

    #[test]
    fn a_forward_carries_the_original_headers_and_body() {
        let out = forward_body(&msg(), "2026年7月26日");
        assert!(out.contains("转发的邮件"));
        assert!(out.contains("李敏 <limin@example.com>"));
        assert!(out.contains("主题: Q3 预算"));
        assert!(out.contains("抄送: carol@example.com"));
        assert!(out.contains("第 9 行要不要重算？"));
    }

    #[test]
    fn a_reply_quotes_every_line_of_the_original() {
        let m = EmailMessage { body_text: Some("一\n二".into()), ..msg() };
        let out = reply_body(&m, "2026年7月26日");
        assert!(out.contains("李敏 <limin@example.com> 写道："));
        assert!(out.contains("> 一\n> 二"));
    }

    /// HTML-only mail is flattened rather than quoted as markup — the compose
    /// window sends text/plain, so the tags would arrive visible.
    #[test]
    fn an_html_only_message_is_flattened_for_quoting() {
        let m = EmailMessage {
            body_text: None,
            body_html: Some("<p>你好</p><p>再见</p>".into()),
            ..msg()
        };
        let out = forward_body(&m, "2026年7月26日");
        assert!(out.contains("你好"), "{out}");
        assert!(!out.contains("<p>"), "{out}");
    }
}
