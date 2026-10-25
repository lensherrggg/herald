#![cfg(feature = "test-support")]

use std::path::Path;
use std::time::Duration;

use herald_core::config::{AccountConfig, Config, Security};
use herald_core::mock_pop3::{MockConfig, MockMessage, MockPop3Server};
use herald_core::model::{MessageFilter, SearchOptions, SyncOptions};
use herald_core::paths::PathLayout;
use herald_core::pop3::{Pop3Client, Pop3Options};
use herald_core::{Herald, SearchKind};

const PASSWORD: &str = "secret";

fn message(uidl: &str, subject: &str, body: &str, attachment: bool) -> MockMessage {
    let mut raw = format!(
        "From: 张三 <zhangsan@example.com>\r\n\
         To: alice@example.com\r\n\
         Cc: bob@example.com\r\n\
         Subject: {subject}\r\n\
         Date: Tue, 03 Sep 2024 10:00:00 +0800\r\n\
         Message-ID: <{uidl}@example.com>\r\n\
         MIME-Version: 1.0\r\n"
    );
    if attachment {
        raw.push_str(
            "Content-Type: multipart/mixed; boundary=\"b\"\r\n\r\n\
             --b\r\n\
             Content-Type: text/plain; charset=utf-8\r\n\
             Content-Transfer-Encoding: 8bit\r\n\r\n",
        );
        raw.push_str(body);
        raw.push_str(
            "\r\n--b\r\n\
             Content-Type: application/pdf; name=\"report.pdf\"\r\n\
             Content-Disposition: attachment; filename=\"report.pdf\"\r\n\
             Content-Transfer-Encoding: base64\r\n\
             Content-ID: <cid-report>\r\n\r\n\
             JVBERi0xLjQK\r\n\
             --b--\r\n",
        );
    } else {
        raw.push_str(
            "Content-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: 8bit\r\n\r\n",
        );
        raw.push_str(body);
        raw.push_str("\r\n");
    }
    MockMessage::new(uidl, raw.into_bytes())
}

async fn server(mode: Security, support_top: bool, messages: Vec<MockMessage>) -> MockPop3Server {
    MockPop3Server::start(MockConfig {
        mode,
        support_top,
        username: "test".to_string(),
        password: PASSWORD.to_string(),
        messages,
    })
    .await
    .expect("mock server")
}

fn account(port: u16, security: Security) -> AccountConfig {
    AccountConfig {
        name: "work".to_string(),
        host: "127.0.0.1".to_string(),
        port,
        security,
        username: "test".to_string(),
        password: Some(PASSWORD.to_string()),
        password_env: None,
        cred_cmd: None,
        cred_cmd_timeout_secs: None,
        cred_cmd_cache: true,
        timeout_secs: Some(5),
    }
}

fn config(port: u16, security: Security, body_threshold: u64) -> Config {
    let mut config = Config {
        default_account: Some("work".to_string()),
        accounts: vec![account(port, security)],
        ..Default::default()
    };
    config.sync.auto_body_max_size = body_threshold;
    config
}

fn herald(config: Config, dir: &Path) -> Herald {
    let layout = PathLayout {
        config_path: dir.join("herald.toml"),
        config_dir: dir.to_path_buf(),
        data_dir: dir.to_path_buf(),
        portable: true,
        fallback_reason: None,
    };
    Herald::open_with_config(config, layout).expect("open herald")
}

#[tokio::test]
async fn connects_and_reads_in_all_security_modes() {
    for mode in [Security::Plain, Security::Stls, Security::Tls] {
        let mock = server(
            mode,
            true,
            vec![message("u1", "测试连接", "你好，世界", false)],
        )
        .await;
        let mut options = Pop3Options {
            host: "127.0.0.1".to_string(),
            port: mock.addr().port(),
            security: mode,
            username: "test".to_string(),
            password: PASSWORD.to_string(),
            timeout: Duration::from_secs(5),
            extra_roots: Vec::new(),
        };
        if let Some(cert) = mock.cert_der() {
            options.extra_roots.push(cert);
        }
        let mut client = Pop3Client::connect(&options)
            .await
            .unwrap_or_else(|err| panic!("{mode:?} connect failed: {err}"));
        let uidls = client.uidl().await.unwrap();
        assert_eq!(uidls, vec![(1, "u1".to_string())]);

        let headers = client.fetch_headers(1).await.unwrap();
        let text = String::from_utf8_lossy(&headers);
        assert!(text.contains("Subject:"));
        assert!(
            !text.contains("你好，世界"),
            "TOP 0 must not return the body"
        );

        let raw = client.retr(1).await.unwrap();
        assert!(String::from_utf8_lossy(&raw).contains("你好，世界"));
        client.quit().await.unwrap();
    }
}

