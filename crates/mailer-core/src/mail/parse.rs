//! RFC 5322 / MIME → [`EmailMessage`] using `mail-parser`.
//!
//! CONTRACT: `parse_mail` never fails on malformed input if it can salvage
//! anything — missing headers degrade to empty strings and the date falls
//! back to `now_ms`. `Err` is reserved for completely unusable payloads.

use std::sync::OnceLock;

use mail_parser::{Address, MessageParser, MessagePart, MimeHeaders, PartType};

use crate::error::{Error, Result};
use crate::mail::RawMail;
use crate::types::{AttachmentMeta, EmailMessage};

/// Preview length in *characters* — never bytes: a byte slice would panic in
/// the middle of a CJK code point.
const SNIPPET_CHARS: usize = 140;

/// How much of an HTML body the snippet is allowed to look at.
///
/// A marketing mail is routinely megabytes of nested tables, and flattening all
/// of it to keep 140 characters costs two allocations the size of the message —
/// on every message of every sync. The readable text of a newsletter is almost
/// entirely markup, so the budget is deliberately generous: 64 KB of source has
/// never failed to contain the first 140 characters a human would read, and
/// when it somehow does the snippet falls back to being empty rather than wrong.
const HTML_SCAN_BYTES: usize = 64 * 1024;

/// Shown for attachments whose headers carry no filename at all.
const UNNAMED_ATTACHMENT: &str = "未命名附件";

/// Fallback MIME type for parts without a usable Content-Type.
const DEFAULT_MIME: &str = "application/octet-stream";

/// The IDs in a `References`-shaped header.
///
/// A single ID parses as `Text` and several as `TextList`, and a client that
/// wrote a malformed one leaves neither — all three shapes have to be handled
/// or threading quietly stops working for whoever sent that mail.
fn id_list(v: &mail_parser::HeaderValue<'_>) -> Vec<String> {
    match v {
        mail_parser::HeaderValue::Text(s) => vec![s.as_ref().to_string()],
        mail_parser::HeaderValue::TextList(list) => {
            list.iter().map(|s| s.as_ref().to_string()).collect()
        }
        _ => Vec::new(),
    }
}

/// The parser itself is stateless, but constructing it builds a header
/// dispatch table; build that once for the whole process.
fn parser() -> &'static MessageParser {
    static PARSER: OnceLock<MessageParser> = OnceLock::new();
    PARSER.get_or_init(MessageParser::default)
}

