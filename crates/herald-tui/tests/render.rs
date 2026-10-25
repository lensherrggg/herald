use std::path::Path;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use herald_core::mock_pop3::{MockConfig, MockMessage, MockPop3Server};
use herald_core::model::{MessageDetail, MessageHeader, SearchField, SearchKind, SyncOptions};
use herald_core::Herald;
use herald_tui::app::{App, Focus};
use herald_tui::ui;
use ratatui::backend::TestBackend;
use ratatui::Terminal;

fn message_one() -> MockMessage {
    let raw = concat!(
        "From: 张三 <zhangsan@example.com>\r\n",
        "To: alice@example.com\r\n",
        "Subject: 项目进度汇报\r\n",
        "Date: Tue, 03 Sep 2024 10:00:00 +0800\r\n",
        "Message-ID: <m1@example.com>\r\n",
        "MIME-Version: 1.0\r\n",
        "Content-Type: multipart/mixed; boundary=\"HBR\"\r\n",
        "\r\n",
        "--HBR\r\n",
        "Content-Type: text/plain; charset=utf-8\r\n",
        "Content-Transfer-Encoding: 8bit\r\n",
        "\r\n",
        "本周项目进度正常，测试覆盖率达到预期。\r\n",
        "--HBR\r\n",
        "Content-Type: text/plain; name=\"notes.txt\"\r\n",
        "Content-Disposition: attachment; filename=\"notes.txt\"\r\n",
        "Content-Transfer-Encoding: base64\r\n",
        "\r\n",
        "aGVsbG8gYXR0YWNobWVudA0K\r\n",
        "--HBR\r\n",
        "Content-Type: application/octet-stream; name=\"数据.bin\"\r\n",
        "Content-Disposition: attachment; filename=\"数据.bin\"\r\n",
        "Content-Transfer-Encoding: base64\r\n",
        "\r\n",
        "AAECAwQ=\r\n",
        "--HBR--\r\n"
    );
    MockMessage::new("m1", raw.as_bytes().to_vec())
}

fn message_two() -> MockMessage {
    let raw = concat!(
        "From: 李四 <lisi@example.com>\r\n",
        "To: bob@example.com\r\n",
        "Cc: alice@example.com\r\n",
        "Subject: 会议纪要\r\n",
        "Date: Wed, 04 Sep 2024 09:00:00 +0800\r\n",
        "Message-ID: <m2@example.com>\r\n",
        "MIME-Version: 1.0\r\n",
        "Content-Type: text/plain; charset=utf-8\r\n",
        "Content-Transfer-Encoding: 8bit\r\n",
        "\r\n",
        "会议纪要正文：确认下周计划。\r\n"
    );
    MockMessage::new("m2", raw.as_bytes().to_vec())
}

fn write_config(dir: &Path, port: u16, export_dir: &Path, page_size: usize) -> std::path::PathBuf {
    let path = dir.join("herald.toml");
    let export_dir = export_dir.to_string_lossy().replace('\\', "/");
    let text = format!(
        r#"default_account = "work"
database = "herald.db"

[[accounts]]
name = "work"
host = "127.0.0.1"
port = {port}
security = "plain"
username = "test"
password = "secret"
timeout_secs = 5

[sync]
default_limit = 50
auto_body_max_size = 0

[attachments]
export_dir = "{export_dir}"

[semantic]
enabled = false

[ui]
page_size = {page_size}
"#
    );
    std::fs::write(&path, text).expect("write config");
    path
}

fn render(app: &App, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("test terminal");
    terminal
        .draw(|frame| ui::draw(frame, app))
        .expect("draw frame");
    terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect()
}

fn compact(text: &str) -> String {
    text.chars().filter(|ch| !ch.is_whitespace()).collect()
}

