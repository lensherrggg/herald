use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use async_trait::async_trait;
use herald_core::model::{MessageFilter, SyncOptions};
use herald_core::pop3::Pop3Options;
use herald_core::{Herald, HeraldError};
use rust_mcp_sdk::macros::{mcp_tool, JsonSchema};
use rust_mcp_sdk::mcp_server::{server_runtime, McpServerOptions, ServerHandler};
use rust_mcp_sdk::schema::{
    schema_utils::CallToolError, CallToolRequestParams, CallToolResult, Implementation,
    InitializeResult, ListToolsResult, PaginatedRequestParams, ProtocolVersion, RpcError,
    ServerCapabilities, ServerCapabilitiesTools, TextContent,
};
use rust_mcp_sdk::{McpServer, StdioTransport, ToMcpServerHandler, TransportOptions};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

const DEFAULT_LIMIT: usize = 20;
const MAX_LIMIT: usize = 100;
const BODY_LIMIT: usize = 32 * 1024;
const RAW_LIMIT: usize = 256 * 1024;

#[mcp_tool(
    name = "list_accounts",
    description = "List configured mail accounts with their last sync time and total message count.",
    read_only_hint = true,
    idempotent_hint = true,
    destructive_hint = false,
    open_world_hint = false
)]
#[derive(Debug, Deserialize, Serialize, JsonSchema, Default)]
pub struct ListAccountsArgs {}

#[mcp_tool(
    name = "sync_mailbox",
    description = "Incrementally fetch new messages from the POP3 server. Read-only: never deletes or marks messages.",
    read_only_hint = false,
    idempotent_hint = true,
    destructive_hint = false,
    open_world_hint = true
)]
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct SyncMailboxArgs {
    /// Account name; defaults to the configured default account.
    #[serde(default)]
    pub account: Option<String>,
    /// Maximum number of new messages to process this call.
    #[serde(default)]
    pub limit: Option<u32>,
    /// Force fetching bodies even when auto_body_max_size would defer them.
    #[serde(default)]
    pub with_body: Option<bool>,
}

#[mcp_tool(
    name = "list_messages",
    description = "List cached message summaries, newest first.",
    read_only_hint = true,
    idempotent_hint = true,
    destructive_hint = false,
    open_world_hint = false
)]
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct ListMessagesArgs {
    #[serde(default)]
    pub account: Option<String>,
    /// Page size, 1-100 (default 20).
    #[serde(default)]
    pub limit: Option<u32>,
    /// Number of messages to skip (default 0).
    #[serde(default)]
    pub offset: Option<u32>,
    /// Case-insensitive substring match on the From header.
    #[serde(default)]
    pub from: Option<String>,
    /// Inclusive lower bound for the message date (ISO 8601 or YYYY-MM-DD).
    #[serde(default)]
    pub since: Option<String>,
    /// Inclusive upper bound for the message date (ISO 8601 or YYYY-MM-DD).
    #[serde(default)]
    pub until: Option<String>,
}

#[mcp_tool(
    name = "get_message",
    description = "Get one parsed message. The body is truncated to 32KB unless include_full_body is true. Attachments are not exposed.",
    read_only_hint = true,
    idempotent_hint = true,
    destructive_hint = false,
    open_world_hint = true
)]
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct GetMessageArgs {
    #[serde(default)]
    pub account: Option<String>,
    /// Stable POP3 UIDL of the message.
    pub uidl: String,
    /// Return the complete body instead of truncating at 32KB.
    #[serde(default)]
    pub include_full_body: Option<bool>,
}

#[mcp_tool(
    name = "get_raw_message",
    description = "Get the raw RFC822 source of one message, capped at 256KB.",
    read_only_hint = true,
    idempotent_hint = true,
    destructive_hint = false,
    open_world_hint = true
)]
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct GetRawMessageArgs {
    #[serde(default)]
    pub account: Option<String>,
    pub uidl: String,
}

#[mcp_tool(
    name = "search_messages",
    description = "Chinese-friendly keyword search over subject, sender and cached bodies.",
    read_only_hint = true,
    idempotent_hint = true,
    destructive_hint = false,
    open_world_hint = false
)]
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct SearchMessagesArgs {
    #[serde(default)]
    pub account: Option<String>,
    pub query: String,
    /// Page size, 1-100 (default 20).
    #[serde(default)]
    pub limit: Option<u32>,
    /// Number of matches to skip (default 0).
    #[serde(default)]
    pub offset: Option<u32>,
}

