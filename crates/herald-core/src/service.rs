use std::path::{Path, PathBuf};

use crate::config::{AccountConfig, Config};
use crate::error::{HeraldError, Result};
use crate::model::{
    default_export_base, AccountSummary, AttachmentData, MessageDetail, MessageFilter,
    MessageSummary, SearchKind, SearchResult, SyncErrorRecord, SyncOptions, SyncReport, SyncStatus,
};
use crate::parse::parse_message;
use crate::paths::{PathLayout, ResolvedPaths};
use crate::pop3::{self, Pop3Options};
use crate::search::index_text;
use crate::store::Store;
use crate::sync::{self, EmbeddingJob};

#[cfg(feature = "semantic")]
use crate::models::{self, DownloadOptions, DownloadReport};
#[cfg(feature = "semantic")]
use crate::semantic::SemanticService;

pub struct Herald {
    pub config: Config,
    pub paths: ResolvedPaths,
    pub store: Store,
    #[cfg(feature = "semantic")]
    semantic: SemanticService,
}

impl Herald {
    /// Resolve portable paths, load the config and open the database.
    pub fn open(config_override: Option<&Path>) -> Result<Self> {
        let layout = PathLayout::resolve(config_override)?;
        Self::open_layout(layout)
    }

    pub fn open_layout(layout: PathLayout) -> Result<Self> {
        let config = Config::load(&layout.config_path)?;
        Self::open_with_config(config, layout)
    }

    pub fn open_with_config(config: Config, layout: PathLayout) -> Result<Self> {
        config.validate()?;
        let paths = layout.complete(&config);
        let store = Store::open(&paths.database_path)?;
        for account in &config.accounts {
            store.ensure_account(account)?;
        }
        #[cfg(feature = "semantic")]
        let semantic = SemanticService::new(&config, paths.models_dir.clone());
        let herald = Self {
            config,
            paths,
            store,
            #[cfg(feature = "semantic")]
            semantic,
        };
        herald.log_startup();
        Ok(herald)
    }

    fn log_startup(&self) {
        for line in self.paths.describe() {
            tracing::info!("{line}");
        }
        if !self.paths.portable {
            if let Some(reason) = &self.paths.fallback_reason {
                tracing::warn!("{reason}");
            }
        }
        tracing::info!(
            "语义检索：{}",
            if self.semantic_enabled() {
                "启用"
            } else {
                "关闭"
            }
        );
    }

    pub fn semantic_enabled(&self) -> bool {
        #[cfg(feature = "semantic")]
        let enabled = self.config.semantic.enabled;
        #[cfg(not(feature = "semantic"))]
        let enabled = false;
        enabled
    }

    pub fn accounts(&self) -> Result<Vec<AccountSummary>> {
        self.store.list_accounts()
    }

    pub fn account(&self, name: Option<&str>) -> Result<AccountConfig> {
        self.config.resolve_account(name).cloned()
    }

    fn account_and_id(&self, name: Option<&str>) -> Result<(AccountConfig, i64)> {
        let account = self.account(name)?;
        let id = self.store.account_id(&account.name)?;
        Ok((account, id))
    }

    pub async fn sync(&mut self, options: SyncOptions) -> Result<SyncReport> {
        let account = self.config.resolve_account(options.account.as_deref())?.clone();
        let outcome = sync::run(&self.config, &mut self.store, &account, &options).await?;
        if options.with_embeddings {
            let jobs = if options.index {
                let account_id = self.store.account_id(&account.name)?;
                sync::jobs_from_all_bodies(&self.store, account_id)?
            } else {
                outcome.jobs
            };
            self.apply_embeddings(&jobs);
        }
        Ok(outcome.report)
    }

    #[cfg(feature = "semantic")]
    fn apply_embeddings(&mut self, jobs: &[EmbeddingJob]) {
        if !self.config.semantic.enabled {
            return;
        }
        let stored = self.semantic.apply_jobs(&self.store, jobs);
        if stored > 0 {
            tracing::info!(stored, "已更新语义索引");
        }
    }

    #[cfg(not(feature = "semantic"))]
    fn apply_embeddings(&mut self, _jobs: &[EmbeddingJob]) {}

    pub fn list_messages(
        &self,
        account: Option<&str>,
        filter: &MessageFilter,
    ) -> Result<(usize, Vec<MessageSummary>)> {
        let (_, account_id) = self.account_and_id(account)?;
        self.store.list_messages(account_id, filter)
    }

    pub fn keyword_search(
        &self,
        account: Option<&str>,
        query: &str,
        limit: usize,
        offset: usize,
    ) -> Result<SearchResult> {
        let (_, account_id) = self.account_and_id(account)?;
        self.store.keyword_search(account_id, query, limit, offset)
    }

