//! stdio transport: a child process speaking newline-delimited JSON.
//!
//! Every frame is one line on stdin; every answer is one line on stdout. The
//! child's stderr is its log, not its protocol, so it is drained to `tracing`
//! rather than parsed — a server that logs a lot would otherwise fill its pipe
//! buffer and deadlock waiting for someone to read it.

use std::process::Stdio;
use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::sync::Mutex;

use super::wire;
use crate::error::{Error, Result};
use crate::types::McpServerConfig;

/// How long one request may take. A local process that has not answered in this
/// long is hung, and the session gets torn down and respawned on the next call.
const CALL_TIMEOUT: Duration = Duration::from_secs(90);
/// A single line of protocol. Large enough for a page of tool results, small
/// enough that a server emitting garbage cannot exhaust memory.
const MAX_LINE_BYTES: u64 = 8 * 1024 * 1024;

struct Pipes {
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

pub struct StdioTransport {
    name: String,
    /// Held so the child is killed when the session is dropped.
    child: std::sync::Mutex<Child>,
    /// One request at a time: answers are matched by reading forward, which two
    /// concurrent callers would interleave.
    pipes: Mutex<Pipes>,
}

impl StdioTransport {
    pub fn spawn(cfg: &McpServerConfig) -> Result<StdioTransport> {
        let command = cfg.command.trim();
        if command.is_empty() {
            return Err(Error::InvalidConfig(format!(
                "MCP 服务器「{}」没有填写要运行的命令",
                cfg.name
            )));
        }

        let mut cmd = tokio::process::Command::new(command);
        cmd.args(&cfg.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // The child outlives one call; without this it would inherit the
            // app's process group and take Ctrl-C with it on a terminal launch.
            .kill_on_drop(true);
        for (k, v) in &cfg.env {
            cmd.env(k, v);
        }

        let mut child = cmd.spawn().map_err(|e| {
            Error::Other(format!("无法启动 MCP 服务器「{}」({command}): {e}", cfg.name))
        })?;

        let stdin = child.stdin.take().ok_or_else(|| Error::Other("无法写入子进程".into()))?;
        let stdout = child.stdout.take().ok_or_else(|| Error::Other("无法读取子进程".into()))?;

        // Drain stderr forever. The task ends when the pipe closes, i.e. when
        // the child exits.
        if let Some(stderr) = child.stderr.take() {
            let label = cfg.name.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    tracing::debug!("mcp[{label}]: {line}");
                }
            });
        }

        Ok(StdioTransport {
            name: cfg.name.clone(),
            child: std::sync::Mutex::new(child),
            pipes: Mutex::new(Pipes { stdin, stdout: BufReader::new(stdout) }),
        })
    }

    /// True once the child has exited. A dead server is replaced rather than
    /// retried, because nothing it holds is recoverable.
    pub fn is_dead(&self) -> bool {
        matches!(self.child.lock().unwrap().try_wait(), Ok(Some(_)) | Err(_))
    }

    async fn write(&self, pipes: &mut Pipes, frame: &Value) -> Result<()> {
        let mut line = serde_json::to_vec(frame)?;
        line.push(b'\n');
        pipes.stdin.write_all(&line).await.map_err(|e| self.gone(e))?;
        pipes.stdin.flush().await.map_err(|e| self.gone(e))?;
        Ok(())
    }

    fn gone(&self, e: std::io::Error) -> Error {
        Error::Other(format!("MCP 服务器「{}」已退出: {e}", self.name))
    }

    pub async fn call(&self, frame: Value, id: u64) -> Result<Value> {
        let mut pipes = self.pipes.lock().await;
        self.write(&mut pipes, &frame).await?;

        let read = async {
            loop {
                let mut line = String::new();
                let n = (&mut pipes.stdout)
                    .take(MAX_LINE_BYTES)
                    .read_line(&mut line)
                    .await
                    .map_err(|e| self.gone(e))?;
                if n == 0 {
                    return Err(Error::Other(format!(
                        "MCP 服务器「{}」在回答前关闭了连接",
                        self.name
                    )));
                }
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let Ok(value) = serde_json::from_str::<Value>(trimmed) else {
                    // Servers that print a banner before speaking protocol are
                    // common enough that treating this as fatal would break
                    // working setups.
                    tracing::debug!(
                        "mcp[{}]: 跳过非 JSON 输出: {}",
                        self.name,
                        super::snippet(trimmed)
                    );
                    continue;
                };
                // Notifications and answers to other ids arrive on the same
                // pipe; only the one being waited on ends the read.
                if wire::is_ignorable(&value, id) {
                    continue;
                }
                return Ok(value);
            }
        };

        match tokio::time::timeout(CALL_TIMEOUT, read).await {
            Ok(result) => result,
            Err(_) => Err(Error::Other(format!(
                "MCP 服务器「{}」超过 {} 秒没有回应",
                self.name,
                CALL_TIMEOUT.as_secs()
            ))),
        }
    }

    pub async fn notify(&self, frame: Value) -> Result<()> {
        let mut pipes = self.pipes.lock().await;
        self.write(&mut pipes, &frame).await
    }
}
