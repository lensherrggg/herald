//! Minimal POP3 server used by the integration tests. Supports plain, STLS
//! and implicit TLS, and can be configured to reject TOP so the client
//! fallback path is exercised.

use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use rustls::ServerConfig;
use rustls_pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;
use tokio_rustls::TlsAcceptor;

use crate::config::Security;

#[derive(Debug, Clone)]
pub struct MockMessage {
    pub uidl: String,
    pub raw: Vec<u8>,
}

impl MockMessage {
    pub fn new(uidl: impl Into<String>, raw: impl Into<Vec<u8>>) -> Self {
        Self {
            uidl: uidl.into(),
            raw: raw.into(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct MockConfig {
    pub mode: Security,
    pub support_top: bool,
    pub username: String,
    pub password: String,
    pub messages: Vec<MockMessage>,
}

impl Default for MockConfig {
    fn default() -> Self {
        Self {
            mode: Security::Plain,
            support_top: true,
            username: "test".to_string(),
            password: "secret".to_string(),
            messages: Vec::new(),
        }
    }
}

pub struct MockPop3Server {
    addr: SocketAddr,
    cert_der: Option<CertificateDer<'static>>,
    task: JoinHandle<()>,
}

impl MockPop3Server {
    pub async fn start(config: MockConfig) -> io::Result<Self> {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;

        let (cert_der, acceptor) = match config.mode {
            Security::Plain => (None, None),
            Security::Stls | Security::Tls => {
                let certified = rcgen::generate_simple_self_signed(vec![
                    "localhost".to_string(),
                    "127.0.0.1".to_string(),
                ])
                .map_err(io::Error::other)?;
                let cert_der = certified.cert.der().clone();
                let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(
                    certified.signing_key.serialize_der(),
                ));
                let server_config = ServerConfig::builder()
                    .with_no_client_auth()
                    .with_single_cert(vec![cert_der.clone()], key)
                    .map_err(io::Error::other)?;
                (
                    Some(cert_der),
                    Some(TlsAcceptor::from(Arc::new(server_config))),
                )
            }
        };

        let shared = Arc::new(config);
        let acceptor = acceptor.map(Arc::new);
        let task = tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    break;
                };
                let config = shared.clone();
                let acceptor = acceptor.clone();
                tokio::spawn(async move {
                    let _ = serve_connection(stream, config, acceptor).await;
                });
            }
        });

        Ok(Self {
            addr,
            cert_der,
            task,
        })
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn cert_der(&self) -> Option<CertificateDer<'static>> {
        self.cert_der.clone()
    }
}

impl Drop for MockPop3Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

enum MockStream {
    Plain(TcpStream),
    Tls(tokio_rustls::server::TlsStream<TcpStream>),
}

impl MockStream {
    fn into_plain(self) -> Option<TcpStream> {
        match self {
            MockStream::Plain(stream) => Some(stream),
            MockStream::Tls(_) => None,
        }
    }
}

impl AsyncRead for MockStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        match self.get_mut() {
            MockStream::Plain(stream) => Pin::new(stream).poll_read(cx, buf),
            MockStream::Tls(stream) => Pin::new(stream).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for MockStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        match self.get_mut() {
            MockStream::Plain(stream) => Pin::new(stream).poll_write(cx, buf),
            MockStream::Tls(stream) => Pin::new(stream).poll_write(cx, buf),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            MockStream::Plain(stream) => Pin::new(stream).poll_flush(cx),
            MockStream::Tls(stream) => Pin::new(stream).poll_flush(cx),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            MockStream::Plain(stream) => Pin::new(stream).poll_shutdown(cx),
            MockStream::Tls(stream) => Pin::new(stream).poll_shutdown(cx),
        }
    }
}