fn render_rows(app: &App, width: u16, height: u16) -> Vec<String> {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("test terminal");
    terminal
        .draw(|frame| ui::draw(frame, app))
        .expect("draw frame");
    let buffer = terminal.backend().buffer();
    (0..height)
        .map(|y| {
            (0..width)
                .map(|x| buffer.cell((x, y)).map_or(" ", |cell| cell.symbol()))
                .collect()
        })
        .collect()
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn input_label(app: &App) -> &'static str {
    match &app.mode {
        herald_tui::app::Mode::Input(state) => state.label(),
        _ => panic!("expected search input"),
    }
}

fn input_field(app: &App) -> &'static str {
    match &app.mode {
        herald_tui::app::Mode::Input(state) => state.field_label().expect("field label"),
        _ => panic!("expected search input"),
    }
}

#[test]
fn renders_mailbox_detail_search_browser_and_raw_views() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let dir = tempfile::tempdir().expect("tempdir");
    let exports = dir.path().join("exports");
    std::fs::create_dir_all(&exports).unwrap();
    let server = runtime
        .block_on(MockPop3Server::start(MockConfig {
            messages: vec![message_one(), message_two()],
            ..Default::default()
        }))
        .expect("mock server");
    let config = write_config(dir.path(), server.addr().port(), &exports, 20);

    let mut herald = Herald::open(Some(&config)).expect("open herald");
    let report = runtime
        .block_on(herald.sync(SyncOptions {
            account: Some("work".to_string()),
            all: true,
            with_body: true,
            with_attachments: true,
            ..Default::default()
        }))
        .expect("sync");
    assert_eq!(report.new_count, 2);
    assert_eq!(report.errors.len(), 0);

    let mut app = App::new(herald, runtime, Some("work".to_string()));
    app.color = true;
    assert_eq!(app.page_size, 20);
    assert_eq!(app.total, 2);
    assert_eq!(app.messages[0].subject.as_deref(), Some("会议纪要"));
    assert_eq!(app.messages[1].subject.as_deref(), Some("项目进度汇报"));
    assert_eq!(
        app.detail
            .as_ref()
            .map(|detail| detail.header.uidl.as_str()),
        Some("m2"),
        "the selected message should be previewed without pressing Enter"
    );
    assert!(app.detail.as_ref().unwrap().body_cached);
    app.handle_key(key(KeyCode::Enter));
    assert_eq!(app.focus, Focus::Detail);
    app.handle_key(key(KeyCode::Tab));
    assert_eq!(app.focus, Focus::List);

    let wide = compact(&render(&app, 120, 40));
    for expected in ["Herald", "work", "会议纪要", "项目进度汇报", "邮件"] {
        assert!(wide.contains(expected), "missing {expected}:\n{wide}");
    }

    app.handle_key(key(KeyCode::Down));
    assert_eq!(app.selected, 1);
    assert_eq!(
        app.detail
            .as_ref()
            .map(|detail| detail.header.uidl.as_str()),
        Some("m1")
    );
    assert!(app.detail.as_ref().unwrap().body_cached);
    let detail = compact(&render(&app, 120, 40));
    for expected in [
        "项目进度汇报",
        "本周项目进度正常",
        "notes.txt",
        "数据.bin",
        "正文",
    ] {
        assert!(detail.contains(expected), "missing {expected}:\n{detail}");
    }

    app.start_export();
    assert_eq!(
        app.attachments.as_ref().map(|picker| picker.metas.len()),
        Some(2)
    );
    let picker = compact(&render(&app, 120, 40));
    assert!(picker.contains("选择要导出的附件"), "{picker}");
    assert!(picker.contains("notes.txt"), "{picker}");

    app.handle_key(key(KeyCode::Enter));
    assert!(app.browser.is_some());
    let browser = compact(&render(&app, 120, 40));
    assert!(browser.contains("导出附件"), "{browser}");
    assert!(browser.contains("文件名：notes.txt"), "{browser}");
    assert!(browser.contains("exports"), "{browser}");

    app.export_to_dir();
    let saved = exports.join("notes.txt");
    assert_eq!(
        std::fs::read(&saved).expect("exported file"),
        b"hello attachment\r\n"
    );
    assert!(app.browser.is_none());

    app.start_export();
    app.handle_key(key(KeyCode::Down));
    app.handle_key(key(KeyCode::Enter));
    assert_eq!(
        app.browser.as_ref().map(|b| b.filename.as_str()),
        Some("数据.bin")
    );
    app.browser.as_mut().unwrap().filename = "报告.bin".to_string();
    app.export_to_filename();
    assert_eq!(
        std::fs::read(exports.join("报告.bin")).expect("renamed export"),
        [0u8, 1, 2, 3, 4]
    );

    app.start_export();
    app.handle_key(key(KeyCode::Enter));
    app.handle_key(key(KeyCode::Char('n')));
    for ch in "归档".chars() {
        app.handle_key(key(KeyCode::Char(ch)));
    }
    app.handle_key(key(KeyCode::Enter));
    assert!(exports.join("归档").is_dir());
    app.handle_key(key(KeyCode::Enter));
    assert_eq!(
        app.browser.as_ref().map(|b| b.dir.clone()),
        Some(exports.join("归档"))
    );
    let nested = compact(&render(&app, 120, 40));
    assert!(nested.contains("归档"), "{nested}");
    app.export_to_dir();
    assert_eq!(
        std::fs::read(exports.join("归档").join("notes.txt")).expect("nested export"),
        b"hello attachment\r\n"
    );

    app.selected = 0;
    app.load_raw();
    assert!(matches!(app.mode, herald_tui::app::Mode::Raw));
    let raw = app.raw.clone().unwrap_or_default();
    assert!(raw.contains("Subject: 会议纪要"), "{raw}");
    assert!(!raw.contains('\r'), "raw view must normalize CRLF");
    assert!(!raw.contains('\t'), "raw view must expand tabs");
    let raw_screen = compact(&render(&app, 120, 40));
    assert!(raw_screen.contains("原始邮件"), "{raw_screen}");
    assert!(raw_screen.contains("Subject:"), "{raw_screen}");
    app.handle_key(key(KeyCode::Esc));
    assert!(matches!(app.mode, herald_tui::app::Mode::Browse));

    app.run_search(SearchKind::Keyword, "会议".to_string());
    assert_eq!(app.total, 1);
    let found = compact(&render(&app, 120, 40));
    assert!(found.contains("关键词"), "{found}");
    assert!(found.contains("会议纪要"), "{found}");
    assert!(!found.contains("项目进度汇报"), "{found}");
    app.clear_search();
    assert_eq!(app.total, 2);

    app.run_search(SearchKind::Semantic, "进度".to_string());
    assert!(app.search.is_none());
    assert!(app.status_text().contains("语义检索未启用"));
    let disabled = compact(&render(&app, 120, 40));
    assert!(disabled.contains("语义检索未启用"), "{disabled}");

    app.run_search(SearchKind::Hybrid, "会议".to_string());
    assert_eq!(app.total, 1);
    assert!(
        app.status_text().contains("已仅用关键词"),
        "{}",
        app.status_text()
    );
    let hybrid = compact(&render(&app, 120, 40));
    assert!(hybrid.contains("混合"), "{hybrid}");
    assert!(hybrid.contains("会议纪要"), "{hybrid}");
    app.clear_search();
    assert_eq!(app.total, 2);

    app.run_search_with(
        SearchKind::Keyword,
        SearchField::Subject,
        "项目".to_string(),
    );
    assert_eq!(app.total, 1);
    assert_eq!(app.messages[0].uidl, "m1");
    let subject_search = compact(&render(&app, 120, 40));
    assert!(subject_search.contains("主题"), "{subject_search}");
    app.run_search_with(SearchKind::Keyword, SearchField::To, "bob".to_string());
    assert_eq!(app.total, 1);
    assert_eq!(app.messages[0].uidl, "m2");
    app.run_search_with(SearchKind::Keyword, SearchField::To, "alice".to_string());
    assert_eq!(app.total, 2, "To should match both To and Cc headers");
    app.run_search_with(SearchKind::Keyword, SearchField::From, "李四".to_string());
    assert_eq!(app.total, 1);
    assert_eq!(app.messages[0].uidl, "m2");
    app.run_search_with(
        SearchKind::Keyword,
        SearchField::Body,
        "下周计划".to_string(),
    );
    assert_eq!(app.total, 1);
    assert_eq!(app.messages[0].uidl, "m2");
    app.clear_search();

    app.handle_key(key(KeyCode::Char('/')));
    assert_eq!(input_label(&app), "关键词搜索");
    assert_eq!(input_field(&app), "全部字段");
    app.handle_key(key(KeyCode::Tab));
    assert_eq!(input_label(&app), "混合搜索");
    app.handle_key(key(KeyCode::Tab));
    assert_eq!(input_label(&app), "语义搜索");
    app.handle_key(key(KeyCode::Tab));
    assert_eq!(input_label(&app), "关键词搜索");
    for field in ["发件人", "收件人", "主题", "正文", "全部字段"] {
        app.handle_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL));
        assert_eq!(input_field(&app), field);
        assert_eq!(input_label(&app), "关键词搜索");
    }
    app.handle_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL));
    assert_eq!(input_field(&app), "发件人");
    for ch in "李四".chars() {
        app.handle_key(key(KeyCode::Char(ch)));
    }
    app.handle_key(key(KeyCode::Enter));
    assert_eq!(app.total, 1);
    assert_eq!(app.messages[0].uidl, "m2");
    let search = app.search.as_ref().expect("field search");
    assert_eq!(search.field, SearchField::From);
    assert_eq!(search.kind, SearchKind::Keyword);
    app.handle_key(key(KeyCode::Esc));
    assert!(matches!(app.mode, herald_tui::app::Mode::Browse));
    assert!(app.search.is_some(), "closing the box keeps the search");
    app.clear_search();
    assert!(app.search.is_none());
    assert_eq!(app.total, 2);

    app.selected = 1;
    app.load_detail();
    app.focus = Focus::Detail;
    let narrow = compact(&render(&app, 60, 20));
    assert!(narrow.contains("项目进度汇报"), "{narrow}");
    assert!(narrow.contains("本周项目进度正常"), "{narrow}");
    assert!(!narrow.contains("会议纪要"), "{narrow}");

    app.focus = Focus::List;
    app.color = false;
    let plain = compact(&render(&app, 120, 40));
    assert!(plain.contains("项目进度汇报"), "{plain}");
    assert!(plain.contains("Herald"), "{plain}");

    let tiny = compact(&render(&app, 30, 6));
    assert!(tiny.contains("终端太小"), "{tiny}");
}