#[mcp_tool(
    name = "get_sync_status",
    description = "Report the last sync time, last UIDL, server/local message counts and error count.",
    read_only_hint = true,
    idempotent_hint = true,
    destructive_hint = false,
    open_world_hint = false
)]
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct GetSyncStatusArgs {
    #[serde(default)]
    pub account: Option<String>,
}

struct HeraldHandler {
    herald: Mutex<Herald>,
}

type ToolOutcome = std::result::Result<serde_json::Value, CallToolResult>;

#[async_trait]
impl ServerHandler for HeraldHandler {
    async fn handle_list_tools_request(
        &self,
        _params: Option<PaginatedRequestParams>,
        _runtime: Arc<dyn McpServer>,
    ) -> std::result::Result<ListToolsResult, RpcError> {
        Ok(ListToolsResult {
            meta: None,
            next_cursor: None,
            tools: vec![
                ListAccountsArgs::tool(),
                SyncMailboxArgs::tool(),
                ListMessagesArgs::tool(),
                GetMessageArgs::tool(),
                GetRawMessageArgs::tool(),
                SearchMessagesArgs::tool(),
                GetSyncStatusArgs::tool(),
            ],
        })
    }

    async fn handle_call_tool_request(
        &self,
        params: CallToolRequestParams,
        _runtime: Arc<dyn McpServer>,
    ) -> std::result::Result<CallToolResult, CallToolError> {
        let mut herald = self.herald.lock().await;
        let outcome: ToolOutcome = match params.name.as_str() {
            "list_accounts" => match parse_args::<ListAccountsArgs>(&params) {
                Ok(_) => run(herald.accounts()).map(|accounts| {
                    serde_json::json!({ "accounts": accounts })
                }),
                Err(result) => Err(result),
            },
            "sync_mailbox" => match parse_args::<SyncMailboxArgs>(&params) {
                Ok(args) => run_sync(&mut herald, args).await,
                Err(result) => Err(result),
            },
            "list_messages" => match parse_args::<ListMessagesArgs>(&params) {
                Ok(args) => run(list_messages(&herald, args)),
                Err(result) => Err(result),
            },
            "get_message" => match parse_args::<GetMessageArgs>(&params) {
                Ok(args) => run_get_message(&mut herald, args).await,
                Err(result) => Err(result),
            },
            "get_raw_message" => match parse_args::<GetRawMessageArgs>(&params) {
                Ok(args) => match herald.pop3_options(args.account.as_deref()) {
                    Ok(options) => run_get_raw(options, args.uidl).await,
                    Err(err) => Err(herald_error(err)),
                },
                Err(result) => Err(result),
            },
            "search_messages" => match parse_args::<SearchMessagesArgs>(&params) {
                Ok(args) => run_search(&herald, args),
                Err(result) => Err(result),
            },
            "get_sync_status" => match parse_args::<GetSyncStatusArgs>(&params) {
                Ok(args) => run_sync_status(&herald, args),
                Err(result) => Err(result),
            },
            other => return Err(CallToolError::unknown_tool(other)),
        };
        Ok(match outcome {
            Ok(value) => success_result(value),
            Err(result) => result,
        })
    }
}

fn parse_args<T: serde::de::DeserializeOwned>(
    params: &CallToolRequestParams,
) -> std::result::Result<T, CallToolResult> {
    let value = serde_json::Value::Object(params.arguments.clone().unwrap_or_default());
    serde_json::from_value(value)
        .map_err(|err| error_result("INVALID_ARGUMENT", &format!("参数无效：{err}")))
}

fn run<T>(result: herald_core::Result<T>) -> ToolOutcome
where
    T: Serialize,
{
    result
        .map(|value| serde_json::to_value(value).unwrap_or(serde_json::Value::Null))
        .map_err(herald_error)
}

async fn run_sync(herald: &mut Herald, args: SyncMailboxArgs) -> ToolOutcome {
    if args.limit == Some(0) {
        return Err(error_result("INVALID_ARGUMENT", "limit 必须大于 0"));
    }
    let options = SyncOptions {
        account: args.account,
        limit: args.limit.map(|value| value as usize),
        all: false,
        with_body: args.with_body.unwrap_or(false),
        with_attachments: false,
        index: false,
        with_embeddings: false,
    };
    match herald.sync(options).await {
        Ok(report) => Ok(serde_json::json!({
            "account": report.account,
            "fetched": report.fetched,
            "new_count": report.new_count,
            "last_uidl": report.last_uidl,
            "synced_at": report.synced_at,
        })),
        Err(err) => Err(herald_error(err)),
    }
}

