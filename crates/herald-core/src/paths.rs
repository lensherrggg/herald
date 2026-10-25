use std::io::Write as _;
use std::path::{Path, PathBuf};

use crate::config::Config;
use crate::error::{HeraldError, Result};

/// Where the instance keeps its files. Computed before the config is read, so
/// it can also serve `herald init`.
#[derive(Debug, Clone)]
pub struct PathLayout {
    pub config_path: PathBuf,
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
    pub portable: bool,
    pub fallback_reason: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ResolvedPaths {
    pub config_path: PathBuf,
    pub config_dir: PathBuf,
    pub database_path: PathBuf,
    pub models_dir: PathBuf,
    pub portable: bool,
    pub fallback_reason: Option<String>,
}

impl PathLayout {
    /// Default resolution: exe directory when writable, otherwise the
    /// platform user directory (never silent).
    pub fn resolve(config_override: Option<&Path>) -> Result<Self> {
        if let Some(path) = config_override {
            let path = absolute(path)?;
            let dir = path
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| PathBuf::from("."));
            return Ok(Self {
                config_path: path,
                config_dir: dir.clone(),
                data_dir: dir,
                portable: false,
                fallback_reason: None,
            });
        }

        let exe = std::env::current_exe().map_err(|err| {
            HeraldError::Config(format!("无法定位可执行文件所在目录：{err}"))
        })?;
        let exe_dir = exe
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        Self::resolve_from_exe_dir(&exe_dir)
    }

    /// Split out for tests and for callers that need custom placements.
    pub fn resolve_from_exe_dir(exe_dir: &Path) -> Result<Self> {
        let exe_dir = absolute(exe_dir)?;
        if dir_is_writable(&exe_dir) {
            return Ok(Self {
                config_path: exe_dir.join("herald.toml"),
                config_dir: exe_dir.clone(),
                data_dir: exe_dir,
                portable: true,
                fallback_reason: None,
            });
        }

        let (config_dir, data_dir) = platform_dirs();
        let reason = format!(
            "可执行文件目录不可写：{}，已降级到用户目录（配置 {}, 数据 {}）",
            exe_dir.display(),
            config_dir.join("herald.toml").display(),
            data_dir.display()
        );
        tracing::warn!("{reason}");
        Ok(Self {
            config_path: config_dir.join("herald.toml"),
            config_dir,
            data_dir,
            portable: false,
            fallback_reason: Some(reason),
        })
    }

    pub fn complete(&self, config: &Config) -> ResolvedPaths {
        let database_path = match &config.database {
            Some(database) => crate::config::resolve_relative(&self.config_dir, database),
            None => self.data_dir.join("herald.db"),
        };
        let models_dir = match &config.semantic.model_path {
            Some(path) => crate::config::resolve_relative(&self.config_dir, path),
            None => self
                .data_dir
                .join("models")
                .join(&config.semantic.model_name),
        };
        ResolvedPaths {
            config_path: self.config_path.clone(),
            config_dir: self.config_dir.clone(),
            database_path,
            models_dir,
            portable: self.portable,
            fallback_reason: self.fallback_reason.clone(),
        }
    }
}

impl ResolvedPaths {
    pub fn describe(&self) -> Vec<String> {
        let mut lines = vec![
            format!("配置文件：{}", self.config_path.display()),
            format!("数据库：{}", self.database_path.display()),
            format!("模型目录：{}", self.models_dir.display()),
            format!(
                "存放模式：{}",
                if self.portable {
                    "便携（随可执行文件目录）"
                } else {
                    "用户目录（非便携）"
                }
            ),
        ];
        if let Some(reason) = &self.fallback_reason {
            lines.push(format!("降级说明：{reason}"));
        }
        lines
    }
}

fn platform_dirs() -> (PathBuf, PathBuf) {
    let config_base = dirs::config_dir()
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| PathBuf::from("."));
    let data_base = dirs::data_dir()
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| PathBuf::from("."));
    (config_base.join("herald"), data_base.join("herald"))
}

fn absolute(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    let cwd = std::env::current_dir()
        .map_err(|err| HeraldError::Config(format!("无法读取当前目录：{err}")))?;
    Ok(cwd.join(path))
}

fn dir_is_writable(dir: &Path) -> bool {
    let name = format!(
        ".herald-write-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );
    let probe = dir.join(name);
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
    {
        Ok(mut file) => {
            let _ = file.write_all(b"ok");
            drop(file);
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    #[test]
    fn portable_dir_is_used_when_writable() {
        let temp = tempfile::tempdir().unwrap();
        let layout = PathLayout::resolve_from_exe_dir(temp.path()).unwrap();
        assert!(layout.portable);
        assert_eq!(layout.config_path, temp.path().join("herald.toml"));
        assert!(!temp.path().join(".herald-write-test").exists());
    }

    #[test]
    fn unwritable_location_falls_back_with_reason() {
        let temp = tempfile::tempdir().unwrap();
        let not_a_dir = temp.path().join("file");
        std::fs::write(&not_a_dir, b"x").unwrap();
        let layout = PathLayout::resolve_from_exe_dir(&not_a_dir).unwrap();
        assert!(!layout.portable);
        assert!(layout.fallback_reason.is_some());
        assert!(layout.config_path.ends_with("herald.toml"));
    }

    #[test]
    fn explicit_config_path_drives_defaults() {
        let temp = tempfile::tempdir().unwrap();
        let cfg_path = temp.path().join("custom.toml");
        let layout = PathLayout::resolve(Some(&cfg_path)).unwrap();
        let cfg = Config::default();
        let paths = layout.complete(&cfg);
        assert_eq!(paths.database_path, temp.path().join("herald.db"));
        assert_eq!(
            paths.models_dir,
            temp.path().join("models").join(crate::config::DEFAULT_MODEL_NAME)
        );
    }

    #[test]
    fn configured_database_and_model_paths_are_relative_to_config_dir() {
        let temp = tempfile::tempdir().unwrap();
        let cfg = Config::from_toml(
            r#"
database = "state/mail.db"
[semantic]
model_path = "weights/zh"
"#,
        )
        .unwrap();
        let paths = PathLayout::resolve_from_exe_dir(temp.path())
            .unwrap()
            .complete(&cfg);
        assert_eq!(paths.database_path, temp.path().join("state/mail.db"));
        assert_eq!(paths.models_dir, temp.path().join("weights/zh"));
    }

    #[test]
    fn describe_mentions_fallback() {
        let paths = ResolvedPaths {
            config_path: PathBuf::from("c"),
            config_dir: PathBuf::from("c"),
            database_path: PathBuf::from("d"),
            models_dir: PathBuf::from("m"),
            portable: false,
            fallback_reason: Some("because".into()),
        };
        assert!(paths.describe().iter().any(|l| l.contains("because")));
    }
}
