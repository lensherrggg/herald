use std::cell::Cell;
use std::path::Path;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use herald_core::model::{
    AccountSummary, AttachmentMeta, MessageDetail, MessageFilter, MessageSort, MessageSummary,
    SearchField, SearchKind, SearchOptions, SearchResult, SyncOptions, SyncStatus,
};
use herald_core::parse::{raw_attachments, RawAttachment};
use herald_core::paths::ResolvedPaths;
use herald_core::{Config, Herald};
use ratatui::DefaultTerminal;
use tokio::runtime::Runtime;
use unicode_width::UnicodeWidthChar;

use crate::browser::{sanitize_filename, Browser};
use crate::dir_history::DirHistory;
use crate::text_input;
use crate::ui;

pub const RAW_VIEW_LIMIT: usize = 256 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputKind {
    Search,
    NewDir,
}

#[derive(Debug)]
pub struct InputState {
    pub kind: InputKind,
    pub value: String,
    pub cursor: usize,
    pub search_kind: Option<SearchKind>,
    pub search_field: Option<SearchField>,
}

impl InputState {
    pub fn label(&self) -> &'static str {
        match (self.kind, self.search_kind) {
            (InputKind::Search, Some(SearchKind::Keyword)) => "关键词搜索",
            (InputKind::Search, Some(SearchKind::Hybrid)) => "混合搜索",
            (InputKind::Search, Some(SearchKind::Semantic)) => "语义搜索",
            (InputKind::Search, None) => "搜索",
            (InputKind::NewDir, _) => "新建目录",
        }
    }

    pub fn field_label(&self) -> Option<&'static str> {
        self.search_field.map(SearchField::label)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    List,
    Detail,
}

#[derive(Debug, Clone)]
pub struct SearchState {
    pub query: String,
    pub kind: SearchKind,
    pub field: SearchField,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawMatch {
    pub line: usize,
    pub start: usize,
    pub end: usize,
}

/// A display row of the raw view: a byte range inside one source line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawRow {
    pub line: usize,
    pub start: usize,
    pub end: usize,
}

#[derive(Debug)]
pub struct RawSearch {
    pub query: String,
    pub cursor: usize,
    pub matches: Vec<RawMatch>,
    pub current: usize,
    pub editing: bool,
}

impl SearchState {
    pub fn label(&self) -> &'static str {
        match self.kind {
            SearchKind::Keyword => "关键词",
            SearchKind::Hybrid => "混合",
            SearchKind::Semantic => "语义",
        }
    }
}

#[derive(Debug)]
pub struct AttachmentPicker {
    pub uidl: String,
    pub metas: Vec<AttachmentMeta>,
    pub selected: usize,
}

#[derive(Debug, Default)]
pub struct StatusMessage {
    pub text: String,
    pub error: bool,
}

impl StatusMessage {
    pub fn info(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            error: false,
        }
    }

    pub fn error(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            error: true,
        }
    }
}

#[derive(Debug)]
pub enum Mode {
    Browse,
    Input(InputState),
    Accounts,
    Attachments,
    Browser,
    Raw,
    Help,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ModeKind {
    Browse,
    Input,
    Accounts,
    Attachments,
    Browser,
    Raw,
    Help,
}

impl Mode {
    fn kind(&self) -> ModeKind {
        match self {
            Mode::Browse => ModeKind::Browse,
            Mode::Input(_) => ModeKind::Input,
            Mode::Accounts => ModeKind::Accounts,
            Mode::Attachments => ModeKind::Attachments,
            Mode::Browser => ModeKind::Browser,
            Mode::Raw => ModeKind::Raw,
            Mode::Help => ModeKind::Help,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ListMode {
    #[default]
    Grouped,
    Compact,
}

impl ListMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Grouped => "分组",
            Self::Compact => "紧凑",
        }
    }
}

fn grouped_list_for(mode: ListMode, searching: bool, sort: MessageSort) -> bool {
    mode == ListMode::Grouped && !searching && sort == MessageSort::Date
}

pub struct App {
    pub herald: Herald,
    pub runtime: Runtime,
    pub accounts: Vec<AccountSummary>,
    pub account: Option<String>,
    pub sync_status: Option<SyncStatus>,
    pub messages: Vec<MessageSummary>,
    pub total: usize,
    pub unread_total: usize,
    pub page_size: usize,
    pub page: usize,
    pub selected: usize,
    pub sort: MessageSort,
    pub descending: bool,
    pub list_mode: ListMode,
    pub search: Option<SearchState>,
    pub search_note: Option<String>,
    pub detail: Option<MessageDetail>,
    pub detail_scroll: u16,
    pub raw: Option<String>,
    pub raw_scroll: u16,
    pub raw_truncated: bool,
    pub raw_search: Option<RawSearch>,
    /// Inner width of the last raw-view render, used to locate matches.
    pub raw_width: Cell<u16>,
    /// Visible text height of the last raw-view render, used for paging.
    pub raw_height: Cell<u16>,
    /// Wrapped row count from the last raw-view render, used for clamping.
    pub raw_rows_total: Cell<usize>,
    pub mode: Mode,
    pub status: StatusMessage,
    pub focus: Focus,
    pub browser: Option<Browser>,
    pub attachments: Option<AttachmentPicker>,
    pub account_pick: usize,
    pub quit: bool,
    /// Set when leaving an overlay so the next frame repaints the whole screen.
    pub full_redraw: bool,
    pub color: bool,
    /// Accounts still configured with a plaintext password.
    pub plaintext_accounts: Vec<String>,
    /// One-time-per-startup advice overlay; any key dismisses it.
    pub cred_notice: bool,
}

impl App {
    pub fn new(herald: Herald, runtime: Runtime, requested_account: Option<String>) -> Self {
        let accounts = herald.accounts().unwrap_or_default();
        let account = pick_account(&herald.config, &accounts, requested_account);
        let color = color_enabled();
        let page_size = herald.config.ui.page_size.max(1);
        let plaintext_accounts = herald.config.plaintext_accounts();
        let cred_notice = !plaintext_accounts.is_empty();
        let mut app = Self {
            herald,
            runtime,
            accounts,
            account,
            sync_status: None,
            messages: Vec::new(),
            total: 0,
            unread_total: 0,
            page_size,
            page: 0,
            selected: 0,
            sort: MessageSort::Date,
            descending: true,
            list_mode: ListMode::Grouped,
            search: None,
            search_note: None,
            detail: None,
            detail_scroll: 0,
            raw: None,
            raw_scroll: 0,
            raw_truncated: false,
            raw_search: None,
            raw_width: Cell::new(80),
            raw_height: Cell::new(20),
            raw_rows_total: Cell::new(0),
            mode: Mode::Browse,
            status: StatusMessage::default(),
            focus: Focus::List,
            browser: None,
            attachments: None,
            account_pick: 0,
            quit: false,
            full_redraw: false,
            color,
            plaintext_accounts,
            cred_notice,
        };
        if app.account.is_none() {
            app.status = StatusMessage::error("没有可用账户，请先编辑 herald.toml");
        } else {
            app.reload();
            app.status = StatusMessage::info("就绪");
        }
        app
    }

    pub fn paths(&self) -> &ResolvedPaths {
        &self.herald.paths
    }

    /// Close the plaintext-password advice; it reappears on the next startup.
    pub fn dismiss_cred_notice(&mut self) {
        if self.cred_notice {
            self.cred_notice = false;
            self.full_redraw = true;
        }
    }

    pub fn run(&mut self, terminal: &mut DefaultTerminal) -> anyhow::Result<()> {
        while !self.quit {
            if self.full_redraw {
                terminal.clear()?;
                self.full_redraw = false;
            }
            terminal.draw(|frame| ui::draw(frame, self))?;
            match event::read()? {
                Event::Key(key)
                    if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) =>
                {
                    self.handle_key(key);
                }
                _ => {}
            }
        }
        Ok(())
    }

