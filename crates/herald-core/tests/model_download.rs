#![cfg(feature = "semantic")]

use std::path::PathBuf;

use herald_core::config::Config;
use herald_core::models::{self, DownloadOptions};
use herald_core::paths::ResolvedPaths;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

const REPO_PREFIX: &str = "/BAAI/bge-small-zh-v1.5/resolve/main/";

fn body_for(path: &str) -> Option<&'static [u8]> {
    if path.ends_with("onnx/model.onnx") {
        Some(b"fake-onnx")
    } else if path.ends_with("tokenizer.json") {
        Some(b"fake-tokenizer")
    } else if path.ends_with("config.json") {
        Some(b"fake-config")
    } else if path.ends_with("special_tokens_map.json") {
        Some(b"fake-special-tokens")
    } else if path.ends_with("tokenizer_config.json") {
        Some(b"fake-tokenizer-config")
    } else {
        None
    }
}

async fn mirror() -> (String, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let handle = tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                break;
            };
            tokio::spawn(async move {
                let mut buffer = vec![0u8; 8192];
                let read = socket.read(&mut buffer).await.unwrap_or(0);
                let request = String::from_utf8_lossy(&buffer[..read]);
                let path = request
                    .lines()
                    .next()
                    .and_then(|line| line.split_whitespace().nth(1))
                    .unwrap_or("");
                let response = if let Some(body) = path.strip_prefix(REPO_PREFIX).and_then(body_for)
                {
                    let mut response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    )
                    .into_bytes();
                    response.extend_from_slice(body);
                    response
                } else {
                    b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                        .to_vec()
                };
                let _ = socket.write_all(&response).await;
                let _ = socket.shutdown().await;
            });
        }
    });
    (format!("http://{addr}"), handle)
}

fn paths(dir: &std::path::Path) -> ResolvedPaths {
    ResolvedPaths {
        config_path: dir.join("herald.toml"),
        config_dir: dir.to_path_buf(),
        database_path: dir.join("herald.db"),
        models_dir: dir.join("models"),
        portable: true,
        fallback_reason: None,
    }
}

#[tokio::test]
async fn downloads_every_file_from_mirror_and_skips_on_second_run() {
    let (mirror, server) = mirror().await;
    let dir = tempfile::tempdir().expect("tempdir");
    let config = Config::default();
    let paths = paths(dir.path());

    let report = models::download(
        &config,
        &paths,
        &DownloadOptions {
            mirror: Some(mirror.clone()),
            ..Default::default()
        },
    )
    .await
    .expect("download");
    assert_eq!(report.model, "bge-small-zh-v1.5");
    assert_eq!(report.files.len(), 5);
    assert!(report.files.iter().all(|file| !file.skipped));
    assert_eq!(
        std::fs::read(paths.models_dir.join("model.onnx")).unwrap(),
        b"fake-onnx"
    );
    assert!(!dir.path().join("models/model.onnx.part").exists());

    let again = models::download(
        &config,
        &paths,
        &DownloadOptions {
            mirror: Some(mirror),
            ..Default::default()
        },
    )
    .await
    .expect("second download");
    assert!(again.files.iter().all(|file| file.skipped));

    server.abort();
}

#[tokio::test]
async fn refuses_unrelated_non_empty_target_without_force() {
    let dir = tempfile::tempdir().expect("tempdir");
    let target = dir.path().join("target");
    std::fs::create_dir(&target).unwrap();
    std::fs::write(target.join("notes.txt"), b"x").unwrap();
    let err = models::download(
        &Config::default(),
        &paths(dir.path()),
        &DownloadOptions {
            dest: Some(PathBuf::from(&target)),
            mirror: Some("https://example.invalid".to_string()),
            ..Default::default()
        },
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("不像模型目录"), "{err}");
}

#[tokio::test]
async fn rejects_non_http_mirror() {
    let dir = tempfile::tempdir().expect("tempdir");
    let err = models::download(
        &Config::default(),
        &paths(dir.path()),
        &DownloadOptions {
            mirror: Some("file:///tmp/model".to_string()),
            ..Default::default()
        },
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("http"), "{err}");
}
