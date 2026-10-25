use std::collections::{HashMap, HashSet};
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use rustls_pki_types::CertificateDer;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::net::TcpStream;
use tokio_rustls::client::TlsStream;

use crate::config::{AccountConfig, Security};
use crate::error::{HeraldError, Result};

#[derive(Debug, Clone)]
pub struct Pop3Options {
    pub host: String,
    pub port: u16,
    pub security: Security,
    pub username: String,
    pub password: String,
    pub timeout: Duration,
    /// Additional trusted roots (internal CA / test server).
    pub extra_roots: Vec<CertificateDer<'static>>,
}

impl Pop3Options {
    pub fn from_account(account: &AccountConfig, password: String) -> Self {
        Self {
            host: account.host.clone(),
            port: account.port,
            security: account.security,
            username: account.username.clone(),
            password,
            timeout: Duration::from_secs(account.timeout_secs()),
            extra_roots: Vec::new(),
        }
    }

    pub fn with_extra_roots(mut self, roots: Vec<CertificateDer<'static>>) -> Self {
        self.extra_roots = roots;
        self
    }
}

#[derive(Debug, Clone, Default)]
pub struct Capabilities {
    pub capa_supported: bool,
    /// Unknown capability means "do not use TOP"; RETR is always safe.
    pub top: bool,
    pub uidl: bool,
    pub stls: bool,
    pub user: bool,
}

impl Capabilities {
    fn from_lines(lines: &[String]) -> Self {
        let mut caps = Capabilities {
            capa_supported: true,
            top: false,
            uidl: false,
            stls: false,
            user: false,
        };
        for line in lines {
            let keyword = line
                .split_whitespace()
                .next()
                .unwrap_or("")
                .to_ascii_uppercase();
            match keyword.as_str() {
                "TOP" => caps.top = true,
                "UIDL" => caps.uidl = true,
                "STLS" => caps.stls = true,
                "USER" => caps.user = true,
                _ => {}
            }
        }
        caps
    }
}

enum Conn {
    Plain(TcpStream),
    Tls(TlsStream<TcpStream>),
    /// Temporary placeholder used while upgrading a plain connection to TLS.
    Closed,
}

impl AsyncRead for Conn {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            Conn::Plain(stream) => Pin::new(stream).poll_read(cx, buf),
            Conn::Tls(stream) => Pin::new(stream).poll_read(cx, buf),
            Conn::Closed => Poll::Ready(Ok(())),
        }
    }
}

impl AsyncWrite for Conn {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        match self.get_mut() {
            Conn::Plain(stream) => Pin::new(stream).poll_write(cx, buf),
            Conn::Tls(stream) => Pin::new(stream).poll_write(cx, buf),
            Conn::Closed => Poll::Ready(Err(std::io::Error::other("连接已关闭"))),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            Conn::Plain(stream) => Pin::new(stream).poll_flush(cx),
            Conn::Tls(stream) => Pin::new(stream).poll_flush(cx),
            Conn::Closed => Poll::Ready(Ok(())),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            Conn::Plain(stream) => Pin::new(stream).poll_shutdown(cx),
            Conn::Tls(stream) => Pin::new(stream).poll_shutdown(cx),
            Conn::Closed => Poll::Ready(Ok(())),
        }
    }
}

/// A deliberately small, read-only POP3 client. Only the commands required by
/// the product are implemented: CAPA, UIDL, LIST, TOP, RETR, USER, PASS, STLS
/// and QUIT. Mailbox-mutating commands (DELE/RSET) are intentionally absent.
pub struct Pop3Client {
    conn: Conn,
    buffer: Vec<u8>,
    timeout: Duration,
    extra_roots: Vec<CertificateDer<'static>>,
    host: String,
    capabilities: Capabilities,
}