fn list_messages(herald: &Herald, args: ListMessagesArgs) -> herald_core::Result<serde_json::Value> {
    let limit = page_limit(args.limit)?;
    let filter = MessageFilter {
        account: args.account.clone(),
        limit,
        offset: args.offset.unwrap_or(0) as usize,
        from: args.from,
        since: args.since,
        until: args.until,
    };
    let (total, messages) = herald.list_messages(args.account.as_deref(), &filter)?;
    Ok(serde_json::json!({ "total": total, "messages": messages }))
}

async fn run_get_message(herald: &mut Herald, args: GetMessageArgs) -> ToolOutcome {
    let detail = herald
        .get_message(args.account.as_deref(), &args.uidl, true)
        .await
        .map_err(herald_error)?;
    let body_text = detail.body_text.unwrap_or_default();
    let body_html = detail.body_html.unwrap_or_default();
    let full = args.include_full_body.unwrap_or(false);
    let (body_text, text_truncated) = if full {
        (body_text, false)
    } else {
        truncate_text(&body_text, BODY_LIMIT)
    };
    let (body_html, html_truncated) = if full {
        (body_html, false)
    } else {
        truncate_text(&body_html, BODY_LIMIT)
    };
    Ok(serde_json::json!({
        "uidl": detail.header.uidl,
        "message_id": detail.header.message_id,
        "from": detail.header.from,
        "to": detail.header.to,
        "cc": detail.header.cc,
        "subject": detail.header.subject,
        "date": detail.header.date,
        "size": detail.header.size,
        "body_text": body_text,
        "body_html": body_html,
        "body_truncated": text_truncated || html_truncated,
    }))
}

async fn run_get_raw(options: Pop3Options, uidl: String) -> ToolOutcome {
    let raw = herald_core::pop3::fetch_raw_uidl(&options, &uidl)
        .await
        .map_err(herald_error)?;
    let truncated = raw.len() > RAW_LIMIT;
    let slice = if truncated { &raw[..RAW_LIMIT] } else { &raw[..] };
    Ok(serde_json::json!({
        "uidl": uidl,
        "raw": String::from_utf8_lossy(slice),
        "truncated": truncated,
    }))
}

fn run_search(herald: &Herald, args: SearchMessagesArgs) -> ToolOutcome {
    let limit = page_limit(args.limit).map_err(herald_error)?;
    let result = herald
        .keyword_search(
            args.account.as_deref(),
            &args.query,
            limit,
            args.offset.unwrap_or(0) as usize,
        )
        .map_err(herald_error)?;
    Ok(serde_json::json!({ "total": result.total, "messages": result.messages }))
}

fn run_sync_status(herald: &Herald, args: GetSyncStatusArgs) -> ToolOutcome {
    let status = herald.status(args.account.as_deref()).map_err(herald_error)?;
    Ok(serde_json::json!({
        "account": status.account,
        "last_sync_at": status.last_sync_at,
        "last_uidl": status.last_uidl,
        "total_messages": status.total_messages,
        "local_messages": status.local_messages,
        "error_count": status.error_count,
    }))
}

fn page_limit(limit: Option<u32>) -> herald_core::Result<usize> {
    let limit = limit
        .map(|value| value as usize)
        .unwrap_or(DEFAULT_LIMIT);
    if limit == 0 || limit > MAX_LIMIT {
        return Err(HeraldError::invalid(format!(
            "limit 必须在 1-{MAX_LIMIT} 之间"
        )));
    }
    Ok(limit)
}

fn truncate_text(text: &str, max: usize) -> (String, bool) {
    if text.len() <= max {
        return (text.to_string(), false);
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    (text[..end].to_string(), true)
}

fn herald_error(err: HeraldError) -> CallToolResult {
    let code = err.code();
    error_result(code, &err.to_string())
}

fn error_result(code: &str, message: &str) -> CallToolResult {
    let value = serde_json::json!({ "code": code, "message": message });
    let mut result = CallToolResult::text_content(vec![TextContent::from(
        serde_json::to_string_pretty(&value).unwrap_or_else(|_| message.to_string()),
    )]);
    result.is_error = Some(true);
    result.structured_content = value.as_object().cloned();
    result
}

fn success_result(value: serde_json::Value) -> CallToolResult {
    let text = serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string());
    let mut result = CallToolResult::text_content(vec![TextContent::from(text)]);
    result.is_error = Some(false);
    result.structured_content = value.as_object().cloned();
    result
}

