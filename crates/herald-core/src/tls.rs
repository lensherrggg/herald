use std::sync::Arc;

use rustls::{ClientConfig, RootCertStore};
use rustls_pki_types::{CertificateDer, ServerName};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;

use crate::error::{HeraldError, Result};

/// Extra roots are appended to the platform trust store. This is required for
/// internal/self-signed mail servers and for the test suite.
pub fn build_client_config(extra_roots: &[CertificateDer<'static>]) -> Result<Arc<ClientConfig>> {
    let _ = rustls::crypto::ring::default_provider().install_default();

    let mut roots = RootCertStore::empty();
    let native = rustls_native_certs::load_native_certs();
    let mut native_added = 0usize;
    for cert in native.certs {
        if roots.add(cert).is_ok() {
            native_added += 1;
        }
    }
    if native_added == 0 {
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    }
    for cert in extra_roots {
        roots
            .add(cert.clone())
            .map_err(|err| HeraldError::Tls(format!("无法添加自定义根证书：{err}")))?;
    }

    let config = ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(Arc::new(config))
}

pub async fn connect(
    stream: TcpStream,
    host: &str,
    extra_roots: &[CertificateDer<'static>],
) -> Result<tokio_rustls::client::TlsStream<TcpStream>> {
    let config = build_client_config(extra_roots)?;
    let server_name = ServerName::try_from(host.to_string())
        .map_err(|err| HeraldError::Tls(format!("无效的服务器名称 {host}：{err}")))?;
    TlsConnector::from(config)
        .connect(server_name, stream)
        .await
        .map_err(|err| HeraldError::Tls(format!("TLS 握手失败：{err}")))
}
