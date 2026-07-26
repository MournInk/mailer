//! External notification channels: Telegram bot, QQ bot (OneBot v11 HTTP),
//! generic webhook, Bark.
//!
//! CONTRACT:
//! - `dispatch` — deliver `payload` through `channel`. Channel `config` shapes
//!   are documented on [`crate::types::ChannelKind`]. Invalid/missing config
//!   fields → `Err(Error::Notify(..))` with a message naming the field.
//! - `test`     — send a short self-describing test message through the
//!   channel; returns a human-readable `TestResult` instead of erroring.
//!
//! Message formatting: compact, mobile-friendly, e.g.
//! ```text
//! 📬 重要邮件 · me@example.com
//! 来自: Stripe <receipts@stripe.com>
//! 主题: Your invoice is due
//! 摘要: 10 月账单 $42.00，11 月 1 日到期
//! ```
//! Verification payloads lead with the code. Telegram uses plain text (no
//! parse_mode) to avoid escaping pitfalls.
//!
//! Every channel is built in two steps: [`prepare`] turns config + payload into
//! a [`Request`] (pure, unit-testable, where all validation happens), then
//! [`send`] performs the single POST.

use std::time::Duration;

use serde_json::{json, Value};

use crate::error::{Error, Result};
use crate::types::{Category, ChannelKind, NotifyChannel, NotifyPayload, TestResult};

/// Per-request ceiling. The shared client already has one, but a channel that
/// stalls must never hold up the classification loop behind it.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
/// Longest excerpt of a response body echoed into an error message.
const SNIPPET_CHARS: usize = 300;
/// Header/body caps for the rendered message. Telegram tops out at 4096
/// characters and a push notification shows far less than that anyway.
const MAX_FROM_CHARS: usize = 80;
const MAX_SUBJECT_CHARS: usize = 120;
const MAX_SUMMARY_CHARS: usize = 300;

const TELEGRAM_API: &str = "https://api.telegram.org";
const BARK_SERVER: &str = "https://api.day.app";
/// Bark folds notifications sharing a group into one stack.
const BARK_GROUP: &str = "Mailer";

/// Deliver a payload through one channel.
pub async fn dispatch(
    http: &reqwest::Client,
    channel: &NotifyChannel,
    payload: &NotifyPayload,
) -> Result<()> {
    let request = prepare(channel, payload)?;
    let body = send(http, &request).await?;
    if request.check_onebot {
        check_onebot(request.channel, &body)?;
    }
    Ok(())
}

/// Send a test message through the channel.
pub async fn test(http: &reqwest::Client, channel: &NotifyChannel) -> TestResult {
    let label = if channel.name.trim().is_empty() {
        kind_name(channel.kind).to_string()
    } else {
        channel.name.trim().to_string()
    };
    match dispatch(http, channel, &test_payload()).await {
        Ok(()) => TestResult {
            ok: true,
            message: format!("已通过「{label}」发送测试通知，请确认是否收到。"),
        },
        Err(e) => TestResult { ok: false, message: format!("发送失败: {e}") },
    }
}

/// The message the user should recognize instantly as a self-test.
fn test_payload() -> NotifyPayload {
    NotifyPayload {
        category: Category::Important,
        account_email: "Mailer".to_string(),
        from: "Mailer <noreply@mailer.local>".to_string(),
        subject: "这是一条来自 Mailer 的测试通知".to_string(),
        summary: "收到本条即说明该渠道配置正确，可以正常推送。".to_string(),
        verification_code: None,
        date: chrono::Utc::now().timestamp_millis(),
    }
}

// ---------------------------------------------------------------------------
// Request building
// ---------------------------------------------------------------------------

/// One prepared POST. Built without any I/O so that config validation and body
/// construction can be tested without a network.
#[derive(Debug, Clone, PartialEq)]
struct Request {
    /// Channel display name; prefixes every error message.
    channel: &'static str,
    url: String,
    /// Same URL with secrets masked — the only form allowed in errors/logs.
    display_url: String,
    content_type: &'static str,
    body: String,
    /// Extra user-configured headers (webhook only).
    headers: Vec<(String, String)>,
    bearer: Option<String>,
    /// OneBot answers 200 even when it refused to send, so its body needs a
    /// second look.
    check_onebot: bool,
}

impl Request {
    fn json(channel: &'static str, url: String, display_url: String, body: &Value) -> Request {
        Request {
            channel,
            url,
            display_url,
            content_type: "application/json",
            body: body.to_string(),
            headers: Vec::new(),
            bearer: None,
            check_onebot: false,
        }
    }
}

