use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use futures_util::StreamExt;
use serde::Serialize;

use crate::config::Config;
use crate::error::{HeraldError, Result};
use crate::paths::ResolvedPaths;
use crate::semantic::MODEL_FILES;

pub const DEFAULT_MIRROR: &str = "https://huggingface.co";
pub const DEFAULT_REPO: &str = "BAAI";

#[derive(Debug, Clone, Default)]
pub struct DownloadOptions {
    pub model: Option<String>,
    pub dest: Option<PathBuf>,
    pub mirror: Option<String>,
    pub force: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct DownloadedFile {
    pub name: String,
    pub bytes: u64,
    pub skipped: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct DownloadReport {
    pub model: String,
    pub dest: PathBuf,
    pub mirror: String,
    pub files: Vec<DownloadedFile>,
}

pub async fn download(
    config: &Config,
    paths: &ResolvedPaths,
    options: &DownloadOptions,
) -> Result<DownloadReport> {
    let model = options
        .model
        .clone()
        .unwrap_or_else(|| config.semantic.model_name.clone());
    if model.trim().is_empty() || model.contains("..") {
        return Err(HeraldError::invalid("模型名称非法"));
    }
    let dest = resolve_dest(config, paths, options, &model);
    let mirror = resolve_mirror(options.mirror.as_deref());
    if !mirror.starts_with("http://") && !mirror.starts_with("https://") {
        return Err(HeraldError::invalid("下载源必须是 http(s) URL"));
    }
    ensure_target_dir(&dest, options.force)?;

    let client = reqwest::Client::builder()
        .user_agent(concat!("herald/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(900))
        .build()
        .map_err(|err| HeraldError::Http(format!("创建 HTTP 客户端失败：{err}")))?;

    let mut files = Vec::new();
    for name in MODEL_FILES {
        let target = dest.join(name);
        if !options.force && target.metadata().map(|m| m.len() > 0).unwrap_or(false) {
            files.push(DownloadedFile {
                name: name.to_string(),
                bytes: target.metadata().map(|m| m.len()).unwrap_or(0),
                skipped: true,
            });
            continue;
        }
        let bytes = download_one(&client, &mirror, &model, name, &target).await?;
        files.push(DownloadedFile {
            name: name.to_string(),
            bytes,
            skipped: false,
        });
    }

    for name in MODEL_FILES {
        let path = dest.join(name);
        if !path.metadata().map(|m| m.is_file() && m.len() > 0).unwrap_or(false) {
            return Err(HeraldError::Http(format!(
                "下载完成后校验失败：缺少或为空 {}",
                path.display()
            )));
        }
    }

    Ok(DownloadReport {
        model,
        dest,
        mirror,
        files,
    })
}

fn resolve_dest(
    config: &Config,
    paths: &ResolvedPaths,
    options: &DownloadOptions,
    model: &str,
) -> PathBuf {
    if let Some(dest) = &options.dest {
        return dest.clone();
    }
    if model == config.semantic.model_name {
        paths.models_dir.clone()
    } else {
        paths.config_dir.join("models").join(model)
    }
}

fn resolve_mirror(explicit: Option<&str>) -> String {
    explicit
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| {
            std::env::var("HF_ENDPOINT")
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        })
        .unwrap_or_else(|| DEFAULT_MIRROR.to_string())
        .trim_end_matches('/')
        .to_string()
}

fn repo_id(model: &str) -> String {
    if model.contains('/') {
        model.to_string()
    } else {
        format!("{DEFAULT_REPO}/{model}")
    }
}

pub fn model_url(mirror: &str, model: &str, remote_path: &str) -> String {
    format!(
        "{}/{}/resolve/main/{}",
        mirror.trim_end_matches('/'),
        repo_id(model),
        remote_path
    )
}

fn remote_candidates(name: &str) -> Vec<&str> {
    match name {
        "model.onnx" => vec!["onnx/model.onnx", "model.onnx"],
        _ => vec![name],
    }
}

fn ensure_target_dir(dest: &Path, force: bool) -> Result<()> {
    if dest.exists() && !dest.is_dir() {
        return Err(HeraldError::invalid(format!(
            "目标路径不是目录：{}",
            dest.display()
        )));
    }
    if dest.is_dir() {
        let entries: Vec<_> = std::fs::read_dir(dest)?.filter_map(|e| e.ok()).collect();
        let looks_like_model = MODEL_FILES
            .iter()
            .any(|name| dest.join(name).exists());
        if !entries.is_empty() && !looks_like_model && !force {
            return Err(HeraldError::invalid(format!(
                "目标目录非空且不像模型目录：{}（如确认请加 --force）",
                dest.display()
            )));
        }
    }
    std::fs::create_dir_all(dest)?;
    let probe = dest.join(format!(".herald-model-write-test-{}", std::process::id()));
    let writable = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
        .and_then(|mut file| file.write_all(b"ok"))
        .is_ok();
    let _ = std::fs::remove_file(&probe);
    if !writable {
        return Err(HeraldError::invalid(format!(
            "目标目录不可写：{}",
            dest.display()
        )));
    }
    Ok(())
}

async fn download_one(
    client: &reqwest::Client,
    mirror: &str,
    model: &str,
    name: &str,
    target: &Path,
) -> Result<u64> {
    let mut last_error = None;
    for remote in remote_candidates(name) {
        let url = model_url(mirror, model, remote);
        match fetch_to_file(client, &url, target).await {
            Ok(bytes) => return Ok(bytes),
            Err(HeraldError::Http(message)) if message.contains("404") => {
                last_error = Some(message);
                continue;
            }
            Err(err) => return Err(err),
        }
    }
    Err(HeraldError::Http(format!(
        "下载 {name} 失败：{}",
        last_error.unwrap_or_else(|| "没有可用的下载地址".to_string())
    )))
}

async fn fetch_to_file(client: &reqwest::Client, url: &str, target: &Path) -> Result<u64> {
    tracing::info!(%url, "下载模型文件");
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|err| HeraldError::Http(format!("{url} 请求失败：{err}")))?;
    let status = response.status();
    if !status.is_success() {
        return Err(HeraldError::Http(format!("{url} 返回 HTTP {status}")));
    }

    let temp = target.with_extension("part");
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let result: Result<u64> = async {
        let mut file = tokio::fs::File::create(&temp).await?;
        let mut stream = response.bytes_stream();
        let mut total = 0u64;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk
                .map_err(|err| HeraldError::Http(format!("{url} 下载中断：{err}")))?;
            tokio::io::AsyncWriteExt::write_all(&mut file, &chunk).await?;
            total += chunk.len() as u64;
        }
        tokio::io::AsyncWriteExt::flush(&mut file).await?;
        drop(file);
        if total == 0 {
            return Err(HeraldError::Http(format!("{url} 返回空文件")));
        }
        tokio::fs::rename(&temp, target).await?;
        Ok(total)
    }
    .await;
    if result.is_err() {
        let _ = tokio::fs::remove_file(&temp).await;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_urls_for_default_mirror() {
        assert_eq!(
            model_url("https://huggingface.co", "bge-small-zh-v1.5", "config.json"),
            "https://huggingface.co/BAAI/bge-small-zh-v1.5/resolve/main/config.json"
        );
        assert_eq!(
            model_url("https://mirror.example/hf/", "Org/model", "model.onnx"),
            "https://mirror.example/hf/Org/model/resolve/main/model.onnx"
        );
    }

    #[test]
    fn refuses_non_model_directory_without_force() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("notes.txt"), b"x").unwrap();
        assert!(ensure_target_dir(temp.path(), false).is_err());
        assert!(ensure_target_dir(temp.path(), true).is_ok());
    }

    #[test]
    fn accepts_empty_directory() {
        let temp = tempfile::tempdir().unwrap();
        assert!(ensure_target_dir(temp.path(), false).is_ok());
        assert!(ensure_target_dir(&temp.path().join("nested"), false).is_ok());
    }
}
