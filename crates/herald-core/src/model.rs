use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MessageSummary {
    pub uidl: String,
    pub subject: Option<String>,
    pub from: Option<String>,
    pub date: Option<String>,
    pub size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MessageHeader {
    pub uidl: String,
    pub message_id: Option<String>,
    pub from: Option<String>,
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub subject: Option<String>,
    pub date: Option<String>,
    pub size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AttachmentMeta {
    /// Zero-based position within the message, in document order.
    pub index: usize,
    pub filename: Option<String>,
    pub mime_type: Option<String>,
    pub size: u64,
    pub content_id: Option<String>,
    pub is_inline: bool,
    pub has_content: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AttachmentData {
    pub meta: AttachmentMeta,
    pub content: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MessageDetail {
    pub header: MessageHeader,
    pub body_text: Option<String>,
    pub body_html: Option<String>,
    pub body_cached: bool,
    pub attachments: Vec<AttachmentMeta>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ParsedMessage {
    pub header: MessageHeader,
    pub body_text: Option<String>,
    pub body_html: Option<String>,
    pub attachments: Vec<ParsedAttachment>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ParsedAttachment {
    pub meta: AttachmentMeta,
    pub content: Option<Vec<u8>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SyncOptions {
    pub account: Option<String>,
    /// Maximum number of new messages to process. Ignored when `all`.
    pub limit: Option<usize>,
    pub all: bool,
    pub with_body: bool,
    pub with_attachments: bool,
    /// Upgrade messages that already have cached bodies to full embeddings.
    pub index: bool,
    /// Build/upgrade embeddings during this sync (CLI/TUI only).
    pub with_embeddings: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SyncErrorRecord {
    pub uidl: Option<String>,
    pub error: String,
    pub at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncReport {
    pub account: String,
    pub fetched: usize,
    pub new_count: usize,
    pub last_uidl: Option<String>,
    pub synced_at: String,
    pub bodies_cached: usize,
    pub attachments_stored: usize,
    pub errors: Vec<SyncErrorRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountSummary {
    pub name: String,
    pub host: String,
    pub username: String,
    pub last_sync_at: Option<String>,
    pub total_messages: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncStatus {
    pub account: String,
    pub last_sync_at: Option<String>,
    pub last_uidl: Option<String>,
    pub total_messages: i64,
    pub local_messages: i64,
    pub error_count: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageSort {
    #[default]
    Date,
    From,
    Subject,
}

impl MessageSort {
    pub fn label(self) -> &'static str {
        match self {
            MessageSort::Date => "日期",
            MessageSort::From => "发件人",
            MessageSort::Subject => "主题",
        }
    }

    pub fn next(self) -> Self {
        match self {
            MessageSort::Date => MessageSort::From,
            MessageSort::From => MessageSort::Subject,
            MessageSort::Subject => MessageSort::Date,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageFilter {
    pub account: Option<String>,
    pub limit: usize,
    pub offset: usize,
    pub from: Option<String>,
    pub since: Option<String>,
    pub until: Option<String>,
    pub sort: MessageSort,
    pub descending: bool,
}

impl Default for MessageFilter {
    fn default() -> Self {
        Self {
            account: None,
            limit: 20,
            offset: 0,
            from: None,
            since: None,
            until: None,
            sort: MessageSort::Date,
            descending: true,
        }
    }
}

impl MessageFilter {
    pub fn normalized(mut self) -> Self {
        self.limit = self.limit.clamp(1, 100);
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchKind {
    Keyword,
    Semantic,
    Hybrid,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    pub total: usize,
    pub messages: Vec<MessageSummary>,
    /// Why a requested mode had to degrade, e.g. hybrid without a model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// Resolve the start directory for attachment export:
/// configured export_dir -> last used -> system Downloads -> home directory.
pub fn default_export_base(
    configured: Option<std::path::PathBuf>,
    last_used: Option<std::path::PathBuf>,
) -> std::path::PathBuf {
    configured
        .filter(|p| p.is_dir())
        .or_else(|| last_used.filter(|p| p.is_dir()))
        .or_else(|| dirs::download_dir().filter(|p| p.is_dir()))
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| std::path::PathBuf::from("."))
}
