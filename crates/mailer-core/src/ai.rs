//! LLM triage over an OpenAI-compatible chat completions API.
//!
//! CONTRACT:
//! - `classify` — one message in, one [`AiAnalysis`] out. The model is asked
//!   for strict JSON; the implementation must survive markdown fences and
//!   sloppy output (extract the first JSON object). On unusable output,
//!   return `Err(Error::Ai(..))` — the caller keeps the message unclassified
//!   and retries on the next cycle.
//! - `test` — cheap round-trip ("ping" prompt) to validate base URL, key and
//!   model name; never panics, returns a human-readable `TestResult`.
//!
//! The prompt must instruct the model to output exactly:
//! `{"category":"verification|spam|normal|important","confidence":0.0-1.0,
//!   "summary":"...","verificationCode":"..."|null,"deletable":bool,"reason":"..."}`
//! with `summary` in the same language as the user's mail (default 中文).

use crate::error::Result;
use crate::types::{AiAnalysis, AiSettings, EmailMessage, TestResult};

/// Classify one message with the configured LLM.
pub async fn classify(
    http: &reqwest::Client,
    settings: &AiSettings,
    msg: &EmailMessage,
) -> Result<AiAnalysis> {
    let _ = (http, settings, msg);
    todo!("implemented in the AI milestone")
}

/// Validate the configured endpoint with a minimal round-trip.
pub async fn test(http: &reqwest::Client, settings: &AiSettings) -> TestResult {
    let _ = (http, settings);
    todo!("implemented in the AI milestone")
}
