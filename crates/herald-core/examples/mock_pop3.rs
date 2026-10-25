//! Local mock POP3 server with sample Chinese mail, for trying the CLI/TUI
//! without a real mailbox.
//!
//! cargo run -p herald-core --features test-support --example mock_pop3 -- --dir D:/herald-demo

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use herald_core::config::Security;
use herald_core::mock_pop3::{MockConfig, MockMessage, MockPop3Server};

const USAGE: &str = "\
用法：mock_pop3 [选项]

  --dir <path>        生成 herald.toml 的目录（默认 ./herald-demo）
  --port <port>       POP3 端口，0 表示自动分配（默认 0）
  --messages <n>      样例邮件数量，1-28（默认 25）
  --username <name>   POP3 用户名（默认 test）
  --password <pass>   POP3 密码（默认 secret）
  --no-top            模拟不支持 TOP 的服务器
  -h, --help          显示帮助

另开终端执行：
  target/debug/herald.exe --config <dir>/herald.toml sync --all --with-attachments
  target/debug/herald.exe --config <dir>/herald.toml tui
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        print!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    let options = match Options::parse(&args) {
        Ok(options) => options,
        Err(message) => {
            eprintln!("{message}\n\n{USAGE}");
            return ExitCode::FAILURE;
        }
    };
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => {
            eprintln!("创建运行时失败：{err}");
            return ExitCode::FAILURE;
        }
    };
    match runtime.block_on(run(options)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("mock POP3 启动失败：{err}");
            ExitCode::FAILURE
        }
    }
}

struct Options {
    dir: PathBuf,
    port: u16,
    messages: usize,
    username: String,
    password: String,
    top: bool,
}

impl Options {
    fn parse(args: &[String]) -> Result<Self, String> {
        let mut options = Self {
            dir: PathBuf::from("herald-demo"),
            port: 0,
            messages: 25,
            username: "test".to_string(),
            password: "secret".to_string(),
            top: true,
        };
        let mut index = 0;
        while index < args.len() {
            let arg = args[index].as_str();
            index += 1;
            let mut value = || {
                let value = args.get(index).cloned();
                index += usize::from(value.is_some());
                value.ok_or_else(|| format!("{arg} 缺少参数"))
            };
            match arg {
                "--dir" => options.dir = PathBuf::from(value()?),
                "--port" => {
                    options.port = value()?
                        .parse()
                        .map_err(|_| "--port 必须是 0-65535".to_string())?
                }
                "--messages" => {
                    options.messages = value()?
                        .parse()
                        .map_err(|_| "--messages 必须是整数".to_string())?;
                    if !(1..=28).contains(&options.messages) {
                        return Err("--messages 必须在 1-28 之间".to_string());
                    }
                }
                "--username" => options.username = value()?,
                "--password" => options.password = value()?,
                "--no-top" => options.top = false,
                other => return Err(format!("未知参数：{other}")),
            }
        }
        Ok(options)
    }
}

async fn run(options: Options) -> std::io::Result<()> {
    let messages = sample_messages(options.messages);
    let server = MockPop3Server::start(MockConfig {
        mode: Security::Plain,
        support_top: options.top,
        username: options.username.clone(),
        password: options.password.clone(),
        messages,
    })
    .await?;
    let port = server.addr().port();
    let config = write_config(&options, port)?;
    let config = display_path(&config);

    println!("Mock POP3 已启动：127.0.0.1:{port}（plain）");
    println!(
        "账户：{} / {}，样例邮件：{} 封",
        options.username, options.password, options.messages
    );
    println!("配置：{config}");
    println!();
    println!("另开终端执行：");
    println!("  target/debug/herald.exe --config {config} sync --all --with-attachments");
    println!("  target/debug/herald.exe --config {config} tui");
    println!();
    println!("按 Ctrl+C 停止 mock 服务。");

    tokio::signal::ctrl_c().await?;
    println!("mock POP3 已停止。");
    Ok(())
}