    pub fn search(
        &mut self,
        account: Option<&str>,
        query: &str,
        kind: SearchKind,
        limit: usize,
        offset: usize,
    ) -> Result<SearchResult> {
        match kind {
            SearchKind::Keyword => self.keyword_search(account, query, limit, offset),
            SearchKind::Semantic => self.semantic_search(account, query, limit, offset),
        }
    }

    #[cfg(feature = "semantic")]
    fn semantic_search(
        &mut self,
        account: Option<&str>,
        query: &str,
        limit: usize,
        offset: usize,
    ) -> Result<SearchResult> {
        let (_, account_id) = self.account_and_id(account)?;
        let mut result = self
            .semantic
            .search(&self.store, account_id, query, limit.saturating_add(offset))?;
        result.total = result.messages.len();
        result.messages = result.messages.into_iter().skip(offset).take(limit).collect();
        Ok(result)
    }

    #[cfg(not(feature = "semantic"))]
    fn semantic_search(
        &mut self,
        _account: Option<&str>,
        _query: &str,
        _limit: usize,
        _offset: usize,
    ) -> Result<SearchResult> {
        Err(HeraldError::Unsupported(
            "此二进制未编译语义检索支持（feature = semantic）".to_string(),
        ))
    }

    pub fn get_stored_message(&self, account: Option<&str>, uidl: &str) -> Result<MessageDetail> {
        let (_, account_id) = self.account_and_id(account)?;
        self.store.get_message(account_id, uidl)
    }

    /// FR-PARSE-04: the first call fetches and caches the body; later calls
    /// never touch the network when the body is already cached.
    pub async fn get_message(
        &mut self,
        account: Option<&str>,
        uidl: &str,
        fetch_body: bool,
    ) -> Result<MessageDetail> {
        let (account, account_id) = self.account_and_id(account)?;
        let detail = self.store.get_message(account_id, uidl)?;
        if !fetch_body || detail.body_cached {
            return Ok(detail);
        }
        let options = Pop3Options::from_account(&account, account.resolve_password()?);
        let raw = pop3::fetch_raw_uidl(&options, uidl).await?;
        let parsed = parse_message(&raw, uidl, detail.header.size, false)?;
        let message_id = self.store.upsert_header(account_id, &parsed.header)?;
        let tokens = index_text(
            parsed.header.subject.as_deref(),
            parsed.header.from.as_deref(),
            parsed.body_text.as_deref(),
        );
        self.store.set_body(
            message_id,
            parsed.body_text.as_deref(),
            parsed.body_html.as_deref(),
            &tokens,
        )?;
        let metas: Vec<_> = parsed
            .attachments
            .iter()
            .map(|attachment| attachment.meta.clone())
            .collect();
        self.store.insert_attachments(message_id, &metas)?;
        self.apply_embeddings(&[EmbeddingJob {
            message_id,
            text: sync::embedding_text(
                parsed.header.subject.as_deref(),
                parsed.header.from.as_deref(),
                parsed.body_text.as_deref(),
            ),
        }]);
        self.store.get_message(account_id, uidl)
    }

    pub fn status(&self, account: Option<&str>) -> Result<SyncStatus> {
        let (account, account_id) = self.account_and_id(account)?;
        let mut status = self.store.sync_status(account_id)?;
        status.account = account.name;
        Ok(status)
    }

    pub fn sync_errors(
        &self,
        account: Option<&str>,
        limit: usize,
    ) -> Result<Vec<SyncErrorRecord>> {
        let (_, account_id) = self.account_and_id(account)?;
        self.store.recent_sync_errors(account_id, limit)
    }