/// Validate the channel config and build its request.
fn prepare(channel: &NotifyChannel, payload: &NotifyPayload) -> Result<Request> {
    let cfg = &channel.config;
    match channel.kind {
        ChannelKind::Telegram => telegram(cfg, payload),
        ChannelKind::Qqbot => qqbot(cfg, payload),
        ChannelKind::Bark => bark(cfg, payload),
        ChannelKind::Webhook => webhook(cfg, payload),
    }
}

/// `{ botToken, chatId, apiBase? }` → `POST {apiBase}/bot{token}/sendMessage`.
fn telegram(cfg: &Value, payload: &NotifyPayload) -> Result<Request> {
    const NAME: &str = "Telegram";
    let token = req_str(cfg, "botToken", NAME)?;
    // Numeric ids arrive as JSON numbers, channel handles as "@name" strings.
    let chat_id = req_scalar(cfg, "chatId", NAME)?;
    let api = base_url(cfg, "apiBase", TELEGRAM_API, NAME)?;

    // Plain text on purpose: Markdown/HTML parse modes reject unescaped
    // characters that appear in real mail all the time, and a rejected
    // notification is worse than an unstyled one.
    let body = json!({ "chat_id": chat_id, "text": format_message(payload) });
    Ok(Request::json(
        NAME,
        format!("{api}/bot{token}/sendMessage"),
        // The bot token is a credential: never let it reach an error string.
        format!("{api}/bot***/sendMessage"),
        &body,
    ))
}

/// `{ apiBase, accessToken?, targetKind, targetId }` → OneBot v11 HTTP.
fn qqbot(cfg: &Value, payload: &NotifyPayload) -> Result<Request> {
    const NAME: &str = "QQ 机器人";
    let api = req_base_url(cfg, "apiBase", NAME)?;
    let kind = req_str(cfg, "targetKind", NAME)?;
    let target = req_id(cfg, "targetId", NAME)?;
    let text = format_message(payload);

    let (path, body) = match kind.to_ascii_lowercase().as_str() {
        "private" => ("send_private_msg", json!({ "user_id": target, "message": text })),
        "group" => ("send_group_msg", json!({ "group_id": target, "message": text })),
        other => {
            return Err(Error::Notify(format!(
                "{NAME} 配置的 targetKind 无效: {other}（应为 private 或 group）"
            )))
        }
    };

    let url = format!("{api}/{path}");
    let mut request = Request::json(NAME, url.clone(), url, &body);
    request.bearer = opt_str(cfg, "accessToken");
    request.check_onebot = true;
    Ok(request)
}

/// `{ deviceKey, server? }` → `POST {server}/{deviceKey}`.
fn bark(cfg: &Value, payload: &NotifyPayload) -> Result<Request> {
    const NAME: &str = "Bark";
    let key = req_str(cfg, "deviceKey", NAME)?;
    let server = base_url(cfg, "server", BARK_SERVER, NAME)?;

    // Bark renders a bold title above the body: the lead line becomes the
    // title, everything else the body.
    let message = format_message(payload);
    let (title, text) = match message.split_once('\n') {
        Some((t, rest)) => (t.to_string(), rest.to_string()),
        None => ("Mailer".to_string(), message),
    };
    let body = json!({ "title": title, "body": text, "group": BARK_GROUP });
    Ok(Request::json(
        NAME,
        format!("{server}/{key}"),
        // The device key is the push credential — mask it like a token.
        format!("{server}/***"),
        &body,
    ))
}

/// `{ url, headers?, bodyTemplate? }` → raw POST.
fn webhook(cfg: &Value, payload: &NotifyPayload) -> Result<Request> {
    const NAME: &str = "Webhook";
    let url = req_url(cfg, "url", NAME)?;
    let headers = webhook_headers(cfg, NAME)?;

    let (content_type, body) = match opt_str(cfg, "bodyTemplate") {
        // No template: the payload itself, camelCase, as the frontend knows it.
        None => ("application/json", serde_json::to_string(payload)?),
        Some(template) => render_body(&template, payload),
    };

    Ok(Request {
        channel: NAME,
        url: url.clone(),
        display_url: url,
        content_type,
        body,
        headers,
        bearer: None,
        check_onebot: false,
    })
}