fn synced_app(page_size: usize, messages: Vec<MockMessage>) -> (App, tempfile::TempDir) {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let dir = tempfile::tempdir().expect("tempdir");
    let exports = dir.path().join("exports");
    std::fs::create_dir_all(&exports).unwrap();
    let server = runtime
        .block_on(MockPop3Server::start(MockConfig {
            messages,
            ..Default::default()
        }))
        .expect("mock server");
    let config = write_config(dir.path(), server.addr().port(), &exports, page_size);
    let mut herald = Herald::open(Some(&config)).expect("open herald");
    runtime
        .block_on(herald.sync(SyncOptions {
            account: Some("work".to_string()),
            all: true,
            with_body: true,
            ..Default::default()
        }))
        .expect("sync");
    (App::new(herald, runtime, Some("work".to_string())), dir)
}

#[test]
fn read_state_is_local_and_tracks_preview_open_and_toggle() {
    let (mut app, _dir) = synced_app(20, vec![message_one(), message_two()]);
    assert_eq!(app.total, 2);
    assert_eq!(app.unread_total, 2);
    assert!(app.messages.iter().all(|message| !message.is_read));
    assert!(
        !app.detail.as_ref().unwrap().is_read,
        "previewing the selected message must not mark it read"
    );

    app.handle_key(key(KeyCode::Char('u')));
    assert!(app.messages[0].is_read);
    assert_eq!(app.unread_total, 1);
    assert!(app.status_text().contains("已读"));
    let marker = compact(&render(&app, 120, 40));
    assert_eq!(
        marker.matches('●').count(),
        1,
        "one unread marker:\n{marker}"
    );

    app.handle_key(key(KeyCode::Char('u')));
    assert!(!app.messages[0].is_read);
    assert_eq!(app.unread_total, 2);

    app.load_detail();
    assert!(app.messages[0].is_read);
    assert_eq!(app.unread_total, 1);
    assert!(app.detail.as_ref().unwrap().is_read);
    let stored = app
        .herald
        .get_stored_message(Some("work"), "m2")
        .expect("stored message");
    assert!(stored.is_read, "the flag must be persisted in SQLite");
}

