//! Outgoing mail via lettre (rustls).
//!
//! CONTRACT:
//! - `send`  — build an RFC 5322 message (plain text) and submit through the
//!   account's SMTP config. Errors if the account has no SMTP configured.
//! - `check` — connectivity + credential test for an SMTP config.

use crate::error::Result;
use crate::types::{AccountConfig, SmtpConfig, TestResult};

/// Send a plain-text message from `account`.
pub async fn send(
    account: &AccountConfig,
    to: &[String],
    subject: &str,
    body: &str,
    in_reply_to: Option<&str>,
) -> Result<()> {
    let _ = (account, to, subject, body, in_reply_to);
    todo!("implemented in the protocol milestone")
}

/// Test SMTP connectivity/credentials.
pub async fn check(smtp: &SmtpConfig, from_email: &str) -> Result<TestResult> {
    let _ = (smtp, from_email);
    todo!("implemented in the protocol milestone")
}