    pub async fn get_attachment(
        &mut self,
        account: Option<&str>,
        uidl: &str,
        index: usize,
    ) -> Result<AttachmentData> {
        let (account, account_id) = self.account_and_id(account)?;
        match self.store.attachment(account_id, uidl, index) {
            Ok(data) => return Ok(data),
            Err(HeraldError::NotFound(_)) => {}
            Err(err) => return Err(err),
        }

        let options = Pop3Options::from_account(&account, account.resolve_password()?);
        let raw = pop3::fetch_raw_uidl(&options, uidl).await?;
        let parsed = parse_message(&raw, uidl, 0, true)?;
        let message_id = self.store.upsert_header(account_id, &parsed.header)?;
        let body_cached = self.store.body_is_cached(account_id, uidl)?;
        if !body_cached {
            let tokens = index_text(
                parsed.header.subject.as_deref(),
                parsed.header.from.as_deref(),
                parsed.body_text.as_deref(),
            );
            self.store.set_body(
                message_id,
                parsed.body_text.as_deref(),
                parsed.body_html.as_deref(),
                &tokens,
            )?;
            self.apply_embeddings(&[EmbeddingJob {
                message_id,
                text: sync::embedding_text(
                    parsed.header.subject.as_deref(),
                    parsed.header.from.as_deref(),
                    parsed.body_text.as_deref(),
                ),
            }]);
        }
        let metas: Vec<_> = parsed
            .attachments
            .iter()
            .map(|attachment| attachment.meta.clone())
            .collect();
        self.store.insert_attachments(message_id, &metas)?;
        let max_size = self.config.attachments.max_attachment_size_bytes;
        for attachment in &parsed.attachments {
            let Some(content) = &attachment.content else {
                continue;
            };
            if content.len() as u64 > max_size {
                return Err(HeraldError::InvalidArgument(format!(
                    "附件超过大小上限 {max_size} 字节"
                )));
            }
            self.store
                .set_attachment_content(message_id, attachment.meta.index, content)?;
        }
        self.store.attachment(account_id, uidl, index)
    }

    pub async fn export_attachment(
        &mut self,
        account: Option<&str>,
        uidl: &str,
        index: usize,
        dest: &Path,
    ) -> Result<PathBuf> {
        let data = self.get_attachment(account, uidl, index).await?;
        let filename = safe_filename(data.meta.filename.as_deref(), index);
        let target = if dest.is_dir() {
            unique_target(dest.join(filename))
        } else if !dest.exists() && dest.extension().is_none() {
            std::fs::create_dir_all(dest)?;
            unique_target(dest.join(filename))
        } else {
            dest.to_path_buf()
        };
        if let Some(parent) = target.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        std::fs::write(&target, &data.content)?;
        if let Some(parent) = target.parent() {
            self.store
                .set_local_state("last_export_dir", &parent.to_string_lossy())?;
        }
        Ok(target)
    }

    pub fn export_base_dir(&self) -> PathBuf {
        let last = self
            .store
            .get_local_state("last_export_dir")
            .ok()
            .flatten()
            .map(PathBuf::from);
        default_export_base(self.config.export_dir(), last)
    }

    /// Resolve an account into connection options without opening a socket.
    /// Used by callers that must release the borrow on `Herald` before awaiting.
    pub fn pop3_options(&self, account: Option<&str>) -> Result<Pop3Options> {
        let account = self.account(account)?;
        let password = account.resolve_password()?;
        Ok(Pop3Options::from_account(&account, password))
    }

    pub async fn raw_message(&self, account: Option<&str>, uidl: &str) -> Result<Vec<u8>> {
        let options = self.pop3_options(account)?;
        pop3::fetch_raw_uidl(&options, uidl).await
    }

    #[cfg(feature = "semantic")]
    pub async fn download_models(&self, options: &DownloadOptions) -> Result<DownloadReport> {
        models::download(&self.config, &self.paths, options).await
    }

    pub fn model_dir(&self) -> &Path {
        &self.paths.models_dir
    }
}

fn safe_filename(filename: Option<&str>, index: usize) -> String {
    let candidate = filename
        .and_then(|name| Path::new(name).file_name())
        .and_then(|name| name.to_str())
        .filter(|name| !name.trim().is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("attachment-{index}.bin"));
    candidate.replace(['/', '\\', '\0'], "_")
}

fn unique_target(path: PathBuf) -> PathBuf {
    if !path.exists() {
        return path;
    }
    let parent = path.parent().map(Path::to_path_buf).unwrap_or_default();
    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("attachment")
        .to_string();
    let extension = path.extension().and_then(|value| value.to_str());
    for counter in 1..10_000 {
        let name = match extension {
            Some(extension) => format!("{stem} ({counter}).{extension}"),
            None => format!("{stem} ({counter})"),
        };
        let candidate = parent.join(name);
        if !candidate.exists() {
            return candidate;
        }
    }
    path
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_filenames_strip_paths() {
        assert_eq!(safe_filename(Some("../../etc/passwd"), 0), "passwd");
        assert_eq!(safe_filename(Some("..\\evil.exe"), 0), "evil.exe");
        assert_eq!(safe_filename(None, 3), "attachment-3.bin");
    }

    #[test]
    fn unique_target_appends_counter() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("a.txt");
        std::fs::write(&path, b"x").unwrap();
        let unique = unique_target(path);
        assert_eq!(unique.file_name().unwrap(), "a (1).txt");
    }
}