#[test]
fn honors_configured_page_size() {
    let (mut app, _dir) = synced_app(1, vec![message_one(), message_two()]);
    assert_eq!(app.page_size, 1);
    assert_eq!(app.total, 2);
    assert_eq!(app.page_count(), 2);
    assert_eq!(app.messages.len(), 1);
    assert_eq!(app.messages[0].uidl, "m2");
    let first = compact(&render(&app, 120, 40));
    assert!(first.contains("第1/2页"), "{first}");
    assert_eq!(
        first.matches("第1/2页").count(),
        1,
        "page indicator must appear once:\n{first}"
    );

    app.handle_key(key(KeyCode::PageDown));
    assert_eq!(app.page, 1);
    assert_eq!(app.messages.len(), 1);
    assert_eq!(app.messages[0].uidl, "m1");
    let second = compact(&render(&app, 120, 40));
    assert!(second.contains("第2/2页"), "{second}");
}

#[test]
fn demo_config_opens_without_accounts() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config = dir.path().join("herald.toml");
    std::fs::write(&config, "demo = true\n\n[semantic]\nenabled = false\n").expect("write config");
    let herald = Herald::open(Some(&config)).expect("open demo herald");
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let app = App::new(herald, runtime, None);
    assert_eq!(app.account.as_deref(), Some("demo"));
    assert_eq!(app.total, 25);
    assert_eq!(app.unread_total, 25);
    assert_eq!(app.messages.len(), 20);
    assert_eq!(app.page_count(), 2);
    let screen = compact(&render(&app, 120, 40));
    assert!(screen.contains("共25封"), "{screen}");
    assert!(screen.contains("未读25"), "{screen}");
}

