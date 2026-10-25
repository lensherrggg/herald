use std::path::Path;
use std::time::Duration;

use herald_core::mock_pop3::{MockConfig, MockMessage, MockPop3Server};
use serde_json::Value;
use tokio::process::Command;
use tokio::time::timeout;

const TIMEOUT: Duration = Duration::from_secs(20);

fn message() -> MockMessage {
    let raw = "From: 张三 <zhangsan@example.com>\r\n\
               To: alice@example.com\r\n\
               Subject: 项目进度汇报\r\n\
               Date: Tue, 03 Sep 2024 10:00:00 +0800\r\n\
               Message-ID: <m1@example.com>\r\n\
               MIME-Version: 1.0\r\n\
               Content-Type: text/plain; charset=utf-8\r\n\
               Content-Transfer-Encoding: 8bit\r\n\r\n\
               本周项目进度正常，测试覆盖率达到预期。\r\n";
    MockMessage::new("m1", raw.as_bytes().to_vec())
}

fn write_config(dir: &Path, port: u16) -> std::path::PathBuf {
    let path = dir.join("herald.toml");
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

[semantic]
enabled = false
"#
    );
    std::fs::write(&path, text).expect("write config");
    path
}

async fn run(config: &Path, args: &[&str]) -> Value {
    let output = timeout(
        TIMEOUT,
        Command::new(env!("CARGO_BIN_EXE_herald"))
            .arg("--config")
            .arg(config)
            .args(args)
            .env("HERALD_LOG", "error")
            .output(),
    )
    .await
    .expect("cli timed out")
    .expect("run cli");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "herald {args:?} failed\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    serde_json::from_str(stdout.trim())
        .unwrap_or_else(|err| panic!("invalid JSON from {args:?}: {err}\n{stdout}"))
}

#[tokio::test(flavor = "multi_thread")]
async fn cli_sync_search_and_show_against_mock_server() {
    let server = MockPop3Server::start(MockConfig {
        mode: herald_core::config::Security::Plain,
        support_top: true,
        username: "test".to_string(),
        password: "secret".to_string(),
        messages: vec![message()],
    })
    .await
    .expect("mock server");
    let dir = tempfile::tempdir().expect("tempdir");
    let config = write_config(dir.path(), server.addr().port());

    let accounts = run(&config, &["accounts", "--json"]).await;
    assert_eq!(accounts["accounts"][0]["name"], "work");

    let report = run(&config, &["sync", "--with-body", "--json"]).await;
    assert_eq!(report["new_count"], 1);
    assert_eq!(report["fetched"], 1);
    assert_eq!(report["last_uidl"], "m1");

    let again = run(&config, &["sync", "--json"]).await;
    assert_eq!(again["new_count"], 0, "second sync must be incremental");

    let listed = run(&config, &["list", "--limit", "5", "--json"]).await;
    assert_eq!(listed["total"], 1);
    assert_eq!(listed["messages"][0]["subject"], "项目进度汇报");

    let found = run(&config, &["search", "项目", "--json"]).await;
    assert_eq!(found["total"], 1);
    assert_eq!(found["messages"][0]["uidl"], "m1");

    let detail = run(&config, &["show", "m1", "--json"]).await;
    assert_eq!(detail["subject"], "项目进度汇报");
    assert!(detail["body_text"].as_str().unwrap().contains("进度正常"));
    assert!(detail["attachments"].is_array());

    let status = run(&config, &["status", "--json"]).await;
    assert_eq!(status["status"]["account"], "work");
    assert_eq!(status["status"]["total_messages"], 1);
    assert_eq!(status["status"]["error_count"], 0);
}