async fn serve_connection(
    tcp: TcpStream,
    config: Arc<MockConfig>,
    acceptor: Option<Arc<TlsAcceptor>>,
) -> io::Result<()> {
    let mut tls_active = config.mode == Security::Tls;
    let mut stream = Some(if tls_active {
        let acceptor = acceptor
            .as_ref()
            .ok_or_else(|| io::Error::other("missing TLS acceptor"))?;
        MockStream::Tls(acceptor.accept(tcp).await?)
    } else {
        MockStream::Plain(tcp)
    });

    write_line(stream.as_mut().unwrap(), "+OK Herald mock POP3 ready").await?;
    let mut buffer = Vec::new();
    let mut authed = false;
    let mut pending_user = String::new();

    loop {
        let Some(stream_ref) = stream.as_mut() else {
            break;
        };
        let Some(line) = read_line(stream_ref, &mut buffer).await? else {
            break;
        };
        let text = String::from_utf8_lossy(&line).to_string();
        let mut parts = text.split_whitespace();
        let command = parts.next().unwrap_or("").to_ascii_uppercase();
        let argument = parts.next().unwrap_or("");

        match command.as_str() {
            "CAPA" => {
                let mut lines = vec!["IMPLEMENTATION Herald-Mock".to_string(), "UIDL".to_string(), "USER".to_string()];
                if config.support_top {
                    lines.push("TOP".to_string());
                }
                if config.mode == Security::Stls && !tls_active {
                    lines.push("STLS".to_string());
                }
                write_line(stream.as_mut().unwrap(), "+OK Capability list follows").await?;
                write_multiline(stream.as_mut().unwrap(), &lines).await?;
            }
            "USER" => {
                pending_user = argument.to_string();
                write_line(stream.as_mut().unwrap(), "+OK user accepted").await?;
            }
            "PASS" => {
                if pending_user == config.username && argument == config.password {
                    authed = true;
                    write_line(stream.as_mut().unwrap(), "+OK authenticated").await?;
                } else {
                    write_line(stream.as_mut().unwrap(), "-ERR authentication failed").await?;
                }
            }
            "STLS" => {
                if config.mode != Security::Stls || tls_active {
                    write_line(stream.as_mut().unwrap(), "-ERR STLS unavailable").await?;
                    continue;
                }
                let acceptor = acceptor
                    .as_ref()
                    .ok_or_else(|| io::Error::other("missing TLS acceptor"))?;
                write_line(stream.as_mut().unwrap(), "+OK begin TLS").await?;
                let old = stream.take().expect("stream present");
                let tcp = old
                    .into_plain()
                    .ok_or_else(|| io::Error::other("already TLS"))?;
                let upgraded = acceptor.accept(tcp).await?;
                stream = Some(MockStream::Tls(upgraded));
                tls_active = true;
                authed = false;
                pending_user.clear();
            }
            "QUIT" => {
                write_line(stream.as_mut().unwrap(), "+OK bye").await?;
                break;
            }
            "NOOP" => {
                write_line(stream.as_mut().unwrap(), "+OK").await?;
            }
            "STAT" | "LIST" | "UIDL" | "TOP" | "RETR" => {
                if !authed {
                    write_line(stream.as_mut().unwrap(), "-ERR not authenticated").await?;
                    continue;
                }
                match command.as_str() {
                    "STAT" => {
                        let total: usize = config.messages.iter().map(|m| m.raw.len()).sum();
                        write_line(
                            stream.as_mut().unwrap(),
                            &format!("+OK {} {}", config.messages.len(), total),
                        )
                        .await?;
                    }
                    "LIST" => {
                        let lines: Vec<String> = config
                            .messages
                            .iter()
                            .enumerate()
                            .map(|(index, message)| {
                                format!("{} {}", index + 1, message.raw.len())
                            })
                            .collect();
                        write_line(stream.as_mut().unwrap(), "+OK scan listing follows").await?;
                        write_multiline(stream.as_mut().unwrap(), &lines).await?;
                    }
                    "UIDL" => {
                        let lines: Vec<String> = config
                            .messages
                            .iter()
                            .enumerate()
                            .map(|(index, message)| format!("{} {}", index + 1, message.uidl))
                            .collect();
                        write_line(stream.as_mut().unwrap(), "+OK unique-id listing follows")
                            .await?;
                        write_multiline(stream.as_mut().unwrap(), &lines).await?;
                    }
                    "TOP" => {
                        if !config.support_top {
                            write_line(stream.as_mut().unwrap(), "-ERR TOP not supported").await?;
                            continue;
                        }
                        let number: usize = argument.parse().unwrap_or(0);
                        let lines: usize = parts.next().and_then(|v| v.parse().ok()).unwrap_or(0);
                        match config.messages.get(number.saturating_sub(1)) {
                            Some(message) => {
                                write_line(stream.as_mut().unwrap(), "+OK top follows").await?;
                                let top = top_lines(&message.raw, lines);
                                write_multiline(stream.as_mut().unwrap(), &top).await?;
                            }
                            None => {
                                write_line(stream.as_mut().unwrap(), "-ERR no such message")
                                    .await?;
                            }
                        }
                    }
                    "RETR" => {
                        let number: usize = argument.parse().unwrap_or(0);
                        match config.messages.get(number.saturating_sub(1)) {
                            Some(message) => {
                                write_line(stream.as_mut().unwrap(), "+OK message follows").await?;
                                let lines = split_lines(&message.raw);
                                write_multiline(stream.as_mut().unwrap(), &lines).await?;
                            }
                            None => {
                                write_line(stream.as_mut().unwrap(), "-ERR no such message")
                                    .await?;
                            }
                        }
                    }
                    _ => unreachable!(),
                }
            }
            _ => {
                write_line(stream.as_mut().unwrap(), "-ERR unknown command").await?;
            }
        }
    }
    Ok(())
}

