//! Password sources: only the first stdout line of `cred-cmd` is used, and a
//! resolved password may be cached for the lifetime of the process.

use std::collections::HashMap;
use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::config::{AccountConfig, CredentialSource};
use crate::error::{HeraldError, Result};

const STDOUT_CAP: usize = 64 * 1024;
const STDERR_CAP: usize = 8 * 1024;
const POLL_INTERVAL: Duration = Duration::from_millis(10);

/// Process-local password cache. Keys are account names; entries never touch
/// the disk and disappear when the process exits.
#[derive(Debug, Default)]
pub struct CredentialResolver {
    cache: Mutex<HashMap<String, String>>,
}

impl CredentialResolver {
    pub fn new() -> Self {
        Self::default()
    }

    /// Resolve an account's password, consulting the cache only for
    /// `cred-cmd` sources (environment variables are cheap and always fresh).
    pub fn resolve(&self, account: &AccountConfig) -> Result<String> {
        match account.credential_source()? {
            CredentialSource::Password(password) => Ok(password),
            CredentialSource::PasswordEnv(_) => account.resolve_password(),
            CredentialSource::CredCmd(command) => {
                if account.cred_cmd_cache {
                    if let Some(password) = self.cached(&account.name) {
                        return Ok(password);
                    }
                }
                let password = run_cred_cmd(&account.name, &command, account.cred_cmd_timeout())?;
                if account.cred_cmd_cache {
                    self.store(&account.name, &password);
                }
                Ok(password)
            }
        }
    }

    /// Drop every cached password, e.g. after the configuration changes.
    pub fn clear(&self) {
        if let Ok(mut cache) = self.cache.lock() {
            cache.clear();
        }
    }

    fn cached(&self, account: &str) -> Option<String> {
        self.cache
            .lock()
            .ok()
            .and_then(|cache| cache.get(account).cloned())
    }

    fn store(&self, account: &str, password: &str) {
        if let Ok(mut cache) = self.cache.lock() {
            cache.insert(account.to_string(), password.to_string());
        }
    }
}

/// Run `cred-cmd` through the platform shell and return the first stdout line.
/// Errors intentionally contain neither stdout nor stderr contents.
pub fn run_cred_cmd(account: &str, command: &str, timeout_secs: u64) -> Result<String> {
    let mut child = shell_command(command)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| spawn_error(account, err))?;

    let stdout_reader = child
        .stdout
        .take()
        .map(|stdout| spawn_reader(stdout, STDOUT_CAP));
    let stderr_reader = child
        .stderr
        .take()
        .map(|stderr| spawn_reader(stderr, STDERR_CAP));

    let timeout = Duration::from_secs(timeout_secs.max(1));
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    break None;
                }
                thread::sleep(POLL_INTERVAL);
            }
            Err(err) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(HeraldError::Io(err));
            }
        }
    };

    let Some(status) = status else {
        // A grandchild may still hold the pipe open; never block a timeout on
        // the reader threads.
        drop(stdout_reader);
        drop(stderr_reader);
        return Err(HeraldError::CredCmdTimeout {
            account: account.to_string(),
            secs: timeout.as_secs(),
        });
    };

    let stdout = join_reader(stdout_reader);
    let stderr = join_reader(stderr_reader);

    if !status.success() {
        let code = status.code().unwrap_or(-1);
        if command_not_found(code, &stderr) {
            return Err(HeraldError::CredCmdNotFound {
                account: account.to_string(),
            });
        }
        return Err(HeraldError::CredCmdFailed {
            account: account.to_string(),
            code,
        });
    }

    let stdout = String::from_utf8_lossy(&stdout);
    first_line_password(&stdout).ok_or_else(|| HeraldError::CredCmdEmptyOutput {
        account: account.to_string(),
    })
}

fn spawn_error(account: &str, err: std::io::Error) -> HeraldError {
    if err.kind() == std::io::ErrorKind::NotFound {
        HeraldError::CredCmdNotFound {
            account: account.to_string(),
        }
    } else {
        HeraldError::CredCmdFailed {
            account: account.to_string(),
            code: -1,
        }
    }
}