    pub fn page_count(&self) -> usize {
        page_count_for(self.total, self.page_size)
    }

    pub fn current_message(&self) -> Option<&MessageSummary> {
        self.messages.get(self.selected)
    }

    pub fn reload(&mut self) {
        self.accounts = self.herald.accounts().unwrap_or_default();
        let Some(account) = self.account.clone() else {
            self.messages.clear();
            self.total = 0;
            return;
        };
        self.sync_status = self.herald.status(Some(&account)).ok();
        self.unread_total = self.herald.unread_count(Some(&account)).unwrap_or(0);
        let result = if let Some(search) = self.search.clone() {
            self.herald.search(
                Some(&account),
                &search.query,
                search.kind,
                &SearchOptions {
                    field: search.field,
                    limit: self.page_size,
                    offset: self.page * self.page_size,
                },
            )
        } else {
            let filter = MessageFilter {
                account: Some(account.clone()),
                limit: self.page_size,
                offset: self.page * self.page_size,
                sort: self.sort,
                descending: self.descending,
                ..Default::default()
            };
            self.herald
                .list_messages(Some(&account), &filter)
                .map(|(total, messages)| SearchResult {
                    total,
                    messages,
                    note: None,
                })
        };
        match result {
            Ok(result) => {
                self.total = result.total;
                self.messages = result.messages;
                self.search_note = result.note;
            }
            Err(err) => {
                self.status = StatusMessage::error(err.to_string());
                self.messages.clear();
                self.total = 0;
                self.search_note = None;
            }
        }
        let last_page = self.page_count().saturating_sub(1);
        if self.page > last_page {
            self.page = last_page;
            self.reload();
            return;
        }
        if self.messages.is_empty() {
            self.selected = 0;
        } else {
            self.selected = self.selected.min(self.messages.len() - 1);
        }
        let still_visible = self
            .detail
            .as_ref()
            .map(|detail| {
                self.messages
                    .iter()
                    .any(|message| message.uidl == detail.header.uidl)
            })
            .unwrap_or(true);
        if !still_visible {
            self.detail = None;
        }
        self.preview_selected();
    }

    /// Show the selected message from the local database only. Bodies that
    /// were not cached are fetched explicitly with Enter, so moving the
    /// selection never blocks on the network.
    pub fn preview_selected(&mut self) {
        let Some((uidl, account)) = self
            .current_message()
            .map(|message| (message.uidl.clone(), self.account.clone()))
        else {
            self.detail = None;
            return;
        };
        self.detail = self
            .herald
            .get_stored_message(account.as_deref(), &uidl)
            .ok();
        self.detail_scroll = 0;
    }

    pub fn load_detail(&mut self) {
        let Some((uidl, account)) = self
            .current_message()
            .map(|message| (message.uidl.clone(), self.account.clone()))
        else {
            return;
        };
        match self
            .runtime
            .block_on(self.herald.get_message(account.as_deref(), &uidl, true))
        {
            Ok(detail) => {
                let title = detail
                    .header
                    .subject
                    .clone()
                    .unwrap_or_else(|| "(无主题)".to_string());
                self.detail = Some(detail);
                self.detail_scroll = 0;
                // Explicit open marks the message read; previewing does not.
                if self.apply_read_state(&uidl, true, false) {
                    self.status = StatusMessage::info(format!("已加载：{title}"));
                }
            }
            Err(err) => {
                self.status = StatusMessage::error(format!("加载邮件失败：{err}"));
            }
        }
    }

    pub fn toggle_read(&mut self) {
        let Some(message) = self.current_message() else {
            self.status = StatusMessage::error("没有选中的邮件");
            return;
        };
        let read = !message.is_read;
        let uidl = message.uidl.clone();
        self.apply_read_state(&uidl, read, true);
    }

    fn apply_read_state(&mut self, uidl: &str, read: bool, announce: bool) -> bool {
        let account = self.account.clone();
        match self.herald.set_read(account.as_deref(), uidl, read) {
            Ok(()) => {
                for message in &mut self.messages {
                    if message.uidl == uidl {
                        message.is_read = read;
                    }
                }
                if let Some(detail) = &mut self.detail {
                    if detail.header.uidl == uidl {
                        detail.is_read = read;
                    }
                }
                self.unread_total = self
                    .herald
                    .unread_count(account.as_deref())
                    .unwrap_or(self.unread_total);
                if announce {
                    self.status = StatusMessage::info(format!(
                        "已标记为{}",
                        if read { "已读" } else { "未读" }
                    ));
                }
                true
            }
            Err(err) => {
                self.status = StatusMessage::error(format!("更新已读状态失败：{err}"));
                false
            }
        }
    }

    pub fn load_raw(&mut self) {
        let Some((uidl, account)) = self
            .current_message()
            .map(|message| (message.uidl.clone(), self.account.clone()))
        else {
            return;
        };
        match self
            .runtime
            .block_on(self.herald.raw_message(account.as_deref(), &uidl))
        {
            Ok(bytes) => {
                let attachments = raw_attachments(&bytes);
                let folded = attachments.len();
                let visible = if folded > 0 {
                    fold_attachments(&bytes, &attachments)
                } else {
                    bytes
                };
                let (text, truncated) = prepare_raw(&visible, RAW_VIEW_LIMIT);
                self.raw = Some(text);
                self.raw_scroll = 0;
                self.raw_truncated = truncated;
                self.raw_search = None;
                self.mode = Mode::Raw;
                self.status = StatusMessage::info(raw_status(folded, truncated));
            }
            Err(err) => {
                self.status = StatusMessage::error(format!("获取原始邮件失败：{err}"));
            }
        }
    }

    pub fn sync(&mut self, all: bool) {
        let options = SyncOptions {
            account: self.account.clone(),
            all,
            with_body: false,
            with_attachments: false,
            index: false,
            with_embeddings: self.herald.semantic_enabled(),
            limit: None,
        };
        match self.runtime.block_on(self.herald.sync(options)) {
            Ok(report) => {
                let hint =
                    sync_limit_hint(all, report.fetched, self.herald.config.sync.default_limit);
                self.status = StatusMessage::info(format!(
                    "同步完成：新邮件 {} 封，处理 {} 封，正文缓存 {} 封{}{hint}",
                    report.new_count,
                    report.fetched,
                    report.bodies_cached,
                    if report.errors.is_empty() {
                        String::new()
                    } else {
                        format!("，警告 {} 条", report.errors.len())
                    }
                ));
                self.page = 0;
                self.selected = 0;
                self.detail = None;
                self.reload();
            }
            Err(err) => {
                self.status = StatusMessage::error(format!("同步失败：{err}"));
            }
        }
    }

    pub fn switch_account(&mut self, name: &str) {
        self.account = Some(name.to_string());
        self.page = 0;
        self.selected = 0;
        self.detail = None;
        self.raw = None;
        self.search = None;
        self.focus = Focus::List;
        self.reload();
        self.status = StatusMessage::info(format!("已切换到账户 {name}"));
    }

    pub fn next_page(&mut self) {
        if self.page + 1 < self.page_count() {
            self.page += 1;
            self.selected = 0;
            self.detail = None;
            self.reload();
        }
    }

    pub fn previous_page(&mut self) {
        if self.page > 0 {
            self.page -= 1;
            self.selected = 0;
            self.detail = None;
            self.reload();
        }
    }

    pub fn cycle_sort(&mut self) {
        self.sort = self.sort.next();
        self.page = 0;
        self.selected = 0;
        self.detail = None;
        self.reload();
        self.status = StatusMessage::info(format!("排序：{}", self.sort.label()));
    }

    pub fn reverse_sort(&mut self) {
        self.descending = !self.descending;
        self.page = 0;
        self.selected = 0;
        self.detail = None;
        self.reload();
        self.status = StatusMessage::info(format!(
            "排序：{}{}",
            self.sort.label(),
            if self.descending { "↓" } else { "↑" }
        ));
    }

