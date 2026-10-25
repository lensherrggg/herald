use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{HeraldError, Result};

pub const DEFAULT_MODEL_NAME: &str = "bge-small-zh-v1.5";
pub const DEFAULT_TIMEOUT_SECS: u64 = 30;
pub const DEFAULT_CRED_CMD_TIMEOUT_SECS: u64 = 30;
pub const DEFAULT_ATTACHMENT_LIMIT: u64 = 50 * 1024 * 1024;
pub const DEFAULT_PAGE_SIZE: usize = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Security {
    Plain,
    Stls,
    Tls,
}

impl Security {
    pub fn as_str(self) -> &'static str {
        match self {
            Security::Plain => "plain",
            Security::Stls => "stls",
            Security::Tls => "tls",
        }
    }
}

fn default_timeout() -> Option<u64> {
    Some(DEFAULT_TIMEOUT_SECS)
}

fn default_cred_cmd_timeout() -> Option<u64> {
    Some(DEFAULT_CRED_CMD_TIMEOUT_SECS)
}

fn default_limit() -> usize {
    50
}

fn default_true() -> bool {
    true
}

fn default_model_name() -> String {
    DEFAULT_MODEL_NAME.to_string()
}

fn default_attachment_limit() -> u64 {
    DEFAULT_ATTACHMENT_LIMIT
}

fn default_page_size() -> usize {
    DEFAULT_PAGE_SIZE
}

/// Where an account's password comes from. Exactly one variant may be
/// configured; there is deliberately no priority order between them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredentialSource {
    Password(String),
    PasswordEnv(String),
    CredCmd(String),
}

impl CredentialSource {
    pub fn kind(&self) -> &'static str {
        match self {
            CredentialSource::Password(_) => "password",
            CredentialSource::PasswordEnv(_) => "password_env",
            CredentialSource::CredCmd(_) => "cred-cmd",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountConfig {
    pub name: String,
    pub host: String,
    pub port: u16,
    pub security: Security,
    pub username: String,
    /// Plaintext password. Deprecated: prefer `cred-cmd` or `password_env`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password_env: Option<String>,
    /// External command whose stdout's first line is the password.
    #[serde(rename = "cred-cmd", default, skip_serializing_if = "Option::is_none")]
    pub cred_cmd: Option<String>,
    #[serde(
        rename = "cred_cmd_timeout_secs",
        default = "default_cred_cmd_timeout",
        skip_serializing_if = "Option::is_none"
    )]
    pub cred_cmd_timeout_secs: Option<u64>,
    /// Keep the resolved password in process memory for this account.
    #[serde(rename = "cred_cmd_cache", default = "default_true")]
    pub cred_cmd_cache: bool,
    #[serde(default = "default_timeout", skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,
}

impl AccountConfig {
    pub fn timeout_secs(&self) -> u64 {
        self.timeout_secs.unwrap_or(DEFAULT_TIMEOUT_SECS)
    }

    pub fn cred_cmd_timeout(&self) -> u64 {
        self.cred_cmd_timeout_secs
            .unwrap_or(DEFAULT_CRED_CMD_TIMEOUT_SECS)
    }

    /// Determine the configured credential source and enforce the three-way
    /// exclusivity rule. Empty `cred-cmd` is rejected here as well so callers
    /// that skip full config validation still get a clear error.
    pub fn credential_source(&self) -> Result<CredentialSource> {
        let configured = [
            self.password.is_some(),
            self.password_env.is_some(),
            self.cred_cmd.is_some(),
        ]
        .into_iter()
        .filter(|flag| *flag)
        .count();
        match configured {
            0 => Err(HeraldError::Config(format!(
                "账户 {} 必须且只能配置 cred-cmd、password_env、password 三者之一",
                self.name
            ))),
            1 => {
                if let Some(password) = &self.password {
                    return Ok(CredentialSource::Password(password.clone()));
                }
                if let Some(var) = &self.password_env {
                    return Ok(CredentialSource::PasswordEnv(var.clone()));
                }
                let command = self.cred_cmd.as_deref().unwrap_or_default();
                if command.trim().is_empty() {
                    return Err(HeraldError::Config(format!(
                        "账户 {} 的 cred-cmd 不能为空",
                        self.name
                    )));
                }
                Ok(CredentialSource::CredCmd(command.to_string()))
            }
            _ => Err(HeraldError::Config(format!(
                "账户 {} 不能同时配置 cred-cmd、password_env、password（只能三选一）",
                self.name
            ))),
        }
    }

