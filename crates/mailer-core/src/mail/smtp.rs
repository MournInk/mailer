//! Outgoing mail via lettre (rustls).
//!
//! CONTRACT:
//! - `send`  — build an RFC 5322 message (plain text) and submit through the
//!   account's SMTP config. Errors if the account has no SMTP configured.
//! - `check` — connectivity + credential test for an SMTP config.

use std::time::Duration;

use lettre::message::header::ContentType;
use lettre::message::Mailbox;
use lettre::transport::smtp::authentication::Credentials;
use lettre::transport::smtp::AsyncSmtpTransport;
use lettre::{Address, AsyncTransport, Message, Tokio1Executor};

use crate::error::{Error, Result};
use crate::types::{AccountConfig, SmtpConfig, TestResult, TlsMode};

/// Per-command timeout for the SMTP conversation. Long enough for a slow
/// relay to accept a message, short enough that the UI isn't stuck.
const TIMEOUT: Duration = Duration::from_secs(30);

type Transport = AsyncSmtpTransport<Tokio1Executor>;

/// Build the transport described by `smtp` (no I/O happens yet).
fn transport(smtp: &SmtpConfig) -> Result<Transport> {
    let host = smtp.host.trim();
    if host.is_empty() {
        return Err(Error::InvalidConfig("SMTP 服务器地址不能为空".into()));
    }

    let builder = match smtp.tls {
        // Implicit TLS from the first byte (SMTPS, usually 465).
        TlsMode::Tls => Transport::relay(host)
            .map_err(|e| Error::Smtp(format!("无法配置 TLS 连接 ({host}): {e}")))?,
        // Plaintext connection upgraded before login (usually 587).
        TlsMode::Starttls => Transport::starttls_relay(host)
            .map_err(|e| Error::Smtp(format!("无法配置 STARTTLS 连接 ({host}): {e}")))?,
        // Unencrypted. Only sensible for localhost bridges (Proton Bridge...).
        TlsMode::None => Transport::builder_dangerous(host),
    };

    let mut builder = builder.port(smtp.port).timeout(Some(TIMEOUT));
    // An empty username means the relay takes anonymous submissions.
    if !smtp.username.is_empty() {
        builder = builder.credentials(Credentials::new(
            smtp.username.clone(),
            smtp.password.clone(),
        ));
    }
    Ok(builder.build())
}

/// Send a plain-text message from `account`.
pub async fn send(
    account: &AccountConfig,
    to: &[String],
    subject: &str,
    body: &str,
    in_reply_to: Option<&str>,
) -> Result<()> {
    let smtp = account
        .smtp
        .as_ref()
        .ok_or_else(|| Error::InvalidConfig("该账户未配置 SMTP 发件服务器".into()))?;
    if to.is_empty() {
        return Err(Error::InvalidConfig("收件人不能为空".into()));
    }

    let mut builder = Message::builder()
        .from(mailbox(Some(&account.label), &account.email)?)
        .subject(subject);
    for recipient in to {
        builder = builder.to(mailbox(None, recipient)?);
    }
    // Threading: both headers carry the parent id so replying clients and
    // servers can stitch the conversation together.
    if let Some(parent) = in_reply_to {
        let parent = angle_wrap(parent);
        builder = builder.in_reply_to(parent.clone()).references(parent);
    }

    let email = builder
        .header(ContentType::TEXT_PLAIN)
        .body(body.to_string())
        .map_err(|e| Error::Smtp(format!("邮件内容无效: {e}")))?;

    transport(smtp)?
        .send(email)
        .await
        .map_err(|e| Error::Smtp(format!("发送失败 ({}:{}) : {e}", smtp.host, smtp.port)))?;
    Ok(())
}

/// Test SMTP connectivity/credentials.
pub async fn check(smtp: &SmtpConfig, from_email: &str) -> Result<TestResult> {
    // A malformed sender address would only surface at send time; name it now.
    if let Err(e) = from_email.trim().parse::<Address>() {
        return Ok(TestResult {
            ok: false,
            message: format!("发件地址无效 {from_email}: {e}"),
        });
    }

    let transport = match transport(smtp) {
        Ok(t) => t,
        // A misconfiguration is a failed test, not a crashed one.
        Err(e) => {
            return Ok(TestResult {
                ok: false,
                message: e.to_string(),
            })
        }
    };

    // `test_connection` connects, authenticates and sends NOOP.
    let result = match transport.test_connection().await {
        Ok(true) => TestResult {
            ok: true,
            message: format!("SMTP {}:{} 连接与登录成功", smtp.host, smtp.port),
        },
        Ok(false) => TestResult {
            ok: false,
            message: format!("SMTP {}:{} 已连接但未响应 NOOP", smtp.host, smtp.port),
        },
        Err(e) => TestResult {
            ok: false,
            message: format!("SMTP {}:{} 连接或认证失败: {e}", smtp.host, smtp.port),
        },
    };
    Ok(result)
}