/// Parse raw bytes into a stored message.
///
/// - `id` is assigned by the caller (uuid v4).
/// - `snippet` is plain text, whitespace-collapsed, ≤ 140 chars.
/// - `body_text`/`body_html` capture the best text and HTML bodies.
/// - `date` = Date header as unix millis, else `now_ms`.
pub fn parse_mail(id: String, account_id: &str, raw: &RawMail, now_ms: i64) -> Result<EmailMessage> {
    let parsed = parser()
        .parse(raw.bytes.as_slice())
        .ok_or_else(|| Error::Parse(format!("无法解析邮件 (uid {})", raw.uid)))?;

    // Missing headers are routine in the wild: degrade, never fail.
    let subject = parsed.subject().unwrap_or_default().to_string();
    let (from_name, from_addr) = first_addr(parsed.from());
    let to_addrs = addr_list(parsed.to());
    let cc_addrs = addr_list(parsed.cc());
    let message_id = parsed.message_id().map(str::to_string);
    let references = crate::thread::ancestors(&id_list(parsed.references()), &id_list(parsed.in_reply_to()));

    // Date header → unix millis. Absent or nonsensical dates (year 0, month
    // 13, ...) fall back to the ingest time so the list still sorts sanely.
    let date = parsed
        .date()
        .filter(|d| d.is_valid())
        .map(|d| d.to_timestamp() * 1000)
        .unwrap_or(now_ms);

    // Only genuine parts: `body_text`/`body_html` would happily synthesise a
    // text body out of HTML (and vice versa), which hides the real shape of
    // the mail from the UI and from the snippet fallback below.
    let body_text = parsed.text_bodies().find_map(|p| match &p.body {
        PartType::Text(text) => Some(text.as_ref().to_string()),
        _ => None,
    });
    let body_html = parsed.html_bodies().find_map(|p| match &p.body {
        PartType::Html(html) => Some(html.as_ref().to_string()),
        _ => None,
    });

    let mut snippet = body_text
        .as_deref()
        .map(|text| preview(text, SNIPPET_CHARS))
        .unwrap_or_default();
    if snippet.is_empty() {
        // No text part, or one that was only whitespace: strip the HTML. Only
        // the head of it — see `HTML_SCAN_BYTES`.
        if let Some(html) = body_html.as_deref() {
            snippet = preview(&html_to_text(head(html, HTML_SCAN_BYTES)), SNIPPET_CHARS);
        }
    }

    let attachments = parsed
        .attachments()
        .map(|part| AttachmentMeta {
            filename: part
                .attachment_name()
                .map(str::to_string)
                .unwrap_or_else(|| UNNAMED_ATTACHMENT.to_string()),
            mime: mime_of(part),
            size: part.len() as u64,
        })
        .collect();

    Ok(EmailMessage {
        id,
        account_id: account_id.to_string(),
        folder: raw.folder.clone(),
        uid: raw.uid.clone(),
        message_id,
        references,
        // The store owns this: it is the one place that can see the mails this
        // one is a reply to.
        thread_id: String::new(),
        subject,
        from_name,
        from_addr,
        to_addrs,
        cc_addrs,
        date,
        snippet,
        body_text,
        body_html,
        attachments,
        unread: true,
        starred: false,
        category: None,
        analysis: None,
        received_at: now_ms,
    })
}

// ---------------------------------------------------------------------------
// Headers
// ---------------------------------------------------------------------------

/// First address of an address header as `(display name, address)`.
fn first_addr(header: Option<&Address<'_>>) -> (String, String) {
    match header.and_then(|a| a.first()) {
        Some(addr) => (
            addr.name().unwrap_or_default().trim().to_string(),
            addr.address().unwrap_or_default().trim().to_string(),
        ),
        None => (String::new(), String::new()),
    }
}