    pub fn grouped_list(&self) -> bool {
        grouped_list_for(self.list_mode, self.search.is_some(), self.sort)
    }

    pub fn toggle_list_mode(&mut self) {
        self.list_mode = match self.list_mode {
            ListMode::Grouped => ListMode::Compact,
            ListMode::Compact => ListMode::Grouped,
        };
        let auto_compact = self.list_mode == ListMode::Grouped && !self.grouped_list();
        let suffix = if auto_compact {
            "（搜索或非日期排序时自动用紧凑模式）"
        } else {
            ""
        };
        self.status = StatusMessage::info(format!("列表：{}{suffix}", self.list_mode.label()));
    }

    pub fn run_search(&mut self, kind: SearchKind, query: String) {
        self.run_search_with(kind, SearchField::All, query);
    }

    pub fn run_search_with(&mut self, kind: SearchKind, field: SearchField, query: String) {
        let query = query.trim().to_string();
        if query.is_empty() {
            self.search = None;
            self.page = 0;
            self.detail = None;
            self.reload();
            return;
        }
        // Embeddings cover the whole message, so field-scoped search is
        // keyword-only.
        let kind = if field != SearchField::All {
            SearchKind::Keyword
        } else {
            kind
        };
        if kind == SearchKind::Semantic && !self.herald.semantic_enabled() {
            self.status = StatusMessage::error(
                "语义检索未启用：请在配置中开启 semantic.enabled，或运行 herald models download",
            );
            return;
        }
        self.search = Some(SearchState { query, kind, field });
        self.page = 0;
        self.selected = 0;
        self.detail = None;
        self.reload();
        if let Some(note) = self.search_note.clone() {
            self.status = StatusMessage::error(note);
        } else {
            self.status = StatusMessage::info(match self.search.as_ref() {
                Some(search) => format!(
                    "{}搜索（{}）“{}”：{} 条",
                    search.label(),
                    search.field.label(),
                    search.query,
                    self.total
                ),
                None => String::new(),
            });
        }
    }

    pub fn clear_search(&mut self) {
        if self.search.is_some() {
            self.search = None;
            self.search_note = None;
            self.page = 0;
            self.selected = 0;
            self.detail = None;
            self.reload();
            self.status = StatusMessage::info("已清除搜索条件");
        }
    }

    pub fn start_export(&mut self) {
        let Some(detail) = &self.detail else {
            self.status = StatusMessage::error("请先按 Enter 打开邮件详情");
            return;
        };
        if detail.attachments.is_empty() {
            self.status = StatusMessage::error("这封邮件没有附件");
            return;
        }
        let (uidl, metas) = (detail.header.uidl.clone(), detail.attachments.clone());
        if metas.len() == 1 {
            self.open_browser(uidl, &metas[0]);
        } else {
            self.attachments = Some(AttachmentPicker {
                uidl,
                metas,
                selected: 0,
            });
            self.mode = Mode::Attachments;
        }
    }

    pub fn open_browser(&mut self, uidl: String, meta: &AttachmentMeta) {
        let index = meta.index;
        let original = meta
            .filename
            .clone()
            .unwrap_or_else(|| format!("attachment-{}", index + 1));
        let name = sanitize_filename(&original, index);
        let start = self.herald.export_base_dir();
        let mut browser = Browser::new(start, uidl, index, name);
        let history_file = self.herald.paths.config_dir.join("dir_history.json");
        browser.attach_history(DirHistory::load(history_file));
        self.browser = Some(browser);
        self.mode = Mode::Browser;
        self.status = StatusMessage::info(
            "↑↓ 选择 · Enter 进入 · 退格/← 上一级 · z 跳转 · / 筛选 · d 存入当前目录 · f 改名",
        );
    }

    pub fn export_to_dir(&mut self) {
        let Some(browser) = self.browser.as_ref() else {
            return;
        };
        let dest = browser.dir_target();
        let uidl = browser.uidl.clone();
        let index = browser.attachment_index;
        self.finish_export(&uidl, index, &dest);
    }

    pub fn export_to_filename(&mut self) {
        let Some(browser) = self.browser.as_ref() else {
            return;
        };
        let Some(target) = browser.file_target() else {
            if let Some(browser) = self.browser.as_mut() {
                browser.note = Some("文件名无效：不能为空或包含路径分隔符".to_string());
            }
            return;
        };
        if target.exists() {
            if let Some(browser) = self.browser.as_mut() {
                browser.note = Some(format!("目标已存在，不会覆盖：{}", target.display()));
            }
            return;
        }
        let uidl = browser.uidl.clone();
        let index = browser.attachment_index;
        self.finish_export(&uidl, index, &target);
    }

    fn finish_export(&mut self, uidl: &str, index: usize, dest: &Path) {
        let account = self.account.clone();
        match self.runtime.block_on(self.herald.export_attachment(
            account.as_deref(),
            uidl,
            index,
            dest,
        )) {
            Ok(path) => {
                self.status = StatusMessage::info(format!("已导出：{}", path.display()));
                self.browser = None;
                self.mode = Mode::Browse;
            }
            Err(err) => {
                if let Some(browser) = self.browser.as_mut() {
                    browser.note = Some(format!("导出失败：{err}"));
                }
                self.status = StatusMessage::error(format!("导出失败：{err}"));
            }
        }
    }

    pub fn toggle_focus(&mut self) {
        self.focus = match self.focus {
            Focus::List => Focus::Detail,
            Focus::Detail => Focus::List,
        };
        if self.focus == Focus::Detail && self.detail.is_none() {
            self.preview_selected();
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) {
        if key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('c')) {
            self.quit = true;
            return;
        }
        if self.cred_notice {
            self.dismiss_cred_notice();
            return;
        }
        let was_overlay = self.mode.kind() != ModeKind::Browse;
        match self.mode {
            Mode::Browse => self.handle_browse_key(key),
            Mode::Input(_) => self.handle_input_key(key),
            Mode::Accounts => self.handle_accounts_key(key),
            Mode::Attachments => self.handle_attachments_key(key),
            Mode::Browser => self.handle_browser_key(key),
            Mode::Raw => self.handle_raw_key(key),
            Mode::Help => {
                if matches!(
                    key.code,
                    KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('h')
                ) {
                    self.mode = Mode::Browse;
                }
            }
        }
        if was_overlay && matches!(self.mode, Mode::Browse) {
            self.full_redraw = true;
        }
    }

