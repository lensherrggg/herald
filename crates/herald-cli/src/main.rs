use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{bail, Context};
use clap::{Parser, Subcommand};
use herald_core::logging::{self, StderrLogging};
use herald_core::model::{MessageFilter, SearchField, SearchKind, SearchOptions, SyncOptions};
use herald_core::paths::PathLayout;
use herald_core::time;
use herald_core::{Config, Herald};

const BODY_PREVIEW_LIMIT: usize = 32 * 1024;

#[derive(Debug, Parser)]
#[command(
    name = "herald",
    version,
    about = "Herald — 面向 AI Agent 的只读 POP3 邮箱客户端"
)]
struct Cli {
    /// 配置文件路径（默认：可执行文件目录下的 herald.toml）
    #[arg(long, global = true, value_name = "PATH")]
    config: Option<PathBuf>,

    /// 以 JSON 输出，便于脚本化
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum SearchMode {
    Auto,
    Keyword,
    Semantic,
    Hybrid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum FieldArg {
    All,
    From,
    To,
    Subject,
    Body,
}

impl From<FieldArg> for SearchField {
    fn from(value: FieldArg) -> Self {
        match value {
            FieldArg::All => SearchField::All,
            FieldArg::From => SearchField::From,
            FieldArg::To => SearchField::To,
            FieldArg::Subject => SearchField::Subject,
            FieldArg::Body => SearchField::Body,
        }
    }
}

#[derive(Debug, Subcommand)]
enum Command {
    /// 生成示例配置
    Init {
        /// 覆盖已有配置文件
        #[arg(long)]
        force: bool,
    },
    /// 启动终端界面
    Tui {
        /// 启动时选中的账户
        #[arg(long, short)]
        account: Option<String>,
    },
    /// 列出账户
    Accounts,
    /// 增量同步邮件
    Sync {
        #[arg(long)]
        account: Option<String>,
        /// 忽略 limit，处理全部新邮件
        #[arg(long)]
        all: bool,
        /// 强制拉取正文（不受 auto_body_max_size 限制）
        #[arg(long)]
        with_body: bool,
        /// 额外下载附件内容并写入本地数据库
        #[arg(long)]
        with_attachments: bool,
        /// 为已缓存正文的邮件批量升级全文 embedding
        #[arg(long)]
        index: bool,
        /// 本次最多处理的新邮件数
        #[arg(long)]
        limit: Option<usize>,
    },
    /// 列出已同步邮件
    List {
        #[arg(long)]
        account: Option<String>,
        /// 只列出未读邮件
        #[arg(long)]
        unread: bool,
        #[arg(long, default_value_t = 20)]
        limit: usize,
        #[arg(long, default_value_t = 0)]
        offset: usize,
    },
    /// 关键词、语义或混合搜索（默认：有模型时混合）
    Search {
        query: String,
        #[arg(long)]
        account: Option<String>,
        /// 检索模式；auto 在模型可用时使用混合检索
        #[arg(long, value_enum)]
        mode: Option<SearchMode>,
        /// 限定匹配字段（默认 all）
        #[arg(long, value_enum, default_value = "all")]
        field: FieldArg,
        /// 等价于 --mode semantic（保留旧写法）
        #[arg(long, conflicts_with = "mode")]
        semantic: bool,
        #[arg(long, default_value_t = 20)]
        limit: usize,
        #[arg(long, default_value_t = 0)]
        offset: usize,
    },
    /// 标记邮件为已读（--unread 则标记为未读，仅影响本地状态）
    Mark {
        uidl: String,
        #[arg(long)]
        account: Option<String>,
        #[arg(long)]
        unread: bool,
    },
    /// 查看邮件详情
    Show {
        uidl: String,
        #[arg(long)]
        account: Option<String>,
        /// 输出原始邮件
        #[arg(long)]
        raw: bool,
        /// 不截断正文（默认 32KB）
        #[arg(long)]
        full_body: bool,
    },
    /// 查看同步状态
    Status {
        #[arg(long)]
        account: Option<String>,
    },
    /// 导出附件到文件系统
    ExportAttachment {
        uidl: String,
        index: usize,
        /// 目标文件或目录（目录不存在且无扩展名时自动创建）
        dest: PathBuf,
        #[arg(long)]
        account: Option<String>,
    },
    /// 本地模型管理
    Models {
        #[command(subcommand)]
        command: ModelsCommand,
    },
}

#[derive(Debug, Subcommand)]
enum ModelsCommand {
    /// 下载 bge-small-zh-v1.5 ONNX 模型
    Download {
        #[arg(long)]
        model: Option<String>,
        /// 覆盖目标目录（默认 semantic.model_path）
        #[arg(long)]
        dest: Option<PathBuf>,
        /// 覆盖下载源，例如内网镜像
        #[arg(long)]
        mirror: Option<String>,
        /// 已存在文件也重新下载
        #[arg(long)]
        force: bool,
    },
}

fn main() -> ExitCode {
    let Cli {
        config,
        json,
        command,
    } = Cli::parse();
    let tui = matches!(&command, Command::Tui { .. });
    let log_dir = PathLayout::resolve(config.as_deref())
        .ok()
        .map(|layout| layout.log_dir());
    let _logging = logging::init(
        log_dir.as_deref(),
        if tui {
            StderrLogging::Off
        } else {
            StderrLogging::Info
        },
    );
    let result = match command {
        Command::Tui { account } => herald_tui::run(config, account),
        command => match tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime.block_on(run(config, json, command)),
            Err(err) => Err(err.into()),
        },
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("错误：{err:#}");
            ExitCode::FAILURE
        }
    }
}

