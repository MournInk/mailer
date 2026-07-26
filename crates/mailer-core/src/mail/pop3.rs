//! Minimal POP3 client (RFC 1939 + UIDL), hand-rolled over `MaybeTlsStream`.
//!
//! CONTRACT (implemented against by `sync.rs`):
//! - `check`     — connect + USER/PASS, then QUIT.
//! - `fetch_new` — UIDL, diff against `known`, RETR the missing ones
//!   (most recent first, at most `max_fetch`), folder is always "INBOX".
//! - `delete`    — DELE by UIDL token, commit with QUIT.

use std::collections::HashSet;

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

use crate::error::{Error, Result};
use crate::mail::RawMail;
use crate::net::{self, MaybeTlsStream};
use crate::types::{AccountConfig, TlsMode};

/// Longest single protocol line we accept outside of message bodies
/// (status lines, UIDL/LIST entries).
const MAX_STATUS_LINE: usize = 8 * 1024;
/// Cap on the UIDL listing: a hostile server must not be able to make us
/// allocate one entry per line forever.
const MAX_UIDL_LINES: usize = 50_000;
/// Cap on a single `RETR` payload (25 MB). Larger mail is refused, not buffered.
const MAX_MESSAGE_BYTES: usize = 25 * 1024 * 1024;
/// RFC 1939 caps unique-ids at 70 chars; stay lenient with sloppy servers.
const MAX_UIDL_TOKEN: usize = 255;

// ---------------------------------------------------------------------------
// Pure protocol helpers (unit-tested below)
// ---------------------------------------------------------------------------

/// A parsed single-line POP3 status response.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Status {
    Ok(String),
    Err(String),
}

/// Strip a leading status keyword. It must be the whole line or be followed by
/// whitespace, so that "+OKAY" is not mistaken for a success.
fn strip_keyword<'a>(line: &'a str, keyword: &str) -> Option<&'a str> {
    let rest = line.strip_prefix(keyword)?;
    if rest.is_empty() || rest.starts_with([' ', '\t']) {
        Some(rest.trim())
    } else {
        None
    }
}

/// Split a status line into `+OK` / `-ERR` plus its explanatory text.
/// Returns `None` for anything that is not a valid status line.
fn split_status(line: &str) -> Option<Status> {
    if let Some(text) = strip_keyword(line, "+OK") {
        return Some(Status::Ok(text.to_string()));
    }
    strip_keyword(line, "-ERR").map(|text| Status::Err(text.to_string()))
}

/// Servers may answer with a bare `-ERR`; keep the surfaced message readable.
fn err_text(text: &str) -> &str {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        "服务器未说明原因"
    } else {
        trimmed
    }
}

/// True for the lone `.` that terminates a multi-line response.
fn is_terminator(wire: &[u8]) -> bool {
    wire.len() == 1 && wire[0] == b'.'
}

/// Undo POP3 byte-stuffing (RFC 1939 §3): the server prepends an extra `.` to
/// any body line that starts with one, so the client strips a single leading
/// dot. Callers must check [`is_terminator`] first.
fn unstuff(wire: &[u8]) -> &[u8] {
    match wire.first() {
        Some(b'.') => &wire[1..],
        _ => wire,
    }
}

/// Append one wire line of a multi-line body to `out`, de-stuffed and with the
/// CRLF the terminator search consumed put back.
fn append_body_line(out: &mut Vec<u8>, wire: &[u8]) {
    out.extend_from_slice(unstuff(wire));
    out.extend_from_slice(b"\r\n");
}

/// Parse one `UIDL` listing line: `"<message-number> <unique-id>"`.
fn parse_uidl_line(line: &str) -> Option<(u32, String)> {
    let mut parts = line.split_whitespace();
    let number: u32 = parts.next()?.parse().ok()?;
    let token = parts.next()?;
    // Message numbers are 1-based; a 0 or an over-long token means the server
    // is not speaking RFC 1939 and the entry is unusable as a stable key.
    if number == 0 || token.len() > MAX_UIDL_TOKEN {
        return None;
    }
    Some((number, token.to_string()))
}

