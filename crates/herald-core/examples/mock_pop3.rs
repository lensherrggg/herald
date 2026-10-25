//! Local mock POP3 server with sample Chinese mail, for trying the CLI/TUI
//! without a real mailbox.
//!
//! For a one-command trial, use `demo = true` in herald.toml instead; this
//! example remains for custom ports and mailbox sizes.
//!
//! cargo run -p herald-core --features test-support --example mock_pop3 -- --dir D:/herald-demo

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use herald_core::config::Security;
use herald_core::demo::{sample_messages, MAX_SAMPLE_MESSAGES};
use herald_core::mock_pop3::{MockConfig, MockPop3Server};

const USAGE: &str = "\
用法：mock_pop3 [选项]

  --dir <path>        生成 herald.toml 的目录（默认 ./herald-demo）
  --port <port>       POP3 端口，0 表示自动分配（默认 0）
  --messages <n>      样例邮件数量，1-28（默认 25）
  --username <name>   POP3 用户名（默认 test）
  --password <pass>   POP3 密码（默认 secret）
  --no-top            模拟不支持 TOP 的服务器
  -h, --help          显示帮助

样例覆盖纯文本 / HTML / multipart、UTF-8 / GBK / BIG5、
8bit / base64 / quoted-printable、无主题、空正文与多附件。

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
                    if !(1..=MAX_SAMPLE_MESSAGES).contains(&options.messages) {
                        return Err(format!("--messages 必须在 1-{MAX_SAMPLE_MESSAGES} 之间"));
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
    println!("语义检索已启用。首次使用前下载模型（约 95 MB）：");
    println!(
        "  target/debug/herald.exe --config {config} models download --mirror https://hf-mirror.com"
    );
    println!("  target/debug/herald.exe --config {config} sync --all --with-attachments --index");
    println!("  target/debug/herald.exe --config {config} search 团队开会讨论了什么 --semantic");
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
enabled = true
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
