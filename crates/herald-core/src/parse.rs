use mail_parser::{Addr, Message, MessageParser, MimeHeaders};

use crate::error::{HeraldError, Result};
use crate::model::{AttachmentMeta, MessageHeader, ParsedAttachment, ParsedMessage};

fn parser() -> MessageParser {
    MessageParser::new()
        .with_minimal_headers()
        .with_message_ids()
}

/// Parse a complete RFC822 message (RETR) or a header-only block (TOP).
/// `include_attachment_content` controls whether decoded attachment bytes are
/// retained; metadata is always produced.
pub fn parse_message(
    raw: &[u8],
    uidl: &str,
    size: u64,
    include_attachment_content: bool,
) -> Result<ParsedMessage> {
    let message = parser()
        .parse(raw)
        .ok_or_else(|| HeraldError::Pop3("邮件解析失败".to_string()))?;
    let mut parsed = parse_parsed(
        &message,
        uidl,
        size,
        raw.len() as u64,
        include_attachment_content,
    )?;
    if parsed
        .header
        .subject
        .as_deref()
        .is_none_or(|subject| subject.contains('\u{FFFD}'))
    {
        if let Some(subject) = raw_8bit_header(raw, "Subject").and_then(|value| decode_8bit(&value))
        {
            let subject = subject.trim();
            if !subject.is_empty() {
                parsed.header.subject = Some(subject.to_string());
            }
        }
    }
    Ok(parsed)
}

/// Raw 8-bit headers are not decoded by mail-parser; recover common Chinese
/// charsets (GBK/BIG5) for headers that are neither UTF-8 nor RFC 2047.
fn raw_8bit_header(raw: &[u8], name: &str) -> Option<Vec<u8>> {
    let header_end = raw
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .or_else(|| raw.windows(2).position(|window| window == b"\n\n"))
        .unwrap_or(raw.len());
    let mut value: Option<Vec<u8>> = None;
    for line in raw[..header_end].split(|byte| *byte == b'\n') {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if let Some(current) = value.as_mut() {
            if line
                .first()
                .is_some_and(|byte| matches!(byte, b' ' | b'\t'))
            {
                current.push(b' ');
                current.extend_from_slice(trim_ascii(line));
                continue;
            }
            break;
        }
        let Some(colon) = line.iter().position(|byte| *byte == b':') else {
            continue;
        };
        if line[..colon].eq_ignore_ascii_case(name.as_bytes()) {
            value = Some(trim_ascii(&line[colon + 1..]).to_vec());
        }
    }
    let value = value?;
    if value.iter().any(|byte| *byte >= 0x80) && !value.windows(2).any(|w| w == b"=?") {
        Some(value)
    } else {
        None
    }
}

fn trim_ascii(mut value: &[u8]) -> &[u8] {
    while value.first().is_some_and(u8::is_ascii_whitespace) {
        value = &value[1..];
    }
    while value.last().is_some_and(u8::is_ascii_whitespace) {
        value = &value[..value.len() - 1];
    }
    value
}

fn decode_8bit(value: &[u8]) -> Option<String> {
    if let Ok(text) = std::str::from_utf8(value) {
        return Some(text.to_string());
    }
    for encoding in [encoding_rs::GBK, encoding_rs::BIG5] {
        let (text, _, had_errors) = encoding.decode(value);
        if !had_errors {
            return Some(text.into_owned());
        }
    }
    None
}

fn parse_parsed(
    message: &Message<'_>,
    uidl: &str,
    size: u64,
    raw_len: u64,
    include_attachment_content: bool,
) -> Result<ParsedMessage> {
    let from = message
        .from()
        .and_then(|list| list.first())
        .map(format_addr);
    let to = message
        .to()
        .map(|list| list.iter().map(format_addr).collect::<Vec<_>>())
        .unwrap_or_default();
    let cc = message
        .cc()
        .map(|list| list.iter().map(format_addr).collect::<Vec<_>>())
        .unwrap_or_default();

    let subject = message
        .subject()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let date = message.date().map(|date| date.to_rfc3339());
    let message_id = message.message_id().map(str::to_string);

    let body_text = message
        .body_text(0)
        .filter(|text| !text.trim().is_empty())
        .map(|text| text.into_owned());
    let body_html = message
        .body_html(0)
        .filter(|html| !html.trim().is_empty())
        .map(|html| html.into_owned());

    let mut attachments = Vec::new();
    for (index, part) in message.attachments().enumerate() {
        let mime_type = part
            .content_type()
            .map(|content_type| match content_type.subtype() {
                Some(subtype) => format!("{}/{}", content_type.ctype(), subtype),
                None => content_type.ctype().to_string(),
            });
        let meta = AttachmentMeta {
            index,
            filename: part.attachment_name().map(str::to_string),
            mime_type,
            size: part.len() as u64,
            content_id: part.content_id().map(str::to_string),
            is_inline: part
                .content_disposition()
                .is_some_and(|disposition| disposition.is_inline()),
            has_content: include_attachment_content,
        };
        let content = if include_attachment_content {
            Some(part.contents().to_vec())
        } else {
            None
        };
        attachments.push(ParsedAttachment { meta, content });
    }

    let size = if size == 0 { raw_len } else { size };
    Ok(ParsedMessage {
        header: MessageHeader {
            uidl: uidl.to_string(),
            message_id,
            from,
            to,
            cc,
            subject,
            date,
            size,
        },
        body_text,
        body_html,
        attachments,
    })
}