    fn handle_browse_key(&mut self, key: KeyEvent) {
        if self.focus == Focus::Detail && self.detail.is_some() {
            match key.code {
                KeyCode::Down | KeyCode::Char('j') => {
                    self.scroll_detail(1);
                    return;
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    self.scroll_detail(-1);
                    return;
                }
                KeyCode::PageDown => {
                    self.scroll_detail(10);
                    return;
                }
                KeyCode::PageUp => {
                    self.scroll_detail(-10);
                    return;
                }
                KeyCode::Tab => {
                    self.toggle_focus();
                    return;
                }
                KeyCode::Esc => {
                    self.focus = Focus::List;
                    return;
                }
                _ => {}
            }
        }
        match key.code {
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Char('h') => self.mode = Mode::Help,
            KeyCode::Char('a') => {
                if self.accounts.is_empty() {
                    self.status = StatusMessage::error("没有可用账户");
                } else {
                    self.account_pick = self
                        .accounts
                        .iter()
                        .position(|account| Some(&account.name) == self.account.as_ref())
                        .unwrap_or(0);
                    self.mode = Mode::Accounts;
                }
            }
            KeyCode::Char('s') => self.sync(false),
            KeyCode::Char('S') => self.sync(true),
            KeyCode::Char('/') => {
                let kind = self.herald.default_search_kind(SearchField::All);
                self.mode = Mode::Input(InputState {
                    kind: InputKind::Search,
                    value: String::new(),
                    cursor: 0,
                    search_kind: Some(kind),
                    search_field: Some(SearchField::All),
                })
            }
            KeyCode::Char('o') => self.cycle_sort(),
            KeyCode::Char('O') => self.reverse_sort(),
            KeyCode::Char('v') => self.toggle_list_mode(),
            KeyCode::Char(']') => self.next_page(),
            KeyCode::Char('[') => self.previous_page(),
            KeyCode::PageDown => self.next_page(),
            KeyCode::PageUp => self.previous_page(),
            KeyCode::Down | KeyCode::Char('j') if !self.messages.is_empty() => {
                self.selected = (self.selected + 1).min(self.messages.len() - 1);
                self.preview_selected();
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.selected = self.selected.saturating_sub(1);
                self.preview_selected();
            }
            KeyCode::Char('g') => {
                self.selected = 0;
                self.preview_selected();
            }
            KeyCode::Char('G') => {
                self.selected = self.messages.len().saturating_sub(1);
                self.preview_selected();
            }
            KeyCode::Enter => {
                self.load_detail();
                if self.detail.is_some() {
                    self.focus = Focus::Detail;
                }
            }
            KeyCode::Tab => self.toggle_focus(),
            KeyCode::Char('r') => self.load_raw(),
            KeyCode::Char('e') => self.start_export(),
            KeyCode::Char('u') => self.toggle_read(),
            KeyCode::Esc => {
                if self.search.is_some() {
                    self.clear_search();
                } else {
                    self.focus = Focus::List;
                    self.preview_selected();
                }
            }
            _ => {}
        }
    }

