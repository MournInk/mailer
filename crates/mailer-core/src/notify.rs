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

use crate::error::Result;
use crate::types::{NotifyChannel, NotifyPayload, TestResult};

/// Deliver a payload through one channel.
pub async fn dispatch(
    http: &reqwest::Client,
    channel: &NotifyChannel,
    payload: &NotifyPayload,
) -> Result<()> {
    let _ = (http, channel, payload);
    todo!("implemented in the notify milestone")
}

/// Send a test message through the channel.
pub async fn test(http: &reqwest::Client, channel: &NotifyChannel) -> TestResult {
    let _ = (http, channel);
    todo!("implemented in the notify milestone")
}