#[test]
fn wraps_long_addresses_and_body_and_shows_china_time() {
    let (mut app, _dir) = synced_app(20, vec![message_one()]);
    let recipients: Vec<String> = (0..40)
        .map(|index| format!("user{index}@example.com"))
        .collect();
    let token = "A".repeat(200);
    app.detail = Some(MessageDetail {
        header: MessageHeader {
            uidl: "m1".to_string(),
            message_id: None,
            from: Some("sender@example.com".to_string()),
            to: recipients,
            cc: vec![],
            subject: Some("长内容测试".to_string()),
            date: Some("2024-09-03T20:00:00Z".to_string()),
            size: 200,
        },
        body_text: Some(token),
        body_html: None,
        body_cached: true,
        attachments: vec![],
        is_read: false,
    });
    app.focus = Focus::Detail;
    let screen = compact(&render(&app, 40, 40));

    assert!(screen.contains("2024-09-0404:00:00"), "{screen}");
    assert!(screen.contains("UTC+8"), "{screen}");
    assert!(screen.contains("user11@example.com"), "{screen}");
    assert!(!screen.contains("user12@example.com"), "{screen}");
    assert!(screen.contains("共40个"), "{screen}");
    assert_eq!(
        screen.matches('A').count(),
        200,
        "long unbroken body tokens must wrap, not truncate:\n{screen}"
    );
}