    fn handle_input_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                let kind = match &self.mode {
                    Mode::Input(state) => state.kind,
                    _ => return,
                };
                self.mode = if kind == InputKind::NewDir {
                    Mode::Browser
                } else {
                    Mode::Browse
                };
            }
            KeyCode::Enter => {
                let (kind, value) = match &self.mode {
                    Mode::Input(state) => (state.kind, state.value.trim().to_string()),
                    _ => return,
                };
                match kind {
                    InputKind::Search => {
                        let (kind, field) = match &self.mode {
                            Mode::Input(state) => (
                                state.search_kind.unwrap_or(SearchKind::Keyword),
                                state.search_field.unwrap_or(SearchField::All),
                            ),
                            _ => (SearchKind::Keyword, SearchField::All),
                        };
                        self.run_search_with(kind, field, value);
                    }
                    InputKind::NewDir => self.submit_new_dir(value),
                }
            }
            KeyCode::Tab => {
                if let Mode::Input(state) = &mut self.mode {
                    if let Some(kind) = state.search_kind {
                        state.search_kind = Some(next_search_kind(kind));
                    }
                }
            }
            KeyCode::Char('f') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Mode::Input(state) = &mut self.mode {
                    if let Some(field) = state.search_field {
                        let field = field.next();
                        state.search_field = Some(field);
                        if field != SearchField::All {
                            state.search_kind = Some(SearchKind::Keyword);
                        }
                    }
                }
            }
            KeyCode::Left => {
                if let Mode::Input(state) = &mut self.mode {
                    state.cursor = text_input::prev_cursor(&state.value, state.cursor);
                }
            }
            KeyCode::Right => {
                if let Mode::Input(state) = &mut self.mode {
                    state.cursor = text_input::next_cursor(&state.value, state.cursor);
                }
            }
            KeyCode::Home => {
                if let Mode::Input(state) = &mut self.mode {
                    state.cursor = 0;
                }
            }
            KeyCode::End => {
                if let Mode::Input(state) = &mut self.mode {
                    state.cursor = state.value.len();
                }
            }
            KeyCode::Backspace => {
                if let Mode::Input(state) = &mut self.mode {
                    text_input::backspace(&mut state.value, &mut state.cursor);
                }
            }
            KeyCode::Delete => {
                if let Mode::Input(state) = &mut self.mode {
                    text_input::delete(&mut state.value, &mut state.cursor);
                }
            }
            KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Mode::Input(state) = &mut self.mode {
                    text_input::insert_char(&mut state.value, &mut state.cursor, ch);
                }
            }
            _ => {}
        }
    }

    fn submit_new_dir(&mut self, name: String) {
        let Some(browser) = self.browser.as_mut() else {
            self.mode = Mode::Browse;
            return;
        };
        match browser.create_dir(&name) {
            Ok(()) => {
                self.mode = Mode::Browser;
                self.status = StatusMessage::info(format!("已创建目录：{name}"));
            }
            Err(err) => {
                browser.note = Some(format!("新建目录失败：{err}"));
                self.mode = Mode::Browser;
            }
        }
    }

    fn handle_accounts_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.mode = Mode::Browse,
            KeyCode::Down | KeyCode::Char('j') if !self.accounts.is_empty() => {
                self.account_pick = (self.account_pick + 1).min(self.accounts.len() - 1);
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.account_pick = self.account_pick.saturating_sub(1);
            }
            KeyCode::Enter => {
                if let Some(account) = self.accounts.get(self.account_pick) {
                    let name = account.name.clone();
                    self.switch_account(&name);
                }
                self.mode = Mode::Browse;
            }
            _ => {}
        }
    }

    fn handle_attachments_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.attachments = None;
                self.mode = Mode::Browse;
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if let Some(picker) = self.attachments.as_mut() {
                    if !picker.metas.is_empty() {
                        picker.selected = (picker.selected + 1).min(picker.metas.len() - 1);
                    }
                }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if let Some(picker) = self.attachments.as_mut() {
                    picker.selected = picker.selected.saturating_sub(1);
                }
            }
            KeyCode::Enter => {
                if let Some(picker) = self.attachments.take() {
                    if let Some(meta) = picker.metas.get(picker.selected) {
                        self.open_browser(picker.uidl, meta);
                    } else {
                        self.mode = Mode::Browse;
                    }
                }
            }
            _ => {}
        }
    }

    fn handle_browser_key(&mut self, key: KeyEvent) {
        let Some(browser) = self.browser.as_mut() else {
            self.mode = Mode::Browse;
            return;
        };
        if browser.jump.is_some() {
            match key.code {
                KeyCode::Esc => browser.cancel_jump(),
                KeyCode::Enter => {
                    if let Some(path) = browser.jump_to_selected() {
                        self.status = StatusMessage::info(format!("已跳转：{}", path.display()));
                    } else {
                        self.status = StatusMessage::error("没有匹配的目录");
                    }
                }
                KeyCode::Down | KeyCode::Tab => browser.move_jump(1),
                KeyCode::Up => browser.move_jump(-1),
                KeyCode::PageDown => browser.move_jump(9),
                KeyCode::PageUp => browser.move_jump(-9),
                KeyCode::Left => {
                    if let Some(jump) = browser.jump.as_mut() {
                        jump.cursor = text_input::prev_cursor(&jump.query, jump.cursor);
                    }
                }
                KeyCode::Right => {
                    if let Some(jump) = browser.jump.as_mut() {
                        jump.cursor = text_input::next_cursor(&jump.query, jump.cursor);
                    }
                }
                KeyCode::Home => {
                    if let Some(jump) = browser.jump.as_mut() {
                        jump.cursor = 0;
                    }
                }
                KeyCode::End => {
                    if let Some(jump) = browser.jump.as_mut() {
                        jump.cursor = jump.query.len();
                    }
                }
                KeyCode::Backspace => {
                    if let Some(jump) = browser.jump.as_mut() {
                        text_input::backspace(&mut jump.query, &mut jump.cursor);
                    }
                    browser.update_jump();
                }
                KeyCode::Delete => {
                    if let Some(jump) = browser.jump.as_mut() {
                        text_input::delete(&mut jump.query, &mut jump.cursor);
                    }
                    browser.update_jump();
                }
                KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    if let Some(jump) = browser.jump.as_mut() {
                        text_input::insert_char(&mut jump.query, &mut jump.cursor, ch);
                    }
                    browser.update_jump();
                }
                _ => {}
            }
            return;
        }
        if browser.filtering {
            match key.code {
                KeyCode::Esc => {
                    browser.clear_filter();
                    browser.apply_filter();
                }
                KeyCode::Enter => {
                    if let Some(target) = browser.filter_jump_target() {
                        browser.jump_to_path(target);
                    } else {
                        browser.filtering = false;
                    }
                }
                KeyCode::Down => browser.move_by(1),
                KeyCode::Up => browser.move_by(-1),
                KeyCode::PageDown => browser.move_by(10),
                KeyCode::PageUp => browser.move_by(-10),
                KeyCode::Left => {
                    browser.filter_cursor =
                        text_input::prev_cursor(&browser.filter, browser.filter_cursor);
                }
                KeyCode::Right => {
                    browser.filter_cursor =
                        text_input::next_cursor(&browser.filter, browser.filter_cursor);
                }
                KeyCode::Home => browser.filter_cursor = 0,
                KeyCode::End => browser.filter_cursor = browser.filter.len(),
                KeyCode::Backspace => {
                    text_input::backspace(&mut browser.filter, &mut browser.filter_cursor);
                    browser.apply_filter();
                }
                KeyCode::Delete => {
                    text_input::delete(&mut browser.filter, &mut browser.filter_cursor);
                    browser.apply_filter();
                }
                KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    text_input::insert_char(&mut browser.filter, &mut browser.filter_cursor, ch);
                    browser.apply_filter();
                }
                _ => {}
            }
            return;
        }
        if browser.filename_focus {
            match key.code {
                KeyCode::Enter => self.export_to_filename(),
                KeyCode::Esc | KeyCode::Tab => browser.unfocus_filename(),
                KeyCode::Left => {
                    browser.filename_cursor =
                        text_input::prev_cursor(&browser.filename, browser.filename_cursor);
                }
                KeyCode::Right => {
                    browser.filename_cursor =
                        text_input::next_cursor(&browser.filename, browser.filename_cursor);
                }
                KeyCode::Home => browser.filename_cursor = 0,
                KeyCode::End => browser.filename_cursor = browser.filename.len(),
                KeyCode::Backspace => {
                    text_input::backspace(&mut browser.filename, &mut browser.filename_cursor)
                }
                KeyCode::Delete => {
                    text_input::delete(&mut browser.filename, &mut browser.filename_cursor);
                }
                KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    text_input::insert_char(
                        &mut browser.filename,
                        &mut browser.filename_cursor,
                        ch,
                    );
                }
                _ => {}
            }
            return;
        }
        match key.code {
            KeyCode::Esc => {
                if browser.filter.is_empty() {
                    self.browser = None;
                    self.mode = Mode::Browse;
                } else {
                    browser.clear_filter();
                    browser.apply_filter();
                }
            }
            KeyCode::Char('q') => {
                self.browser = None;
                self.mode = Mode::Browse;
            }
            KeyCode::Char('/') => {
                browser.filtering = true;
                browser.filter_cursor = browser.filter.len();
            }
            KeyCode::Char('z') => browser.open_jump(),
            KeyCode::Down | KeyCode::Char('j') => browser.move_by(1),
            KeyCode::Up | KeyCode::Char('k') => browser.move_by(-1),
            KeyCode::PageDown => browser.move_by(10),
            KeyCode::PageUp => browser.move_by(-10),
            KeyCode::Backspace | KeyCode::Left | KeyCode::Char('h') => {
                if !browser.go_up() {
                    browser.note = Some("已到根目录；如有其他盘符，可在列表中选择".to_string());
                }
            }
            KeyCode::Enter => browser.enter(),
            KeyCode::Char('f') | KeyCode::Tab => browser.focus_filename(),
            KeyCode::Char('d') => self.export_to_dir(),
            KeyCode::Char('n') => {
                self.mode = Mode::Input(InputState {
                    kind: InputKind::NewDir,
                    value: String::new(),
                    cursor: 0,
                    search_kind: None,
                    search_field: None,
                })
            }
            _ => {}
        }
    }

    fn handle_raw_key(&mut self, key: KeyEvent) {
        if self
            .raw_search
            .as_ref()
            .is_some_and(|search| search.editing)
        {
            self.handle_raw_search_key(key);
            return;
        }
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.mode = Mode::Browse;
                self.raw = None;
                self.raw_search = None;
                self.raw_truncated = false;
            }
            KeyCode::Char('/') => self.open_raw_search(),
            KeyCode::Char('n') => self.step_raw_match(true),
            KeyCode::Char('N') => self.step_raw_match(false),
            KeyCode::Down | KeyCode::Char('j') => self.scroll_raw_by(1),
            KeyCode::Up | KeyCode::Char('k') => self.scroll_raw_by(-1),
            KeyCode::PageDown | KeyCode::Char(' ') | KeyCode::Char('f') => {
                self.scroll_raw_by(self.raw_page() as i32)
            }
            KeyCode::PageUp | KeyCode::Char('b') => self.scroll_raw_by(-(self.raw_page() as i32)),
            KeyCode::Char('d') => self.scroll_raw_by(self.half_raw_page()),
            KeyCode::Char('u') => self.scroll_raw_by(-self.half_raw_page()),
            KeyCode::Char('g') => self.raw_scroll = 0,
            KeyCode::Char('G') => self.raw_scroll = self.raw_max_scroll(),
            _ => {}
        }
    }

    fn raw_page(&self) -> u16 {
        self.raw_height.get().max(1)
    }

    fn half_raw_page(&self) -> i32 {
        (self.raw_page() / 2).max(1) as i32
    }

    fn raw_max_scroll(&self) -> u16 {
        self.raw_rows_total
            .get()
            .saturating_sub(self.raw_page() as usize)
            .min(u16::MAX as usize) as u16
    }

    fn scroll_raw_by(&mut self, delta: i32) {
        let next = if delta >= 0 {
            self.raw_scroll.saturating_add(delta as u16)
        } else {
            self.raw_scroll.saturating_sub((-delta) as u16)
        };
        self.raw_scroll = next.min(self.raw_max_scroll());
    }

    fn handle_raw_search_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                if let Some(search) = &mut self.raw_search {
                    search.editing = false;
                    self.status = if search.matches.is_empty() {
                        StatusMessage::info("查找已关闭")
                    } else {
                        StatusMessage::info(format!(
                            "查找：{} · {}/{}（n/N 跳转，/ 重新查找）",
                            search.query,
                            search.current + 1,
                            search.matches.len()
                        ))
                    };
                }
            }
            KeyCode::Enter => self.step_raw_match(true),
            KeyCode::Left => {
                if let Some(search) = &mut self.raw_search {
                    search.cursor = text_input::prev_cursor(&search.query, search.cursor);
                }
            }
            KeyCode::Right => {
                if let Some(search) = &mut self.raw_search {
                    search.cursor = text_input::next_cursor(&search.query, search.cursor);
                }
            }
            KeyCode::Home => {
                if let Some(search) = &mut self.raw_search {
                    search.cursor = 0;
                }
            }
            KeyCode::End => {
                if let Some(search) = &mut self.raw_search {
                    search.cursor = search.query.len();
                }
            }
            KeyCode::Backspace => {
                if let Some(search) = &mut self.raw_search {
                    text_input::backspace(&mut search.query, &mut search.cursor);
                }
                self.refresh_raw_search(true);
            }
            KeyCode::Delete => {
                if let Some(search) = &mut self.raw_search {
                    text_input::delete(&mut search.query, &mut search.cursor);
                }
                self.refresh_raw_search(true);
            }
            KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(search) = &mut self.raw_search {
                    text_input::insert_char(&mut search.query, &mut search.cursor, ch);
                }
                self.refresh_raw_search(true);
            }
            _ => {}
        }
    }

    fn open_raw_search(&mut self) {
        if self.raw.is_none() {
            return;
        }
        self.raw_search = Some(RawSearch {
            query: String::new(),
            cursor: 0,
            matches: Vec::new(),
            current: 0,
            editing: true,
        });
        self.status = StatusMessage::info("查找原文：输入关键字，Enter 下一个，Esc 完成");
    }

    fn refresh_raw_search(&mut self, jump: bool) {
        let Some(raw) = self.raw.as_deref() else {
            return;
        };
        let Some(search) = self.raw_search.as_mut() else {
            return;
        };
        search.matches = raw_matches(raw, &search.query);
        search.current = 0;
        let query = search.query.clone();
        if query.is_empty() {
            self.status = StatusMessage::info("查找原文：输入关键字，Enter 下一个，Esc 完成");
            return;
        }
        if search.matches.is_empty() {
            self.status = StatusMessage::error(format!("未找到：{query}"));
            return;
        }
        let total = search.matches.len();
        let first = if jump {
            Some(search.matches[0].clone())
        } else {
            None
        };
        if let Some(first) = first {
            self.scroll_to_raw_match(&first);
        }
        self.status =
            StatusMessage::info(format!("匹配 {total} 处：{query} · Enter 下一个，Esc 完成"));
    }

    fn step_raw_match(&mut self, forward: bool) {
        let Some(search) = self.raw_search.as_ref() else {
            return;
        };
        if search.matches.is_empty() {
            self.status = StatusMessage::error(if search.query.is_empty() {
                "请先输入要查找的关键字"
            } else {
                "没有可跳转的匹配"
            });
            return;
        }
        let len = search.matches.len();
        let current = if forward {
            (search.current + 1) % len
        } else {
            (search.current + len - 1) % len
        };
        let matched = search.matches[current].clone();
        let query = search.query.clone();
        if let Some(search) = self.raw_search.as_mut() {
            search.current = current;
        }
        self.scroll_to_raw_match(&matched);
        self.status = StatusMessage::info(format!("匹配 {}/{}：{query}", current + 1, len));
    }

    fn scroll_to_raw_match(&mut self, matched: &RawMatch) {
        let Some(raw) = self.raw.as_deref() else {
            return;
        };
        let row = raw_match_row(raw, matched, self.raw_width.get().max(1));
        self.raw_scroll = row.min(self.raw_max_scroll());
    }

    pub fn scroll_detail(&mut self, delta: i16) {
        if delta >= 0 {
            self.detail_scroll = self.detail_scroll.saturating_add(delta as u16);
        } else {
            self.detail_scroll = self.detail_scroll.saturating_sub((-delta) as u16);
        }
    }

    pub fn semantic_model_present(&self) -> bool {
        self.herald.semantic_model_present()
    }

    pub fn status_text(&self) -> String {
        self.status.text.clone()
    }
}

