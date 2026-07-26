//! Minimal POP3 client (RFC 1939 + UIDL), hand-rolled over `MaybeTlsStream`.
//!
//! CONTRACT (implemented against by `sync.rs`):
//! - `check`     — connect + USER/PASS, then QUIT.
//! - `fetch_new` — UIDL, diff against `known`, RETR the missing ones
//!   (most recent first, at most `max_fetch`), folder is always "INBOX".
//! - `delete`    — DELE by UIDL token, commit with QUIT.

use std::collections::HashSet;

use crate::error::Result;
use crate::mail::RawMail;
use crate::types::AccountConfig;

/// Connectivity + credential test.
pub async fn check(account: &AccountConfig) -> Result<()> {
    let _ = account;
    todo!("implemented in the protocol milestone")
}

/// Fetch messages whose UIDL tokens are not in `known`.
pub async fn fetch_new(
    account: &AccountConfig,
    known: &HashSet<String>,
    max_fetch: u32,
) -> Result<Vec<RawMail>> {
    let _ = (account, known, max_fetch);
    todo!("implemented in the protocol milestone")
}

/// Permanently delete messages by UIDL token.
pub async fn delete(account: &AccountConfig, uids: &[String]) -> Result<()> {
    let _ = (account, uids);
    todo!("implemented in the protocol milestone")
}
