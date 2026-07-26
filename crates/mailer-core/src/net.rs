//! TCP + rustls connection helper shared by the IMAP and POP3 clients.

use std::sync::{Arc, OnceLock};

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

/// The shared client configuration.
///
/// Building it copies every root in the webpki bundle into a fresh store and
/// then has rustls re-derive its cipher-suite and key-exchange tables. That is
/// the same work every time and none of it depends on the peer, so doing it per
/// connection made every sync, STARTTLS upgrade and server-side delete pay for
/// a certificate store nobody had asked to change. One `Arc` clone instead.
fn tls_config() -> Arc<ClientConfig> {
    static CONFIG: OnceLock<Arc<ClientConfig>> = OnceLock::new();
    Arc::clone(CONFIG.get_or_init(|| {
        let mut roots = RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        Arc::new(
            ClientConfig::builder()
                .with_root_certificates(roots)
                .with_no_client_auth(),
        )
    }))
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Every connection must share one configuration. Rebuilding it per
    /// connection is invisible in behaviour and expensive in aggregate, so the
    /// sharing is what the test pins down.
    #[test]
    fn the_tls_config_is_built_once_and_shared() {
        assert!(Arc::ptr_eq(&tls_config(), &tls_config()));
    }

    /// A host that is not a valid server name must be named, not turned into an
    /// opaque handshake failure later.
    #[tokio::test]
    async fn an_invalid_server_name_is_refused_before_the_handshake() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let accept = tokio::spawn(async move { listener.accept().await.map(|_| ()) });

        let tcp = tcp_connect("127.0.0.1", addr.port()).await.unwrap();
        let err = tls_wrap("not a hostname", tcp).await.unwrap_err();
        assert!(matches!(err, Error::Tls(_)), "got {err:?}");
        let _ = accept.await;
    }
}