#[tokio::test]
async fn wrong_password_is_an_auth_error() {
    let mock = server(Security::Plain, true, vec![]).await;
    let options = Pop3Options {
        host: "127.0.0.1".to_string(),
        port: mock.addr().port(),
        security: Security::Plain,
        username: "test".to_string(),
        password: "wrong".to_string(),
        timeout: Duration::from_secs(5),
        extra_roots: Vec::new(),
    };
    let err = Pop3Client::connect(&options)
        .await
        .err()
        .expect("must fail");
    assert!(matches!(err, herald_core::HeraldError::Auth(_)));
}

#[tokio::test]
async fn sync_is_incremental_and_respects_limit() {
    let mock = server(
        Security::Plain,
        true,
        vec![
            message("u1", "季度项目进度", "本周项目进度正常。", false),
            message("u2", "会议纪要", "项目评审会议纪要。", false),
            message("u3", "周报", "本周工作汇总，无项目更新。", false),
        ],
    )
    .await;
    let temp = tempfile::tempdir().unwrap();
    let mut herald = herald(config(mock.addr().port(), Security::Plain, 0), temp.path());

    let first = herald
        .sync(SyncOptions {
            limit: Some(2),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(first.fetched, 2);
    assert_eq!(first.new_count, 2);
    assert_eq!(first.bodies_cached, 2);
    assert!(first.errors.is_empty());

    let (total, messages) = herald
        .list_messages(
            None,
            &MessageFilter {
                limit: 20,
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(total, 2);
    assert!(messages.iter().all(|m| m.uidl != "u1"));
    assert!(messages.iter().any(|m| m.uidl == "u2"));
    assert!(messages.iter().any(|m| m.uidl == "u3"));

    let second = herald.sync(SyncOptions::default()).await.unwrap();
    assert_eq!(second.fetched, 1);
    assert_eq!(second.new_count, 1);

    let third = herald.sync(SyncOptions::default()).await.unwrap();
    assert_eq!(third.fetched, 0);
    assert_eq!(third.new_count, 0);

    let status = herald.status(None).unwrap();
    assert_eq!(status.total_messages, 3);
    assert_eq!(status.local_messages, 3);
    assert!(status.last_sync_at.is_some());
    assert_eq!(status.last_uidl.as_deref(), Some("u3"));

    let detail = herald.get_stored_message(None, "u1").unwrap();
    assert_eq!(detail.header.subject.as_deref(), Some("季度项目进度"));
    assert_eq!(
        detail.body_text.as_deref().map(str::trim),
        Some("本周项目进度正常。")
    );
    assert!(detail.body_cached);
}

#[tokio::test]
async fn chinese_search_and_pagination() {
    let mut messages = Vec::new();
    for index in 1..=5 {
        let subject = if index == 1 {
            "季度项目进度".to_string()
        } else {
            format!("周报 {index}")
        };
        let body = if index <= 3 {
            format!("项目进度汇报 {index}")
        } else {
            format!("午餐通知 {index}")
        };
        messages.push(message(&format!("u{index}"), &subject, &body, false));
    }
    let mock = server(Security::Plain, true, messages).await;
    let temp = tempfile::tempdir().unwrap();
    let mut herald = herald(config(mock.addr().port(), Security::Plain, 0), temp.path());
    herald.sync(SyncOptions::default()).await.unwrap();

    let result = herald.keyword_search(None, "项目", 2, 0).unwrap();
    assert_eq!(result.total, 3);
    assert_eq!(result.messages.len(), 2);
    let page2 = herald.keyword_search(None, "项目", 2, 2).unwrap();
    assert_eq!(page2.total, 3);
    assert_eq!(page2.messages.len(), 1);
    let page3 = herald.keyword_search(None, "项目", 2, 4).unwrap();
    assert!(page3.messages.is_empty());

    let exact = herald.keyword_search(None, "季度项目", 10, 0).unwrap();
    assert!(exact.messages.iter().any(|m| m.uidl == "u1"));

    let semantic = herald.search(
        None,
        "项目",
        SearchKind::Semantic,
        &SearchOptions {
            limit: 10,
            ..Default::default()
        },
    );
    if cfg!(feature = "semantic") {
        // The model is not downloaded in tests; the error must be structured.
        assert!(semantic.is_err());
    } else {
        assert!(matches!(
            semantic.err(),
            Some(herald_core::HeraldError::Unsupported(_))
        ));
    }
}

#[tokio::test]
async fn top_less_server_falls_back_to_retr() {
    let mock = server(
        Security::Plain,
        false,
        vec![message("u1", "无 TOP 服务器", "正文仍然可读", false)],
    )
    .await;
    let temp = tempfile::tempdir().unwrap();
    let mut herald = herald(config(mock.addr().port(), Security::Plain, 0), temp.path());
    let report = herald.sync(SyncOptions::default()).await.unwrap();
    assert_eq!(report.new_count, 1);
    assert!(report.errors.is_empty());
    let detail = herald.get_stored_message(None, "u1").unwrap();
    assert_eq!(
        detail.body_text.as_deref().map(str::trim),
        Some("正文仍然可读")
    );
}

#[tokio::test]
async fn body_threshold_defers_body_until_requested() {
    let mock = server(
        Security::Plain,
        true,
        vec![message("u1", "大邮件", "这是一封超过阈值的邮件正文", false)],
    )
    .await;
    let temp = tempfile::tempdir().unwrap();
    let mut herald = herald(config(mock.addr().port(), Security::Plain, 1), temp.path());

    let report = herald.sync(SyncOptions::default()).await.unwrap();
    assert_eq!(report.new_count, 1);
    assert_eq!(report.bodies_cached, 0);
    let detail = herald.get_stored_message(None, "u1").unwrap();
    assert!(!detail.body_cached);

    let fetched = herald.get_message(None, "u1", true).await.unwrap();
    assert!(fetched.body_cached);
    assert_eq!(
        fetched.body_text.as_deref().map(str::trim),
        Some("这是一封超过阈值的邮件正文")
    );

    drop(mock);
    let cached = herald.get_message(None, "u1", true).await.unwrap();
    assert_eq!(
        cached.body_text.as_deref().map(str::trim),
        Some("这是一封超过阈值的邮件正文")
    );
}

#[tokio::test]
async fn attachments_are_metadata_only_until_requested_and_exported() {
    let mock = server(
        Security::Plain,
        true,
        vec![message("u1", "附件测试", "请查收附件。", true)],
    )
    .await;
    let temp = tempfile::tempdir().unwrap();
    let mut herald = herald(config(mock.addr().port(), Security::Plain, 0), temp.path());

    let report = herald.sync(SyncOptions::default()).await.unwrap();
    assert_eq!(report.attachments_stored, 0);
    let detail = herald.get_stored_message(None, "u1").unwrap();
    assert_eq!(detail.attachments.len(), 1);
    let meta = &detail.attachments[0];
    assert_eq!(meta.filename.as_deref(), Some("report.pdf"));
    assert_eq!(meta.mime_type.as_deref(), Some("application/pdf"));
    assert_eq!(meta.content_id.as_deref(), Some("cid-report"));
    assert!(!meta.has_content);

    let data = herald.get_attachment(None, "u1", 0).await.unwrap();
    assert_eq!(data.content, b"%PDF-1.4\n");

    let export_dir = temp.path().join("exports");
    let target = herald
        .export_attachment(None, "u1", 0, &export_dir)
        .await
        .unwrap();
    assert_eq!(std::fs::read(&target).unwrap(), b"%PDF-1.4\n");
    assert!(target.starts_with(&export_dir));
    assert_eq!(
        herald.export_base_dir(),
        export_dir,
        "last export dir must be remembered"
    );
}

#[tokio::test]
async fn sync_with_attachments_stores_blobs() {
    let mock = server(
        Security::Plain,
        true,
        vec![message("u1", "附件测试", "请查收附件。", true)],
    )
    .await;
    let temp = tempfile::tempdir().unwrap();
    let mut herald = herald(config(mock.addr().port(), Security::Plain, 0), temp.path());
    let report = herald
        .sync(SyncOptions {
            with_attachments: true,
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(report.attachments_stored, 1);
    let detail = herald.get_stored_message(None, "u1").unwrap();
    assert!(detail.attachments[0].has_content);
    let data = herald.get_attachment(None, "u1", 0).await.unwrap();
    assert_eq!(data.content, b"%PDF-1.4\n");
}

#[tokio::test]
async fn multi_account_requires_explicit_selection() {
    let mock = server(Security::Plain, true, vec![]).await;
    let temp = tempfile::tempdir().unwrap();
    let mut config = config(mock.addr().port(), Security::Plain, 0);
    config.default_account = None;
    config.accounts.push(AccountConfig {
        name: "second".to_string(),
        host: "127.0.0.1".to_string(),
        port: mock.addr().port(),
        security: Security::Plain,
        username: "test".to_string(),
        password: Some(PASSWORD.to_string()),
        password_env: None,
        cred_cmd: None,
        cred_cmd_timeout_secs: None,
        cred_cmd_cache: true,
        timeout_secs: Some(5),
    });
    let herald = herald(config, temp.path());
    assert!(matches!(
        herald.status(None),
        Err(herald_core::HeraldError::Account(_))
    ));
    assert!(herald.status(Some("second")).is_ok());
}