fn spawn_reader<R: Read + Send + 'static>(mut reader: R, cap: usize) -> JoinHandle<Vec<u8>> {
    thread::spawn(move || drain_capped(&mut reader, cap))
}

fn drain_capped(reader: &mut impl Read, cap: usize) -> Vec<u8> {
    let mut kept = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        match reader.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(read) => {
                if kept.len() < cap {
                    let room = cap - kept.len();
                    kept.extend_from_slice(&chunk[..read.min(room)]);
                }
            }
        }
    }
    kept
}

fn join_reader(handle: Option<JoinHandle<Vec<u8>>>) -> Vec<u8> {
    handle
        .and_then(|handle| handle.join().ok())
        .unwrap_or_default()
}

/// Take the first line and strip only the line ending: leading/trailing spaces
/// inside the line are part of the password. Blank output is a failure.
fn first_line_password(stdout: &str) -> Option<String> {
    let line = stdout.split('\n').next().unwrap_or_default();
    let line = line.strip_suffix('\r').unwrap_or(line);
    if line.trim().is_empty() {
        None
    } else {
        Some(line.to_string())
    }
}

/// Unix shells report "command not found" as 127; PowerShell returns 1 and
/// explains itself on stderr.
fn command_not_found(code: i32, stderr: &[u8]) -> bool {
    if cfg!(unix) && code == 127 {
        return true;
    }
    if !cfg!(windows) {
        return false;
    }
    let stderr = String::from_utf8_lossy(stderr).to_ascii_lowercase();
    stderr.contains("commandnotfoundexception")
        || stderr.contains("is not recognized as")
        || stderr.contains("无法识别")
        || stderr.contains("不是内部或外部命令")
}

fn shell_command(command: &str) -> Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let mut cmd = Command::new("powershell");
        cmd.args(["-NoProfile", "-Command", command]);
        cmd.creation_flags(CREATE_NO_WINDOW);
        cmd
    }
    #[cfg(not(windows))]
    {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", command]);
        cmd
    }
}

/// Human-readable shell name for `auth status`; never exposes the command.
pub fn shell_label() -> &'static str {
    if cfg!(windows) {
        "PowerShell"
    } else {
        "sh"
    }
}

