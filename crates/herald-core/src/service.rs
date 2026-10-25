#[cfg(feature = "semantic")]
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::config::{AccountConfig, Config};
use crate::error::{HeraldError, Result};
use crate::model::{
    default_export_base, AccountSummary, AttachmentData, MessageDetail, MessageFilter,
    MessageSummary, SearchField, SearchKind, SearchOptions, SearchResult, SyncErrorRecord,
    SyncOptions, SyncReport, SyncStatus,
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
    demo: Option<crate::demo::DemoSession>,
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

    pub fn open_with_config(mut config: Config, layout: PathLayout) -> Result<Self> {
        config.validate()?;
        let demo = if config.demo {
            Some(crate::demo::configure(&mut config)?)
        } else {
            None
        };
        let paths = layout.complete(&config);
        let mut store = Store::open(&paths.database_path)?;
        for account in &config.accounts {
            store.ensure_account(account)?;
        }
        let mut demo_jobs = Vec::new();
        if let Some(session) = &demo {
            let (primed, jobs) = crate::demo::initial_sync(&config, store)?;
            store = primed;
            demo_jobs = jobs;
            tracing::info!(
                "演示模式：内置假邮箱 127.0.0.1:{}，临时数据库 {}（退出后自动清空）",
                session.addr().port(),
                paths.database_path.display()
            );
        }
        #[cfg(feature = "semantic")]
        let semantic = SemanticService::new(
            &config,
            paths.models_dir.clone(),
            paths.tokenizer_dir.clone(),
        );
        #[allow(unused_mut)]
        let mut herald = Self {
            config,
            paths,
            store,
            #[cfg(feature = "semantic")]
            semantic,
            demo,
        };
        // Build embeddings up front only when the model is already on disk, so
        // semantic search works immediately in the trial session.
        #[cfg(feature = "semantic")]
        if herald.semantic_enabled() && herald.semantic.model_files_present() {
            herald.apply_embeddings(&demo_jobs);
        }
        #[cfg(not(feature = "semantic"))]
        let _ = demo_jobs;
        herald.log_startup();
        Ok(herald)
    }

    pub fn demo_active(&self) -> bool {
        self.demo.is_some()
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

    /// Hybrid search when embeddings are usable, keyword-only otherwise.
    /// Field-scoped queries always stay on the keyword path because
    /// embeddings are built from the whole message.
    pub fn default_search_kind(&self, field: SearchField) -> SearchKind {
        if field != SearchField::All {
            return SearchKind::Keyword;
        }
        #[cfg(feature = "semantic")]
        if self.semantic_enabled() && self.semantic.model_files_present() {
            return SearchKind::Hybrid;
        }
        SearchKind::Keyword
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
        let account = self
            .config
            .resolve_account(options.account.as_deref())?
            .clone();
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
        self.keyword_search_with(
            account,
            query,
            &SearchOptions {
                limit,
                offset,
                ..Default::default()
            },
        )
    }

    pub fn keyword_search_with(
        &self,
        account: Option<&str>,
        query: &str,
        options: &SearchOptions,
    ) -> Result<SearchResult> {
        let (_, account_id) = self.account_and_id(account)?;
        self.store.keyword_search_with(
            account_id,
            query,
            options.field,
            options.limit,
            options.offset,
        )
    }

    pub fn search(
        &mut self,
        account: Option<&str>,
        query: &str,
        kind: SearchKind,
        options: &SearchOptions,
    ) -> Result<SearchResult> {
        match kind {
            SearchKind::Keyword => self.keyword_search_with(account, query, options),
            SearchKind::Semantic => self.semantic_search(account, query, options),
            SearchKind::Hybrid => self.hybrid_search(account, query, options),
        }
    }

    #[cfg(feature = "semantic")]
    fn semantic_search(
        &mut self,
        account: Option<&str>,
        query: &str,
        options: &SearchOptions,
    ) -> Result<SearchResult> {
        if options.field != SearchField::All {
            return Err(HeraldError::Unsupported(
                "语义检索不支持字段限定；请改用关键词或混合检索".to_string(),
            ));
        }
        let (_, account_id) = self.account_and_id(account)?;
        let window = options.limit.saturating_add(options.offset).max(1);
        let result = self
            .semantic
            .search(&self.store, account_id, query, window)?;
        Ok(finish_results(result.messages, options, None))
    }

    #[cfg(not(feature = "semantic"))]
    fn semantic_search(
        &mut self,
        _account: Option<&str>,
        _query: &str,
        _options: &SearchOptions,
    ) -> Result<SearchResult> {
        Err(HeraldError::Unsupported(
            "此二进制未编译语义检索支持（feature = semantic）".to_string(),
        ))
    }

    #[cfg(feature = "semantic")]
    fn hybrid_search(
        &mut self,
        account: Option<&str>,
        query: &str,
        options: &SearchOptions,
    ) -> Result<SearchResult> {
        if options.field != SearchField::All {
            let mut result = self.keyword_search_with(account, query, options)?;
            result.note = Some("字段限定检索仅使用关键词".to_string());
            return Ok(result);
        }
        let (_, account_id) = self.account_and_id(account)?;
        // Take as many candidates from each side as we intend to return; RRF
        // only needs the relative order, not the raw scores.
        let window = options.limit.saturating_add(options.offset).max(1);
        let keyword =
            self.store
                .keyword_search_with(account_id, query, SearchField::All, window, 0)?;
        let (semantic, note) = match self.semantic.search(&self.store, account_id, query, window) {
            Ok(result) => (result.messages, None),
            Err(err) => (
                Vec::new(),
                Some(format!("语义检索不可用，已仅用关键词：{err}")),
            ),
        };
        let fused = fuse_rrf(&keyword.messages, &semantic);
        Ok(finish_results(fused, options, note))
    }

    #[cfg(not(feature = "semantic"))]
    fn hybrid_search(
        &mut self,
        account: Option<&str>,
        query: &str,
        options: &SearchOptions,
    ) -> Result<SearchResult> {
        let mut result = self.keyword_search_with(account, query, options)?;
        result.note = Some("此二进制未编译语义检索支持，已仅用关键词".to_string());
        Ok(result)
    }

    pub fn get_stored_message(&self, account: Option<&str>, uidl: &str) -> Result<MessageDetail> {
        let (_, account_id) = self.account_and_id(account)?;
        self.store.get_message(account_id, uidl)
    }

    /// Read state is local-only; the POP3 mailbox is never modified.
    pub fn set_read(&self, account: Option<&str>, uidl: &str, read: bool) -> Result<()> {
        let (_, account_id) = self.account_and_id(account)?;
        self.store.set_read(account_id, uidl, read)
    }

    pub fn unread_count(&self, account: Option<&str>) -> Result<usize> {
        let (_, account_id) = self.account_and_id(account)?;
        self.store.unread_count(account_id)
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

    pub fn sync_errors(&self, account: Option<&str>, limit: usize) -> Result<Vec<SyncErrorRecord>> {
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

    pub fn tokenizer_dir(&self) -> &Path {
        &self.paths.tokenizer_dir
    }

    #[cfg(feature = "semantic")]
    pub fn semantic_model_present(&self) -> bool {
        self.semantic.model_files_present()
    }
}

fn safe_filename(filename: Option<&str>, index: usize) -> String {
    // Split on both separators regardless of the host OS: attachment names
    // come from the mail message, so a Windows-style path can arrive on Unix.
    let candidate = filename
        .and_then(|name| name.rsplit(['/', '\\']).next())
        .map(str::trim)
        .filter(|name| !name.is_empty() && *name != "." && *name != "..")
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

/// Reciprocal Rank Fusion of two ranked lists. BM25 and vector distance live
/// on different scales, so only the positions are combined.
#[cfg(feature = "semantic")]
fn fuse_rrf(keyword: &[MessageSummary], semantic: &[MessageSummary]) -> Vec<MessageSummary> {
    const RRF_K: f64 = 60.0;
    let mut fused: HashMap<&str, (f64, &MessageSummary)> = HashMap::new();
    for (rank, message) in keyword.iter().enumerate() {
        let score = 1.0 / (RRF_K + (rank + 1) as f64);
        fused.insert(message.uidl.as_str(), (score, message));
    }
    for (rank, message) in semantic.iter().enumerate() {
        let score = 1.0 / (RRF_K + (rank + 1) as f64);
        fused
            .entry(message.uidl.as_str())
            .and_modify(|(total, _)| *total += score)
            .or_insert((score, message));
    }
    let mut ranked: Vec<(f64, &MessageSummary)> = fused.into_values().collect();
    ranked.sort_by(|a, b| {
        b.0.partial_cmp(&a.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| b.1.date.cmp(&a.1.date))
    });
    ranked
        .into_iter()
        .map(|(_, message)| message.clone())
        .collect()
}

/// Paginate and annotate a candidate list that was fetched in memory
/// (semantic / hybrid paths).
#[cfg(feature = "semantic")]
fn finish_results(
    messages: Vec<MessageSummary>,
    options: &SearchOptions,
    note: Option<String>,
) -> SearchResult {
    let total = messages.len();
    SearchResult {
        total,
        messages: messages
            .into_iter()
            .skip(options.offset)
            .take(options.limit)
            .collect(),
        note,
    }
}

#[cfg(all(test, feature = "semantic"))]
mod tests {
    use super::*;

    fn summary(uidl: &str, date: &str) -> MessageSummary {
        MessageSummary {
            uidl: uidl.to_string(),
            subject: Some(uidl.to_string()),
            from: None,
            date: Some(date.to_string()),
            size: 0,
            is_read: false,
        }
    }

    #[test]
    fn rrf_prefers_messages_found_by_both_sides() {
        let keyword = vec![
            summary("both", "2024-09-03T00:00:00Z"),
            summary("keyword", "2024-09-02T00:00:00Z"),
        ];
        let semantic = vec![
            summary("semantic", "2024-09-04T00:00:00Z"),
            summary("both", "2024-09-03T00:00:00Z"),
        ];
        let result = fuse_rrf(&keyword, &semantic);
        assert_eq!(result.len(), 3);
        assert_eq!(result[0].uidl, "both");
    }

    #[test]
    fn rrf_paginates_after_fusing() {
        let keyword = vec![
            summary("a", "2024-09-03T00:00:00Z"),
            summary("b", "2024-09-02T00:00:00Z"),
            summary("c", "2024-09-04T00:00:00Z"),
        ];
        let semantic = vec![
            summary("c", "2024-09-04T00:00:00Z"),
            summary("d", "2024-09-01T00:00:00Z"),
        ];
        let fused = fuse_rrf(&keyword, &semantic);
        let options = SearchOptions {
            limit: 2,
            offset: 1,
            ..Default::default()
        };
        let page = finish_results(fused, &options, None);
        assert_eq!(page.total, 4);
        assert_eq!(page.messages.len(), 2);
        let all = finish_results(
            fuse_rrf(&keyword, &semantic),
            &SearchOptions::default(),
            None,
        );
        assert_eq!(page.messages[0].uidl, all.messages[1].uidl);
        assert_eq!(page.messages[1].uidl, all.messages[2].uidl);
    }

    #[test]
    fn rrf_ties_break_on_newer_date() {
        let keyword = vec![summary("old", "2024-09-01T00:00:00Z")];
        let semantic = vec![summary("new", "2024-09-05T00:00:00Z")];
        let result = fuse_rrf(&keyword, &semantic);
        assert_eq!(result[0].uidl, "new");
    }

    #[test]
    fn safe_filenames_strip_paths() {
        assert_eq!(safe_filename(Some("../../etc/passwd"), 0), "passwd");
        assert_eq!(safe_filename(Some("..\\evil.exe"), 0), "evil.exe");
        assert_eq!(safe_filename(Some("folder/evil.exe"), 0), "evil.exe");
        assert_eq!(safe_filename(Some(".."), 1), "attachment-1.bin");
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
