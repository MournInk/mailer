//! mailer-core — engine behind the Mailer app.
//!
//! Layout:
//! - [`types`]   shared data model (mirrored in TypeScript)
//! - [`store`]   SQLite persistence
//! - [`net`]     TCP/TLS connection helper shared by protocol clients
//! - [`mail`]    IMAP / POP3 / SMTP clients + MIME parsing
//! - [`ai`]      LLM triage (OpenAI-compatible chat completions)
//! - [`notify`]  external notification channels (Telegram / QQ bot / webhook / Bark)
//! - [`sync`]    orchestration: fetch → parse → store → classify → act

pub mod ai;
pub mod error;
pub mod mail;
pub mod net;
pub mod notify;
pub mod store;
pub mod sync;
pub mod types;

pub use error::{Error, Result};
