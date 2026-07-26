//! MCP stdio server for Mailer.
//!
//! Speaks newline-delimited JSON-RPC 2.0 over stdin/stdout so any Model
//! Context Protocol client — Claude Desktop, an IDE, another agent — can
//! search, read and triage the user's mail through the same tool layer the
//! in-app assistant uses.
//!
//! stdout carries protocol frames and nothing else. A stray `println!` there
//! desynchronises the client's parser and kills the session, so every
//! diagnostic goes to stderr, which MCP clients collect as a log.

mod core_tools;
mod protocol;
mod server;

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use mailer_core::store::Store;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::core_tools::CoreTools;

/// Consulted when no path is given on the command line.
const DB_ENV: &str = "MAILER_DB";

#[tokio::main]
async fn main() -> ExitCode {
    init_logging();

    let db = match resolve_db(std::env::args().nth(1), std::env::var(DB_ENV).ok()) {
        Ok(path) => path,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::FAILURE;
        }
    };

    let store = match Store::open(&db) {
        Ok(store) => Arc::new(store),
        Err(e) => {
            eprintln!("打开数据库失败 ({}): {e}", db.display());
            return ExitCode::FAILURE;
        }
    };

    let http = match reqwest::Client::builder().build() {
        Ok(client) => client,
        Err(e) => {
            eprintln!("初始化 HTTP 客户端失败: {e}");
            return ExitCode::FAILURE;
        }
    };

    tracing::info!(db = %db.display(), "mailer-mcp 已就绪，等待 MCP 客户端");
    match serve(&CoreTools::new(store, http)).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            // A closed pipe is how a client says goodbye; anything else is real.
            tracing::error!(error = %e, "传输中断");
            eprintln!("传输中断: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Read requests until stdin closes or a `shutdown` arrives.
async fn serve<H: server::ToolHost>(host: &H) -> std::io::Result<()> {
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut stdout = tokio::io::stdout();

    while let Some(line) = lines.next_line().await? {
        // Some clients pad frames with blank lines to flush their writer.
        if line.trim().is_empty() {
            continue;
        }

        let handled = server::handle(host, &line).await;
        if let Some(frame) = handled.frame {
            stdout.write_all(frame.as_bytes()).await?;
            stdout.write_all(b"\n").await?;
            // Clients block waiting for the reply, so nothing may sit in the
            // buffer until the next request pushes it out.
            stdout.flush().await?;
        }
        if handled.stop {
            tracing::info!("收到 shutdown，退出");
            break;
        }
    }
    Ok(())
}

/// Resolve the database path from `argv[1]`, then `$MAILER_DB`.
///
/// There is deliberately no default location. `Store::open` creates a database
/// when it finds none, and an MCP client happily answering "你没有任何邮件"
/// from an empty file it just made is far worse than refusing to start.
fn resolve_db(arg: Option<String>, env: Option<String>) -> Result<PathBuf, String> {
    fn cleaned(v: Option<String>) -> Option<String> {
        v.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
    }

    let raw = cleaned(arg).or_else(|| cleaned(env)).ok_or_else(|| {
        format!(
            "未指定数据库路径。\n用法: mailer-mcp <mailer.db 路径>\n或设置环境变量 {DB_ENV}。"
        )
    })?;

    let path = PathBuf::from(raw);
    if !path.exists() {
        return Err(format!(
            "数据库不存在: {}\n请先运行 Mailer 应用完成初始化，再用它的 mailer.db 启动本服务。",
            path.display()
        ));
    }
    if !path.is_file() {
        return Err(format!("数据库路径不是文件: {}", path.display()));
    }
    Ok(path)
}

/// Logging goes to stderr; stdout belongs to the protocol.
fn init_logging() {
    let directives = std::env::var("RUST_LOG")
        .unwrap_or_else(|_| "mailer_mcp=info,mailer_core=warn".to_string());
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        // Client log panes show raw bytes; escape codes there are noise.
        .with_ansi(false)
        .with_env_filter(tracing_subscriber::EnvFilter::new(directives))
        .init();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_argument_wins_over_the_environment() {
        let dir = std::env::temp_dir().join("mailer-mcp-resolve-arg");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let file = dir.join("from-arg.db");
        std::fs::write(&file, b"").expect("touch");

        let got = resolve_db(
            Some(file.to_string_lossy().into_owned()),
            Some("/nonexistent/from-env.db".to_string()),
        )
        .expect("argument path resolves");
        assert_eq!(got, file);
    }

    #[test]
    fn the_environment_is_the_fallback() {
        let dir = std::env::temp_dir().join("mailer-mcp-resolve-env");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let file = dir.join("from-env.db");
        std::fs::write(&file, b"").expect("touch");

        let got = resolve_db(None, Some(file.to_string_lossy().into_owned()))
            .expect("env path resolves");
        assert_eq!(got, file);
    }

    #[test]
    fn a_blank_argument_falls_through_to_the_environment() {
        let dir = std::env::temp_dir().join("mailer-mcp-resolve-blank");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let file = dir.join("blank.db");
        std::fs::write(&file, b"").expect("touch");

        let got = resolve_db(Some("   ".to_string()), Some(file.to_string_lossy().into_owned()))
            .expect("env path resolves");
        assert_eq!(got, file);
    }

    #[test]
    fn nothing_configured_is_an_error() {
        let err = resolve_db(None, None).expect_err("no path anywhere");
        assert!(err.contains(DB_ENV));
    }

    #[test]
    fn a_missing_file_is_never_created() {
        let missing = std::env::temp_dir().join("mailer-mcp-does-not-exist.db");
        let _ = std::fs::remove_file(&missing);
        let err = resolve_db(Some(missing.to_string_lossy().into_owned()), None)
            .expect_err("must refuse");
        assert!(err.contains("数据库不存在"));
        assert!(!missing.exists(), "resolution must not touch the filesystem");
    }

    #[test]
    fn a_directory_is_refused() {
        let dir = std::env::temp_dir().join("mailer-mcp-resolve-dir");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let err =
            resolve_db(Some(dir.to_string_lossy().into_owned()), None).expect_err("must refuse");
        assert!(err.contains("不是文件"));
    }
}
