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
use std::fmt;
use std::pin::Pin;
use std::task::{Context, Poll};

use async_imap::error::Error as ImapError;
use async_imap::imap_proto::{Response, Status};
use async_imap::{Client, Session};
use futures::TryStreamExt;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

use crate::error::{Error, Result};
use crate::mail::RawMail;
use crate::net::{self, MaybeTlsStream};
use crate::types::{AccountConfig, TlsMode};

/// Bodies pulled per `UID FETCH` round trip. Batching keeps the command line
/// short and bounds how much of the mailbox is buffered at once.
const FETCH_BATCH: usize = 20;

/// The only folder we poll. IMAP servers all expose it under this exact name.
const INBOX: &str = "INBOX";

// ---------------------------------------------------------------------------
// Transport
// ---------------------------------------------------------------------------

/// `async-imap` bounds its transport with `Debug`, which [`MaybeTlsStream`]
/// does not implement; this newtype supplies the missing bound and forwards
/// everything else.
struct Transport(MaybeTlsStream);

impl fmt::Debug for Transport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            MaybeTlsStream::Plain(_) => f.write_str("Transport(plain)"),
            MaybeTlsStream::Tls(_) => f.write_str("Transport(tls)"),
        }
    }
}

impl AsyncRead for Transport {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().0).poll_read(cx, buf)
    }
}

impl AsyncWrite for Transport {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.get_mut().0).poll_write(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().0).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().0).poll_shutdown(cx)
    }
}

// ---------------------------------------------------------------------------
// Connect / login
// ---------------------------------------------------------------------------

/// Read the untagged greeting the server sends right after the connection is
/// established. A `BYE` greeting means the server refused us outright (rate
/// limit, blocked IP, ...) and would otherwise surface as a confusing login
/// failure.
async fn read_greeting(client: &mut Client<Transport>) -> Result<()> {
    let greeting = client
        .read_response()
        .await
        .map_err(|e| Error::Imap(format!("读取服务器问候失败: {e}")))?
        .ok_or_else(|| Error::Imap("服务器未返回问候信息，连接已被关闭".to_string()))?;

    if let Response::Data {
        status: Status::Bye,
        information,
        ..
    } = greeting.parsed()
    {
        let info = information.as_deref().unwrap_or("无附加信息");
        return Err(Error::Imap(format!("服务器拒绝了连接: {info}")));
    }
    Ok(())
}

/// Open a connection according to `account.tls` and read the greeting.
async fn connect(account: &AccountConfig) -> Result<Client<Transport>> {
    match account.tls {
        TlsMode::Tls => {
            let tls = net::connect_tls(&account.host, account.port).await?;
            let mut client = Client::new(Transport(MaybeTlsStream::Tls(Box::new(tls))));
            read_greeting(&mut client).await?;
            Ok(client)
        }
        TlsMode::None => {
            let tcp = net::tcp_connect(&account.host, account.port).await?;
            let mut client = Client::new(Transport(MaybeTlsStream::Plain(tcp)));
            read_greeting(&mut client).await?;
            Ok(client)
        }
        TlsMode::Starttls => {
            let tcp = net::tcp_connect(&account.host, account.port).await?;
            let mut client = Client::new(Transport(MaybeTlsStream::Plain(tcp)));
            read_greeting(&mut client).await?;

            // Upgrade in place: issue STARTTLS on the plain client, take the
            // raw socket back and re-wrap it. There is no second greeting
            // after the handshake, so the new client goes straight to LOGIN.
            client
                .run_command_and_check_ok("STARTTLS", None)
                .await
                .map_err(|e| Error::Imap(format!("STARTTLS 升级失败: {e}")))?;

            let tcp = match client.into_inner().0 {
                MaybeTlsStream::Plain(tcp) => tcp,
                MaybeTlsStream::Tls(_) => {
                    return Err(Error::Imap("STARTTLS: 连接已经处于 TLS 状态".to_string()))
                }
            };
            let tls = net::tls_wrap(&account.host, tcp).await?;
            Ok(Client::new(Transport(MaybeTlsStream::Tls(Box::new(tls)))))
        }
    }
}

/// A rejected `LOGIN` is a credential problem; anything else is transport noise.
fn login_error(e: ImapError) -> Error {
    match e {
        ImapError::No(info) => Error::Auth(format!("登录被拒绝，请检查用户名或授权码: {info}")),
        ImapError::Bad(info) => Error::Auth(format!("服务器拒绝了登录命令: {info}")),
        other => Error::Imap(format!("登录失败: {other}")),
    }
}

/// Connect and authenticate, yielding a ready-to-use session.
async fn login(account: &AccountConfig) -> Result<Session<Transport>> {
    let client = connect(account).await?;
    client
        .login(&account.username, &account.password)
        .await
        .map_err(|(e, _client)| login_error(e))
}

