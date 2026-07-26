//! TCP + rustls connection helper shared by the IMAP and POP3 clients.

use std::sync::Arc;

use rustls_pki_types::ServerName;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio_rustls::client::TlsStream;
use tokio_rustls::rustls::{ClientConfig, RootCertStore};
use tokio_rustls::TlsConnector;

use crate::error::{Error, Result};

/// Either a raw TCP stream or a TLS-wrapped one, behind one type so protocol
/// clients don't need to be generic over the transport.
pub enum MaybeTlsStream {
    Plain(TcpStream),
    Tls(Box<TlsStream<TcpStream>>),
}

impl AsyncRead for MaybeTlsStream {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        match self.get_mut() {
            MaybeTlsStream::Plain(s) => std::pin::Pin::new(s).poll_read(cx, buf),
            MaybeTlsStream::Tls(s) => std::pin::Pin::new(s.as_mut()).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for MaybeTlsStream {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        match self.get_mut() {
            MaybeTlsStream::Plain(s) => std::pin::Pin::new(s).poll_write(cx, buf),
            MaybeTlsStream::Tls(s) => std::pin::Pin::new(s.as_mut()).poll_write(cx, buf),
        }
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        match self.get_mut() {
            MaybeTlsStream::Plain(s) => std::pin::Pin::new(s).poll_flush(cx),
            MaybeTlsStream::Tls(s) => std::pin::Pin::new(s.as_mut()).poll_flush(cx),
        }
    }

    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        match self.get_mut() {
            MaybeTlsStream::Plain(s) => std::pin::Pin::new(s).poll_shutdown(cx),
            MaybeTlsStream::Tls(s) => std::pin::Pin::new(s.as_mut()).poll_shutdown(cx),
        }
    }
}

fn tls_config() -> Arc<ClientConfig> {
    let mut roots = RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    Arc::new(
        ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth(),
    )
}

/// Open a TCP connection to `host:port`.
pub async fn tcp_connect(host: &str, port: u16) -> Result<TcpStream> {
    let stream = TcpStream::connect((host, port)).await?;
    stream.set_nodelay(true).ok();
    Ok(stream)
}

/// Wrap an existing TCP stream in TLS (used both for implicit TLS and after a
/// STARTTLS handshake).
pub async fn tls_wrap(host: &str, stream: TcpStream) -> Result<TlsStream<TcpStream>> {
    let server_name = ServerName::try_from(host.to_string())
        .map_err(|e| Error::Tls(format!("无效的服务器名 {host}: {e}")))?;
    let connector = TlsConnector::from(tls_config());
    let tls = connector
        .connect(server_name, stream)
        .await
        .map_err(|e| Error::Tls(format!("TLS 握手失败 ({host}): {e}")))?;
    Ok(tls)
}

/// Connect with implicit TLS.
pub async fn connect_tls(host: &str, port: u16) -> Result<TlsStream<TcpStream>> {
    let tcp = tcp_connect(host, port).await?;
    tls_wrap(host, tcp).await
}
