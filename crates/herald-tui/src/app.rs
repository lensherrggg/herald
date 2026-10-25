use std::path::Path;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use herald_core::model::{
    AccountSummary, AttachmentMeta, MessageDetail, MessageFilter, MessageSort, MessageSummary,
    SearchKind, SearchResult, SyncOptions, SyncStatus,
};
use herald_core::paths::ResolvedPaths;
use herald_core::{Config, Herald};
use ratatui::DefaultTerminal;
use tokio::runtime::Runtime;

use crate::browser::{sanitize_filename, Browser};
use crate::ui;

pub const PAGE_SIZE: usize = 20;
pub const RAW_VIEW_LIMIT: usize = 256 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputKind {
    KeywordSearch,
    SemanticSearch,
    NewDir,
}

impl InputKind {
    pub fn label(self) -> &'static str {
        match self {
            InputKind::KeywordSearch => "关键词搜索",
            InputKind::SemanticSearch => "语义搜索",
            InputKind::NewDir => "新建目录",
        }
    }
}

#[derive(Debug)]
pub struct InputState {
    pub kind: InputKind,
    pub value: String,
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
}

impl SearchState {
    pub fn label(&self) -> &'static str {
        match self.kind {
            SearchKind::Keyword => "关键词",
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

pub struct App {
    pub herald: Herald,
    pub runtime: Runtime,
    pub accounts: Vec<AccountSummary>,
    pub account: Option<String>,
    pub sync_status: Option<SyncStatus>,
    pub messages: Vec<MessageSummary>,
    pub total: usize,
    pub page: usize,
    pub selected: usize,
    pub sort: MessageSort,
    pub descending: bool,
    pub search: Option<SearchState>,
    pub detail: Option<MessageDetail>,
    pub detail_scroll: u16,
    pub raw: Option<String>,
    pub raw_scroll: u16,
    pub raw_truncated: bool,
    pub mode: Mode,
    pub status: StatusMessage,
    pub focus: Focus,
    pub browser: Option<Browser>,
    pub attachments: Option<AttachmentPicker>,
    pub account_pick: usize,
    pub quit: bool,
    pub color: bool,
}

impl App {
    pub fn new(herald: Herald, runtime: Runtime, requested_account: Option<String>) -> Self {
        let accounts = herald.accounts().unwrap_or_default();
        let account = pick_account(&herald.config, &accounts, requested_account);
        let color = color_enabled();
        let mut app = Self {
            herald,
            runtime,
            accounts,
            account,
            sync_status: None,
            messages: Vec::new(),
            total: 0,
            page: 0,
            selected: 0,
            sort: MessageSort::Date,
            descending: true,
            search: None,
            detail: None,
            detail_scroll: 0,
            raw: None,
            raw_scroll: 0,
            raw_truncated: false,
            mode: Mode::Browse,
            status: StatusMessage::default(),
            focus: Focus::List,
            browser: None,
            attachments: None,
            account_pick: 0,
            quit: false,
            color,
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

    pub fn run(&mut self, terminal: &mut DefaultTerminal) -> anyhow::Result<()> {
        while !self.quit {
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
        page_count_for(self.total)
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
        let result = if let Some(search) = self.search.clone() {
            self.herald.search(
                Some(&account),
                &search.query,
                search.kind,
                PAGE_SIZE,
                self.page * PAGE_SIZE,
            )
        } else {
            let filter = MessageFilter {
                account: Some(account.clone()),
                limit: PAGE_SIZE,
                offset: self.page * PAGE_SIZE,
                sort: self.sort,
                descending: self.descending,
                ..Default::default()
            };
            self.herald
                .list_messages(Some(&account), &filter)
                .map(|(total, messages)| SearchResult { total, messages })
        };
        match result {
            Ok(result) => {
                self.total = result.total;
                self.messages = result.messages;
            }
            Err(err) => {
                self.status = StatusMessage::error(err.to_string());
                self.messages.clear();
                self.total = 0;
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
                self.status = StatusMessage::info(format!(
                    "已加载：{}",
                    detail.header.subject.as_deref().unwrap_or("(无主题)")
                ));
                self.detail = Some(detail);
                self.detail_scroll = 0;
            }
            Err(err) => {
                self.status = StatusMessage::error(format!("加载邮件失败：{err}"));
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
                let (text, truncated) = prepare_raw(&bytes, RAW_VIEW_LIMIT);
                self.raw = Some(text);
                self.raw_scroll = 0;
                self.raw_truncated = truncated;
                self.mode = Mode::Raw;
                self.status = StatusMessage::info(if truncated {
                    "原始邮件：超过 256KB，已截断 · Esc 返回"
                } else {
                    "原始邮件：↑/↓ 滚动，Esc 返回"
                });
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
                self.status = StatusMessage::info(format!(
                    "同步完成：新邮件 {} 封，处理 {} 封，正文缓存 {} 封{}",
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

    pub fn run_search(&mut self, kind: SearchKind, query: String) {
        let query = query.trim().to_string();
        if query.is_empty() {
            self.search = None;
            self.page = 0;
            self.detail = None;
            self.reload();
            return;
        }
        if kind == SearchKind::Semantic && !self.herald.semantic_enabled() {
            self.status = StatusMessage::error(
                "语义检索未启用：请在配置中开启 semantic.enabled，或运行 herald models download",
            );
            return;
        }
        self.search = Some(SearchState { query, kind });
        self.page = 0;
        self.selected = 0;
        self.detail = None;
        self.reload();
        self.status = StatusMessage::info(match self.search.as_ref() {
            Some(search) => format!(
                "{}搜索“{}”：{} 条",
                search.label(),
                search.query,
                self.total
            ),
            None => String::new(),
        });
    }

    pub fn clear_search(&mut self) {
        if self.search.is_some() {
            self.search = None;
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
        self.browser = Some(Browser::new(start, uidl, index, name));
        self.mode = Mode::Browser;
        self.status = StatusMessage::info("选择目录后按 d 保存，或 f 编辑文件名");
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
                self.mode = Mode::Input(InputState {
                    kind: InputKind::KeywordSearch,
                    value: String::new(),
                })
            }
            KeyCode::Char('?') => {
                self.mode = Mode::Input(InputState {
                    kind: InputKind::SemanticSearch,
                    value: String::new(),
                })
            }
            KeyCode::Char('o') => self.cycle_sort(),
            KeyCode::Char('O') => self.reverse_sort(),
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
                    InputKind::KeywordSearch => self.run_search(SearchKind::Keyword, value),
                    InputKind::SemanticSearch => self.run_search(SearchKind::Semantic, value),
                    InputKind::NewDir => self.submit_new_dir(value),
                }
            }
            KeyCode::Backspace => {
                if let Mode::Input(state) = &mut self.mode {
                    state.value.pop();
                }
            }
            KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Mode::Input(state) = &mut self.mode {
                    state.value.push(ch);
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
        if browser.filename_focus {
            match key.code {
                KeyCode::Enter => self.export_to_filename(),
                KeyCode::Esc | KeyCode::Tab => browser.unfocus_filename(),
                KeyCode::Backspace => {
                    browser.filename.pop();
                }
                KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    browser.filename.push(ch);
                }
                _ => {}
            }
            return;
        }
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.browser = None;
                self.mode = Mode::Browse;
            }
            KeyCode::Down | KeyCode::Char('j') => browser.move_by(1),
            KeyCode::Up | KeyCode::Char('k') => browser.move_by(-1),
            KeyCode::PageDown => browser.move_by(10),
            KeyCode::PageUp => browser.move_by(-10),
            KeyCode::Enter => browser.enter(),
            KeyCode::Char('f') | KeyCode::Tab => browser.focus_filename(),
            KeyCode::Char('d') => self.export_to_dir(),
            KeyCode::Char('n') => {
                self.mode = Mode::Input(InputState {
                    kind: InputKind::NewDir,
                    value: String::new(),
                })
            }
            _ => {}
        }
    }

    fn handle_raw_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.mode = Mode::Browse;
                self.raw = None;
                self.raw_truncated = false;
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.raw_scroll = self.raw_scroll.saturating_add(1)
            }
            KeyCode::Up | KeyCode::Char('k') => self.raw_scroll = self.raw_scroll.saturating_sub(1),
            KeyCode::PageDown | KeyCode::Char(' ') => {
                self.raw_scroll = self.raw_scroll.saturating_add(15)
            }
            KeyCode::PageUp => self.raw_scroll = self.raw_scroll.saturating_sub(15),
            KeyCode::Char('g') => self.raw_scroll = 0,
            KeyCode::Char('G') => self.raw_scroll = u16::MAX,
            _ => {}
        }
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
    let Some(date) = date else {
        return "-".to_string();
    };
    let date = date.trim();
    if date.len() >= 10 && date.as_bytes().get(4) == Some(&b'-') {
        return date[..10].to_string();
    }
    if let Some((_, rest)) = date.split_once(", ") {
        return rest.chars().take(11).collect();
    }
    date.chars().take(16).collect()
}

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
        "-".to_string()
    } else {
        addrs.join(", ")
    }
}

pub fn body_text(detail: &MessageDetail) -> String {
    if !detail.body_cached {
        return "（正文未缓存，按 Enter 从服务器加载）".to_string();
    }
    match (&detail.body_text, &detail.body_html) {
        (Some(text), _) if !text.trim().is_empty() => text.clone(),
        (_, Some(html)) if !html.trim().is_empty() => format!("[HTML 正文]\n{html}"),
        _ => "(没有可显示的正文)".to_string(),
    }
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

fn page_count_for(total: usize) -> usize {
    if total == 0 {
        1
    } else {
        total.div_ceil(PAGE_SIZE)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_sizes_and_dates() {
        assert_eq!(format_size(900), "900 B");
        assert_eq!(format_size(2048), "2.0 KB");
        assert_eq!(
            shorten_date(Some("Tue, 03 Sep 2024 10:00:00 +0800")),
            "03 Sep 2024"
        );
        assert_eq!(shorten_date(Some("2024-09-03T10:00:00Z")), "2024-09-03");
        assert_eq!(sender_short(Some("张三 <a@b.c>")), "张三");
    }

    #[test]
    fn pages_cover_remainders() {
        assert_eq!(page_count_for(41), 3);
        assert_eq!(page_count_for(40), 2);
        assert_eq!(page_count_for(0), 1);
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
}