#[test]
fn raw_view_searches_keywords_and_keeps_the_input_at_the_bottom() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let dir = tempfile::tempdir().expect("tempdir");
    let exports = dir.path().join("exports");
    std::fs::create_dir_all(&exports).unwrap();
    let server = runtime
        .block_on(MockPop3Server::start(MockConfig {
            messages: vec![message_one(), message_two()],
            ..Default::default()
        }))
        .expect("mock server");
    let config = write_config(dir.path(), server.addr().port(), &exports, 20);
    let mut herald = Herald::open(Some(&config)).expect("open herald");
    runtime
        .block_on(herald.sync(SyncOptions {
            account: Some("work".to_string()),
            all: true,
            with_body: true,
            ..Default::default()
        }))
        .expect("sync");
    let mut app = App::new(herald, runtime, Some("work".to_string()));
    assert_eq!(app.messages[0].uidl, "m2");
    app.load_raw();
    assert!(
        matches!(app.mode, herald_tui::app::Mode::Raw),
        "{}",
        app.status_text()
    );

    app.handle_key(key(KeyCode::Char('/')));
    assert!(app.raw_search.as_ref().is_some_and(|search| search.editing));
    assert!(app.status_text().contains("输入关键字"));
    let empty = compact(&render(&app, 120, 40));
    assert!(empty.contains("查找:"), "{empty}");
    assert!(empty.contains("0/0"), "{empty}");

    for ch in "会议".chars() {
        app.handle_key(key(KeyCode::Char(ch)));
    }
    let search = app.raw_search.as_ref().expect("raw search");
    assert_eq!(search.matches.len(), 2, "subject and body both match");
    assert_eq!(search.current, 0);
    assert!(app.status_text().contains("匹配 2 处"));
    let first = compact(&render(&app, 120, 40));
    assert!(first.contains("查找:会议"), "{first}");
    assert!(first.contains("1/2"), "{first}");

    app.handle_key(key(KeyCode::Enter));
    assert_eq!(app.raw_search.as_ref().unwrap().current, 1);
    assert!(app.status_text().contains("匹配 2/2"));
    let second = compact(&render(&app, 120, 40));
    assert!(second.contains("2/2"), "{second}");

    app.handle_key(key(KeyCode::Enter));
    assert_eq!(
        app.raw_search.as_ref().unwrap().current,
        0,
        "Enter wraps around"
    );

    app.handle_key(key(KeyCode::Esc));
    let search = app.raw_search.as_ref().unwrap();
    assert!(!search.editing);
    assert_eq!(search.matches.len(), 2, "closing input keeps highlights");
    let closed = compact(&render(&app, 120, 40));
    assert!(!closed.contains("查找:"), "{closed}");

    app.handle_key(key(KeyCode::Char('n')));
    assert_eq!(app.raw_search.as_ref().unwrap().current, 1);
    app.handle_key(key(KeyCode::Char('n')));
    assert_eq!(app.raw_search.as_ref().unwrap().current, 0);
    app.handle_key(key(KeyCode::Char('N')));
    assert_eq!(app.raw_search.as_ref().unwrap().current, 1);

    app.handle_key(key(KeyCode::Char('/')));
    for ch in "不存在XYZ".chars() {
        app.handle_key(key(KeyCode::Char(ch)));
    }
    assert!(app.raw_search.as_ref().unwrap().matches.is_empty());
    assert!(app.status_text().contains("未找到"));
    let missing = compact(&render(&app, 120, 40));
    assert!(missing.contains("0/0"), "{missing}");

    app.handle_key(key(KeyCode::Esc));
    app.handle_key(key(KeyCode::Esc));
    assert!(matches!(app.mode, herald_tui::app::Mode::Browse));
    assert!(app.raw_search.is_none());
    assert!(app.raw.is_none());
}