fn format_addr(addr: &Addr<'_>) -> String {
    match (addr.name.as_deref(), addr.address.as_deref()) {
        (Some(name), Some(address)) => {
            if name.eq_ignore_ascii_case(address) {
                address.to_string()
            } else {
                format!("{name} <{address}>")
            }
        }
        (None, Some(address)) => address.to_string(),
        (Some(name), None) => name.to_string(),
        (None, None) => String::new(),
    }
}

/// A cruder "display name" used for embeddings (subject + sender display name).
pub fn sender_display_name(from: Option<&str>) -> Option<&str> {
    let from = from?;
    match from.rsplit_once('<') {
        Some((name, _)) => {
            let name = name.trim().trim_matches('"').trim();
            if name.is_empty() {
                None
            } else {
                Some(name)
            }
        }
        None => Some(from),
    }
}

/// Flatten a header-only address list for search/index text. Groups are
/// already flattened by mail-parser's address iterator.
pub fn addresses_to_text(addresses: &[String]) -> String {
    addresses.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(crlf: &str) -> Vec<u8> {
        crlf.replace('\n', "\r\n").into_bytes()
    }

    #[test]
    fn parses_chinese_subject_and_base64_body() {
        // "主题：项目进度" in base64 UTF-8, body "你好，世界" base64 UTF-8.
        let subject = base64_subject("主题：项目进度");
        let body = "5L2g5aW977yM5LiW55WM";
        let raw = format!(
            "From: 张三 <zhangsan@example.com>\nTo: 李四 <lisi@example.com>\nSubject: {subject}\nDate: Tue, 03 Sep 2024 10:00:00 +0800\nMessage-ID: <abc@example.com>\nContent-Type: text/plain; charset=utf-8\nContent-Transfer-Encoding: base64\n\n{body}\n"
        );
        let parsed = parse_message(&sample(&raw), "u1", 100, false).unwrap();
        assert_eq!(parsed.header.subject.as_deref(), Some("主题：项目进度"));
        assert_eq!(
            parsed.header.from.as_deref(),
            Some("张三 <zhangsan@example.com>")
        );
        assert_eq!(parsed.header.to, vec!["李四 <lisi@example.com>"]);
        assert_eq!(parsed.body_text.as_deref(), Some("你好，世界"));
        assert!(parsed.attachments.is_empty());
    }

    fn base64_subject(text: &str) -> String {
        use base64::Engine;
        format!(
            "=?UTF-8?B?{}?=",
            base64::engine::general_purpose::STANDARD.encode(text)
        )
    }

    #[test]
    fn parses_gbk_header_and_body() {
        // GBK bytes for subject "测试" and body "中文"
        let subject = [0xB2, 0xE2, 0xCA, 0xD4];
        let body = [0xD6, 0xD0, 0xCE, 0xC4];
        let mut raw = Vec::new();
        raw.extend_from_slice(b"From: a@example.com\r\nSubject: ");
        raw.extend_from_slice(&subject);
        raw.extend_from_slice(b"\r\nContent-Type: text/plain; charset=gbk\r\n\r\n");
        raw.extend_from_slice(&body);
        raw.extend_from_slice(b"\r\n");
        let parsed = parse_message(&raw, "gbk", 0, false).unwrap();
        assert_eq!(parsed.header.subject.as_deref(), Some("测试"));
        assert_eq!(parsed.body_text.as_deref().map(str::trim), Some("中文"));
    }

    #[test]
    fn parses_html_and_attachment_metadata() {
        let raw = sample(
            "From: a@example.com\nSubject: report\nMIME-Version: 1.0\nContent-Type: multipart/mixed; boundary=\"b\"\n\n--b\nContent-Type: text/html; charset=utf-8\n\n<p>hello <b>world</b></p>\n--b\nContent-Type: application/pdf; name=\"report.pdf\"\nContent-Disposition: attachment; filename=\"report.pdf\"\nContent-Transfer-Encoding: base64\nContent-ID: <cid1>\n\nJVBERi0xLjQK\n--b--\n",
        );
        let parsed = parse_message(&raw, "x", 0, true).unwrap();
        assert!(parsed
            .body_html
            .as_deref()
            .unwrap_or("")
            .contains("<b>world</b>"));
        assert_eq!(parsed.attachments.len(), 1);
        assert_eq!(
            parsed.attachments[0].meta.filename.as_deref(),
            Some("report.pdf")
        );
        assert_eq!(
            parsed.attachments[0].meta.mime_type.as_deref(),
            Some("application/pdf")
        );
        assert_eq!(
            parsed.attachments[0].meta.content_id.as_deref(),
            Some("cid1")
        );
        assert!(parsed.attachments[0].meta.has_content);
        assert!(parsed.attachments[0].content.is_some());
    }

    #[test]
    fn header_only_parse_works() {
        let raw = sample("From: a@example.com\nSubject: only header\n\n");
        let parsed = parse_message(&raw, "h", 10, false).unwrap();
        assert_eq!(parsed.header.subject.as_deref(), Some("only header"));
        assert!(parsed.body_text.is_none());
    }
}