fn pick_account(
    config: &Config,
    accounts: &[AccountSummary],
    requested: Option<String>,
) -> Option<String> {
    if let Some(name) = requested {
        if accounts.iter().any(|account| account.name == name) {
            return Some(name);
        }
    }
    if let Some(name) = &config.default_account {
        if accounts.iter().any(|account| account.name == *name) {
            return Some(name.clone());
        }
    }
    accounts.first().map(|account| account.name.clone())
}

fn color_enabled() -> bool {
    if std::env::var_os("NO_COLOR").is_some() {
        return false;
    }
    !matches!(
        std::env::var("TERM").ok().as_deref(),
        Some("dumb") | Some("unknown")
    )
}

pub fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} {}", UNITS[0])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

pub fn shorten_date(date: Option<&str>) -> String {
    herald_core::time::china_list_date(date)
}

/// Long address lists would otherwise push the body far down the detail pane;
/// the raw view is still available for the complete headers.
pub const DETAIL_ADDR_LIMIT: usize = 12;
/// Keep per-frame rendering and scrolling responsive for very large bodies.
pub const DETAIL_BODY_LIMIT: usize = 32 * 1024;

pub fn sender_short(from: Option<&str>) -> String {
    let Some(from) = from else {
        return "-".to_string();
    };
    let name = from
        .split('<')
        .next()
        .unwrap_or(from)
        .trim()
        .trim_matches('"');
    if name.is_empty() {
        from.to_string()
    } else {
        name.to_string()
    }
}

pub fn join_addrs(addrs: &[String]) -> String {
    if addrs.is_empty() {
        return "-".to_string();
    }
    if addrs.len() <= DETAIL_ADDR_LIMIT {
        return addrs.join(", ");
    }
    format!(
        "{} …（共 {} 个）",
        addrs[..DETAIL_ADDR_LIMIT].join(", "),
        addrs.len()
    )
}

pub fn body_text(detail: &MessageDetail) -> String {
    if !detail.body_cached {
        return "（正文未缓存，按 Enter 从服务器加载）".to_string();
    }
    let text = match (&detail.body_text, &detail.body_html) {
        (Some(text), _) if !text.trim().is_empty() => text.clone(),
        (_, Some(html)) if !html.trim().is_empty() => format!("[HTML 正文]\n{html}"),
        _ => return "(没有可显示的正文)".to_string(),
    };
    truncate_body(text)
}

fn truncate_body(text: String) -> String {
    if text.len() <= DETAIL_BODY_LIMIT {
        return text;
    }
    let mut end = DETAIL_BODY_LIMIT;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "[正文过长，仅显示前 {} KB；按 r 查看原始邮件（最多 256KB）]\n\n{}",
        DETAIL_BODY_LIMIT / 1024,
        &text[..end]
    )
}

pub fn attachment_line(meta: &AttachmentMeta) -> String {
    format!(
        "[{}] {} · {} · {}{}",
        meta.index,
        meta.filename.as_deref().unwrap_or("(未命名)"),
        meta.mime_type
            .as_deref()
            .unwrap_or("application/octet-stream"),
        format_size(meta.size),
        if meta.has_content {
            " · 已缓存"
        } else {
            ""
        }
    )
}

fn next_search_kind(kind: SearchKind) -> SearchKind {
    match kind {
        SearchKind::Keyword => SearchKind::Hybrid,
        SearchKind::Hybrid => SearchKind::Semantic,
        SearchKind::Semantic => SearchKind::Keyword,
    }
}

/// Find non-overlapping keyword matches line by line. ASCII queries ignore
/// case so header names and URLs are easy to find.
pub fn raw_matches(text: &str, query: &str) -> Vec<RawMatch> {
    if query.is_empty() {
        return Vec::new();
    }
    let mut matches = Vec::new();
    for (line, content) in text.lines().enumerate() {
        let mut from = 0;
        while from <= content.len() {
            let Some(start) = find_from(content, from, query) else {
                break;
            };
            let end = start + query.len();
            matches.push(RawMatch { line, start, end });
            from = end;
        }
    }
    matches
}

fn find_from(line: &str, from: usize, query: &str) -> Option<usize> {
    let haystack = line.get(from..)?;
    if query.is_ascii() {
        haystack
            .as_bytes()
            .windows(query.len())
            .position(|window| window.eq_ignore_ascii_case(query.as_bytes()))
            .map(|offset| from + offset)
    } else {
        haystack.find(query).map(|offset| from + offset)
    }
}

/// Wrap the normalized raw text into terminal rows. Breaks at spaces and
/// between CJK characters, so Chinese paragraphs fill each row instead of
/// pushing a whole run to the next line; overlong ASCII tokens hard-wrap.
pub fn raw_rows(text: &str, width: u16) -> Vec<RawRow> {
    let width = width.max(1);
    let mut rows = Vec::new();
    for (line, content) in text.lines().enumerate() {
        if content.is_empty() {
            rows.push(RawRow {
                line,
                start: 0,
                end: 0,
            });
            continue;
        }
        for (start, end) in line_ranges(content, width) {
            rows.push(RawRow { line, start, end });
        }
    }
    rows
}