async fn run(config: Option<PathBuf>, json: bool, command: Command) -> anyhow::Result<()> {
    if let Command::Init { force } = &command {
        return init(config.as_deref(), *force);
    }

    let mut herald = Herald::open(config.as_deref()).context("打开 Herald 失败")?;
    match command {
        Command::Init { .. } | Command::Tui { .. } => {
            unreachable!("handled before opening the async entry point")
        }
        Command::Accounts => accounts(&herald, json),
        Command::Sync {
            account,
            all,
            with_body,
            with_attachments,
            index,
            limit,
        } => {
            let options = SyncOptions {
                account,
                limit,
                all,
                with_body,
                with_attachments,
                index,
                with_embeddings: herald.semantic_enabled(),
            };
            let report = herald.sync(options).await?;
            if json {
                print_json(&report)?;
            } else {
                println!(
                    "账户 {}：新邮件 {} / 处理 {}，正文缓存 {}，附件 {}",
                    report.account,
                    report.new_count,
                    report.fetched,
                    report.bodies_cached,
                    report.attachments_stored
                );
                if let Some(uidl) = &report.last_uidl {
                    println!("最后 UIDL：{uidl}");
                }
                println!(
                    "同步时间：{}（北京时间）",
                    time::china_datetime(Some(&report.synced_at))
                );
                for error in &report.errors {
                    let uidl = error.uidl.as_deref().unwrap_or("-");
                    println!("警告 [{uidl}]：{}", error.error);
                }
            }
            Ok(())
        }
        Command::List {
            account,
            unread,
            limit,
            offset,
        } => {
            let filter = MessageFilter {
                account: account.clone(),
                limit,
                offset,
                unread_only: unread,
                ..Default::default()
            }
            .normalized();
            let (total, messages) = herald.list_messages(account.as_deref(), &filter)?;
            if json {
                print_json(&serde_json::json!({ "total": total, "messages": messages }))?;
            } else {
                print_messages(total, &messages);
            }
            Ok(())
        }
        Command::Search {
            query,
            account,
            mode,
            field,
            semantic,
            limit,
            offset,
        } => {
            let field = SearchField::from(field);
            let kind = match (semantic, mode) {
                (true, _) | (false, Some(SearchMode::Semantic)) => SearchKind::Semantic,
                (false, Some(SearchMode::Keyword)) => SearchKind::Keyword,
                (false, Some(SearchMode::Hybrid)) => SearchKind::Hybrid,
                (false, None | Some(SearchMode::Auto)) => herald.default_search_kind(field),
            };
            let result = herald.search(
                account.as_deref(),
                &query,
                kind,
                &SearchOptions {
                    field,
                    limit,
                    offset,
                },
            )?;
            if let Some(note) = &result.note {
                eprintln!("提示：{note}");
            }
            if json {
                print_json(&result)?;
            } else {
                print_messages(result.total, &result.messages);
            }
            Ok(())
        }
        Command::Mark {
            uidl,
            account,
            unread,
        } => {
            let read = !unread;
            herald.set_read(account.as_deref(), &uidl, read)?;
            if json {
                print_json(&serde_json::json!({ "uidl": uidl, "is_read": read }))?;
            } else {
                println!("{uidl} 已标记为{}", if read { "已读" } else { "未读" });
            }
            Ok(())
        }
        Command::Show {
            uidl,
            account,
            raw,
            full_body,
        } => show(&mut herald, account.as_deref(), &uidl, raw, full_body, json).await,
        Command::Status { account } => {
            let status = herald.status(account.as_deref())?;
            let errors = herald.sync_errors(account.as_deref(), 5)?;
            if json {
                print_json(&serde_json::json!({
                    "status": status,
                    "recent_errors": errors,
                }))?;
            } else {
                println!("账户：{}", status.account);
                println!(
                    "最后同步：{}",
                    status
                        .last_sync_at
                        .as_deref()
                        .map(|value| format!("{}（北京时间）", time::china_datetime(Some(value))))
                        .unwrap_or_else(|| "从未".to_string())
                );
                println!("最后 UIDL：{}", status.last_uidl.as_deref().unwrap_or("-"));
                println!("服务器邮件数：{}", status.total_messages);
                println!("本地邮件数：{}", status.local_messages);
                println!("同步错误数：{}", status.error_count);
                for error in &errors {
                    let uidl = error.uidl.as_deref().unwrap_or("-");
                    println!(
                        "  最近错误 [{uidl}] {}：{}",
                        time::china_datetime(Some(&error.at)),
                        error.error
                    );
                }
            }
            Ok(())
        }
        Command::ExportAttachment {
            uidl,
            index,
            dest,
            account,
        } => {
            let target = herald
                .export_attachment(account.as_deref(), &uidl, index, &dest)
                .await?;
            if json {
                print_json(&serde_json::json!({ "path": target }))?;
            } else {
                println!("已导出：{}", target.display());
            }
            Ok(())
        }
        Command::Models {
            command:
                ModelsCommand::Download {
                    model,
                    dest,
                    mirror,
                    force,
                },
        } => {
            let report = herald
                .download_models(&herald_core::models::DownloadOptions {
                    model,
                    dest,
                    mirror,
                    force,
                })
                .await?;
            if json {
                print_json(&report)?;
            } else {
                println!("模型：{}", report.model);
                println!("模型目录：{}", report.dest.display());
                println!("Tokenizer 目录：{}", report.tokenizer_dest.display());
                println!("下载源：{}", report.mirror);
                for file in &report.files {
                    println!(
                        "  {} {}（{} 字节）",
                        if file.skipped { "跳过" } else { "下载" },
                        file.name,
                        file.bytes
                    );
                }
            }
            Ok(())
        }
    }
}

