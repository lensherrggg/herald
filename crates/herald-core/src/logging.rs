use std::path::Path;

use tracing_appender::non_blocking::{NonBlocking, WorkerGuard};
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::fmt::writer::BoxMakeWriter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer};

const MAX_LOG_FILES: usize = 14;

/// Controls the extra stderr output. The TUI turns it off so log lines never
/// corrupt the alternate screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StderrLogging {
    Off,
    Info,
    Warn,
}

/// Holds the background writer open; dropping it flushes pending records.
pub struct LoggingGuard {
    _file: Option<WorkerGuard>,
}

/// Install the global subscriber. `log_dir` defaults to daily files named
/// `herald.YYYY-MM-DD.log`; when it is unavailable logs only reach stderr.
pub fn init(log_dir: Option<&Path>, stderr: StderrLogging) -> LoggingGuard {
    let (writer, file_guard) = match log_dir.and_then(start_file_writer) {
        Some((writer, guard)) => (BoxMakeWriter::new(writer), Some(guard)),
        None => (BoxMakeWriter::new(std::io::sink), None),
    };

    let file_layer = tracing_subscriber::fmt::layer()
        .with_writer(writer)
        .with_ansi(false)
        .with_filter(env_filter("info"));
    let registry = tracing_subscriber::registry().with(file_layer);
    let initialized = match stderr {
        StderrLogging::Off => registry.try_init(),
        StderrLogging::Info | StderrLogging::Warn => registry
            .with(
                tracing_subscriber::fmt::layer()
                    .with_writer(std::io::stderr)
                    .with_target(false)
                    .with_filter(env_filter(stderr.level())),
            )
            .try_init(),
    };
    if let Err(err) = initialized {
        eprintln!("日志初始化失败：{err}");
    }

    if let Some(dir) = log_dir {
        tracing::debug!(dir = %dir.display(), "本地日志目录");
    }
    LoggingGuard { _file: file_guard }
}

impl StderrLogging {
    fn level(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Info => "info",
            Self::Warn => "warn",
        }
    }
}

fn env_filter(fallback: &str) -> EnvFilter {
    std::env::var("HERALD_LOG")
        .ok()
        .and_then(|value| EnvFilter::try_new(value).ok())
        .or_else(|| EnvFilter::try_from_default_env().ok())
        .unwrap_or_else(|| EnvFilter::new(fallback))
}

fn start_file_writer(log_dir: &Path) -> Option<(NonBlocking, WorkerGuard)> {
    if let Err(err) = std::fs::create_dir_all(log_dir) {
        eprintln!("无法创建日志目录 {}：{err}", log_dir.display());
        return None;
    }
    let appender = match RollingFileAppender::builder()
        .rotation(Rotation::DAILY)
        .filename_prefix("herald")
        .filename_suffix("log")
        .max_log_files(MAX_LOG_FILES)
        .build(log_dir)
    {
        Ok(appender) => appender,
        Err(err) => {
            eprintln!("无法创建日志文件 {}：{err}", log_dir.display());
            return None;
        }
    };
    Some(tracing_appender::non_blocking(appender))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_log_records_to_a_daily_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let guard = init(Some(dir.path()), StderrLogging::Off);
        tracing::info!(marker = "herald-log-test", "hello from the logger");
        drop(guard);

        let entry = std::fs::read_dir(dir.path())
            .expect("read log dir")
            .filter_map(Result::ok)
            .find(|entry| {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                name.starts_with("herald.") && name.ends_with(".log")
            })
            .expect("daily log file");
        let content = std::fs::read_to_string(entry.path()).expect("read log file");
        assert!(content.contains("herald-log-test"), "{content}");
        assert!(content.contains("hello from the logger"), "{content}");
    }
}