/// SELECT a mailbox, reporting which one failed.
async fn select(
    session: &mut Session<Transport>,
    folder: &str,
) -> Result<async_imap::types::Mailbox> {
    session
        .select(folder)
        .await
        .map_err(|e| Error::Imap(format!("选择邮箱 {folder} 失败: {e}")))
}

/// LOGOUT. Best-effort in spirit, but a failure here still means the session
/// ended badly, so callers propagate it.
async fn logout(session: &mut Session<Transport>) -> Result<()> {
    session
        .logout()
        .await
        .map_err(|e| Error::Imap(format!("退出登录失败: {e}")))
}

// ---------------------------------------------------------------------------
// UID helpers (pure — unit tested below)
// ---------------------------------------------------------------------------

/// Pick the newest `max` UIDs from `all` that `known` doesn't already hold.
/// The result is sorted ascending so the caller stores oldest-first.
fn select_new_uids(all: &[u32], known: &HashSet<String>, max: u32) -> Vec<u32> {
    let mut missing: Vec<u32> = all
        .iter()
        .copied()
        .filter(|uid| !known.contains(&uid.to_string()))
        .collect();
    missing.sort_unstable();
    missing.dedup();
    // Keep the tail: highest UIDs are the most recently delivered messages.
    if missing.len() > max as usize {
        missing.drain(..missing.len() - max as usize);
    }
    missing
}

/// Render an ascending, de-duplicated UID list as an IMAP sequence set,
/// collapsing consecutive runs into `a:b` so big batches stay on one line.
fn uid_set(uids: &[u32]) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut i = 0;
    while i < uids.len() {
        let start = uids[i];
        let mut end = start;
        while i + 1 < uids.len() && end.checked_add(1) == Some(uids[i + 1]) {
            i += 1;
            end = uids[i];
        }
        if start == end {
            parts.push(start.to_string());
        } else {
            parts.push(format!("{start}:{end}"));
        }
        i += 1;
    }
    parts.join(",")
}

