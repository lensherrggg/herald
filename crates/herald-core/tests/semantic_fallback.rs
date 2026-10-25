#![cfg(all(feature = "semantic", feature = "test-support"))]

use std::path::Path;

use herald_core::config::{AccountConfig, Config, Security};
use herald_core::mock_pop3::{MockConfig, MockMessage, MockPop3Server};
use herald_core::model::SyncOptions;
use herald_core::paths::PathLayout;
use herald_core::{Herald, HeraldError, SearchKind};

fn message() -> MockMessage {
    let raw = "From: 张三 <zhangsan@example.com>\r\n\
               Subject: 项目进度汇报\r\n\
               Date: Tue, 03 Sep 2024 10:00:00 +0800\r\n\
               MIME-Version: 1.0\r\n\
               Content-Type: text/plain; charset=utf-8\r\n\
               Content-Transfer-Encoding: 8bit\r\n\r\n\
               本周项目进度正常。\r\n";
    MockMessage::new("m1", raw.as_bytes().to_vec())
}

async fn setup(dir: &Path) -> (Herald, MockPop3Server) {
    let server = MockPop3Server::start(MockConfig {
        mode: Security::Plain,
        support_top: true,
        username: "test".to_string(),
        password: "secret".to_string(),
        messages: vec![message()],
    })
    .await
    .expect("mock server");
    let config = Config {
        default_account: Some("work".to_string()),
        accounts: vec![AccountConfig {
            name: "work".to_string(),
            host: "127.0.0.1".to_string(),
            port: server.addr().port(),
            security: Security::Plain,
            username: "test".to_string(),
            password: Some("secret".to_string()),
            password_env: None,
            timeout_secs: Some(5),
        }],
        ..Default::default()
    };
    let layout = PathLayout {
        config_path: dir.join("herald.toml"),
        config_dir: dir.to_path_buf(),
        data_dir: dir.to_path_buf(),
        portable: true,
        fallback_reason: None,
    };
    let herald = Herald::open_with_config(config, layout).expect("open herald");
    (herald, server)
}

#[tokio::test(flavor = "multi_thread")]
async fn sync_survives_missing_model_and_semantic_search_reports_it() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (mut herald, _server) = setup(dir.path()).await;
    assert!(herald.semantic_enabled());

    let report = herald
        .sync(SyncOptions {
            with_body: true,
            with_embeddings: true,
            ..Default::default()
        })
        .await
        .expect("sync must succeed without a model");
    assert_eq!(report.new_count, 1);

    let keyword = herald
        .search(None, "项目", SearchKind::Keyword, 10, 0)
        .expect("keyword search");
    assert_eq!(keyword.total, 1);

    let err = herald
        .search(None, "项目", SearchKind::Semantic, 10, 0)
        .unwrap_err();
    match err {
        HeraldError::Model(message) => {
            assert!(message.contains("模型文件缺失"), "{message}");
            assert!(message.contains("models download"), "{message}");
        }
        other => panic!("expected MODEL_ERROR, got {other:?}"),
    }
}