/// Best-effort check that the platform shell can be launched. The command
/// itself is validated by `herald auth test`.
pub fn shell_available() -> bool {
    let mut cmd = shell_command("exit 0");
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let Ok(mut child) = cmd.spawn() else {
        return false;
    };
    // Windows PowerShell can take a couple of seconds to start under load.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) if Instant::now() < deadline => thread::sleep(POLL_INTERVAL),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
            Err(_) => return false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Security, DEFAULT_CRED_CMD_TIMEOUT_SECS};

    fn account(name: &str, cred_cmd: Option<String>, cache: bool) -> AccountConfig {
        AccountConfig {
            name: name.to_string(),
            host: "h".to_string(),
            port: 110,
            security: Security::Plain,
            username: "u".to_string(),
            password: None,
            password_env: None,
            cred_cmd,
            cred_cmd_timeout_secs: Some(DEFAULT_CRED_CMD_TIMEOUT_SECS),
            cred_cmd_cache: cache,
            timeout_secs: None,
        }
    }

    fn simple_command() -> String {
        if cfg!(windows) {
            "Write-Output 'secret'".to_string()
        } else {
            "printf 'secret'".to_string()
        }
    }

    #[test]
    fn first_line_is_used_and_line_endings_are_stripped() {
        assert_eq!(
            first_line_password("secret\nsecond\n"),
            Some("secret".to_string())
        );
        assert_eq!(
            first_line_password("secret\r\nsecond\r\n"),
            Some("secret".to_string())
        );
        assert_eq!(
            first_line_password("secret\r"),
            Some("secret".to_string()),
            "a lone trailing CR must not survive"
        );
        assert_eq!(
            first_line_password("  padded  \n"),
            Some("  padded  ".to_string()),
            "inner spaces are part of the password"
        );
        assert_eq!(first_line_password(""), None);
        assert_eq!(first_line_password("\r\nsecond"), None);
        assert_eq!(first_line_password("   \nsecret"), None);
    }

    #[test]
    fn runs_the_platform_shell() {
        let password = run_cred_cmd("work", &simple_command(), 10).unwrap();
        assert_eq!(password, "secret");
    }

    #[cfg(unix)]
    #[test]
    fn unix_reports_missing_command_and_nonzero_exit() {
        let missing = run_cred_cmd("work", "herald_missing_command_xyz", 10).unwrap_err();
        assert!(matches!(missing, HeraldError::CredCmdNotFound { .. }));
        assert_eq!(missing.code(), "cred_cmd_not_found");

        let failed = run_cred_cmd("work", "exit 3", 10).unwrap_err();
        assert!(matches!(failed, HeraldError::CredCmdFailed { code: 3, .. }));
        assert_eq!(failed.code(), "cred_cmd_failed");
    }

    #[cfg(windows)]
    #[test]
    fn windows_reports_missing_command_and_nonzero_exit() {
        let missing =
            run_cred_cmd("work", "herald-command-that-does-not-exist-xyz", 10).unwrap_err();
        assert!(
            matches!(missing, HeraldError::CredCmdNotFound { .. }),
            "{missing:?}"
        );
        let failed = run_cred_cmd("work", "exit 3", 10).unwrap_err();
        assert!(matches!(failed, HeraldError::CredCmdFailed { code: 3, .. }));
    }

    #[test]
    fn timeout_is_reported() {
        let command = if cfg!(windows) {
            "Start-Sleep -Seconds 5"
        } else {
            "sleep 5"
        };
        let started = Instant::now();
        let error = run_cred_cmd("work", command, 1).unwrap_err();
        assert!(matches!(error, HeraldError::CredCmdTimeout { .. }));
        assert_eq!(error.code(), "cred_cmd_timeout");
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn empty_output_is_reported() {
        let error = run_cred_cmd("work", "", 10).unwrap_err();
        assert!(matches!(error, HeraldError::CredCmdEmptyOutput { .. }));
        assert_eq!(error.code(), "cred_cmd_empty_output");
    }

    #[test]
    fn cache_hits_within_the_same_resolver() {
        let temp = tempfile::tempdir().unwrap();
        let counter = temp.path().join("counter.txt");
        let counter = counter.to_string_lossy().replace('\\', "/");
        let command = if cfg!(windows) {
            format!("Write-Output 'secret'; Add-Content -LiteralPath '{counter}' -Value 'hit'")
        } else {
            format!("printf 'secret'; echo hit >> '{counter}'")
        };

        let resolver = CredentialResolver::new();
        let account = account("cached", Some(command), true);
        assert_eq!(resolver.resolve(&account).unwrap(), "secret");
        assert_eq!(resolver.resolve(&account).unwrap(), "secret");
        let content = std::fs::read_to_string(&counter).unwrap();
        assert_eq!(content.lines().count(), 1, "second resolve must hit cache");
    }

    #[test]
    fn cache_can_be_disabled() {
        let temp = tempfile::tempdir().unwrap();
        let counter = temp.path().join("counter.txt");
        let counter = counter.to_string_lossy().replace('\\', "/");
        let command = if cfg!(windows) {
            format!("Write-Output 'secret'; Add-Content -LiteralPath '{counter}' -Value 'hit'")
        } else {
            format!("printf 'secret'; echo hit >> '{counter}'")
        };

        let resolver = CredentialResolver::new();
        let account = account("uncached", Some(command), false);
        resolver.resolve(&account).unwrap();
        resolver.resolve(&account).unwrap();
        let content = std::fs::read_to_string(&counter).unwrap();
        assert_eq!(content.lines().count(), 2);
    }

    #[test]
    fn shell_is_available_in_this_environment() {
        assert!(shell_available(), "{} must be runnable", shell_label());
    }
}