/// Split one logical line into display rows that fit `width` columns.
pub fn wrap_line(text: &str, width: u16) -> Vec<String> {
    let width = width.max(1);
    if text.is_empty() {
        return vec![String::new()];
    }
    line_ranges(text, width)
        .into_iter()
        .map(|(start, end)| text[start..end].to_string())
        .collect()
}

fn line_ranges(content: &str, width: u16) -> Vec<(usize, usize)> {
    let width = width.max(1) as usize;
    let mut ranges = Vec::new();
    let mut start = 0;
    while start < content.len() {
        let mut end = start;
        let mut row_width = 0usize;
        let mut last_break = None;
        let mut overflowed = false;
        for (offset, ch) in content[start..].char_indices() {
            let index = start + offset;
            let ch_width = ch.width().unwrap_or(0);
            if row_width + ch_width > width && row_width > 0 {
                overflowed = true;
                break;
            }
            row_width += ch_width;
            end = index + ch.len_utf8();
            let next = content[end..].chars().next();
            if break_allowed(ch, next) {
                last_break = Some(end);
            }
        }
        let row_end = if overflowed {
            last_break.unwrap_or(end)
        } else {
            end
        };
        let row_end = if row_end > start {
            row_end
        } else {
            content[start..]
                .chars()
                .next()
                .map(|ch| start + ch.len_utf8())
                .unwrap_or(content.len())
        };
        ranges.push((start, row_end));
        start = row_end;
    }
    ranges
}

fn break_allowed(after: char, next: Option<char>) -> bool {
    after.is_whitespace() || is_wide_script(after) || next.is_some_and(is_wide_script)
}

fn is_wide_script(ch: char) -> bool {
    matches!(
        ch as u32,
        0x1100..=0x11FF
            | 0x2E80..=0x2FFF
            | 0x3000..=0x303F
            | 0x3040..=0x30FF
            | 0x3100..=0x31FF
            | 0x3400..=0x4DBF
            | 0x4E00..=0x9FFF
            | 0xA960..=0xA97F
            | 0xAC00..=0xD7FF
            | 0xF900..=0xFAFF
            | 0xFE30..=0xFE4F
            | 0xFF00..=0xFFEF
            | 0x1B000..=0x1B16F
            | 0x20000..=0x3FFFF
    )
}

/// Rendered row of a match, used to scroll it into view.
pub fn raw_match_row(text: &str, matched: &RawMatch, width: u16) -> u16 {
    raw_rows(text, width)
        .iter()
        .position(|row| row.line == matched.line && matched.start < row.end)
        .unwrap_or(0)
        .min(u16::MAX as usize) as u16
}

fn fold_attachments(raw: &[u8], attachments: &[RawAttachment]) -> Vec<u8> {
    let mut spans: Vec<(usize, &RawAttachment)> = attachments
        .iter()
        .enumerate()
        .filter(|(_, attachment)| {
            attachment.body_start < attachment.end && attachment.end <= raw.len()
        })
        .collect();
    spans.sort_by_key(|(_, attachment)| attachment.body_start);

    let mut output = Vec::with_capacity(raw.len());
    let mut cursor = 0;
    for (ordinal, attachment) in spans {
        if attachment.body_start < cursor {
            continue;
        }
        output.extend_from_slice(&raw[cursor..attachment.body_start]);
        output.extend_from_slice(attachment_placeholder(ordinal, attachment).as_bytes());
        cursor = attachment.end;
    }
    output.extend_from_slice(&raw[cursor..]);
    output
}

fn attachment_placeholder(ordinal: usize, attachment: &RawAttachment) -> String {
    let label = if attachment.is_inline {
        "内嵌资源"
    } else {
        "附件"
    };
    let raw_name = attachment.filename.as_deref().unwrap_or("未命名");
    let name: String = raw_name
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect();
    let name = if name.trim().is_empty() {
        "未命名".to_string()
    } else {
        name
    };
    let kind = attachment.mime_type.as_deref().unwrap_or("未知类型");
    let size = format_size(attachment.size);
    let action = if attachment.is_inline {
        ""
    } else {
        "，按 e 导出"
    };
    format!(
        "[{label} {}] {name} · {kind} · {size}{action}\n",
        ordinal + 1
    )
}

fn raw_status(folded: usize, truncated: bool) -> String {
    match (folded, truncated) {
        (0, false) => "原始邮件：↑/↓ 滚动，Esc 返回".to_string(),
        (0, true) => "原始邮件：超过 256KB，已截断 · Esc 返回".to_string(),
        (count, false) => format!("原始邮件：{count} 个附件已折叠 · ↑/↓ 滚动，Esc 返回"),
        (count, true) => {
            format!("原始邮件：{count} 个附件已折叠，仍超过 256KB 已截断 · Esc 返回")
        }
    }
}

fn prepare_raw(bytes: &[u8], limit: usize) -> (String, bool) {
    let truncated = bytes.len() > limit;
    let bytes = if truncated { &bytes[..limit] } else { bytes };
    let text = String::from_utf8_lossy(bytes);
    let mut output = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\r' => {
                if chars.peek() != Some(&'\n') {
                    output.push('\n');
                }
            }
            '\t' => output.push_str("    "),
            ch if ch.is_control() && ch != '\n' => output.push(' '),
            ch => output.push(ch),
        }
    }
    if truncated {
        output.push_str("\n\n[原始邮件过长，已截断]");
    }
    (output, truncated)
}

fn page_count_for(total: usize, page_size: usize) -> usize {
    if total == 0 {
        1
    } else {
        total.div_ceil(page_size.max(1))
    }
}