async fn write_line(stream: &mut MockStream, line: &str) -> io::Result<()> {
    stream.write_all(line.as_bytes()).await?;
    stream.write_all(b"\r\n").await?;
    stream.flush().await
}

async fn write_multiline(stream: &mut MockStream, lines: &[String]) -> io::Result<()> {
    for line in lines {
        if line.starts_with('.') {
            stream.write_all(b".").await?;
        }
        stream.write_all(line.as_bytes()).await?;
        stream.write_all(b"\r\n").await?;
    }
    stream.write_all(b".\r\n").await?;
    stream.flush().await
}

async fn read_line(stream: &mut MockStream, buffer: &mut Vec<u8>) -> io::Result<Option<Vec<u8>>> {
    loop {
        if let Some(position) = buffer.iter().position(|&byte| byte == b'\n') {
            let mut line: Vec<u8> = buffer.drain(..=position).collect();
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            return Ok(Some(line));
        }
        let mut chunk = [0u8; 4096];
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            if buffer.is_empty() {
                return Ok(None);
            }
            let mut line = std::mem::take(buffer);
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            return Ok(Some(line));
        }
        buffer.extend_from_slice(&chunk[..read]);
    }
}

fn split_lines(raw: &[u8]) -> Vec<String> {
    raw.split(|&byte| byte == b'\n')
        .map(|line| {
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            String::from_utf8_lossy(line).to_string()
        })
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .skip_while(|line| line.is_empty())
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect()
}

fn top_lines(raw: &[u8], body_lines: usize) -> Vec<String> {
    let lines = split_lines(raw);
    let blank = lines.iter().position(|line| line.is_empty());
    match blank {
        Some(index) => {
            let mut result = lines[..=index].to_vec();
            result.extend(lines[index + 1..].iter().take(body_lines).cloned());
            result
        }
        None => lines,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn top_includes_headers_and_requested_body_lines() {
        let raw = b"Subject: x\r\nTo: y\r\n\r\nline1\r\nline2\r\n";
        let lines = top_lines(raw, 1);
        assert_eq!(
            lines,
            vec!["Subject: x", "To: y", "", "line1"],
            "TOP must keep the header/body blank line"
        );
    }

    #[test]
    fn split_lines_trims_trailing_blank_lines() {
        assert_eq!(split_lines(b"a\nb\n\n"), vec!["a", "b"]);
    }
}
