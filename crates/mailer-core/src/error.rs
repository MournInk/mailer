//! Unified error type. Everything user-facing crosses the IPC boundary as a
//! plain string, so variants carry human-readable context.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("数据库错误: {0}")]
    Db(#[from] rusqlite::Error),

    #[error("网络连接失败: {0}")]
    Io(#[from] std::io::Error),

    #[error("TLS 错误: {0}")]
    Tls(String),

    #[error("IMAP 错误: {0}")]
    Imap(String),

    #[error("POP3 错误: {0}")]
    Pop3(String),

    #[error("SMTP 错误: {0}")]
    Smtp(String),

    #[error("邮件解析失败: {0}")]
    Parse(String),

    #[error("AI 接口错误: {0}")]
    Ai(String),

    #[error("通知渠道错误: {0}")]
    Notify(String),

    #[error("HTTP 错误: {0}")]
    Http(#[from] reqwest::Error),

    #[error("认证失败: {0}")]
    Auth(String),

    #[error("未找到: {0}")]
    NotFound(String),

    #[error("配置无效: {0}")]
    InvalidConfig(String),

    #[error("{0}")]
    Other(String),
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error::Parse(format!("JSON: {e}"))
    }
}

pub type Result<T> = std::result::Result<T, Error>;