fn sync_limit_hint(all: bool, fetched: usize, limit: usize) -> String {
    if !all && limit > 0 && fetched >= limit {
        format!("（已达单次上限 {limit} 封，按 S 同步全部新邮件）")
    } else {
        String::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_sizes_and_dates() {
        assert_eq!(format_size(900), "900 B");
        assert_eq!(format_size(2048), "2.0 KB");
        assert_eq!(shorten_date(None), "-");
        assert_eq!(shorten_date(Some("not a date")), "not a date");
        assert_eq!(sender_short(Some("张三 <a@b.c>")), "张三");
    }

    #[test]
    fn long_address_lists_are_summarized() {
        let addrs: Vec<String> = (0..20)
            .map(|index| format!("user{index}@example.com"))
            .collect();
        let joined = join_addrs(&addrs);
        assert!(joined.contains("user11@example.com"), "{joined}");
        assert!(!joined.contains("user12@example.com"), "{joined}");
        assert!(joined.contains("共 20 个"), "{joined}");
        assert_eq!(
            join_addrs(&addrs[..2]),
            "user0@example.com, user1@example.com"
        );
    }

    #[test]
    fn long_bodies_are_truncated_with_a_hint() {
        let detail = MessageDetail {
            header: herald_core::model::MessageHeader {
                uidl: "u1".to_string(),
                message_id: None,
                from: None,
                to: vec![],
                cc: vec![],
                subject: Some("s".to_string()),
                date: None,
                size: 1,
            },
            body_text: Some("正文".repeat(DETAIL_BODY_LIMIT)),
            body_html: None,
            body_cached: true,
            attachments: vec![],
            is_read: false,
        };
        let shown = body_text(&detail);
        assert!(
            shown.contains("正文过长"),
            "{}",
            &shown[shown.len() - 120..]
        );
        assert!(shown.len() < DETAIL_BODY_LIMIT + 200);
    }

    #[test]
    fn pages_cover_remainders() {
        assert_eq!(page_count_for(41, 20), 3);
        assert_eq!(page_count_for(40, 20), 2);
        assert_eq!(page_count_for(0, 20), 1);
        assert_eq!(page_count_for(5, 3), 2);
    }

    #[test]
    fn uncached_body_asks_for_enter_instead_of_showing_empty_text() {
        let detail = MessageDetail {
            header: herald_core::model::MessageHeader {
                uidl: "u1".to_string(),
                message_id: None,
                from: None,
                to: vec![],
                cc: vec![],
                subject: Some("s".to_string()),
                date: None,
                size: 1,
            },
            body_text: None,
            body_html: None,
            body_cached: false,
            attachments: vec![],
            is_read: false,
        };
        assert!(body_text(&detail).contains("按 Enter"));
    }

    #[test]
    fn raw_view_normalizes_control_characters_and_truncates() {
        let (text, truncated) = prepare_raw(b"a\r\nb\tc\x01d", 1024);
        assert_eq!(text, "a\nb    c d");
        assert!(!truncated);

        let (text, truncated) = prepare_raw(b"abcdef", 3);
        assert!(truncated);
        assert!(text.starts_with("abc"));
        assert!(text.contains("已截断"));
    }

    #[test]
    fn raw_matches_are_line_scoped_and_case_insensitive_for_ascii() {
        let text = "会议纪要\n下周会议安排\nSubject: Weekly HELLO\nhello again";
        let found = raw_matches(text, "会议");
        assert_eq!(found.len(), 2);
        assert_eq!(
            found[0],
            RawMatch {
                line: 0,
                start: 0,
                end: 6
            }
        );
        assert_eq!(
            found[1],
            RawMatch {
                line: 1,
                start: 6,
                end: 12
            }
        );

        let hello = raw_matches(text, "hello");
        assert_eq!(hello.len(), 2);
        assert_eq!((hello[0].line, hello[0].start), (2, 16));
        assert_eq!((hello[1].line, hello[1].start), (3, 0));

        assert!(raw_matches(text, "").is_empty());
        assert!(raw_matches(text, "不存在").is_empty());
    }

    #[test]
    fn raw_match_row_accounts_for_wrapping_and_earlier_lines() {
        let long = "A".repeat(100);
        let text = format!("header\n{long}\ntail");
        let on_long = RawMatch {
            line: 1,
            start: 90,
            end: 93,
        };
        // 100 columns at width 40 leave the 91st character on row 2.
        assert_eq!(raw_match_row(&text, &on_long, 40), 3);
        let first_line = RawMatch {
            line: 0,
            start: 90,
            end: 93,
        };
        assert_eq!(raw_match_row(&long, &first_line, 40), 2);
        assert_eq!(raw_match_row(&long, &first_line, 100), 0);
        assert_eq!(
            raw_match_row(
                &long,
                &RawMatch {
                    line: 0,
                    start: 40,
                    end: 43
                },
                40
            ),
            1
        );

        let words = "aaaa bbbb cccc";
        let at = words.find("cccc").unwrap();
        let found = raw_matches(words, "cccc");
        assert_eq!(found[0].start, at);
        assert_eq!(raw_match_row(words, &found[0], 8), 2);
    }

    #[test]
    fn raw_rows_fill_cjk_lines_without_orphaning_labels() {
        let text = format!("第 01 段：{}", "中文长文本测试。".repeat(20));
        let rows = raw_rows(&text, 40);
        assert!(rows.len() > 1);
        let first = &text[rows[0].start..rows[0].end];
        assert!(first.contains("第 01 段："), "{first}");
        for row in &rows {
            let piece = &text[row.start..row.end];
            let width: usize = piece.chars().map(|ch| ch.width().unwrap_or(0)).sum();
            assert!(width <= 40, "row wider than 40 columns: {width} {piece:?}");
        }
        let joined: String = rows.iter().map(|row| &text[row.start..row.end]).collect();
        assert_eq!(joined, text);

        let at = text.find("测试").unwrap();
        let found = RawMatch {
            line: 0,
            start: at,
            end: at + "测试".len(),
        };
        let row = raw_match_row(&text, &found, 40) as usize;
        let shown = &text[rows[row].start..rows[row].end];
        assert!(shown.contains("测试"), "{shown}");
    }

    #[test]
    fn raw_rows_keep_short_header_lines_intact() {
        let text = "Date: Sat, 14 Sep 2024 10:30:00 +0800\n第二行";
        let rows = raw_rows(text, 78);
        assert_eq!(rows.len(), 2);
        assert_eq!(
            &text[rows[0].start..rows[0].end],
            "Date: Sat, 14 Sep 2024 10:30:00 +0800"
        );
    }

    #[test]
    fn raw_rows_hard_wrap_unbroken_ascii() {
        let text = "A".repeat(100);
        let rows = raw_rows(&text, 40);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].end - rows[0].start, 40);
        assert_eq!(rows[1].end - rows[1].start, 40);
        assert_eq!(rows[2].end - rows[2].start, 20);
    }

    #[test]
    fn wrap_line_keeps_cjk_labels_together() {
        let text = format!("第 01 段：{}", "中文长文本测试。".repeat(10));
        let pieces = wrap_line(&text, 40);
        assert!(pieces.len() > 1);
        assert!(pieces[0].contains("第 01 段："), "{}", pieces[0]);
        assert_eq!(pieces.concat(), text);
        assert_eq!(wrap_line("", 20), vec![String::new()]);
    }

    #[test]
    fn sync_hint_only_appears_when_the_limit_was_hit() {
        assert!(sync_limit_hint(true, 50, 50).is_empty());
        assert!(sync_limit_hint(false, 49, 50).is_empty());
        let hint = sync_limit_hint(false, 50, 50);
        assert!(hint.contains("50"), "{hint}");
        assert!(hint.contains('S'), "{hint}");
    }

    #[test]
    fn grouped_list_falls_back_to_compact_for_search_and_other_sorts() {
        assert!(grouped_list_for(
            ListMode::Grouped,
            false,
            MessageSort::Date
        ));
        assert!(!grouped_list_for(
            ListMode::Grouped,
            true,
            MessageSort::Date
        ));
        assert!(!grouped_list_for(
            ListMode::Grouped,
            false,
            MessageSort::From
        ));
        assert!(!grouped_list_for(
            ListMode::Compact,
            false,
            MessageSort::Date
        ));
    }

    fn multipart_with_attachment(body: &str) -> Vec<u8> {
        format!(
            "From: a@example.com\r\nSubject: report\r\nMIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=\"b\"\r\n\r\n--b\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nhello\r\n--b\r\nContent-Type: application/pdf; name=\"report.pdf\"\r\nContent-Disposition: attachment; filename=\"report.pdf\"\r\nContent-Transfer-Encoding: base64\r\n\r\n{body}\r\n--b--\r\n"
        )
        .into_bytes()
    }

    #[test]
    fn raw_view_folds_attachment_bodies_into_a_list() {
        let raw = multipart_with_attachment(&"A".repeat(4000));
        let attachments = raw_attachments(&raw);
        assert_eq!(attachments.len(), 1);
        let folded = String::from_utf8(fold_attachments(&raw, &attachments)).unwrap();
        assert!(folded.contains("hello"), "{folded}");
        assert!(folded.contains("[附件 1] report.pdf"), "{folded}");
        assert!(folded.contains("application/pdf"), "{folded}");
        assert!(!folded.contains(&"A".repeat(100)), "attachment body leaked");
        assert!(folded.contains("--b--"), "{folded}");
        assert!(folded.len() < raw.len());
    }

    #[test]
    fn folded_attachment_keeps_the_raw_view_under_the_limit() {
        let raw = multipart_with_attachment(&"A".repeat(400_000));
        let attachments = raw_attachments(&raw);
        let folded = fold_attachments(&raw, &attachments);
        let (text, truncated) = prepare_raw(&folded, RAW_VIEW_LIMIT);
        assert!(!truncated);
        assert!(text.contains("[附件 1] report.pdf"), "{text}");
        assert!(!text.contains(&"A".repeat(100)));
        assert!(raw_status(1, false).contains("1 个附件"));
        assert!(raw_status(1, true).contains("截断"));
    }
}
