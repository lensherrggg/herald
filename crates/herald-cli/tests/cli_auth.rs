use std::path::Path;
use std::time::Duration;

use herald_core::mock_pop3::{MockConfig, MockPop3Server};
use tokio::process::Command;
use tokio::time::timeout;

const TIMEOUT: Duration = Duration::from_secs(20);

fn write_config(dir: &Path, port: u16, credential: &str) -> std::path::PathBuf {
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
{credential}
timeout_secs = 5

[semantic]
enabled = false
"#
    );
    std::fs::write(&path, text).expect("write config");
    path
}

async fn run(config: &Path, args: &[&str]) -> std::process::Output {
    timeout(
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
    .expect("run cli")
}

async fn mock_server() -> MockPop3Server {
    MockPop3Server::start(MockConfig::default())
        .await
        .expect("mock server")
}

#[tokio::test(flavor = "multi_thread")]
async fn auth_status_lists_the_source_without_the_command() {
    let server = mock_server().await;
    let dir = tempfile::tempdir().unwrap();
    let config = write_config(
        dir.path(),
        server.addr().port(),
        r#"cred-cmd = "echo secret""#,
    );

    let output = run(&config, &["auth", "status"]).await;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(stdout.contains("work"), "{stdout}");
    assert!(stdout.contains("cred-cmd"), "{stdout}");
    assert!(stdout.contains("可执行"), "{stdout}");
    assert!(!stdout.contains("echo secret"), "command leaked:\n{stdout}");
    assert!(!stdout.contains("secret"), "password leaked:\n{stdout}");
}

#[tokio::test(flavor = "multi_thread")]
async fn auth_test_authenticates_and_never_prints_the_password() {
    let server = mock_server().await;
    let dir = tempfile::tempdir().unwrap();
    let config = write_config(
        dir.path(),
        server.addr().port(),
        r#"cred-cmd = "echo secret""#,
    );

    let output = run(&config, &["auth", "test"]).await;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(stdout.contains("认证成功"), "{stdout}");
    assert!(stdout.contains("cred-cmd"), "{stdout}");
    assert!(!stdout.contains("secret"), "password leaked:\n{stdout}");
}

#[tokio::test(flavor = "multi_thread")]
async fn auth_test_reports_a_failed_command_without_its_output() {
    let server = mock_server().await;
    let dir = tempfile::tempdir().unwrap();
    let config = write_config(dir.path(), server.addr().port(), r#"cred-cmd = "exit 3""#);

    let output = run(&config, &["auth", "test"]).await;
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "expected failure");
    assert!(stderr.contains("凭据获取失败"), "{stderr}");
    assert!(stderr.contains("退出码 3"), "{stderr}");
}

#[tokio::test(flavor = "multi_thread")]
async fn plaintext_advice_is_the_first_line() {
    let server = mock_server().await;
    let dir = tempfile::tempdir().unwrap();
    let config = write_config(dir.path(), server.addr().port(), r#"password = "secret""#);

    let output = run(&config, &["accounts"]).await;
    let stderr = String::from_utf8_lossy(&output.stderr);
    let first = stderr.lines().next().unwrap_or_default();
    assert!(
        first.starts_with("[安全建议]"),
        "advice must be the first line:\n{stderr}"
    );
    assert!(first.contains("work"), "{first}");
    assert!(first.contains("cred-cmd"), "{first}");

    let status = run(&config, &["auth", "status"]).await;
    let stdout = String::from_utf8_lossy(&status.stdout);
    assert!(stdout.contains("password"), "{stdout}");
    assert!(stdout.contains("已弃用"), "{stdout}");
    assert!(!stdout.contains("secret"), "password leaked:\n{stdout}");
}