impl Pop3Client {
    pub async fn connect(options: &Pop3Options) -> Result<Self> {
        if options.username.contains(['\r', '\n']) || options.password.contains(['\r', '\n']) {
            return Err(HeraldError::Config(
                "用户名或密码包含非法换行符".to_string(),
            ));
        }
        let timeout = options.timeout;
        let addrs = tokio::time::timeout(
            timeout,
            tokio::net::lookup_host((options.host.as_str(), options.port)),
        )
        .await
        .map_err(|_| HeraldError::Timeout(timeout.as_secs()))?
        .map_err(|err| HeraldError::Pop3(format!("无法解析服务器地址：{err}")))?
        .collect::<Vec<_>>();
        let addr = addrs
            .first()
            .copied()
            .ok_or_else(|| HeraldError::Pop3("服务器地址解析结果为空".to_string()))?;

        let tcp = tokio::time::timeout(timeout, TcpStream::connect(addr))
            .await
            .map_err(|_| HeraldError::Timeout(timeout.as_secs()))?
            .map_err(|err| HeraldError::Pop3(format!("连接失败：{err}")))?;
        let _ = tcp.set_nodelay(true);

        let conn = match options.security {
            Security::Plain | Security::Stls => Conn::Plain(tcp),
            Security::Tls => {
                let tls = tokio::time::timeout(
                    timeout,
                    crate::tls::connect(tcp, &options.host, &options.extra_roots),
                )
                .await
                .map_err(|_| HeraldError::Timeout(timeout.as_secs()))??;
                Conn::Tls(tls)
            }
        };

        let mut client = Self {
            conn,
            buffer: Vec::new(),
            timeout,
            extra_roots: options.extra_roots.clone(),
            host: options.host.clone(),
            capabilities: Capabilities {
                top: true,
                ..Default::default()
            },
        };

        client.read_status_line().await?;
        client.capabilities = client.capa().await.unwrap_or_default();

        if options.security == Security::Stls {
            if !client.capabilities.capa_supported {
                return Err(HeraldError::Pop3(
                    "服务器未提供 CAPA，无法确认 STLS 支持".to_string(),
                ));
            }
            if !client.capabilities.stls {
                return Err(HeraldError::Pop3("服务器不支持 STLS".to_string()));
            }
            client.command("STLS").await?;
            client.upgrade_to_tls().await?;
            client.capabilities = client.capa().await.unwrap_or(Capabilities {
                top: true,
                ..Default::default()
            });
        }

        client.login(&options.username, &options.password).await?;
        Ok(client)
    }

    async fn upgrade_to_tls(&mut self) -> Result<()> {
        let conn = std::mem::replace(&mut self.conn, Conn::Closed);
        let Conn::Plain(tcp) = conn else {
            return Err(HeraldError::Pop3("连接已处于 TLS".to_string()));
        };
        let tls = tokio::time::timeout(
            self.timeout,
            crate::tls::connect(tcp, &self.host, &self.extra_roots),
        )
        .await
        .map_err(|_| HeraldError::Timeout(self.timeout.as_secs()))??;
        self.conn = Conn::Tls(tls);
        Ok(())
    }

    async fn login(&mut self, username: &str, password: &str) -> Result<()> {
        let response = self.command(&format!("USER {username}")).await?;
        tracing::debug!(user = %username, response = %response, "POP3 USER");
        self.command(&format!("PASS {password}"))
            .await
            .map_err(|err| HeraldError::Auth(format!("POP3 认证失败：{err}")))?;
        tracing::debug!("POP3 认证成功");
        Ok(())
    }

    pub fn capabilities(&self) -> &Capabilities {
        &self.capabilities
    }

    pub async fn capa(&mut self) -> Result<Capabilities> {
        let response = match self.command("CAPA").await {
            Ok(response) => response,
            Err(_) => return Ok(Capabilities::default()),
        };
        let _ = response;
        let lines = self.read_multiline_text().await?;
        Ok(Capabilities::from_lines(&lines))
    }