fn webhook_headers(cfg: &Value, channel: &str) -> Result<Vec<(String, String)>> {
    let mut out = Vec::new();
    match cfg.get("headers") {
        None | Some(Value::Null) => {}
        Some(Value::Object(map)) => {
            for (k, v) in map {
                let value = match v {
                    Value::String(s) => s.clone(),
                    Value::Number(n) => n.to_string(),
                    Value::Bool(b) => b.to_string(),
                    _ => {
                        return Err(Error::Notify(format!(
                            "{channel} 配置的 headers.{k} 必须是字符串"
                        )))
                    }
                };
                out.push((k.clone(), value));
            }
        }
        Some(_) => {
            return Err(Error::Notify(format!("{channel} 配置的 headers 必须是对象")))
        }
    }
    Ok(out)
}

/// Fill a user template and decide how to send it.
///
/// Values are JSON-escaped first: a subject containing `"` must never break —
/// or worse, restructure — the JSON the user designed. If the result still is
/// not valid JSON (a template with an unquoted placeholder, or a plain-text
/// template), we send the *unescaped* rendering as text/plain rather than
/// posting a broken document.
fn render_body(template: &str, payload: &NotifyPayload) -> (&'static str, String) {
    let escaped = render(template, &placeholders(payload, true));
    if serde_json::from_str::<Value>(&escaped).is_ok() {
        return ("application/json", escaped);
    }
    ("text/plain; charset=utf-8", render(template, &placeholders(payload, false)))
}

/// Supported placeholders, in their raw or JSON-escaped form.
fn placeholders(payload: &NotifyPayload, escape: bool) -> Vec<(&'static str, String)> {
    let values = [
        ("category", payload.category.as_str().to_string()),
        ("subject", payload.subject.clone()),
        ("from", payload.from.clone()),
        ("summary", payload.summary.clone()),
        ("code", payload.verification_code.clone().unwrap_or_default()),
        ("account", payload.account_email.clone()),
        ("date", local_time(payload.date)),
    ];
    values
        .into_iter()
        .map(|(k, v)| (k, if escape { json_escape(&v) } else { v }))
        .collect()
}

/// Substitute `{{name}}` in one pass, so a value that happens to contain a
/// placeholder is never expanded again. Unknown names are left untouched.
fn render(template: &str, values: &[(&'static str, String)]) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find("{{") {
        out.push_str(&rest[..open]);
        let after = &rest[open + 2..];
        let Some(close) = after.find("}}") else {
            // Unterminated placeholder: the remainder is literal text.
            out.push_str(&rest[open..]);
            return out;
        };
        let name = after[..close].trim();
        match values.iter().find(|(k, _)| *k == name) {
            Some((_, v)) => out.push_str(v),
            None => out.push_str(&rest[open..open + 4 + close]),
        }
        rest = &after[close + 2..];
    }
    out.push_str(rest);
    out
}

/// Escape a value for the inside of a JSON string (no surrounding quotes).
fn json_escape(s: &str) -> String {
    let quoted = Value::String(s.to_string()).to_string();
    // `to_string` of a JSON string always yields ASCII quotes around it.
    quoted[1..quoted.len() - 1].to_string()
}

// ---------------------------------------------------------------------------
// Config helpers
// ---------------------------------------------------------------------------

fn kind_name(kind: ChannelKind) -> &'static str {
    match kind {
        ChannelKind::Telegram => "Telegram",
        ChannelKind::Qqbot => "QQ 机器人",
        ChannelKind::Webhook => "Webhook",
        ChannelKind::Bark => "Bark",
    }
}