#[tokio::main]
async fn main() -> ExitCode {
    init_tracing();
    let config = match config_path() {
        Ok(config) => config,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::FAILURE;
        }
    };
    let herald = match Herald::open(config.as_deref()) {
        Ok(herald) => herald,
        Err(err) => {
            eprintln!("Herald 启动失败：{err}");
            return ExitCode::FAILURE;
        }
    };
    serve(HeraldHandler {
        herald: Mutex::new(herald),
    })
    .await
}

async fn serve(handler: HeraldHandler) -> ExitCode {
    let server_details = InitializeResult {
        server_info: Implementation {
            name: "herald".to_string(),
            version: herald_core::VERSION.to_string(),
            title: Some("Herald 邮件 MCP".to_string()),
            description: Some(
                "Read-only POP3 mailbox access for AI agents (stdio transport).".to_string(),
            ),
            icons: vec![],
            website_url: None,
        },
        capabilities: ServerCapabilities {
            tools: Some(ServerCapabilitiesTools { list_changed: None }),
            ..Default::default()
        },
        protocol_version: ProtocolVersion::V2025_11_25.into(),
        instructions: Some(
            "Herald exposes cached POP3 mail. Use list_accounts, sync_mailbox, list_messages, \
             search_messages, get_message, get_raw_message and get_sync_status. \
             Attachments are intentionally not exposed to agents."
                .to_string(),
        ),
        meta: None,
    };
    let transport = match StdioTransport::new(TransportOptions::default()) {
        Ok(transport) => transport,
        Err(err) => {
            eprintln!("无法创建 stdio 传输：{err}");
            return ExitCode::FAILURE;
        }
    };
    let server = server_runtime::create_server(McpServerOptions {
        server_details,
        transport,
        handler: handler.to_mcp_server_handler(),
        task_store: None,
        client_task_store: None,
        message_observer: None,
    });
    if let Err(err) = server.start().await {
        eprintln!("MCP 服务退出：{err}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

fn config_path() -> std::result::Result<Option<PathBuf>, String> {
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--config" => {
                return args
                    .next()
                    .map(|value| Some(PathBuf::from(value)))
                    .ok_or_else(|| "--config 缺少路径参数".to_string())
            }
            "--help" | "-h" => {
                return Err(
                    "用法：herald-mcp [--config <path>]\n通过 stdio 提供 MCP，日志写入 stderr。"
                        .to_string(),
                )
            }
            "--version" | "-V" => return Err(format!("herald-mcp {}", herald_core::VERSION)),
            other if other.starts_with("--config=") => {
                return Ok(Some(PathBuf::from(&other["--config=".len()..])))
            }
            other => return Err(format!("未知参数：{other}")),
        }
    }
    Ok(std::env::var("HERALD_CONFIG").ok().map(PathBuf::from))
}

fn init_tracing() {
    let filter = std::env::var("HERALD_LOG")
        .ok()
        .and_then(|value| tracing_subscriber::EnvFilter::try_new(value).ok())
        .or_else(|| tracing_subscriber::EnvFilter::try_from_default_env().ok())
        .unwrap_or_else(|| tracing_subscriber::EnvFilter::new("warn"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_target(false)
        .try_init();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limits_are_validated() {
        assert_eq!(page_limit(None).unwrap(), DEFAULT_LIMIT);
        assert_eq!(page_limit(Some(1)).unwrap(), 1);
        assert_eq!(page_limit(Some(100)).unwrap(), 100);
        assert!(page_limit(Some(0)).is_err());
        assert!(page_limit(Some(101)).is_err());
    }

    #[test]
    fn truncation_respects_char_boundaries() {
        let (text, truncated) = truncate_text("你好世界", 4);
        assert_eq!(text, "你");
        assert!(truncated);
        let (text, truncated) = truncate_text("abc", 10);
        assert_eq!(text, "abc");
        assert!(!truncated);
    }
}
