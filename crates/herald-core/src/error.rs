use std::path::PathBuf;

/// All errors surfaced by the core. Variants map to stable machine-readable
/// codes so the MCP layer can return structured failures without panicking.
#[derive(Debug, thiserror::Error)]
pub enum HeraldError {
    #[error("配置错误：{0}")]
    Config(String),

    #[error("配置错误：配置文件不存在：{0}")]
    ConfigMissing(PathBuf),

    #[error("账户错误：{0}")]
    Account(String),

    #[error("认证失败：{0}")]
    Auth(String),

    #[error("POP3 错误：{0}")]
    Pop3(String),

    #[error("连接超时（{0} 秒）")]
    Timeout(u64),

    #[error("TLS 错误：{0}")]
    Tls(String),

    #[error("数据库错误：{0}")]
    Db(#[from] rusqlite::Error),

    #[error("IO 错误：{0}")]
    Io(#[from] std::io::Error),

    #[error("未找到：{0}")]
    NotFound(String),

    #[error("参数错误：{0}")]
    InvalidArgument(String),

    #[error("模型不可用：{0}")]
    Model(String),

    #[error("下载失败：{0}")]
    Http(String),

    #[error("不支持的操作：{0}")]
    Unsupported(String),

    #[error("配置解析失败：{0}")]
    Toml(#[from] toml::de::Error),
}

pub type Result<T> = std::result::Result<T, HeraldError>;

impl HeraldError {
    /// Stable error code for MCP structured errors.
    pub fn code(&self) -> &'static str {
        match self {
            HeraldError::Config(_) | HeraldError::ConfigMissing(_) | HeraldError::Toml(_) => {
                "CONFIG_ERROR"
            }
            HeraldError::Account(_) => "ACCOUNT_ERROR",
            HeraldError::Auth(_) => "AUTH_ERROR",
            HeraldError::Pop3(_) => "POP3_ERROR",
            HeraldError::Timeout(_) => "TIMEOUT",
            HeraldError::Tls(_) => "TLS_ERROR",
            HeraldError::Db(_) => "DB_ERROR",
            HeraldError::Io(_) => "IO_ERROR",
            HeraldError::NotFound(_) => "NOT_FOUND",
            HeraldError::InvalidArgument(_) => "INVALID_ARGUMENT",
            HeraldError::Model(_) => "MODEL_ERROR",
            HeraldError::Http(_) => "HTTP_ERROR",
            HeraldError::Unsupported(_) => "UNSUPPORTED",
        }
    }

    pub fn invalid(message: impl Into<String>) -> Self {
        HeraldError::InvalidArgument(message.into())
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        HeraldError::NotFound(message.into())
    }
}
