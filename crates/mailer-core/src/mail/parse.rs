//! RFC 5322 / MIME → [`EmailMessage`] using `mail-parser`.
//!
//! CONTRACT: `parse_mail` never fails on malformed input if it can salvage
//! anything — missing headers degrade to empty strings and the date falls
//! back to `now_ms`. `Err` is reserved for completely unusable payloads.

use crate::error::Result;
use crate::mail::RawMail;
use crate::types::EmailMessage;

/// Parse raw bytes into a stored message.
///
/// - `id` is assigned by the caller (uuid v4).
/// - `snippet` is plain text, whitespace-collapsed, ≤ 140 chars.
/// - `body_text`/`body_html` capture the best text and HTML bodies.
/// - `date` = Date header as unix millis, else `now_ms`.
pub fn parse_mail(id: String, account_id: &str, raw: &RawMail, now_ms: i64) -> Result<EmailMessage> {
    let _ = (id, account_id, raw, now_ms);
    todo!("implemented in the protocol milestone")
}