    pub async fn uidl(&mut self) -> Result<Vec<(u32, String)>> {
        if self.capabilities.capa_supported && !self.capabilities.uidl {
            return Err(HeraldError::Unsupported(
                "服务器不支持 UIDL，无法进行增量同步".to_string(),
            ));
        }
        self.command("UIDL").await?;
        let lines = self.read_multiline_text().await?;
        let mut result = Vec::with_capacity(lines.len());
        for line in lines {
            let (number, uidl) = line
                .split_once(' ')
                .ok_or_else(|| HeraldError::Pop3(format!("无法解析 UIDL 行：{line}")))?;
            let number: u32 = number
                .parse()
                .map_err(|_| HeraldError::Pop3(format!("无法解析 UIDL 序号：{number}")))?;
            result.push((number, uidl.trim().to_string()));
        }
        Ok(result)
    }

    pub async fn list(&mut self) -> Result<HashMap<u32, u64>> {
        self.command("LIST").await?;
        let lines = self.read_multiline_text().await?;
        let mut result = HashMap::with_capacity(lines.len());
        for line in lines {
            let mut parts = line.split_whitespace();
            let number = parts.next().and_then(|v| v.parse::<u32>().ok());
            let size = parts.next().and_then(|v| v.parse::<u64>().ok());
            if let (Some(number), Some(size)) = (number, size) {
                result.insert(number, size);
            }
        }
        Ok(result)
    }

    pub async fn top(&mut self, number: u32, lines: u32) -> Result<Vec<u8>> {
        self.command(&format!("TOP {number} {lines}")).await?;
        self.read_multiline_raw().await
    }

    pub async fn retr(&mut self, number: u32) -> Result<Vec<u8>> {
        self.command(&format!("RETR {number}")).await?;
        self.read_multiline_raw().await
    }

    /// Fetch at least the header block. Uses TOP when available and falls back
    /// to RETR when TOP is unsupported or rejected.
    pub async fn fetch_headers(&mut self, number: u32) -> Result<Vec<u8>> {
        if self.capabilities.top {
            match self.top(number, 0).await {
                Ok(raw) => return Ok(raw),
                Err(err) => {
                    tracing::warn!(%err, number, "TOP 失败，回退 RETR");
                    self.capabilities.top = false;
                }
            }
        }
        self.retr(number).await
    }

    pub async fn quit(mut self) -> Result<()> {
        let _ = self.command("QUIT").await;
        let _ = self.conn.shutdown().await;
        Ok(())
    }

    async fn command(&mut self, command: &str) -> Result<String> {
        debug_assert!(!command.contains(['\r', '\n']));
        if command.contains(['\r', '\n']) {
            return Err(HeraldError::InvalidArgument(
                "POP3 命令包含非法换行符".to_string(),
            ));
        }
        let mut line = command.as_bytes().to_vec();
        line.extend_from_slice(b"\r\n");
        tokio::time::timeout(self.timeout, async {
            self.conn.write_all(&line).await?;
            self.conn.flush().await
        })
        .await
        .map_err(|_| HeraldError::Timeout(self.timeout.as_secs()))?
        .map_err(HeraldError::Io)?;
        self.read_status_line().await
    }

    async fn read_status_line(&mut self) -> Result<String> {
        let line = self.read_line_raw().await?;
        let text = String::from_utf8_lossy(&line).to_string();
        if let Some(rest) = text.strip_prefix("+OK") {
            Ok(rest.trim_start().to_string())
        } else if let Some(rest) = text.strip_prefix("-ERR") {
            Err(HeraldError::Pop3(rest.trim_start().to_string()))
        } else {
            Err(HeraldError::Pop3(format!("非预期的响应：{text}")))
        }
    }