/// Optional non-empty string field.
fn opt_str(cfg: &Value, key: &str) -> Option<String> {
    cfg.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Required non-empty string field.
fn req_str(cfg: &Value, key: &str, channel: &str) -> Result<String> {
    opt_str(cfg, key).ok_or_else(|| Error::Notify(format!("{channel} 配置缺少 {key}")))
}

/// Required field that may legitimately be a JSON string *or* number (chat ids
/// are numeric but survive a round trip through a text input as strings).
fn req_scalar(cfg: &Value, key: &str, channel: &str) -> Result<String> {
    let missing = || Error::Notify(format!("{channel} 配置缺少 {key}"));
    match cfg.get(key) {
        Some(Value::String(s)) if !s.trim().is_empty() => Ok(s.trim().to_string()),
        Some(Value::Number(n)) => Ok(n.to_string()),
        _ => Err(missing()),
    }
}

/// Required numeric id, accepted as a JSON number or a numeric string.
fn req_id(cfg: &Value, key: &str, channel: &str) -> Result<i64> {
    let raw = req_scalar(cfg, key, channel)?;
    raw.parse::<i64>()
        .map_err(|_| Error::Notify(format!("{channel} 配置的 {key} 不是合法的数字: {raw}")))
}

/// Required absolute http(s) URL, kept verbatim (a trailing slash may matter).
fn req_url(cfg: &Value, key: &str, channel: &str) -> Result<String> {
    let url = req_str(cfg, key, channel)?;
    check_scheme(&url, key, channel)?;
    Ok(url)
}

/// Optional base URL with a fallback; trailing slashes are trimmed so that
/// joining a path never produces `//`.
fn base_url(cfg: &Value, key: &str, default: &str, channel: &str) -> Result<String> {
    let raw = opt_str(cfg, key).unwrap_or_else(|| default.to_string());
    trim_base(&raw, key, channel)
}

/// Required base URL, trimmed like [`base_url`].
fn req_base_url(cfg: &Value, key: &str, channel: &str) -> Result<String> {
    let raw = req_str(cfg, key, channel)?;
    trim_base(&raw, key, channel)
}

fn trim_base(raw: &str, key: &str, channel: &str) -> Result<String> {
    let trimmed = raw.trim_end_matches('/').to_string();
    check_scheme(&trimmed, key, channel)?;
    Ok(trimmed)
}

/// Without a scheme reqwest fails with an opaque "relative URL" error; say what
/// is actually wrong instead.
fn check_scheme(url: &str, key: &str, channel: &str) -> Result<()> {
    if url.starts_with("http://") || url.starts_with("https://") {
        return Ok(());
    }
    Err(Error::Notify(format!(
        "{channel} 配置的 {key} 必须以 http:// 或 https:// 开头: {url}"
    )))
}

// ---------------------------------------------------------------------------
// Message formatting
// ---------------------------------------------------------------------------

/// Emoji + label for the notification's lead line.
fn category_label(category: Category) -> &'static str {
    match category {
        Category::Verification => "🔑 验证邮件",
        Category::Important => "📬 重要邮件",
        Category::Spam => "🗑 垃圾邮件",
        Category::Normal => "📨 新邮件",
    }
}

/// Render the payload as the compact block every channel sends.
fn format_message(payload: &NotifyPayload) -> String {
    let from = clip(&collapse_ws(&payload.from), MAX_FROM_CHARS);
    let subject = clip(&collapse_ws(&payload.subject), MAX_SUBJECT_CHARS);
    let summary = clip(&collapse_ws(&payload.summary), MAX_SUMMARY_CHARS);
    let account = collapse_ws(&payload.account_email);
    let code = payload
        .verification_code
        .as_deref()
        .map(str::trim)
        .filter(|c| !c.is_empty());

    let mut lines: Vec<String> = Vec::with_capacity(5);
    match (payload.category, code) {
        // A one-time code is the only thing the user wants from the lock
        // screen, so it leads and the account moves to the bottom.
        (Category::Verification, Some(code)) => {
            lines.push(format!("🔑 验证码 {code}"));
            if !from.is_empty() {
                lines.push(format!("来自: {from}"));
            }
            if !subject.is_empty() {
                lines.push(format!("主题: {subject}"));
            }
            if !account.is_empty() {
                lines.push(format!("账户: {account}"));
            }
        }
        // Everything else — including a code-less verification mail, e.g. a
        // magic link — leads with the category and the receiving account.
        (category, _) => {
            let label = category_label(category);
            lines.push(if account.is_empty() {
                label.to_string()
            } else {
                format!("{label} · {account}")
            });
            if !from.is_empty() {
                lines.push(format!("来自: {from}"));
            }
            if !subject.is_empty() {
                lines.push(format!("主题: {subject}"));
            }
            if !summary.is_empty() {
                lines.push(format!("摘要: {summary}"));
            }
        }
    }
    lines.push(format!("时间: {}", local_time(payload.date)));
    lines.join("\n")
}

/// Unix millis as local wall-clock time — a UTC stamp on a phone is useless.
fn local_time(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|d| d.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|| "未知时间".to_string())
}

/// Flatten to a single line: every whitespace run becomes one space.
fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Cap at `max` characters (character-wise: subjects are routinely Chinese),
/// marking the cut so the reader knows something was dropped.
fn clip(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_string(),
    }
}

/// One-line excerpt of a response body, for error messages.
fn snippet(s: &str) -> String {
    match collapse_ws(s) {
        t if t.is_empty() => "(空响应)".to_string(),
        t => clip(&t, SNIPPET_CHARS),
    }
}

// ---------------------------------------------------------------------------
// HTTP
// ---------------------------------------------------------------------------