fn init(config_override: Option<&Path>, force: bool) -> anyhow::Result<()> {
    let layout = PathLayout::resolve(config_override)?;
    let path = &layout.config_path;
    if path.exists() && !force {
        bail!("配置文件已存在：{}（如需覆盖请加 --force）", path.display());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, Config::sample())
        .with_context(|| format!("写入配置文件失败：{}", path.display()))?;
    println!("已生成配置文件：{}", path.display());
    println!("请修改账户信息后运行：herald sync");
    Ok(())
}

fn accounts(herald: &Herald, json: bool) -> anyhow::Result<()> {
    let accounts = herald.accounts()?;
    if json {
        print_json(&serde_json::json!({ "accounts": accounts }))?;
        return Ok(());
    }
    if accounts.is_empty() {
        println!("没有已入库的账户。");
        return Ok(());
    }
    for account in accounts {
        println!(
            "{}  {}@{}  最后同步：{}  邮件：{}",
            account.name,
            account.username,
            account.host,
            account
                .last_sync_at
                .as_deref()
                .map(|value| format!("{}（北京时间）", time::china_datetime(Some(value))))
                .unwrap_or_else(|| "从未".to_string()),
            account.total_messages
        );
    }
    Ok(())
}

async fn show(
    herald: &mut Herald,
    account: Option<&str>,
    uidl: &str,
    raw: bool,
    full_body: bool,
    json: bool,
) -> anyhow::Result<()> {
    if raw {
        let bytes = herald.raw_message(account, uidl).await?;
        if json {
            print_json(&serde_json::json!({
                "uidl": uidl,
                "raw": String::from_utf8_lossy(&bytes),
                "truncated": false,
            }))?;
        } else {
            print!("{}", String::from_utf8_lossy(&bytes));
        }
        return Ok(());
    }

    let detail = herald.get_message(account, uidl, true).await?;
    // Opening a message marks it read locally; `herald mark --unread` reverts.
    let is_read = herald.set_read(account, uidl, true).is_ok();
    let body_text = detail.body_text.as_deref().unwrap_or("");
    let (preview, truncated) = if full_body || json {
        (body_text, false)
    } else {
        truncate(body_text, BODY_PREVIEW_LIMIT)
    };
    if json {
        print_json(&serde_json::json!({
            "uidl": detail.header.uidl,
            "message_id": detail.header.message_id,
            "from": detail.header.from,
            "to": detail.header.to,
            "cc": detail.header.cc,
            "subject": detail.header.subject,
            "date": detail.header.date,
            "size": detail.header.size,
            "body_text": preview,
            "body_html": detail.body_html,
            "body_truncated": truncated,
            "body_cached": detail.body_cached,
            "attachments": detail.attachments,
            "is_read": is_read,
        }))?;
        return Ok(());
    }

    println!("UIDL：{}", detail.header.uidl);
    println!(
        "主题：{}",
        detail.header.subject.as_deref().unwrap_or("(无)")
    );
    println!(
        "发件人：{}",
        detail.header.from.as_deref().unwrap_or("(无)")
    );
    println!("收件人：{}", detail.header.to.join(", "));
    if !detail.header.cc.is_empty() {
        println!("抄送：{}", detail.header.cc.join(", "));
    }
    println!(
        "时间：{}（北京时间）",
        time::china_datetime(detail.header.date.as_deref())
    );
    println!("大小：{} 字节", detail.header.size);
    println!("状态：已读（本地）");
    if !detail.attachments.is_empty() {
        println!("附件：");
        for attachment in &detail.attachments {
            println!(
                "  [{}] {} ({}){}",
                attachment.index,
                attachment.filename.as_deref().unwrap_or("未命名"),
                attachment.mime_type.as_deref().unwrap_or("未知类型"),
                if attachment.has_content {
                    " 已缓存"
                } else {
                    ""
                }
            );
        }
    }
    println!("----");
    if body_text.is_empty() {
        if let Some(html) = &detail.body_html {
            println!("(纯 HTML 正文)");
            println!("{html}");
        } else {
            println!("(无正文)");
        }
    } else {
        println!("{preview}");
        if truncated {
            println!(
                "\n[正文已截断至 {} 字节，使用 --full-body 查看全部]",
                BODY_PREVIEW_LIMIT
            );
        }
    }
    Ok(())
}

fn print_messages(total: usize, messages: &[herald_core::MessageSummary]) {
    println!("共 {total} 封（● 未读）");
    for message in messages {
        let subject = message.subject.as_deref().unwrap_or("(无主题)");
        let subject = truncate(subject, 48).0;
        println!(
            "{} {}  {}  {}  {}",
            if message.is_read { " " } else { "●" },
            time::china_datetime(message.date.as_deref()),
            message.from.as_deref().unwrap_or("-"),
            subject,
            message.uidl
        );
    }
}

fn print_json<T: serde::Serialize>(value: &T) -> anyhow::Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

fn truncate(text: &str, max: usize) -> (&str, bool) {
    if text.len() <= max {
        return (text, false);
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    (&text[..end], true)
}