    async fn read_line_raw(&mut self) -> Result<Vec<u8>> {
        loop {
            if let Some(position) = self.buffer.iter().position(|&byte| byte == b'\n') {
                let mut line: Vec<u8> = self.buffer.drain(..=position).collect();
                line.pop();
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                return Ok(line);
            }
            let mut chunk = [0u8; 4096];
            let read = tokio::time::timeout(self.timeout, self.conn.read(&mut chunk))
                .await
                .map_err(|_| HeraldError::Timeout(self.timeout.as_secs()))?
                .map_err(HeraldError::Io)?;
            if read == 0 {
                if self.buffer.is_empty() {
                    return Err(HeraldError::Pop3("服务器关闭了连接".to_string()));
                }
                let mut line = std::mem::take(&mut self.buffer);
                if line.last() == Some(&b'\n') {
                    line.pop();
                }
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                return Ok(line);
            }
            self.buffer.extend_from_slice(&chunk[..read]);
        }
    }

    async fn read_multiline_text(&mut self) -> Result<Vec<String>> {
        let lines = self.read_multiline_lines().await?;
        Ok(lines
            .into_iter()
            .map(|line| String::from_utf8_lossy(&line).to_string())
            .collect())
    }

    async fn read_multiline_raw(&mut self) -> Result<Vec<u8>> {
        let lines = self.read_multiline_lines().await?;
        let mut out = Vec::new();
        for line in lines {
            out.extend_from_slice(&line);
            out.extend_from_slice(b"\r\n");
        }
        Ok(out)
    }

    async fn read_multiline_lines(&mut self) -> Result<Vec<Vec<u8>>> {
        let mut lines = Vec::new();
        loop {
            let mut line = self.read_line_raw().await?;
            if line == b"." {
                break;
            }
            if line.first() == Some(&b'.') {
                line.remove(0);
            }
            lines.push(line);
        }
        Ok(lines)
    }
}

/// Sort new UIDLs in server (ascending mailbox) order and take the most recent
/// `limit` when one is given.
pub fn select_new_uidls(
    server_uidls: &[(u32, String)],
    known: &HashSet<String>,
    limit: Option<usize>,
) -> Vec<(u32, String)> {
    let mut new: Vec<(u32, String)> = server_uidls
        .iter()
        .filter(|(_, uidl)| !known.contains(uidl))
        .cloned()
        .collect();
    new.sort_by_key(|(number, _)| *number);
    if let Some(limit) = limit {
        if new.len() > limit {
            new = new.split_off(new.len() - limit);
        }
    }
    new
}

/// Fetch the raw RFC822 source of `uidl` without touching any local state.
pub async fn fetch_raw_uidl(options: &Pop3Options, uidl: &str) -> Result<Vec<u8>> {
    let mut client = Pop3Client::connect(options).await?;
    let listing = match client.uidl().await {
        Ok(listing) => listing,
        Err(err) => {
            let _ = client.quit().await;
            return Err(err);
        }
    };
    let number = listing
        .iter()
        .find(|(_, candidate)| candidate == uidl)
        .map(|(number, _)| *number);
    let Some(number) = number else {
        let _ = client.quit().await;
        return Err(HeraldError::not_found(format!(
            "服务器上不存在邮件：{uidl}"
        )));
    };
    let result = client.retr(number).await;
    let _ = client.quit().await;
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn select_new_uidls_takes_most_recent_limit() {
        let server = vec![
            (1, "a".to_string()),
            (2, "b".to_string()),
            (3, "c".to_string()),
            (4, "d".to_string()),
        ];
        let known = HashSet::from(["b".to_string()]);
        let new = select_new_uidls(&server, &known, Some(2));
        assert_eq!(
            new.into_iter().map(|(_, u)| u).collect::<Vec<_>>(),
            vec!["c", "d"]
        );
    }

    #[test]
    fn capability_parsing() {
        let caps = Capabilities::from_lines(&[
            "TOP".into(),
            "UIDL".into(),
            "STLS".into(),
            "SASL PLAIN".into(),
        ]);
        assert!(caps.top && caps.uidl && caps.stls && !caps.user);
    }
}