/// Every address of an address header, groups flattened, names dropped.
fn addr_list(header: Option<&Address<'_>>) -> Vec<String> {
    header
        .map(|a| {
            a.iter()
                .filter_map(|addr| addr.address())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// `type/subtype`, lowercased, for one MIME part.
fn mime_of(part: &MessagePart<'_>) -> String {
    match part.content_type() {
        Some(ct) => match ct.subtype() {
            Some(sub) => format!(
                "{}/{}",
                ct.ctype().to_ascii_lowercase(),
                sub.to_ascii_lowercase()
            ),
            None => ct.ctype().to_ascii_lowercase(),
        },
        None => DEFAULT_MIME.to_string(),
    }
}

// ---------------------------------------------------------------------------
// Text extraction
// ---------------------------------------------------------------------------

/// Whitespace-collapse `text` and cut it to at most `max_chars` **characters**.
/// Slicing happens on char boundaries, so CJK bodies can't panic here.
fn preview(text: &str, max_chars: usize) -> String {
    let mut out = String::new();
    for word in text.split_whitespace() {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(word);
        // One overshooting word is enough; the take() below does the cut.
        if out.chars().count() >= max_chars {
            break;
        }
    }
    out.chars().take(max_chars).collect::<String>().trim_end().to_string()
}

/// At most `max_bytes` of `s`, cut on a character boundary so the result is
/// still a `str` the tag stripper can walk.
fn head(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    // Walk back to the start of the character that straddles the limit.
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Crude HTML → text for previews: drop markup, decode the entities that
/// actually show up in mail. Good enough for a 140-char snippet, and it never
/// allocates more than the source.
pub fn html_to_text(html: &str) -> String {
    decode_entities(&strip_tags(html))
}

/// Remove tags, replacing each with a space so words don't run together.
/// `<script>`/`<style>` bodies are markup noise and are dropped whole.
fn strip_tags(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    while let Some(lt) = rest.find('<') {
        out.push_str(&rest[..lt]);
        out.push(' ');
        let after = &rest[lt + 1..];

        // Opening <script>/<style>: jump to the matching closer. The closing
        // tag itself starts with '/', so it never re-enters this branch.
        let name: String = after.chars().take_while(char::is_ascii_alphanumeric).collect();
        if name.eq_ignore_ascii_case("script") || name.eq_ignore_ascii_case("style") {
            match find_ascii_ci(after, &format!("</{name}")) {
                Some(close) => {
                    rest = &after[close..];
                    continue;
                }
                // Unterminated block: nothing readable can follow.
                None => return out,
            }
        }

        rest = match after.find('>') {
            Some(gt) => &after[gt + 1..],
            // Unterminated tag: drop the tail rather than emit markup.
            None => return out,
        };
    }
    out.push_str(rest);
    out
}

/// Byte offset of the first ASCII-case-insensitive match of `needle`.
/// `needle` must be ASCII, which keeps the returned offset on a char boundary.
fn find_ascii_ci(haystack: &str, needle: &str) -> Option<usize> {
    let (hay, nee) = (haystack.as_bytes(), needle.as_bytes());
    if nee.is_empty() || hay.len() < nee.len() {
        return None;
    }
    (0..=hay.len() - nee.len()).find(|&i| hay[i..i + nee.len()].eq_ignore_ascii_case(nee))
}

/// Decode the handful of entities worth caring about in a preview.
fn decode_entities(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let tail = &rest[amp..];
        let decoded = [
            ("&amp;", "&"),
            ("&lt;", "<"),
            ("&gt;", ">"),
            ("&quot;", "\""),
            ("&#39;", "'"),
            ("&nbsp;", " "),
        ]
        .into_iter()
        .find(|(entity, _)| tail.starts_with(entity));
        match decoded {
            Some((entity, text)) => {
                out.push_str(text);
                rest = &tail[entity.len()..];
            }
            // Not an entity we know: keep the '&' verbatim.
            None => {
                out.push('&');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod charset_tests {
    use super::*;

    fn raw(headers: &str, body: &str) -> RawMail {
        RawMail {
            uid: "1".into(),
            folder: "INBOX".into(),
            bytes: format!("{headers}\r\n\r\n{body}").into_bytes(),
        }
    }

    /// Chinese senders routinely encode headers in GB2312/GBK rather than
    /// UTF-8. Without mail-parser's full_encoding feature those decode to
    /// replacement characters and the user sees a subject of question marks.
    #[test]
    fn gb2312_encoded_headers_decode_to_chinese() {
        let mail = raw(
            "From: =?GB2312?B?0KHD17+qt8XGvcyo?= <support@example.com>\r\n\
             Subject: =?GB2312?B?0+C27tSkvq8=?=\r\n\
             Date: Sat, 14 Jun 2026 13:12:07 +0800",
            "balance warning",
        );
        let m = parse_mail("id".into(), "acc", &mail, 0).unwrap();
        assert_eq!(m.subject, "余额预警", "subject: {}", m.subject);
        assert_eq!(m.from_name, "小米开放平台", "from: {}", m.from_name);
        assert!(!m.subject.contains('\u{fffd}'));
    }

    /// A GBK body must survive too, not just the headers.
    #[test]
    fn gbk_body_decodes_to_chinese() {
        let body = encoding_bytes("你好，世界");
        let mut bytes =
            b"Content-Type: text/plain; charset=GBK\r\nSubject: t\r\n\r\n".to_vec();
        bytes.extend_from_slice(&body);
        let mail = RawMail { uid: "2".into(), folder: "INBOX".into(), bytes };
        let m = parse_mail("id2".into(), "acc", &mail, 0).unwrap();
        let text = m.body_text.unwrap_or_default();
        assert!(text.contains("你好"), "body: {text}");
    }

    /// GBK bytes for a string, written out so the test does not depend on a
    /// codec crate of its own.
    fn encoding_bytes(s: &str) -> Vec<u8> {
        // 你好，世界 in GBK.
        assert_eq!(s, "你好，世界");
        vec![0xC4, 0xE3, 0xBA, 0xC3, 0xA3, 0xAC, 0xCA, 0xC0, 0xBD, 0xE7]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(bytes: &str) -> RawMail {
        RawMail {
            uid: "42".to_string(),
            folder: "INBOX".to_string(),
            bytes: bytes.as_bytes().to_vec(),
        }
    }

    fn parse(bytes: &str, now_ms: i64) -> EmailMessage {
        parse_mail("id-1".to_string(), "acc1", &raw(bytes), now_ms).unwrap()
    }

    const PLAIN: &str = concat!(
        "From: Alice Example <alice@example.com>\r\n",
        "To: Bob <bob@example.com>, carol@example.com\r\n",
        "Subject: Weekly report\r\n",
        "Message-ID: <abc-123@example.com>\r\n",
        "Date: Tue, 1 Jul 2025 09:30:00 +0000\r\n",
        "\r\n",
        "Hello   Bob,\r\n",
        "\r\n",
        "the   report is attached.\r\n",
    );

    /// Threading needs both headers, unwrapped and in chain order.
    #[test]
    fn reference_headers_become_the_ancestor_chain() {
        let msg = parse(
            concat!(
                "Subject: Re: Weekly report\r\n",
                "References: <a@example.com> <b@example.com>\r\n",
                "In-Reply-To: <b@example.com>\r\n",
                "\r\n",
                "body\r\n",
            ),
            999,
        );
        assert_eq!(msg.references, ["a@example.com", "b@example.com"]);
    }

    /// A single ID parses as a different header shape than several.
    #[test]
    fn a_lone_in_reply_to_still_counts() {
        let msg = parse("In-Reply-To: <a@example.com>\r\n\r\nbody\r\n", 999);
        assert_eq!(msg.references, ["a@example.com"]);
    }

    #[test]
    fn mail_that_cites_nothing_has_no_ancestors() {
        let msg = parse(PLAIN, 999);
        assert!(msg.references.is_empty());
        assert!(msg.thread_id.is_empty(), "the store assigns this, not the parser");
    }

    #[test]
    fn plain_ascii_mail() {
        let msg = parse(PLAIN, 999);

        assert_eq!(msg.id, "id-1");
        assert_eq!(msg.account_id, "acc1");
        assert_eq!(msg.folder, "INBOX");
        assert_eq!(msg.uid, "42");
        assert_eq!(msg.subject, "Weekly report");
        assert_eq!(msg.from_name, "Alice Example");
        assert_eq!(msg.from_addr, "alice@example.com");
        assert_eq!(msg.to_addrs, ["bob@example.com", "carol@example.com"]);
        assert_eq!(msg.message_id.as_deref(), Some("abc-123@example.com"));
        // 2025-07-01T09:30:00Z
        assert_eq!(msg.date, 1_751_362_200_000);
        assert_eq!(msg.received_at, 999);
        // Runs of whitespace and the blank line collapse to single spaces.
        assert_eq!(msg.snippet, "Hello Bob, the report is attached.");
        assert!(msg.body_text.unwrap().contains("Hello   Bob,"));
        assert!(msg.body_html.is_none());
        assert!(msg.attachments.is_empty());
        assert!(msg.unread);
        assert!(!msg.starred);
        assert!(msg.category.is_none());
        assert!(msg.analysis.is_none());
    }

    /// UTF-8 everywhere: RFC 2047 subject, Chinese body long enough to force
    /// the 140-char cut (a byte slice would panic), plus an attachment.
    #[test]
    fn utf8_multipart_with_attachment() {
        let mail = concat!(
            "From: 张三 <zhangsan@example.cn>\r\n",
            "To: me@example.com\r\n",
            "Subject: =?UTF-8?B?5rWL6K+V6YKu5Lu277ya6LSm5Y2V5o+Q6YaS?=\r\n",
            "Date: Wed, 2 Jul 2025 10:00:00 +0800\r\n",
            "MIME-Version: 1.0\r\n",
            "Content-Type: multipart/mixed; boundary=\"BND\"\r\n",
            "\r\n",
            "--BND\r\n",
            "Content-Type: text/plain; charset=utf-8\r\n",
            "Content-Transfer-Encoding: 8bit\r\n",
            "\r\n",
            "你好，这是一封中文测试邮件。\r\n",
            "你好，这是一封中文测试邮件。\r\n",
            "你好，这是一封中文测试邮件。\r\n",
            "你好，这是一封中文测试邮件。\r\n",
            "你好，这是一封中文测试邮件。\r\n",
            "你好，这是一封中文测试邮件。\r\n",
            "你好，这是一封中文测试邮件。\r\n",
            "你好，这是一封中文测试邮件。\r\n",
            "你好，这是一封中文测试邮件。\r\n",
            "你好，这是一封中文测试邮件。\r\n",
            "--BND\r\n",
            "Content-Type: application/pdf; name=\"invoice.pdf\"\r\n",
            "Content-Disposition: attachment; filename=\"invoice.pdf\"\r\n",
            "Content-Transfer-Encoding: base64\r\n",
            "\r\n",
            "SGVsbG8=\r\n",
            "--BND--\r\n",
        );

        let msg = parse(mail, 5_000);

        assert_eq!(msg.subject, "测试邮件：账单提醒");
        assert_eq!(msg.from_name, "张三");
        assert_eq!(msg.from_addr, "zhangsan@example.cn");
        // +0800 offset applied: 2025-07-02T02:00:00Z.
        assert_eq!(msg.date, 1_751_421_600_000);

        // Exactly 140 chars, cut on a char boundary, not mid code point.
        assert_eq!(msg.snippet.chars().count(), 140);
        assert!(msg.snippet.starts_with("你好，这是一封中文测试邮件。 你好"));

        assert_eq!(msg.attachments.len(), 1);
        assert_eq!(msg.attachments[0].filename, "invoice.pdf");
        assert_eq!(msg.attachments[0].mime, "application/pdf");
        assert_eq!(msg.attachments[0].size, 5); // decoded "Hello"
    }

    /// No Subject, no Date, no Message-ID, no To: everything degrades.
    #[test]
    fn missing_headers_degrade() {
        let mail = "From: nobody@example.com\r\n\r\nbody only\r\n";
        let msg = parse(mail, 123_456);

        assert_eq!(msg.subject, "");
        assert_eq!(msg.from_name, "");
        assert_eq!(msg.from_addr, "nobody@example.com");
        assert!(msg.to_addrs.is_empty());
        assert!(msg.message_id.is_none());
        assert_eq!(msg.date, 123_456, "absent Date falls back to now_ms");
        assert_eq!(msg.snippet, "body only");
    }

    /// An unparseable Date must not poison the sort order either.
    #[test]
    fn invalid_date_falls_back() {
        let mail = "Subject: x\r\nDate: not a date at all\r\n\r\nhi\r\n";
        assert_eq!(parse(mail, 777).date, 777);
    }

    /// HTML-only mail: the snippet is stripped of markup and never panics on
    /// the multi-byte content.
    #[test]
    fn html_only_mail_is_stripped() {
        let mail = concat!(
            "From: shop@example.com\r\n",
            "Subject: Sale\r\n",
            "MIME-Version: 1.0\r\n",
            "Content-Type: text/html; charset=utf-8\r\n",
            "\r\n",
            "<html><head><style>body { color: red; }</style></head>\r\n",
            "<body><p>Hello&nbsp;&amp; welcome&#39;s &lt;world&gt;</p>\r\n",
            "<script>var x = \"<b>no</b>\";</script>\r\n",
            "<div>中文 &quot;引号&quot; 测试</div></body></html>\r\n",
        );

        let msg = parse(mail, 1);

        assert!(msg.body_text.is_none(), "no text/plain part exists");
        assert!(msg.body_html.unwrap().contains("<div>"));
        for entity in ["&amp;", "&nbsp;", "&quot;", "&#39;", "&lt;", "&gt;"] {
            assert!(!msg.snippet.contains(entity), "{entity} left in {}", msg.snippet);
        }
        assert!(msg.snippet.contains("Hello & welcome's <world>"));
        assert!(msg.snippet.contains("中文 \"引号\" 测试"));
        assert!(!msg.snippet.contains("color: red"), "style body dropped");
        assert!(!msg.snippet.contains("var x"), "script body dropped");
        assert!(!msg.snippet.contains("<p>"), "tags dropped");
    }

    /// Long CJK HTML is the classic byte-slice panic; assert the cut is clean.
    #[test]
    fn html_snippet_cuts_on_char_boundary() {
        let body = "验证码".repeat(200);
        let mail = format!(
            "Content-Type: text/html; charset=utf-8\r\n\r\n<p>{body}</p>\r\n"
        );
        let msg = parse(&mail, 1);

        assert_eq!(msg.snippet.chars().count(), SNIPPET_CHARS);
        assert!(msg.snippet.starts_with("验证码验证码"));
    }

    /// Garbage in, no panic and no error: parsing is deliberately forgiving.
    #[test]
    fn garbage_still_parses() {
        let msg = parse("not even close to a mail", 42);
        assert_eq!(msg.subject, "");
        assert_eq!(msg.from_addr, "");
        assert_eq!(msg.date, 42);
    }

    #[test]
    fn strip_tags_handles_unterminated_markup() {
        assert_eq!(strip_tags("a<b>c").trim(), "a c");
        assert_eq!(strip_tags("keep me <span").trim(), "keep me");
        assert_eq!(strip_tags("<style>x").trim(), "");
    }

    #[test]
    fn decode_entities_leaves_unknown_ampersands() {
        assert_eq!(decode_entities("a &amp; b &unknown; c"), "a & b &unknown; c");
        assert_eq!(decode_entities("&lt;&gt;&quot;&#39;&nbsp;"), "<>\"' ");
    }

    /// A huge HTML body must not be flattened in full to produce 140
    /// characters, and the cut must never land inside a character.
    #[test]
    fn only_the_head_of_a_huge_html_body_is_flattened() {
        assert_eq!(head("abc", 10), "abc");
        assert_eq!(head("abcdef", 3), "abc");
        // The 3-byte character straddling the limit is dropped whole.
        assert_eq!(head("验证码", 4), "验");
        assert_eq!(head("验证码", 3), "验");
        assert!(head("验证码", 2).is_empty());

        let body = format!(
            "<p>开头就是正文</p>{}",
            "<td>填充</td>".repeat(HTML_SCAN_BYTES / 4)
        );
        let mail = format!("Content-Type: text/html; charset=utf-8\r\n\r\n{body}\r\n");
        let msg = parse(&mail, 1);
        assert!(msg.snippet.starts_with("开头就是正文"), "{}", msg.snippet);
        assert_eq!(msg.snippet.chars().count(), SNIPPET_CHARS);
        // The body itself is kept whole — only the preview scan is bounded.
        assert!(msg.body_html.unwrap().len() > HTML_SCAN_BYTES);
    }

    #[test]
    fn preview_collapses_and_trims() {
        assert_eq!(preview("  a \n\t b  ", 140), "a b");
        assert_eq!(preview("", 140), "");
        assert_eq!(preview("abcdef", 3), "abc");
    }
}
