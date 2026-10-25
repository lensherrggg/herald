use std::path::Path;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use herald_core::mock_pop3::{MockConfig, MockMessage, MockPop3Server};
use herald_core::model::{SearchKind, SyncOptions};
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
        "To: alice@example.com\r\n",
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

fn write_config(dir: &Path, port: u16, export_dir: &Path) -> std::path::PathBuf {
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

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn input_label(app: &App) -> &'static str {
    match &app.mode {
        herald_tui::app::Mode::Input(state) => state.label(),
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
    let config = write_config(dir.path(), server.addr().port(), &exports);

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

    app.handle_key(key(KeyCode::Char('/')));
    assert_eq!(input_label(&app), "关键词搜索");
    app.handle_key(key(KeyCode::Tab));
    assert_eq!(input_label(&app), "混合搜索");
    app.handle_key(key(KeyCode::Tab));
    assert_eq!(input_label(&app), "语义搜索");
    app.handle_key(key(KeyCode::Tab));
    assert_eq!(input_label(&app), "关键词搜索");
    app.handle_key(key(KeyCode::Esc));
    assert!(matches!(app.mode, herald_tui::app::Mode::Browse));

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