/// Pick what to `RETR`: unknown tokens, newest (highest number) first, capped
/// at `max_fetch`, handed back oldest-first so the caller ingests in order.
fn select_fetch_targets(
    listing: &[(u32, String)],
    known: &HashSet<String>,
    max_fetch: u32,
) -> Vec<(u32, String)> {
    let mut newest_first: Vec<&(u32, String)> = listing.iter().collect();
    newest_first.sort_by(|a, b| b.0.cmp(&a.0));

    let mut seen: HashSet<&str> = HashSet::new();
    let mut picked: Vec<(u32, String)> = Vec::new();
    for (number, token) in newest_first {
        if picked.len() >= max_fetch as usize {
            break;
        }
        // `seen` drops duplicate tokens: a repeated unique-id would otherwise
        // cost us a second RETR for mail the store dedups anyway.
        if known.contains(token) || !seen.insert(token.as_str()) {
            continue;
        }
        picked.push((*number, token.clone()));
    }
    picked.sort_by_key(|(number, _)| *number);
    picked
}

/// Resolve UIDL tokens back to the message numbers of the current session.
fn resolve_delete_targets(listing: &[(u32, String)], uids: &[String]) -> Vec<u32> {
    let wanted: HashSet<&str> = uids.iter().map(String::as_str).collect();
    let mut numbers: Vec<u32> = listing
        .iter()
        .filter(|(_, token)| wanted.contains(token.as_str()))
        .map(|(number, _)| *number)
        .collect();
    numbers.sort_unstable();
    numbers.dedup();
    numbers
}