#[test]
fn raw_view_wraps_long_cjk_lines_without_splitting_labels() {
    let (mut app, _dir) = synced_app(20, vec![message_one()]);
    let body = format!("第 01 段：{}", "中文长文本测试。".repeat(20));
    app.raw = Some(format!("Subject: 原文折行\n{body}"));
    app.raw_truncated = false;
    app.mode = herald_tui::app::Mode::Raw;

    let rows = render_rows(&app, 80, 30);
    let screen = rows.join("\n");
    let compacted = compact(&screen);
    assert!(compacted.contains("第01段：中文"), "{screen}");
    assert!(!compacted.contains("第01│段"), "{screen}");
    assert!(compacted.contains("Subject:原文折行"), "{screen}");

    app.handle_key(key(KeyCode::Char('/')));
    for ch in "测试".chars() {
        app.handle_key(key(KeyCode::Char(ch)));
    }
    let search = app.raw_search.as_ref().expect("raw search");
    assert_eq!(search.matches.len(), 20);
    assert_eq!(search.current, 0);

    let rows = render_rows(&app, 80, 30);
    let screen = rows.join("\n");
    assert!(screen.contains("1/20"), "{screen}");
    let content = rows[1]
        .strip_prefix('│')
        .and_then(|row| row.strip_suffix('│'))
        .expect("bordered raw row");
    let content_text = compact(content);
    assert!(content_text.contains("第01段："), "{screen}");
    assert!(
        content_text.contains("测试"),
        "current match row must be visible:\n{screen}"
    );
    assert!(
        content.chars().count() <= 78,
        "content row wider than the inner width:\n{screen}"
    );
}

#[test]
fn detail_preview_wraps_long_cjk_body_without_orphaning_labels() {
    let (mut app, _dir) = synced_app(20, vec![message_one()]);
    let body = format!("第 01 段：{}", "中文长文本测试。".repeat(20));
    app.detail = Some(MessageDetail {
        header: MessageHeader {
            uidl: "m1".to_string(),
            message_id: None,
            from: Some("sender@example.com".to_string()),
            to: vec!["bob@example.com".to_string()],
            cc: vec![],
            subject: Some("长正文测试".to_string()),
            date: Some("2024-09-03T10:00:00+08:00".to_string()),
            size: 500,
        },
        body_text: Some(body),
        body_html: None,
        body_cached: true,
        attachments: vec![],
        is_read: false,
    });
    app.focus = Focus::Detail;

    let rows = render_rows(&app, 80, 40);
    let screen = rows.join("\n");
    let compacted = compact(&screen);
    assert!(compacted.contains("第01段：中文"), "{screen}");
    assert!(!compacted.contains("第01│段"), "{screen}");

    let body_rows: Vec<&String> = rows
        .iter()
        .filter(|row| compact(row).contains("第01段"))
        .collect();
    assert_eq!(
        body_rows.len(),
        1,
        "label should stay on one row:\n{screen}"
    );
    let content = body_rows[0]
        .strip_prefix('│')
        .and_then(|row| row.strip_suffix('│'))
        .expect("bordered detail row");
    assert!(
        content.chars().count() <= 78,
        "detail row wider than the panel:\n{screen}"
    );
}

#[test]
fn raw_view_pages_by_the_visible_height() {
    let (mut app, _dir) = synced_app(20, vec![message_one()]);
    let text: Vec<String> = (0..200).map(|index| format!("line {index}")).collect();
    app.raw = Some(text.join("\n"));
    app.raw_truncated = false;
    app.mode = herald_tui::app::Mode::Raw;
    let _ = render_rows(&app, 80, 30);

    let page = app.raw_height.get();
    assert!(page > 15, "page should follow the viewport, got {page}");
    app.handle_key(key(KeyCode::PageDown));
    assert_eq!(app.raw_scroll, page);
    app.handle_key(key(KeyCode::Char(' ')));
    assert_eq!(app.raw_scroll, page * 2);
    app.handle_key(key(KeyCode::PageUp));
    assert_eq!(app.raw_scroll, page);
    app.handle_key(key(KeyCode::Down));
    assert_eq!(app.raw_scroll, page + 1);
    app.handle_key(key(KeyCode::Char('g')));
    assert_eq!(app.raw_scroll, 0);
}
