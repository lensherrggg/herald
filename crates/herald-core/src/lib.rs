//! Herald core: configuration, portable paths, POP3 access, SQLite storage,
//! MIME parsing, keyword/semantic search and incremental synchronization.
//!
//! Entry points (CLI/TUI/MCP) are thin layers on top of [`Herald`].

pub mod config;
pub mod demo;
pub mod error;
pub mod mock_pop3;
pub mod model;
pub mod parse;
pub mod paths;
pub mod pop3;
pub mod search;
pub mod service;
pub mod store;
pub mod sync;
pub mod time;
pub mod tls;

#[cfg(feature = "semantic")]
pub mod models;
#[cfg(feature = "semantic")]
pub mod semantic;

pub use config::{AccountConfig, Config, Security};
pub use error::{HeraldError, Result};
pub use model::{
    AccountSummary, AttachmentData, AttachmentMeta, MessageDetail, MessageFilter, MessageHeader,
    MessageSummary, SearchKind, SearchResult, SyncErrorRecord, SyncOptions, SyncReport, SyncStatus,
};
pub use paths::{PathLayout, ResolvedPaths};
pub use service::Herald;
pub use store::Store;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
