#![cfg(feature = "test-support")]

use std::path::Path;

use herald_core::config::{AccountConfig, Config, Security};
use herald_core::mock_pop3::{MockConfig, MockMessage, MockPop3Server};
use herald_core::model::SyncOptions;
use herald_core::paths::PathLayout;
use herald_core::Herald;

fn message() -> MockMessage {
    let raw = "From: 张三 <zhangsan@example.com>\r\n\
               To: alice@example.com\r\n\
               Subject: cred-cmd 测试\r\n\
               Date: Tue, 03 Sep 2024 10:00:00 +0800\r\n\
               Message-ID: <m1@example.com>\r\n\
               MIME-Version: 1.0\r\n\
               Content-Type: text/plain; charset=utf-8\r\n\
               Content-Transfer-Encoding: 8bit\r\n\r\n\
               凭据来自外部命令。\r\n";
    MockMessage::new("m1", raw.as_bytes().to_vec())
}

fn plain_command() -> String {
    if cfg!(windows) {
        "Write-Output 'secret'".to_string()
    } else {
        "printf 'secret'".to_string()
    }
}

fn counter_command(counter: &Path) -> String {
    let counter = counter.to_string_lossy().replace('\\', "/");
    if cfg!(windows) {
        format!("Write-Output 'secret'; Add-Content -LiteralPath '{counter}' -Value 'hit'")
    } else {
        format!("printf 'secret'; echo hit >> '{counter}'")
    }
}

fn account(port: u16, cred_cmd: String, cache: bool) -> AccountConfig {
    AccountConfig {
        name: "work".to_string(),
        host: "127.0.0.1".to_string(),
        port,
        security: Security::Plain,
        username: "test".to_string(),
        password: None,
        password_env: None,
        cred_cmd: Some(cred_cmd),
        cred_cmd_timeout_secs: Some(10),
        cred_cmd_cache: cache,
        timeout_secs: Some(5),
    }
}

fn herald(account: AccountConfig, dir: &Path) -> Herald {
    let config = Config {
        default_account: Some("work".to_string()),
        accounts: vec![account],
        ..Default::default()
    };
    let layout = PathLayout {
        config_path: dir.join("herald.toml"),
        config_dir: dir.to_path_buf(),
        data_dir: dir.to_path_buf(),
        portable: true,
        fallback_reason: None,
    };
    Herald::open_with_config(config, layout).expect("open herald")
}

async fn server(messages: Vec<MockMessage>) -> MockPop3Server {
    MockPop3Server::start(MockConfig {
        messages,
        ..Default::default()
    })
    .await
    .expect("mock server")
}

fn sync_options() -> SyncOptions {
    SyncOptions {
        account: Some("work".to_string()),
        all: true,
        with_body: true,
        ..Default::default()
    }
}

#[tokio::test]
async fn cred_cmd_syncs_a_mailbox() {
    let mock = server(vec![message()]).await;
    let temp = tempfile::tempdir().unwrap();
    let mut herald = herald(
        account(mock.addr().port(), plain_command(), true),
        temp.path(),
    );

    let report = herald.sync(sync_options()).await.expect("sync");
    assert_eq!(report.new_count, 1);
    let detail = herald
        .get_stored_message(Some("work"), "m1")
        .expect("stored message");
    assert!(detail.body_cached);
    assert!(detail
        .body_text
        .as_deref()
        .unwrap_or_default()
        .contains("凭据来自外部命令"));
}

#[tokio::test]
async fn cache_reuses_the_password_across_syncs() {
    let mock = server(vec![message()]).await;
    let temp = tempfile::tempdir().unwrap();
    let counter = temp.path().join("counter.txt");
    let mut herald = herald(
        account(mock.addr().port(), counter_command(&counter), true),
        temp.path(),
    );

    herald.sync(sync_options()).await.expect("first sync");
    herald.sync(sync_options()).await.expect("second sync");
    let runs = std::fs::read_to_string(&counter).unwrap().lines().count();
    assert_eq!(runs, 1, "cached credential must run cred-cmd only once");
}

#[tokio::test]
async fn disabling_the_cache_runs_the_command_every_time() {
    let mock = server(vec![message()]).await;
    let temp = tempfile::tempdir().unwrap();
    let counter = temp.path().join("counter.txt");
    let mut herald = herald(
        account(mock.addr().port(), counter_command(&counter), false),
        temp.path(),
    );

    herald.sync(sync_options()).await.expect("first sync");
    herald.sync(sync_options()).await.expect("second sync");
    let runs = std::fs::read_to_string(&counter).unwrap().lines().count();
    assert_eq!(runs, 2);
}

#[tokio::test]
async fn auth_test_authenticates_without_exposing_the_password() {
    let mock = server(vec![]).await;
    let temp = tempfile::tempdir().unwrap();
    let herald = herald(
        account(mock.addr().port(), plain_command(), true),
        temp.path(),
    );

    let report = herald.test_account(Some("work")).await.expect("auth test");
    assert_eq!(report.account, "work");
    assert_eq!(report.source, "cred-cmd");
    assert_eq!(report.port, mock.addr().port());
}
