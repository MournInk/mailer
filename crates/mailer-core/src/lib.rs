//! mailer-core — engine behind the Mailer app.
//!
//! Layout:
//! - [`types`]   shared data model (mirrored in TypeScript)
//! - [`store`]   SQLite persistence
//! - [`net`]     TCP/TLS connection helper shared by protocol clients
//! - [`mail`]    IMAP / POP3 / SMTP clients + MIME parsing
//! - [`ai`]      LLM triage (OpenAI-compatible chat completions)
//! - [`mcp`]     MCP *client*: tools borrowed from external servers
//! - [`memory`]  what the assistant knows about the user, and how it is revised
//! - [`notify`]  external notification channels (Telegram / QQ bot / webhook / Bark)
//! - [`sync`]    orchestration: fetch → parse → store → classify → act

pub mod ai;
pub mod assistant;
pub mod error;
pub mod mail;
pub mod mcp;
pub mod memory;
pub mod net;
pub mod notify;
pub mod rag;
pub mod store;
pub mod sync;
pub mod tools;
pub mod types;

pub use error::{Error, Result};