/// POST one prepared request and return the response body.
async fn send(http: &reqwest::Client, request: &Request) -> Result<String> {
    let channel = request.channel;
    let mut builder = http
        .post(&request.url)
        .timeout(REQUEST_TIMEOUT)
        .body(request.body.clone());
    // User headers first, so an explicit Content-Type wins instead of ending up
    // duplicated next to ours.
    let mut has_content_type = false;
    for (k, v) in &request.headers {
        has_content_type |= k.eq_ignore_ascii_case("content-type");
        builder = builder.header(k, v);
    }
    if !has_content_type {
        builder = builder.header(reqwest::header::CONTENT_TYPE, request.content_type);
    }
    if let Some(token) = &request.bearer {
        builder = builder.bearer_auth(token);
    }

    let resp = builder.send().await.map_err(|e| {
        Error::Notify(format!("{channel} 请求 {} 失败: {e}", request.display_url))
    })?;
    let status = resp.status();
    let text = resp
        .text()
        .await
        .map_err(|e| Error::Notify(format!("{channel} 读取响应失败: {e}")))?;

    if !status.is_success() {
        // The body says *why* (bad token, unknown chat, blocked bot), so it
        // travels alongside the status.
        return Err(Error::Notify(format!(
            "{channel} 返回 {}: {}",
            status.as_u16(),
            snippet(&text)
        )));
    }
    Ok(text)
}