    fn env_password(&self, var: &str) -> Result<String> {
        match std::env::var(var) {
            Ok(value) if !value.is_empty() => Ok(value),
            Ok(_) => Err(HeraldError::Credential {
                account: self.name.clone(),
                message: format!("环境变量 {var} 为空"),
            }),
            Err(_) => Err(HeraldError::Credential {
                account: self.name.clone(),
                message: format!("环境变量 {var} 不存在"),
            }),
        }
    }

    /// Resolve the password without caching and without ever logging it. A
    /// missing/empty environment variable is a hard error (never fall back to
    /// plaintext); a failing `cred-cmd` never falls back either.
    pub fn resolve_password(&self) -> Result<String> {
        match self.credential_source()? {
            CredentialSource::Password(password) => Ok(password),
            CredentialSource::PasswordEnv(var) => self.env_password(&var),
            CredentialSource::CredCmd(command) => {
                crate::credentials::run_cred_cmd(&self.name, &command, self.cred_cmd_timeout())
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncConfig {
    #[serde(default = "default_limit")]
    pub default_limit: usize,
    /// 0 = cache every new body; a positive value caches only messages whose
    /// octet size is at most this threshold.
    #[serde(default)]
    pub auto_body_max_size: u64,
}

impl Default for SyncConfig {
    fn default() -> Self {
        Self {
            default_limit: default_limit(),
            auto_body_max_size: 0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttachmentsConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub export_dir: Option<String>,
    #[serde(default = "default_attachment_limit")]
    pub max_attachment_size_bytes: u64,
}

impl Default for AttachmentsConfig {
    fn default() -> Self {
        Self {
            export_dir: None,
            max_attachment_size_bytes: default_attachment_limit(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SemanticConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_model_name")]
    pub model_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_path: Option<String>,
    /// Directory holding tokenizer.json and its companion files. Defaults to
    /// the model directory so split and single-directory layouts both work.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokenizer_path: Option<String>,
}

impl Default for SemanticConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            model_name: default_model_name(),
            model_path: None,
            tokenizer_path: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiConfig {
    /// Messages per page in the TUI (also used for search results).
    #[serde(default = "default_page_size")]
    pub page_size: usize,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            page_size: DEFAULT_PAGE_SIZE,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    /// Local trial mode: start an embedded mock POP3 server with sample mail
    /// and a throw-away database that is removed when the process exits.
    #[serde(default)]
    pub demo: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub database: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_account: Option<String>,
    #[serde(default)]
    pub accounts: Vec<AccountConfig>,
    #[serde(default)]
    pub sync: SyncConfig,
    #[serde(default)]
    pub attachments: AttachmentsConfig,
    #[serde(default)]
    pub semantic: SemanticConfig,
    #[serde(default)]
    pub ui: UiConfig,
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Err(HeraldError::ConfigMissing(path.to_path_buf()));
        }
        let text = std::fs::read_to_string(path)?;
        let config: Config = toml::from_str(&text)?;
        config.validate()?;
        Ok(config)
    }

    pub fn from_toml(text: &str) -> Result<Self> {
        let config: Config = toml::from_str(text)?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<()> {
        if self.sync.default_limit == 0 {
            return Err(HeraldError::Config(
                "sync.default_limit 必须大于 0".to_string(),
            ));
        }
        if self.ui.page_size == 0 {
            return Err(HeraldError::Config("ui.page_size 必须大于 0".to_string()));
        }
        if self.demo {
            return Ok(());
        }
        let mut names = HashSet::new();
        for account in &self.accounts {
            if account.name.trim().is_empty() {
                return Err(HeraldError::Config("账户 name 不能为空".to_string()));
            }
            if !names.insert(account.name.as_str()) {
                return Err(HeraldError::Config(format!(
                    "账户 name 重复：{}",
                    account.name
                )));
            }
            if account.host.trim().is_empty() {
                return Err(HeraldError::Config(format!(
                    "账户 {} 的 host 不能为空",
                    account.name
                )));
            }
            if account.username.trim().is_empty() {
                return Err(HeraldError::Config(format!(
                    "账户 {} 的 username 不能为空",
                    account.name
                )));
            }
            account.credential_source()?;
            if account.timeout_secs == Some(0) {
                return Err(HeraldError::Config(format!(
                    "账户 {} 的 timeout_secs 必须大于 0",
                    account.name
                )));
            }
            if account.cred_cmd_timeout_secs == Some(0) {
                return Err(HeraldError::Config(format!(
                    "账户 {} 的 cred_cmd_timeout_secs 必须大于 0",
                    account.name
                )));
            }
        }
        if let Some(default) = &self.default_account {
            if !self.accounts.iter().any(|a| &a.name == default) {
                return Err(HeraldError::Config(format!(
                    "default_account 指向不存在的账户：{default}"
                )));
            }
        }
        Ok(())
    }

    /// Accounts that still store a plaintext password. Trial mode is excluded:
    /// its demo account is generated by Herald itself, not by the user.
    pub fn plaintext_accounts(&self) -> Vec<String> {
        if self.demo {
            return Vec::new();
        }
        self.accounts
            .iter()
            .filter(|account| account.password.is_some())
            .map(|account| account.name.clone())
            .collect()
    }

    /// One-line CLI advice; `None` when no account uses a plaintext password.
    pub fn plaintext_notice(&self) -> Option<String> {
        let names = self.plaintext_accounts();
        if names.is_empty() {
            return None;
        }
        Some(format!(
            "[安全建议] 账户 {} 使用明文 password；建议改用 cred-cmd 从系统凭据存储读取，详见 herald auth status。",
            names.join("、")
        ))
    }

    /// Account selection rules: explicit name wins; otherwise the configured
    /// default; a single configured account is unambiguous.
    pub fn resolve_account(&self, name: Option<&str>) -> Result<&AccountConfig> {
        if let Some(name) = name {
            return self
                .accounts
                .iter()
                .find(|account| account.name == name)
                .ok_or_else(|| HeraldError::Account(format!("账户不存在：{name}")));
        }
        if let Some(default) = &self.default_account {
            return self
                .accounts
                .iter()
                .find(|account| &account.name == default)
                .ok_or_else(|| HeraldError::Account(format!("默认账户不存在：{default}")));
        }
        match self.accounts.as_slice() {
            [only] => Ok(only),
            [] => Err(HeraldError::Account("未配置任何账户".to_string())),
            _ => Err(HeraldError::Account(
                "存在多个账户且未配置 default_account，请显式指定账户".to_string(),
            )),
        }
    }

    pub fn database_path(&self, config_dir: &Path) -> PathBuf {
        match &self.database {
            Some(database) => resolve_relative(config_dir, database),
            None => config_dir.join("herald.db"),
        }
    }

    pub fn models_dir(&self, config_dir: &Path) -> PathBuf {
        match &self.semantic.model_path {
            Some(path) => resolve_relative(config_dir, path),
            None => config_dir.join("models").join(&self.semantic.model_name),
        }
    }

    pub fn export_dir(&self) -> Option<PathBuf> {
        self.attachments.export_dir.as_deref().map(expand_tilde)
    }

    pub fn sample() -> String {
        r#"# Herald 配置文件
# 本地试用开关：true 时忽略下方账户，启动内置假邮箱和临时数据库，
# 进程退出后数据自动清空。适合不接真实邮箱的试用。
demo = false
# database 可省略；省略时默认为本配置文件所在目录下的 herald.db
# database = "herald.db"
default_account = "work"

[[accounts]]
name = "work"
host = "mail.intra.example.com"
port = 995
security = "tls"                 # plain | stls | tls
username = "alice"
# 凭据来源必须且只能三选一：cred-cmd / password_env / password
# cred-cmd 示例（stdout 第一行为密码，不要交互式命令）：
#   Windows PowerShell SecretManagement：
# cred-cmd = "powershell -NoProfile -Command \"(Get-Secret -Name 'herald-work' -AsPlainText)\""
#   macOS Keychain：
# cred-cmd = "security find-generic-password -s 'herald-work' -w"
#   Linux Secret Service / pass：
# cred-cmd = "secret-tool lookup service herald username alice"
# cred-cmd = "pass show herald/work/alice"
# cred_cmd_timeout_secs = 30     # 默认 30 秒
# cred_cmd_cache = true          # 默认缓存到进程内存，退出即清除
password = "plaintext"           # 已弃用：仅用于本地试用，请迁移到 cred-cmd
# password_env = "HERALD_WORK_PASSWORD"
# timeout_secs = 30

# 发件服务器（第一版不支持，预留）
# [accounts.smtp]
# host = ""
# port = 465
# security = "tls"

[sync]
default_limit = 50
auto_body_max_size = 0           # 0 = 不限；正值则仅自动缓存不大于阈值的正文

[attachments]
export_dir = "~/Downloads/herald"  # 可选；附件导出浏览器的默认起始目录
max_attachment_size_bytes = 52428800

[semantic]
enabled = true
model_name = "bge-small-zh-v1.5"
# model_path = "models/bge-small-zh-v1.5"
# tokenizer_path = "models/bge-small-zh-v1.5-tokenizer"  # 缺省与 model_path 相同

[ui]
page_size = 20                   # TUI 每页邮件数
"#
        .to_string()
    }
}

pub fn resolve_relative(base: &Path, value: &str) -> PathBuf {
    let path = PathBuf::from(value);
    if path.is_absolute() {
        path
    } else {
        base.join(path)
    }
}

pub fn expand_tilde(value: &str) -> PathBuf {
    if value == "~" {
        return dirs::home_dir().unwrap_or_else(|| PathBuf::from(value));
    }
    if let Some(rest) = value
        .strip_prefix("~/")
        .or_else(|| value.strip_prefix("~\\"))
    {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    }
    PathBuf::from(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn account(name: &str) -> AccountConfig {
        AccountConfig {
            name: name.to_string(),
            host: "h".into(),
            port: 110,
            security: Security::Plain,
            username: "u".into(),
            password: Some("p".into()),
            password_env: None,
            cred_cmd: None,
            cred_cmd_timeout_secs: None,
            cred_cmd_cache: true,
            timeout_secs: None,
        }
    }

    #[test]
    fn credential_sources_are_mutually_exclusive() {
        let mut a = account("a");
        a.password_env = Some("X".into());
        let cfg = Config {
            accounts: vec![a],
            ..Default::default()
        };
        assert!(cfg.validate().is_err());

        let mut a = account("a");
        a.cred_cmd = Some("echo p".into());
        let cfg = Config {
            accounts: vec![a],
            ..Default::default()
        };
        assert!(cfg.validate().is_err());

        let mut a = account("a");
        a.password = None;
        a.cred_cmd = Some("echo p".into());
        a.password_env = Some("X".into());
        let cfg = Config {
            accounts: vec![a],
            ..Default::default()
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn empty_cred_cmd_is_rejected() {
        let mut a = account("a");
        a.password = None;
        a.cred_cmd = Some("   ".into());
        let cfg = Config {
            accounts: vec![a],
            ..Default::default()
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn cred_cmd_defaults_and_cache_flag() {
        let cfg = Config::from_toml(
            r#"
[[accounts]]
name = "a"
host = "h"
port = 110
security = "plain"
username = "u"
cred-cmd = "echo p"
"#,
        )
        .unwrap();
        let account = &cfg.accounts[0];
        assert_eq!(account.cred_cmd_timeout(), DEFAULT_CRED_CMD_TIMEOUT_SECS);
        assert!(account.cred_cmd_cache);
        assert_eq!(
            account.credential_source().unwrap(),
            CredentialSource::CredCmd("echo p".into())
        );

        let cfg = Config::from_toml(
            r#"
[[accounts]]
name = "a"
host = "h"
port = 110
security = "plain"
username = "u"
cred-cmd = "echo p"
cred_cmd_timeout_secs = 5
cred_cmd_cache = false
"#,
        )
        .unwrap();
        let account = &cfg.accounts[0];
        assert_eq!(account.cred_cmd_timeout(), 5);
        assert!(!account.cred_cmd_cache);
        assert!(Config::from_toml(
            r#"
[[accounts]]
name = "a"
host = "h"
port = 110
security = "plain"
username = "u"
cred-cmd = "echo p"
cred_cmd_timeout_secs = 0
"#
        )
        .is_err());
    }

    #[test]
    fn plaintext_notice_lists_accounts_and_skips_demo() {
        let cfg = Config::from_toml(
            r#"
[[accounts]]
name = "a"
host = "h"
port = 110
security = "plain"
username = "u"
password = "p"
"#,
        )
        .unwrap();
        assert_eq!(cfg.plaintext_accounts(), vec!["a".to_string()]);
        assert!(cfg.plaintext_notice().unwrap().contains('a'));

        let mut demo = Config::from_toml("demo = true").unwrap();
        demo.accounts = cfg.accounts.clone();
        assert!(demo.plaintext_accounts().is_empty());
        assert!(demo.plaintext_notice().is_none());
    }

    #[test]
    fn missing_password_is_rejected() {
        let mut a = account("a");
        a.password = None;
        let cfg = Config {
            accounts: vec![a],
            ..Default::default()
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn duplicate_account_names_rejected() {
        let cfg = Config {
            accounts: vec![account("a"), account("a")],
            ..Default::default()
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn default_account_must_exist() {
        let cfg = Config {
            accounts: vec![account("a")],
            default_account: Some("b".into()),
            ..Default::default()
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn account_resolution_rules() {
        let cfg = Config {
            accounts: vec![account("a"), account("b")],
            ..Default::default()
        };
        assert!(cfg.resolve_account(None).is_err());
        assert_eq!(cfg.resolve_account(Some("b")).unwrap().name, "b");

        let cfg = Config {
            accounts: vec![account("a")],
            ..Default::default()
        };
        assert_eq!(cfg.resolve_account(None).unwrap().name, "a");
    }

    #[test]
    fn env_password_missing_is_error() {
        let mut a = account("a");
        a.password = None;
        a.password_env = Some("HERALD_TEST_DEFINITELY_MISSING".into());
        assert!(a.resolve_password().is_err());
    }

    #[test]
    fn defaults_and_relative_paths() {
        let cfg = Config::from_toml(
            r#"
[[accounts]]
name = "a"
host = "h"
port = 110
security = "plain"
username = "u"
password = "p"
"#,
        )
        .unwrap();
        assert_eq!(cfg.sync.default_limit, 50);
        assert_eq!(cfg.sync.auto_body_max_size, 0);
        assert_eq!(cfg.semantic.model_name, DEFAULT_MODEL_NAME);
        let dir = Path::new("/tmp/x");
        assert_eq!(cfg.database_path(dir), dir.join("herald.db"));
        assert_eq!(
            cfg.models_dir(dir),
            dir.join("models").join(DEFAULT_MODEL_NAME)
        );
    }

    #[test]
    fn page_size_defaults_and_validation() {
        let cfg = Config::from_toml(
            r#"
[[accounts]]
name = "a"
host = "h"
port = 110
security = "plain"
username = "u"
password = "p"
"#,
        )
        .unwrap();
        assert_eq!(cfg.ui.page_size, DEFAULT_PAGE_SIZE);
        assert!(cfg.validate().is_ok());

        let mut cfg = cfg;
        cfg.ui.page_size = 0;
        assert!(cfg.validate().is_err());

        let cfg = Config::from_toml(
            r#"
[ui]
page_size = 50

[[accounts]]
name = "a"
host = "h"
port = 110
security = "plain"
username = "u"
password = "p"
"#,
        )
        .unwrap();
        assert_eq!(cfg.ui.page_size, 50);
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn relative_database_is_resolved_against_config_dir() {
        let cfg = Config::from_toml(
            r#"
database = "data/mail.db"
[[accounts]]
name = "a"
host = "h"
port = 110
security = "plain"
username = "u"
password = "p"
"#,
        )
        .unwrap();
        assert_eq!(
            cfg.database_path(Path::new("/etc/herald")),
            Path::new("/etc/herald/data/mail.db")
        );
    }
}