/// Parse stored UID strings back into IMAP UIDs, dropping anything that isn't
/// numeric (POP3 UIDL tokens can never reach here, but be defensive).
fn parse_uids(uids: &[String]) -> Vec<u32> {
    let mut out: Vec<u32> = uids
        .iter()
        .filter_map(|u| u.trim().parse::<u32>().ok())
        .collect();
    out.sort_unstable();
    out.dedup();
    out
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Connectivity + credential test.
pub async fn check(account: &AccountConfig) -> Result<()> {
    let mut session = login(account).await?;
    select(&mut session, INBOX).await?;
    logout(&mut session).await
}

/// What one `fetch_new` round trip learned about the mailbox.
pub struct Fetched {
    /// The mailbox's current UIDVALIDITY. The caller persists it and passes it
    /// back as `expected_validity` next time.
    pub uid_validity: u32,
    /// True when the server's UIDVALIDITY no longer matches `expected_validity`,
    /// meaning every stored UID for this folder is stale and must be discarded.
    pub uids_reset: bool,
    pub mails: Vec<RawMail>,
}

/// Fetch messages from INBOX whose UIDs are not in `known`.
///
/// `expected_validity` is the UIDVALIDITY observed on the previous sync, if
/// any. When the server reports a different one it has reissued UIDs from
/// scratch, so `known` is meaningless and gets ignored for this run.
pub async fn fetch_new(
    account: &AccountConfig,
    known: &HashSet<String>,
    max_fetch: u32,
    expected_validity: Option<u32>,
) -> Result<Fetched> {
    let mut session = login(account).await?;
    let mailbox = select(&mut session, INBOX).await?;

    let uid_validity = mailbox.uid_validity.unwrap_or(0);
    let uids_reset = matches!(expected_validity, Some(prev) if prev != uid_validity);
    if uids_reset {
        tracing::warn!(
            "imap: UIDVALIDITY changed on {} ({:?} -> {}); stored UIDs are stale",
            account.email,
            expected_validity,
            uid_validity
        );
    }
    // After a reset every stored UID may alias a different message, so trusting
    // `known` would silently skip real mail.
    let empty;
    let known = if uids_reset {
        empty = HashSet::new();
        &empty
    } else {
        known
    };

    // Empty mailbox: nothing to diff, and some servers dislike SEARCH on it.
    if mailbox.exists == 0 || max_fetch == 0 {
        logout(&mut session).await?;
        return Ok(Fetched { uid_validity, uids_reset, mails: Vec::new() });
    }

    // One round trip for the full UID list, then diff locally.
    let existing = session
        .uid_search("ALL")
        .await
        .map_err(|e| Error::Imap(format!("搜索收件箱 UID 失败: {e}")))?;
    let all: Vec<u32> = existing.into_iter().collect();
    let wanted = select_new_uids(&all, known, max_fetch);
    if wanted.is_empty() {
        logout(&mut session).await?;
        return Ok(Fetched { uid_validity, uids_reset, mails: Vec::new() });
    }

    // BODY.PEEK[] is RFC822 without the \Seen side effect: read state is ours
    // to manage locally, not the server's to guess.
    let mut fetched: Vec<(u32, Vec<u8>)> = Vec::with_capacity(wanted.len());
    for batch in wanted.chunks(FETCH_BATCH) {
        let set = uid_set(batch);
        let mut stream = session
            .uid_fetch(&set, "(UID BODY.PEEK[])")
            .await
            .map_err(|e| Error::Imap(format!("拉取邮件正文失败 (UID {set}): {e}")))?;
        while let Some(fetch) = stream
            .try_next()
            .await
            .map_err(|e| Error::Imap(format!("读取邮件正文失败 (UID {set}): {e}")))?
        {
            match (fetch.uid, fetch.body()) {
                (Some(uid), Some(body)) => fetched.push((uid, body.to_vec())),
                (uid, _) => {
                    tracing::warn!("imap: fetch response without body (uid {uid:?}), skipped");
                }
            }
        }
    }

    logout(&mut session).await?;

    // Newest-last, regardless of the order the server chose to answer in.
    fetched.sort_by_key(|(uid, _)| *uid);
    Ok(Fetched {
        uid_validity,
        uids_reset,
        mails: fetched
            .into_iter()
            .map(|(uid, bytes)| RawMail {
                uid: uid.to_string(),
                folder: INBOX.to_string(),
                bytes,
            })
            .collect(),
    })
}

/// Permanently delete the given UIDs from `folder` on the server.
pub async fn delete(account: &AccountConfig, folder: &str, uids: &[String]) -> Result<()> {
    let targets = parse_uids(uids);
    if targets.is_empty() {
        return Ok(()); // nothing addressable on the server
    }

    let mut session = login(account).await?;
    select(&mut session, folder).await?;

    let set = uid_set(&targets);
    // `.SILENT` suppresses the untagged FETCH echo, but the tagged completion
    // still has to be drained before the next command.
    {
        let mut stream = Box::pin(
            session
                .uid_store(&set, "+FLAGS.SILENT (\\Deleted)")
                .await
                .map_err(|e| Error::Imap(format!("标记删除失败 (UID {set}): {e}")))?,
        );
        while stream
            .try_next()
            .await
            .map_err(|e| Error::Imap(format!("标记删除失败 (UID {set}): {e}")))?
            .is_some()
        {}
    }

    {
        let mut stream = Box::pin(
            session
                .expunge()
                .await
                .map_err(|e| Error::Imap(format!("清除邮件失败 (邮箱 {folder}): {e}")))?,
        );
        while stream
            .try_next()
            .await
            .map_err(|e| Error::Imap(format!("清除邮件失败 (邮箱 {folder}): {e}")))?
            .is_some()
        {}
    }

    logout(&mut session).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn known(uids: &[&str]) -> HashSet<String> {
        uids.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn select_new_uids_keeps_newest_and_sorts_ascending() {
        let all = [5, 1, 9, 3, 7];
        let picked = select_new_uids(&all, &HashSet::new(), 3);
        assert_eq!(picked, vec![5, 7, 9]);
    }

    #[test]
    fn select_new_uids_skips_known() {
        let all = [1, 2, 3, 4, 5];
        let picked = select_new_uids(&all, &known(&["4", "5"]), 10);
        assert_eq!(picked, vec![1, 2, 3]);
    }

    #[test]
    fn select_new_uids_handles_empty_and_zero_max() {
        assert!(select_new_uids(&[], &HashSet::new(), 10).is_empty());
        assert!(select_new_uids(&[1, 2, 3], &HashSet::new(), 0).is_empty());
        assert!(select_new_uids(&[1, 2], &known(&["1", "2"]), 10).is_empty());
    }

    #[test]
    fn select_new_uids_dedups() {
        let picked = select_new_uids(&[2, 2, 1, 1], &HashSet::new(), 10);
        assert_eq!(picked, vec![1, 2]);
    }

    #[test]
    fn uid_set_collapses_runs() {
        assert_eq!(uid_set(&[1]), "1");
        assert_eq!(uid_set(&[1, 2, 3]), "1:3");
        assert_eq!(uid_set(&[1, 3, 4, 5, 9]), "1,3:5,9");
        assert_eq!(uid_set(&[2, 4, 6]), "2,4,6");
        assert_eq!(uid_set(&[]), "");
    }

    #[test]
    fn uid_set_handles_u32_max_without_overflow() {
        assert_eq!(uid_set(&[u32::MAX - 1, u32::MAX]), "4294967294:4294967295");
    }

    #[test]
    fn parse_uids_filters_and_normalizes() {
        let input = vec![
            "12".to_string(),
            " 3 ".to_string(),
            "abc".to_string(),
            "12".to_string(),
            String::new(),
        ];
        assert_eq!(parse_uids(&input), vec![3, 12]);
        assert!(parse_uids(&["not-a-uid".to_string()]).is_empty());
    }
}