/// OneBot answers `200 OK` with `{"status":"failed","retcode":N}` when it
/// refused to send, so a green HTTP status proves nothing on its own.
fn check_onebot(channel: &str, text: &str) -> Result<()> {
    // A non-JSON 2xx body is not ours to judge; the status already vouched.
    let Ok(value) = serde_json::from_str::<Value>(text) else {
        return Ok(());
    };
    let status = value.get("status").and_then(Value::as_str).unwrap_or_default();
    let retcode = value.get("retcode").and_then(Value::as_i64);
    // v11: status is "ok" | "async" | "failed"; retcode 0 = done, 1 = queued.
    let failed =
        status.eq_ignore_ascii_case("failed") || !matches!(retcode, None | Some(0) | Some(1));
    if !failed {
        return Ok(());
    }

    let code = retcode.map(|c| c.to_string()).unwrap_or_else(|| "未知".to_string());
    let detail = ["message", "wording", "msg", "error"]
        .iter()
        .find_map(|k| value.get(*k).and_then(Value::as_str))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| clip(s, SNIPPET_CHARS))
        .unwrap_or_else(|| snippet(text));
    Err(Error::Notify(format!("{channel} 发送失败 (retcode {code}): {detail}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn channel(kind: ChannelKind, config: Value) -> NotifyChannel {
        NotifyChannel {
            id: "ch1".into(),
            name: "测试渠道".into(),
            kind,
            enabled: true,
            notify_categories: vec![Category::Important],
            config,
        }
    }

    fn important() -> NotifyPayload {
        NotifyPayload {
            category: Category::Important,
            account_email: "me@example.com".into(),
            from: "Stripe <receipts@stripe.com>".into(),
            subject: "Your invoice is due".into(),
            summary: "10 月账单 $42.00，11 月 1 日到期".into(),
            verification_code: None,
            date: 1_700_000_000_000,
        }
    }

    fn verification() -> NotifyPayload {
        NotifyPayload {
            category: Category::Verification,
            account_email: "me@example.com".into(),
            from: "GitHub <noreply@github.com>".into(),
            subject: "Sign-in verification".into(),
            summary: "GitHub 登录验证码".into(),
            verification_code: Some("482913".into()),
            date: 1_700_000_000_000,
        }
    }

    fn body_of(request: &Request) -> Value {
        serde_json::from_str(&request.body).expect("body is JSON")
    }

    // -- Formatting --------------------------------------------------------

    /// A one-time code has to be readable without opening anything.
    #[test]
    fn verification_leads_with_the_code() {
        let text = format_message(&verification());
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "🔑 验证码 482913");
        assert_eq!(lines[1], "来自: GitHub <noreply@github.com>");
        assert_eq!(lines[2], "主题: Sign-in verification");
        assert_eq!(lines[3], "账户: me@example.com");
        assert!(lines[4].starts_with("时间: "), "missing time: {text}");
    }

    #[test]
    fn important_leads_with_category_and_account() {
        let text = format_message(&important());
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "📬 重要邮件 · me@example.com");
        assert_eq!(lines[1], "来自: Stripe <receipts@stripe.com>");
        assert_eq!(lines[2], "主题: Your invoice is due");
        assert_eq!(lines[3], "摘要: 10 月账单 $42.00，11 月 1 日到期");
        assert!(lines[4].starts_with("时间: "), "missing time: {text}");
    }

    /// A magic-link login carries no code: it must not render "验证码 " alone.
    #[test]
    fn verification_without_code_uses_the_summary_layout() {
        let mut p = verification();
        p.verification_code = Some("   ".into());
        let text = format_message(&p);
        assert!(text.starts_with("🔑 验证邮件 · me@example.com"), "got {text}");
        assert!(text.contains("摘要: GitHub 登录验证码"), "got {text}");
    }

    /// Folded headers must not turn one notification into five lines.
    #[test]
    fn multiline_fields_are_flattened_and_capped() {
        let mut p = important();
        p.subject = "Your\n  invoice\tis due".into();
        p.summary = "账".repeat(MAX_SUMMARY_CHARS + 10);
        let text = format_message(&p);
        assert!(text.contains("主题: Your invoice is due"), "got {text}");
        assert_eq!(text.lines().count(), 5);
        assert_eq!(text.chars().filter(|c| *c == '账').count(), MAX_SUMMARY_CHARS);
        assert!(text.contains('…'), "truncation not marked: {text}");
    }

    #[test]
    fn empty_fields_are_dropped_not_rendered_blank() {
        let mut p = important();
        p.account_email = String::new();
        p.summary = String::new();
        let text = format_message(&p);
        assert!(text.starts_with("📬 重要邮件\n"), "got {text}");
        assert!(!text.contains("摘要:"), "got {text}");
    }

    #[test]
    fn unknown_timestamps_do_not_panic() {
        let mut p = important();
        p.date = i64::MAX;
        assert!(format_message(&p).contains("时间: 未知时间"));
    }

    // -- Telegram ----------------------------------------------------------

    #[test]
    fn telegram_posts_plain_text_to_send_message() {
        let ch = channel(
            ChannelKind::Telegram,
            json!({ "botToken": "123:ABC", "chatId": "-1001234" }),
        );
        let req = prepare(&ch, &verification()).unwrap();
        assert_eq!(req.url, "https://api.telegram.org/bot123:ABC/sendMessage");
        let body = body_of(&req);
        assert_eq!(body["chat_id"], "-1001234");
        assert!(body["text"].as_str().unwrap().starts_with("🔑 验证码 482913"));
        // parse_mode would reject unescaped characters from real mail.
        assert!(body.get("parse_mode").is_none(), "parse_mode was set");
    }

    /// The bot token is a credential; it may never reach an error string.
    #[test]
    fn telegram_masks_the_token_in_the_display_url() {
        let ch = channel(
            ChannelKind::Telegram,
            json!({ "botToken": "123:ABC", "chatId": 42, "apiBase": "https://tg.example.com/" }),
        );
        let req = prepare(&ch, &important()).unwrap();
        assert_eq!(req.url, "https://tg.example.com/bot123:ABC/sendMessage");
        assert_eq!(req.display_url, "https://tg.example.com/bot***/sendMessage");
        assert!(!req.display_url.contains("123:ABC"));
        // A numeric chatId survives the trip through the config blob.
        assert_eq!(body_of(&req)["chat_id"], "42");
    }

    #[test]
    fn telegram_names_the_missing_field() {
        let cases = [
            (json!({ "chatId": "1" }), "Telegram 配置缺少 botToken"),
            (json!({ "botToken": "t" }), "Telegram 配置缺少 chatId"),
            (json!({ "botToken": "t", "chatId": "  " }), "Telegram 配置缺少 chatId"),
        ];
        for (cfg, want) in cases {
            let err = prepare(&channel(ChannelKind::Telegram, cfg), &important()).unwrap_err();
            assert!(err.to_string().contains(want), "got {err}");
        }
    }

    #[test]
    fn telegram_rejects_a_schemeless_api_base() {
        let ch = channel(
            ChannelKind::Telegram,
            json!({ "botToken": "t", "chatId": "1", "apiBase": "tg.example.com" }),
        );
        let err = prepare(&ch, &important()).unwrap_err();
        assert!(err.to_string().contains("apiBase 必须以 http://"), "got {err}");
    }

    // -- QQ bot (OneBot v11) -----------------------------------------------

    #[test]
    fn qqbot_routes_private_and_group_targets() {
        let private = prepare(
            &channel(
                ChannelKind::Qqbot,
                json!({ "apiBase": "http://127.0.0.1:5700", "targetKind": "private", "targetId": 10001 }),
            ),
            &important(),
        )
        .unwrap();
        assert_eq!(private.url, "http://127.0.0.1:5700/send_private_msg");
        assert_eq!(body_of(&private)["user_id"], 10001);
        assert!(private.check_onebot);
        assert!(private.bearer.is_none());

        let group = prepare(
            &channel(
                ChannelKind::Qqbot,
                json!({ "apiBase": "http://127.0.0.1:5700/", "targetKind": "Group",
                        "targetId": "20002", "accessToken": "s3cret" }),
            ),
            &important(),
        )
        .unwrap();
        assert_eq!(group.url, "http://127.0.0.1:5700/send_group_msg");
        assert_eq!(group.bearer.as_deref(), Some("s3cret"));
        let body = body_of(&group);
        assert!(body.get("user_id").is_none());
        // A string id from the settings form must still go out as a number.
        assert_eq!(body["group_id"], 20002);
        assert!(body["group_id"].is_number(), "group_id sent as {}", body["group_id"]);
        assert!(body["message"].as_str().unwrap().contains("📬 重要邮件"));
    }

    #[test]
    fn qqbot_names_the_missing_or_invalid_field() {
        let base = json!({ "apiBase": "http://127.0.0.1:5700", "targetKind": "private", "targetId": 1 });
        let cases = [
            (json!({ "targetKind": "private", "targetId": 1 }), "QQ 机器人 配置缺少 apiBase"),
            (
                json!({ "apiBase": "http://127.0.0.1:5700", "targetId": 1 }),
                "QQ 机器人 配置缺少 targetKind",
            ),
            (
                json!({ "apiBase": "http://127.0.0.1:5700", "targetKind": "private" }),
                "QQ 机器人 配置缺少 targetId",
            ),
            (
                json!({ "apiBase": "http://127.0.0.1:5700", "targetKind": "friend", "targetId": 1 }),
                "targetKind 无效: friend",
            ),
            (
                json!({ "apiBase": "http://127.0.0.1:5700", "targetKind": "private", "targetId": "abc" }),
                "targetId 不是合法的数字",
            ),
        ];
        assert!(prepare(&channel(ChannelKind::Qqbot, base), &important()).is_ok());
        for (cfg, want) in cases {
            let err = prepare(&channel(ChannelKind::Qqbot, cfg), &important()).unwrap_err();
            assert!(err.to_string().contains(want), "got {err}");
        }
    }

    /// HTTP 200 with `status: failed` is the whole point of the body check.
    #[test]
    fn onebot_failure_body_surfaces_the_retcode() {
        let err = check_onebot("QQ 机器人", r#"{"status":"failed","retcode":100,"message":"账号未登录"}"#)
            .unwrap_err();
        let text = err.to_string();
        assert!(text.contains("retcode 100"), "got {text}");
        assert!(text.contains("账号未登录"), "got {text}");

        let err = check_onebot("QQ 机器人", r#"{"status":"ok","retcode":1200}"#).unwrap_err();
        assert!(err.to_string().contains("retcode 1200"), "got {err}");
    }

    #[test]
    fn onebot_success_bodies_pass() {
        for body in [
            r#"{"status":"ok","retcode":0,"data":{"message_id":1}}"#,
            r#"{"status":"async","retcode":1}"#,
            "OK", // not every fork answers JSON
        ] {
            assert!(check_onebot("QQ 机器人", body).is_ok(), "rejected {body}");
        }
    }

    // -- Bark --------------------------------------------------------------

    #[test]
    fn bark_splits_the_message_into_title_and_body() {
        let ch = channel(ChannelKind::Bark, json!({ "deviceKey": "abc123" }));
        let req = prepare(&ch, &verification()).unwrap();
        assert_eq!(req.url, "https://api.day.app/abc123");
        assert_eq!(req.display_url, "https://api.day.app/***");
        let body = body_of(&req);
        assert_eq!(body["title"], "🔑 验证码 482913");
        assert!(body["body"].as_str().unwrap().starts_with("来自: GitHub"));
        assert_eq!(body["group"], BARK_GROUP);
    }

    #[test]
    fn bark_honours_a_self_hosted_server() {
        let ch = channel(
            ChannelKind::Bark,
            json!({ "deviceKey": "abc123", "server": "https://bark.example.com/" }),
        );
        assert_eq!(prepare(&ch, &important()).unwrap().url, "https://bark.example.com/abc123");

        let err = prepare(&channel(ChannelKind::Bark, json!({ "server": "https://x" })), &important())
            .unwrap_err();
        assert!(err.to_string().contains("Bark 配置缺少 deviceKey"), "got {err}");
    }

    // -- Webhook -----------------------------------------------------------

    #[test]
    fn webhook_without_a_template_posts_the_payload() {
        let ch = channel(
            ChannelKind::Webhook,
            json!({ "url": "https://hooks.example.com/x", "headers": { "X-Token": "t" } }),
        );
        let req = prepare(&ch, &important()).unwrap();
        assert_eq!(req.content_type, "application/json");
        assert_eq!(req.headers, vec![("X-Token".to_string(), "t".to_string())]);
        let body = body_of(&req);
        // camelCase, exactly as the frontend model spells it.
        assert_eq!(body["accountEmail"], "me@example.com");
        assert_eq!(body["category"], "important");
        assert_eq!(body["verificationCode"], Value::Null);
    }

    #[test]
    fn webhook_template_substitutes_every_placeholder() {
        let ch = channel(
            ChannelKind::Webhook,
            json!({
                "url": "https://hooks.example.com/x",
                "bodyTemplate": r#"{"c":"{{category}}","s":"{{subject}}","f":"{{from}}","m":"{{summary}}","k":"{{code}}","a":"{{account}}","d":"{{date}}","x":"{{unknown}}"}"#,
            }),
        );
        let req = prepare(&ch, &verification()).unwrap();
        assert_eq!(req.content_type, "application/json");
        let body = body_of(&req);
        assert_eq!(body["c"], "verification");
        assert_eq!(body["s"], "Sign-in verification");
        assert_eq!(body["f"], "GitHub <noreply@github.com>");
        assert_eq!(body["m"], "GitHub 登录验证码");
        assert_eq!(body["k"], "482913");
        assert_eq!(body["a"], "me@example.com");
        assert!(!body["d"].as_str().unwrap().is_empty());
        // An unknown placeholder stays literal instead of vanishing silently.
        assert_eq!(body["x"], "{{unknown}}");
    }

    /// A quote in a subject must never break — or restructure — the user's JSON.
    #[test]
    fn webhook_template_escapes_values_inside_json() {
        let mut p = important();
        p.subject = r#"He said "hi","injected":true"#.into();
        p.summary = "line1\nline2".into();
        let ch = channel(
            ChannelKind::Webhook,
            json!({
                "url": "https://hooks.example.com/x",
                "bodyTemplate": r#"{"text":"{{subject}} / {{summary}}"}"#,
            }),
        );
        let req = prepare(&ch, &p).unwrap();
        assert_eq!(req.content_type, "application/json");
        let body = body_of(&req);
        assert_eq!(body.as_object().unwrap().len(), 1, "injected a key: {}", req.body);
        assert_eq!(body["text"], "He said \"hi\",\"injected\":true / line1\nline2");
    }

    /// A template that cannot produce JSON goes out as text, never as a broken
    /// JSON document.
    #[test]
    fn webhook_template_falls_back_to_text_plain() {
        for template in [
            "验证码 {{code}} 来自 {{from}}",     // plain text
            r#"{"code": {{code}}, "s": {{subject}}}"#, // unquoted placeholder
        ] {
            let ch = channel(
                ChannelKind::Webhook,
                json!({ "url": "https://hooks.example.com/x", "bodyTemplate": template }),
            );
            let req = prepare(&ch, &verification()).unwrap();
            assert_eq!(req.content_type, "text/plain; charset=utf-8", "for {template}");
            // Text keeps the raw values: no stray backslashes from escaping.
            assert!(req.body.contains("482913"), "got {}", req.body);
            assert!(!req.body.contains("\\\""), "escaped text body: {}", req.body);
        }
    }

    #[test]
    fn webhook_rejects_a_missing_url_and_a_bad_header_map() {
        let err = prepare(&channel(ChannelKind::Webhook, json!({})), &important()).unwrap_err();
        assert!(err.to_string().contains("Webhook 配置缺少 url"), "got {err}");

        let err = prepare(
            &channel(
                ChannelKind::Webhook,
                json!({ "url": "https://hooks.example.com/x", "headers": { "X": ["a"] } }),
            ),
            &important(),
        )
        .unwrap_err();
        assert!(err.to_string().contains("headers.X 必须是字符串"), "got {err}");

        let err = prepare(
            &channel(
                ChannelKind::Webhook,
                json!({ "url": "https://hooks.example.com/x", "headers": "X: 1" }),
            ),
            &important(),
        )
        .unwrap_err();
        assert!(err.to_string().contains("headers 必须是对象"), "got {err}");
    }

    // -- Test payload ------------------------------------------------------

    /// Whatever the channel, the probe must be unmistakably a test.
    #[test]
    fn the_test_payload_labels_itself() {
        let text = format_message(&test_payload());
        assert!(text.contains("这是一条来自 Mailer 的测试通知"), "got {text}");
    }
}