/// Reject credentials carrying CRLF: they would let a crafted value smuggle
/// extra commands into the session.
fn check_credential(value: &str, what: &str) -> Result<()> {
    if value.contains(['\r', '\n', '\0']) {
        return Err(Error::InvalidConfig(format!("POP3 {what}包含非法的换行或空字符")));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Session
// ---------------------------------------------------------------------------

/// One POP3 conversation. Writes go straight through the `BufReader` (its
/// `AsyncWrite` impl delegates to the inner stream); reads are buffered so we
/// can slice the response into lines.
struct Session {
    reader: BufReader<MaybeTlsStream>,
}

impl Session {
    fn new(stream: MaybeTlsStream) -> Self {
        Session {
            reader: BufReader::new(stream),
        }
    }

    /// Read one CRLF-terminated line without its line ending, reading at most
    /// `limit` bytes so an endless line cannot exhaust memory.
    async fn read_line(&mut self, limit: usize) -> Result<Vec<u8>> {
        let mut wire = Vec::new();
        let read = (&mut self.reader)
            .take(limit as u64 + 1)
            .read_until(b'\n', &mut wire)
            .await?;
        if read == 0 {
            return Err(Error::Pop3("连接被服务器提前关闭".to_string()));
        }
        if wire.last() != Some(&b'\n') {
            return Err(if wire.len() > limit {
                Error::Pop3(format!("服务器响应行超过 {limit} 字节上限"))
            } else {
                Error::Pop3("连接在响应结束前被服务器关闭".to_string())
            });
        }
        wire.pop();
        if wire.last() == Some(&b'\r') {
            wire.pop();
        }
        Ok(wire)
    }

    /// Read and classify a single-line response. The `Result` covers transport
    /// failures and unparseable lines; `Status` carries the server's verdict.
    async fn read_status(&mut self) -> Result<Status> {
        let wire = self.read_line(MAX_STATUS_LINE).await?;
        // Banners occasionally carry non-ASCII; never fail on encoding alone.
        let line = String::from_utf8_lossy(&wire).into_owned();
        split_status(&line).ok_or_else(|| Error::Pop3(format!("无法识别的服务器响应: {line}")))
    }

    /// Write a command and flush it. Callers must keep CRLF out of arguments.
    async fn send(&mut self, command: &str) -> Result<()> {
        self.reader.write_all(command.as_bytes()).await?;
        self.reader.write_all(b"\r\n").await?;
        self.reader.flush().await?;
        Ok(())
    }

    /// Send a command that must succeed; `-ERR` becomes `Error::Pop3`.
    /// The command itself is never echoed into the error (it may hold a password).
    async fn command(&mut self, command: &str) -> Result<String> {
        self.send(command).await?;
        match self.read_status().await? {
            Status::Ok(text) => Ok(text),
            Status::Err(text) => Err(Error::Pop3(format!("命令被拒绝: {}", err_text(&text)))),
        }
    }

    /// The greeting the server sends before any command.
    async fn read_greeting(&mut self) -> Result<()> {
        match self.read_status().await? {
            Status::Ok(_) => Ok(()),
            Status::Err(text) => Err(Error::Pop3(format!("服务器拒绝连接: {}", err_text(&text)))),
        }
    }

    /// Read a multi-line text response (UIDL / LIST) up to the lone `.`.
    async fn read_listing(&mut self, max_lines: usize) -> Result<Vec<String>> {
        let mut lines = Vec::new();
        loop {
            let wire = self.read_line(MAX_STATUS_LINE).await?;
            if is_terminator(&wire) {
                break;
            }
            if lines.len() >= max_lines {
                return Err(Error::Pop3(format!("服务器返回的列表超过 {max_lines} 行上限")));
            }
            lines.push(String::from_utf8_lossy(unstuff(&wire)).into_owned());
        }
        Ok(lines)
    }

    /// Read a multi-line message body (RETR) up to the lone `.`, de-stuffed.
    async fn read_body(&mut self, max_bytes: usize) -> Result<Vec<u8>> {
        let mut body = Vec::new();
        loop {
            // The per-line ceiling is the whole-message ceiling, so neither a
            // single endless line nor endless lines can outgrow `max_bytes`.
            let wire = self.read_line(max_bytes).await?;
            if is_terminator(&wire) {
                break;
            }
            append_body_line(&mut body, &wire);
            if body.len() > max_bytes {
                return Err(Error::Pop3(format!(
                    "邮件超过 {} MB 上限",
                    max_bytes / (1024 * 1024)
                )));
            }
        }
        Ok(body)
    }

    /// USER/PASS login. A refusal here is a credential problem, not a protocol one.
    async fn login(&mut self, account: &AccountConfig) -> Result<()> {
        check_credential(&account.username, "用户名")?;
        check_credential(&account.password, "密码")?;

        self.send(&format!("USER {}", account.username)).await?;
        if let Status::Err(text) = self.read_status().await? {
            return Err(Error::Auth(format!("POP3 用户名被拒绝: {}", err_text(&text))));
        }
        self.send(&format!("PASS {}", account.password)).await?;
        if let Status::Err(text) = self.read_status().await? {
            return Err(Error::Auth(format!("POP3 密码被拒绝: {}", err_text(&text))));
        }
        Ok(())
    }

    /// `UIDL` — message number → stable unique-id for the whole maildrop.
    async fn uidl(&mut self) -> Result<Vec<(u32, String)>> {
        self.send("UIDL").await?;
        if let Status::Err(text) = self.read_status().await? {
            return Err(Error::Pop3(format!(
                "服务器不支持 UIDL，无法识别新邮件: {}",
                err_text(&text)
            )));
        }
        let lines = self.read_listing(MAX_UIDL_LINES).await?;
        let mut listing = Vec::with_capacity(lines.len());
        for line in &lines {
            match parse_uidl_line(line) {
                Some(entry) => listing.push(entry),
                None => tracing::warn!("pop3: skipping malformed UIDL line {line:?}"),
            }
        }
        Ok(listing)
    }

    /// `RETR n`. `None` means the server refused this one (e.g. another client
    /// deleted it) — the session stays in sync and usable.
    async fn retr(&mut self, number: u32) -> Result<Option<Vec<u8>>> {
        self.send(&format!("RETR {number}")).await?;
        if let Status::Err(text) = self.read_status().await? {
            tracing::warn!("pop3: RETR {number} refused: {}", err_text(&text));
            return Ok(None);
        }
        Ok(Some(self.read_body(MAX_MESSAGE_BYTES).await?))
    }

    /// `QUIT` — POP3 only commits `DELE`s when the session ends this way.
    async fn quit(&mut self) -> Result<()> {
        self.command("QUIT").await.map(|_| ())
    }

    /// Unwrap the plaintext socket for the STARTTLS upgrade.
    fn into_plain(self) -> Result<TcpStream> {
        // Anything already buffered was injected before the handshake — a
        // classic STARTTLS command-stuffing attack. Refuse to carry it over.
        if !self.reader.buffer().is_empty() {
            return Err(Error::Pop3(
                "STLS 之后仍有未处理的明文数据，连接可能被劫持".to_string(),
            ));
        }
        match self.reader.into_inner() {
            MaybeTlsStream::Plain(tcp) => Ok(tcp),
            MaybeTlsStream::Tls(_) => {
                Err(Error::Pop3("连接已经是 TLS，无法重复升级".to_string()))
            }
        }
    }
}

/// Open a session per the account's TLS mode and consume the greeting.
async fn connect(account: &AccountConfig) -> Result<Session> {
    match account.tls {
        TlsMode::Tls => {
            let tls = net::connect_tls(&account.host, account.port).await?;
            let mut session = Session::new(MaybeTlsStream::Tls(Box::new(tls)));
            session.read_greeting().await?;
            Ok(session)
        }
        TlsMode::Starttls => {
            let tcp = net::tcp_connect(&account.host, account.port).await?;
            let mut session = Session::new(MaybeTlsStream::Plain(tcp));
            session.read_greeting().await?;
            session.command("STLS").await.map_err(|e| match e {
                Error::Pop3(msg) => Error::Pop3(format!("服务器不支持 STLS: {msg}")),
                other => other,
            })?;
            let tcp = session.into_plain()?;
            let tls = net::tls_wrap(&account.host, tcp).await?;
            // No second greeting: the session continues where STLS left off.
            Ok(Session::new(MaybeTlsStream::Tls(Box::new(tls))))
        }
        TlsMode::None => {
            let tcp = net::tcp_connect(&account.host, account.port).await?;
            let mut session = Session::new(MaybeTlsStream::Plain(tcp));
            session.read_greeting().await?;
            Ok(session)
        }
    }
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Connectivity + credential test.
pub async fn check(account: &AccountConfig) -> Result<()> {
    let mut session = connect(account).await?;
    session.login(account).await?;
    // Credentials are already proven; a rough hang-up on QUIT is not a failure.
    if let Err(e) = session.quit().await {
        tracing::warn!("pop3: QUIT failed after a successful check: {e}");
    }
    Ok(())
}

/// Fetch messages whose UIDL tokens are not in `known`.
pub async fn fetch_new(
    account: &AccountConfig,
    known: &HashSet<String>,
    max_fetch: u32,
) -> Result<Vec<RawMail>> {
    if max_fetch == 0 {
        return Ok(Vec::new());
    }

    let mut session = connect(account).await?;
    session.login(account).await?;

    let listing = session.uidl().await?;
    let targets = select_fetch_targets(&listing, known, max_fetch);

    let mut mails = Vec::with_capacity(targets.len());
    for (number, token) in targets {
        if let Some(bytes) = session.retr(number).await? {
            mails.push(RawMail {
                uid: token,
                folder: "INBOX".to_string(),
                bytes,
            });
        }
    }

    // Nothing is pending server-side, so a failed QUIT must not discard mail
    // we already hold — but do release the maildrop lock when we can.
    if let Err(e) = session.quit().await {
        tracing::warn!("pop3: QUIT failed after fetching {} messages: {e}", mails.len());
    }
    Ok(mails)
}

/// Permanently delete messages by UIDL token.
pub async fn delete(account: &AccountConfig, uids: &[String]) -> Result<()> {
    if uids.is_empty() {
        return Ok(());
    }

    let mut session = connect(account).await?;
    session.login(account).await?;

    let listing = session.uidl().await?;
    let targets = resolve_delete_targets(&listing, uids);
    if targets.len() < uids.len() {
        tracing::warn!(
            "pop3: {} of {} UIDL tokens are no longer on the server",
            uids.len() - targets.len(),
            uids.len()
        );
    }

    for number in targets {
        match session.command(&format!("DELE {number}")).await {
            Ok(_) => {}
            // One stale message must not abort the rest of the batch.
            Err(Error::Pop3(msg)) => tracing::warn!("pop3: DELE {number} failed: {msg}"),
            // A broken transport means QUIT could not commit anything anyway.
            Err(e) => return Err(e),
        }
    }

    // POP3 only applies DELE at QUIT time — this one is mandatory.
    session.quit().await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn listing(entries: &[(u32, &str)]) -> Vec<(u32, String)> {
        entries.iter().map(|(n, t)| (*n, t.to_string())).collect()
    }

    fn known(tokens: &[&str]) -> HashSet<String> {
        tokens.iter().map(|t| t.to_string()).collect()
    }

    #[test]
    fn status_lines_are_classified() {
        assert_eq!(split_status("+OK"), Some(Status::Ok(String::new())));
        assert_eq!(
            split_status("+OK POP3 ready"),
            Some(Status::Ok("POP3 ready".to_string()))
        );
        assert_eq!(
            split_status("-ERR invalid password"),
            Some(Status::Err("invalid password".to_string()))
        );
        assert_eq!(split_status("-ERR"), Some(Status::Err(String::new())));
        // Not status lines.
        assert_eq!(split_status("+OKAY sure"), None);
        assert_eq!(split_status("1 abc"), None);
        assert_eq!(split_status(""), None);
    }

    #[test]
    fn missing_error_text_is_replaced() {
        assert_eq!(err_text(""), "服务器未说明原因");
        assert_eq!(err_text("   "), "服务器未说明原因");
        assert_eq!(err_text(" boom "), "boom");
    }

    #[test]
    fn only_a_lone_dot_terminates() {
        assert!(is_terminator(b"."));
        assert!(!is_terminator(b".."));
        assert!(!is_terminator(b""));
        assert!(!is_terminator(b". "));
    }

    #[test]
    fn byte_stuffing_is_undone() {
        assert_eq!(unstuff(b".."), b"." as &[u8]);
        assert_eq!(unstuff(b"..hidden"), b".hidden" as &[u8]);
        assert_eq!(unstuff(b"...."), b"..." as &[u8]);
        assert_eq!(unstuff(b".hidden"), b"hidden" as &[u8]);
        assert_eq!(unstuff(b"plain"), b"plain" as &[u8]);
        assert_eq!(unstuff(b""), b"" as &[u8]);
    }

    #[test]
    fn body_is_reassembled_with_crlf() {
        let wire: [&[u8]; 5] = [
            b"Subject: hi",
            b"",
            b"body",
            b"..stuffed", // a real line starting with a dot
            b"..",        // a real line that is just a dot
        ];
        let mut body = Vec::new();
        for line in wire {
            assert!(!is_terminator(line), "no terminator in the fixture");
            append_body_line(&mut body, line);
        }
        let expected: &[u8] = b"Subject: hi\r\n\r\nbody\r\n.stuffed\r\n.\r\n";
        assert_eq!(body, expected);
    }

    #[test]
    fn uidl_lines_are_parsed() {
        let one = parse_uidl_line("1 whqtswO00WBw");
        assert_eq!(one, Some((1, "whqtswO00WBw".to_string())));
        let two = parse_uidl_line("  2\tQhdPYR:00WBw  ");
        assert_eq!(two, Some((2, "QhdPYR:00WBw".to_string())));

        // Junk and unusable entries.
        assert_eq!(parse_uidl_line("3"), None);
        assert_eq!(parse_uidl_line("x y"), None);
        assert_eq!(parse_uidl_line(""), None);
        assert_eq!(parse_uidl_line("0 zero-is-not-a-message"), None);
        let overlong = format!("1 {}", "t".repeat(MAX_UIDL_TOKEN + 1));
        assert_eq!(parse_uidl_line(&overlong), None);
    }

    #[test]
    fn fetch_targets_are_newest_first_but_returned_oldest_first() {
        let all = listing(&[(1, "a"), (2, "b"), (3, "c"), (4, "d"), (5, "e")]);
        // Newest three of the unknown ones, handed back in arrival order.
        let picked = select_fetch_targets(&all, &known(&["b"]), 3);
        assert_eq!(picked, listing(&[(3, "c"), (4, "d"), (5, "e")]));
    }

    #[test]
    fn fetch_targets_skip_known_and_duplicate_tokens() {
        let all = listing(&[(1, "a"), (2, "b"), (3, "b"), (4, "c")]);
        let picked = select_fetch_targets(&all, &known(&["a"]), 10);
        // "b" is listed twice; only the newest copy is fetched.
        assert_eq!(picked, listing(&[(3, "b"), (4, "c")]));

        assert!(select_fetch_targets(&all, &known(&["a", "b", "c"]), 10).is_empty());
        assert!(select_fetch_targets(&all, &HashSet::new(), 0).is_empty());
        assert!(select_fetch_targets(&[], &HashSet::new(), 10).is_empty());
    }

    #[test]
    fn delete_targets_resolve_tokens_to_numbers() {
        let all = listing(&[(1, "a"), (2, "b"), (3, "c")]);
        let uids = vec!["c".to_string(), "a".to_string(), "gone".to_string()];
        assert_eq!(resolve_delete_targets(&all, &uids), vec![1, 3]);
        assert!(resolve_delete_targets(&all, &[]).is_empty());
    }

    #[test]
    fn credentials_with_crlf_are_rejected() {
        assert!(check_credential("user@example.com", "用户名").is_ok());
        assert!(check_credential("p@ss word", "密码").is_ok());
        assert!(check_credential("user\r\nDELE 1", "用户名").is_err());
        assert!(check_credential("pass\nQUIT", "密码").is_err());
        assert!(check_credential("pass\0", "密码").is_err());
    }
}
