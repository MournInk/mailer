//! IMAP4rev1 client built on `async-imap` + rustls.
//!
//! CONTRACT (implemented against by `sync.rs`):
//! - `check`     — connect + login + select INBOX, then logout.
//! - `fetch_new` — return messages in INBOX whose UID is not in `known`,
//!   newest-last, at most `max_fetch` (fetch the most recent ones).
//! - `delete`    — flag `\Deleted` + EXPUNGE the given UIDs.
//!
//! TLS modes: `TlsMode::Tls` (implicit, port 993), `TlsMode::Starttls`,
//! `TlsMode::None` (plain, for localhost bridges only).

use std::collections::HashSet;

use crate::error::Result;
use crate::mail::RawMail;
use crate::types::AccountConfig;

/// Connectivity + credential test.
pub async fn check(account: &AccountConfig) -> Result<()> {
    let _ = account;
    todo!("implemented in the protocol milestone")
}

/// Fetch messages from INBOX whose UIDs are not in `known`.
pub async fn fetch_new(
    account: &AccountConfig,
    known: &HashSet<String>,
    max_fetch: u32,
) -> Result<Vec<RawMail>> {
    let _ = (account, known, max_fetch);
    todo!("implemented in the protocol milestone")
}

/// Permanently delete the given UIDs from `folder` on the server.
pub async fn delete(account: &AccountConfig, folder: &str, uids: &[String]) -> Result<()> {
    let _ = (account, folder, uids);
    todo!("implemented in the protocol milestone")
}