fn write_config(options: &Options, port: u16) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(&options.dir)?;
    let exports = options.dir.join("exports");
    std::fs::create_dir_all(&exports)?;
    let dir = options.dir.to_string_lossy().replace('\\', "/");
    let path = options.dir.join("herald.toml");
    let text = format!(
        r#"# 由 examples/mock_pop3.rs 生成，仅用于本地试用
default_account = "demo"
database = "herald.db"

[[accounts]]
name = "demo"
host = "127.0.0.1"
port = {port}
security = "plain"
username = "{username}"
password = "{password}"
timeout_secs = 5

[sync]
default_limit = 50
auto_body_max_size = 0

[attachments]
export_dir = "{dir}/exports"

[semantic]
enabled = false
"#,
        username = toml_escape(&options.username),
        password = toml_escape(&options.password),
    );
    std::fs::write(&path, text)?;
    Ok(path)
}

fn display_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn toml_escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

fn sample_messages(count: usize) -> Vec<MockMessage> {
    (1..=count)
        .map(|n| {
            let uidl = format!("demo-{n:03}");
            let from = if n % 2 == 0 {
                "李四 <lisi@example.com>"
            } else {
                "张三 <zhangsan@example.com>"
            };
            let subject = if n == count {
                "本周项目进度汇报（含附件）".to_string()
            } else if n % 5 == 0 {
                format!("第 {n} 封：会议纪要")
            } else {
                format!("第 {n} 封：项目进度同步")
            };
            let body = format!(
                "这是第 {n} 封假邮件，用于试用 Herald。\r\n本周项目进度正常，测试覆盖率稳定。\r\n"
            );
            let raw = if n == count {
                multipart_message(from, &subject, n, &body)
            } else if n == count.saturating_sub(1) {
                html_message(from, &subject, n)
            } else {
                plain_message(from, &subject, n, &body)
            };
            MockMessage::new(uidl, raw)
        })
        .collect()
}

fn headers(from: &str, subject: &str, n: usize) -> String {
    format!(
        "From: {from}\r\n\
         To: demo@example.com\r\n\
         Subject: {subject}\r\n\
         Date: {} Sep 2024 10:{:02}:00 +0800\r\n\
         Message-ID: <demo-{n}@example.com>\r\n\
         MIME-Version: 1.0\r\n",
        n,
        n % 60
    )
}

fn plain_message(from: &str, subject: &str, n: usize, body: &str) -> Vec<u8> {
    format!(
        "{}Content-Type: text/plain; charset=utf-8\r\n\
         Content-Transfer-Encoding: 8bit\r\n\r\n{body}",
        headers(from, subject, n)
    )
    .into_bytes()
}

fn html_message(from: &str, subject: &str, n: usize) -> Vec<u8> {
    format!(
        "{}Content-Type: text/html; charset=utf-8\r\n\
         Content-Transfer-Encoding: 8bit\r\n\r\n\
         <html><body><p>这是第 {n} 封 HTML 假邮件。</p>\
         <p>项目进度正常，<b>测试覆盖率稳定</b>。</p></body></html>\r\n",
        headers(from, subject, n)
    )
    .into_bytes()
}

fn multipart_message(from: &str, subject: &str, n: usize, body: &str) -> Vec<u8> {
    format!(
        "{}Content-Type: multipart/mixed; boundary=\"DEMO\"\r\n\r\n\
         --DEMO\r\n\
         Content-Type: text/plain; charset=utf-8\r\n\
         Content-Transfer-Encoding: 8bit\r\n\r\n\
         {body}--DEMO\r\n\
         Content-Type: text/plain; name=\"说明.txt\"\r\n\
         Content-Disposition: attachment; filename=\"说明.txt\"\r\n\
         Content-Transfer-Encoding: base64\r\n\r\n\
         aGVsbG8gYXR0YWNobWVudA0K\r\n\
         --DEMO\r\n\
         Content-Type: application/octet-stream; name=\"data.bin\"\r\n\
         Content-Disposition: attachment; filename=\"data.bin\"\r\n\
         Content-Transfer-Encoding: base64\r\n\r\n\
         AAECAwQ=\r\n\
         --DEMO--\r\n",
        headers(from, subject, n)
    )
    .into_bytes()
}