/// Build a lettre mailbox, turning parse failures into user-facing config
/// errors instead of leaking lettre's English message on its own.
fn mailbox(name: Option<&str>, email: &str) -> Result<Mailbox> {
    let email = email.trim();
    let address: Address = email
        .parse()
        .map_err(|e| Error::InvalidConfig(format!("邮件地址无效 {email}: {e}")))?;
    let name = name
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .map(str::to_string);
    Ok(Mailbox::new(name, address))
}

/// RFC 5322 message ids travel inside angle brackets, but `mail-parser`
/// strips them when reading, so put them back before writing the header.
fn angle_wrap(id: &str) -> String {
    let id = id.trim();
    if id.starts_with('<') && id.ends_with('>') {
        id.to_string()
    } else {
        format!("<{id}>")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Protocol;

    fn smtp_config(tls: TlsMode) -> SmtpConfig {
        SmtpConfig {
            host: "smtp.example.com".into(),
            port: 465,
            username: "me@example.com".into(),
            password: "secret".into(),
            tls,
        }
    }

    fn account(smtp: Option<SmtpConfig>) -> AccountConfig {
        AccountConfig {
            id: "acc1".into(),
            label: "私人邮箱".into(),
            email: "me@example.com".into(),
            protocol: Protocol::Imap,
            host: "imap.example.com".into(),
            port: 993,
            username: "me@example.com".into(),
            password: "secret".into(),
            tls: TlsMode::Tls,
            smtp,
            sync_interval_secs: 0,
            color_hue: 20,
            created_at: 1,
        }
    }

    /// Every TLS mode must produce a transport without touching the network.
    /// Async because the pooled transport needs a runtime in context to drop.
    #[tokio::test]
    async fn transport_builds_for_every_tls_mode() {
        for tls in [TlsMode::Tls, TlsMode::Starttls, TlsMode::None] {
            assert!(transport(&smtp_config(tls)).is_ok(), "{tls:?}");
        }
    }

    #[test]
    fn transport_rejects_empty_host() {
        let mut cfg = smtp_config(TlsMode::Tls);
        cfg.host = "  ".into();
        assert!(matches!(transport(&cfg), Err(Error::InvalidConfig(_))));
    }

    /// A receive-only account must fail with the documented message.
    #[tokio::test]
    async fn send_without_smtp_is_a_config_error() {
        let err = send(&account(None), &["a@example.com".into()], "s", "b", None)
            .await
            .unwrap_err();
        match err {
            Error::InvalidConfig(m) => assert_eq!(m, "该账户未配置 SMTP 发件服务器"),
            other => panic!("got {other:?}"),
        }
    }

    /// No recipient is caught before any connection attempt.
    #[tokio::test]
    async fn send_without_recipients_is_a_config_error() {
        let err = send(&account(Some(smtp_config(TlsMode::Tls))), &[], "s", "b", None)
            .await
            .unwrap_err();
        assert!(matches!(err, Error::InvalidConfig(_)), "got {err:?}");
    }

    /// A bad recipient must be rejected locally, not by the relay.
    #[tokio::test]
    async fn send_rejects_malformed_recipient() {
        let err = send(
            &account(Some(smtp_config(TlsMode::Tls))),
            &["not-an-address".into()],
            "s",
            "b",
            None,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, Error::InvalidConfig(_)), "got {err:?}");
    }

    /// `check` reports failures through `TestResult`, never through `Err`.
    #[tokio::test]
    async fn check_reports_bad_sender_without_connecting() {
        let result = check(&smtp_config(TlsMode::Tls), "nonsense").await.unwrap();
        assert!(!result.ok);
        assert!(result.message.contains("发件地址无效"), "{}", result.message);
    }

    #[tokio::test]
    async fn check_reports_bad_config_without_connecting() {
        let mut cfg = smtp_config(TlsMode::Starttls);
        cfg.host = String::new();
        let result = check(&cfg, "me@example.com").await.unwrap();
        assert!(!result.ok);
        assert!(result.message.contains("SMTP 服务器地址"), "{}", result.message);
    }

    #[test]
    fn mailbox_keeps_display_name_and_rejects_junk() {
        let mbox = mailbox(Some("  私人邮箱 "), " me@example.com ").unwrap();
        assert_eq!(mbox.name.as_deref(), Some("私人邮箱"));
        assert_eq!(mbox.email.to_string(), "me@example.com");
        // Blank labels must not produce an empty `"" <addr>` display name.
        assert!(mailbox(Some("   "), "me@example.com").unwrap().name.is_none());
        assert!(mailbox(None, "me@").is_err());
    }

    #[test]
    fn angle_wrap_normalises_message_ids() {
        assert_eq!(angle_wrap("abc@example.com"), "<abc@example.com>");
        assert_eq!(angle_wrap("<abc@example.com>"), "<abc@example.com>");
        assert_eq!(angle_wrap("  abc@example.com  "), "<abc@example.com>");
    }
}
