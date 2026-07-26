//! Mail protocol clients and MIME parsing.
//!
//! - [`imap`]  — IMAP4rev1 client (async-imap over rustls)
//! - [`pop3`]  — minimal POP3 client (hand-rolled, RFC 1939)
//! - [`parse`] — raw RFC 5322 bytes → [`crate::types::EmailMessage`]
//! - [`smtp`]  — outgoing mail via lettre
//!
//! Both fetch clients implement the same contract:
//!
//! ```text
//! fetch_new(account, known_uids) -> Vec<RawMail>   // only mail we don't have
//! delete(account, uids)          -> ()             // permanent server-side delete
//! check(account)                 -> ()             // connectivity + login test
//! ```

pub mod imap;
pub mod parse;
pub mod pop3;
pub mod smtp;

/// A fetched-but-not-yet-parsed message.
#[derive(Debug, Clone)]
pub struct RawMail {
    /// IMAP UID (stringified) or POP3 UIDL token.
    pub uid: String,
    /// Folder it came from ("INBOX" for POP3).
    pub folder: String,
    /// Full RFC 5322 payload.
    pub bytes: Vec<u8>,
}
