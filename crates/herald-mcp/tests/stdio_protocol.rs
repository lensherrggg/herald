use std::process::Stdio;
use std::time::Duration;

use herald_core::mock_pop3::{MockConfig, MockMessage, MockPop3Server};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::time::timeout;

const TIMEOUT: Duration = Duration::from_secs(15);

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

fn write_config(dir: &std::path::Path, port: u16) -> std::path::PathBuf {
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

struct Client {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<tokio::process::ChildStdout>,
    next_id: u64,
}

impl Client {
    fn spawn(config: &std::path::Path) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_herald-mcp"))
            .arg("--config")
            .arg(config)
            .env("HERALD_LOG", "error")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .expect("spawn herald-mcp");
        let stdin = child.stdin.take().expect("stdin");
        let stdout = BufReader::new(child.stdout.take().expect("stdout"));
        Self {
            child,
            stdin,
            stdout,
            next_id: 1,
        }
    }

    async fn send(&mut self, value: Value) {
        let line = serde_json::to_string(&value).expect("serialize");
        timeout(TIMEOUT, async {
            self.stdin.write_all(line.as_bytes()).await.expect("write");
            self.stdin.write_all(b"\n").await.expect("write");
            self.stdin.flush().await.expect("flush");
        })
        .await
        .expect("send timeout");
    }

    async fn recv(&mut self) -> Value {
        let mut line = String::new();
        timeout(TIMEOUT, self.stdout.read_line(&mut line))
            .await
            .expect("response timeout")
            .expect("read response");
        assert!(!line.trim().is_empty(), "server closed stdout");
        serde_json::from_str(line.trim())
            .unwrap_or_else(|err| panic!("invalid JSON response {line:?}: {err}"))
    }

    async fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))
            .await;
        let response = self.recv().await;
        assert_eq!(response["id"], json!(id), "unexpected response: {response}");
        assert!(
            response.get("error").is_none(),
            "JSON-RPC error: {response}"
        );
        response["result"].clone()
    }

    async fn call_tool(&mut self, name: &str, args: Value) -> Value {
        self.request("tools/call", json!({"name": name, "arguments": args}))
            .await
    }
}

fn tool_payload(result: &Value) -> Value {
    result["structuredContent"].clone()
}

async fn shutdown(mut client: Client) {
    drop(client.stdin);
    let _ = timeout(Duration::from_secs(5), client.child.wait()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn seven_tools_round_trip_over_stdio() {
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

    let mut client = Client::spawn(&config);
    let init = client
        .request(
            "initialize",
            json!({
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {"name": "herald-tests", "version": "0.1.0"}
            }),
        )
        .await;
    assert_eq!(init["serverInfo"]["name"], "herald");
    client
        .send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
        .await;

    let tools = client.request("tools/list", json!({})).await;
    let mut names: Vec<&str> = tools["tools"]
        .as_array()
        .expect("tools array")
        .iter()
        .map(|tool| tool["name"].as_str().expect("tool name"))
        .collect();
    names.sort_unstable();
    assert_eq!(
        names,
        vec![
            "get_message",
            "get_raw_message",
            "get_sync_status",
            "list_accounts",
            "list_messages",
            "search_messages",
            "sync_mailbox",
        ]
    );

    let accounts = tool_payload(&client.call_tool("list_accounts", json!({})).await);
    assert_eq!(accounts["accounts"][0]["name"], "work");

    let sync = client
        .call_tool(
            "sync_mailbox",
            json!({"account": "work", "with_body": true}),
        )
        .await;
    assert_eq!(sync["isError"], json!(false));
    let report = tool_payload(&sync);
    assert_eq!(report["fetched"], 1);
    assert_eq!(report["new_count"], 1);
    assert_eq!(report["last_uidl"], "m1");

    let listed = tool_payload(&client.call_tool("list_messages", json!({"limit": 5})).await);
    assert_eq!(listed["total"], 1);
    assert_eq!(listed["messages"][0]["uidl"], "m1");
    assert_eq!(listed["messages"][0]["subject"], "项目进度汇报");
    assert_eq!(listed["messages"][0]["from"], "张三 <zhangsan@example.com>");

    let found = tool_payload(
        &client
            .call_tool("search_messages", json!({"query": "项目"}))
            .await,
    );
    assert_eq!(found["total"], 1);
    assert_eq!(found["messages"][0]["uidl"], "m1");

    let detail = tool_payload(&client.call_tool("get_message", json!({"uidl": "m1"})).await);
    assert_eq!(detail["subject"], "项目进度汇报");
    assert!(detail["body_text"].as_str().unwrap().contains("进度正常"));
    assert_eq!(detail["body_truncated"], false);
    assert!(
        detail.get("attachments").is_none(),
        "get_message must not expose attachments: {detail}"
    );

    let raw = tool_payload(
        &client
            .call_tool("get_raw_message", json!({"uidl": "m1"}))
            .await,
    );
    assert!(raw["raw"]
        .as_str()
        .unwrap()
        .contains("Subject: 项目进度汇报"));
    assert_eq!(raw["truncated"], false);

    let status = tool_payload(&client.call_tool("get_sync_status", json!({})).await);
    assert_eq!(status["account"], "work");
    assert_eq!(status["total_messages"], 1);
    assert_eq!(status["error_count"], 0);

    let rejected = client
        .call_tool("list_messages", json!({"limit": 101}))
        .await;
    assert_eq!(rejected["isError"], json!(true));
    assert_eq!(
        rejected["structuredContent"]["code"], "INVALID_ARGUMENT",
        "unexpected error payload: {rejected}"
    );

    shutdown(client).await;
}
