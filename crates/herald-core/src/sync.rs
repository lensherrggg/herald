use crate::config::{AccountConfig, Config};
use crate::error::Result;
use crate::model::{SyncErrorRecord, SyncOptions, SyncReport};
use crate::parse::{parse_message, sender_display_name};
use crate::pop3::{select_new_uidls, Pop3Client, Pop3Options};
use crate::search::index_text;
use crate::store::{now_rfc3339, Store};

#[derive(Debug, Clone)]
pub struct EmbeddingJob {
    pub message_id: i64,
    pub text: String,
}

pub struct SyncOutcome {
    pub report: SyncReport,
    pub jobs: Vec<EmbeddingJob>,
}

pub async fn run(
    config: &Config,
    store: &mut Store,
    account: &AccountConfig,
    options: &SyncOptions,
) -> Result<SyncOutcome> {
    let password = account.resolve_password()?;
    let pop3_options = Pop3Options::from_account(account, password);
    run_with_options(config, store, account, options, pop3_options).await
}

/// Connection injection point used by the integration tests (custom CA roots).
pub async fn run_with_options(
    config: &Config,
    store: &mut Store,
    account: &AccountConfig,
    options: &SyncOptions,
    pop3_options: Pop3Options,
) -> Result<SyncOutcome> {
    let account_id = store.ensure_account(account)?;
    let mut client = Pop3Client::connect(&pop3_options).await?;

    let server_uidls = client.uidl().await?;
    let known = store.known_uidls(account_id)?;
    let limit = if options.all {
        None
    } else {
        Some(options.limit.unwrap_or(config.sync.default_limit).max(1))
    };
    let new_messages = select_new_uidls(&server_uidls, &known, limit);
    let sizes = if new_messages.is_empty() {
        Default::default()
    } else {
        client.list().await.unwrap_or_default()
    };

    let threshold = config.sync.auto_body_max_size;
    let max_attachment = config.attachments.max_attachment_size_bytes;
    let mut fetched = 0usize;
    let mut new_count = 0usize;
    let mut bodies_cached = 0usize;
    let mut attachments_stored = 0usize;
    let mut errors: Vec<SyncErrorRecord> = Vec::new();
    let mut jobs: Vec<EmbeddingJob> = Vec::new();

    for (number, uidl) in &new_messages {
        fetched += 1;
        let size = sizes.get(number).copied().unwrap_or(0);
        let need_body = options.with_body
            || threshold == 0
            || size == 0
            || size <= threshold;
        match process_one(
            &mut client,
            store,
            account_id,
            *number,
            uidl,
            size,
            need_body,
            options.with_attachments,
            max_attachment,
        )
        .await
        {
            Ok(processed) => {
                new_count += 1;
                if processed.body_cached {
                    bodies_cached += 1;
                }
                attachments_stored += processed.attachments_stored;
                for warning in processed.warnings {
                    let _ = store.record_sync_error(account_id, Some(uidl), &warning);
                    errors.push(SyncErrorRecord {
                        uidl: Some(uidl.clone()),
                        error: warning,
                        at: now_rfc3339(),
                    });
                }
                if !processed.embedding_text.trim().is_empty() {
                    jobs.push(EmbeddingJob {
                        message_id: processed.message_id,
                        text: processed.embedding_text,
                    });
                }
            }
            Err(error) => {
                let message = error.to_string();
                let _ = store.record_sync_error(account_id, Some(uidl), &message);
                tracing::warn!(%uidl, %message, "单封邮件同步失败，继续处理其余邮件");
                errors.push(SyncErrorRecord {
                    uidl: Some(uidl.clone()),
                    error: message,
                    at: now_rfc3339(),
                });
            }
        }
    }

    let last_uidl = server_uidls.last().map(|(_, uidl)| uidl.clone());
    let synced_at = store.update_sync_state(account_id, last_uidl.as_deref(), server_uidls.len())?;
    let _ = client.quit().await;

    Ok(SyncOutcome {
        report: SyncReport {
            account: account.name.clone(),
            fetched,
            new_count,
            last_uidl,
            synced_at,
            bodies_cached,
            attachments_stored,
            errors,
        },
        jobs,
    })
}

struct Processed {
    message_id: i64,
    embedding_text: String,
    body_cached: bool,
    attachments_stored: usize,
    warnings: Vec<String>,
}

#[allow(clippy::too_many_arguments)]
async fn process_one(
    client: &mut Pop3Client,
    store: &mut Store,
    account_id: i64,
    number: u32,
    uidl: &str,
    size: u64,
    need_body: bool,
    with_attachments: bool,
    max_attachment: u64,
) -> Result<Processed> {
    let full_fetch = need_body || with_attachments;
    let raw = if full_fetch {
        client.retr(number).await?
    } else {
        client.fetch_headers(number).await?
    };
    let parsed = parse_message(&raw, uidl, size, with_attachments)?;
    let message_id = store.upsert_header(account_id, &parsed.header)?;

    if need_body {
        let tokens = index_text(
            parsed.header.subject.as_deref(),
            parsed.header.from.as_deref(),
            parsed.body_text.as_deref(),
        );
        store.set_body(
            message_id,
            parsed.body_text.as_deref(),
            parsed.body_html.as_deref(),
            &tokens,
        )?;
    }

    let mut warnings = Vec::new();
    let mut attachments_stored = 0usize;
    if full_fetch {
        let metas: Vec<_> = parsed
            .attachments
            .iter()
            .map(|attachment| attachment.meta.clone())
            .collect();
        store.insert_attachments(message_id, &metas)?;

        if with_attachments {
            for attachment in &parsed.attachments {
                let Some(content) = &attachment.content else {
                    continue;
                };
                if content.len() as u64 > max_attachment {
                    warnings.push(format!(
                        "附件 {} 超过大小上限 {} 字节，已跳过内容下载",
                        attachment
                            .meta
                            .filename
                            .as_deref()
                            .unwrap_or("<未命名>"),
                        max_attachment
                    ));
                    continue;
                }
                store.set_attachment_content(message_id, attachment.meta.index, content)?;
                attachments_stored += 1;
            }
        }
    }

    let embedding_text = embedding_text(
        parsed.header.subject.as_deref(),
        parsed.header.from.as_deref(),
        if need_body {
            parsed.body_text.as_deref()
        } else {
            None
        },
    );

    Ok(Processed {
        message_id,
        embedding_text,
        body_cached: need_body,
        attachments_stored,
        warnings,
    })
}

pub fn embedding_text(subject: Option<&str>, from: Option<&str>, body: Option<&str>) -> String {
    let mut text = String::new();
    if let Some(subject) = subject {
        text.push_str(subject);
        text.push('\n');
    }
    if let Some(name) = sender_display_name(from) {
        text.push_str(name);
        text.push('\n');
    }
    if let Some(body) = body {
        let body = body.trim();
        if !body.is_empty() {
            text.push_str(body);
        }
    }
    text.trim().to_string()
}

pub fn jobs_from_all_bodies(store: &Store, account_id: i64) -> Result<Vec<EmbeddingJob>> {
    let rows = store.messages_with_body(account_id)?;
    let mut jobs = Vec::with_capacity(rows.len());
    for (message_id, _uidl, subject, from, body) in rows {
        jobs.push(EmbeddingJob {
            message_id,
            text: embedding_text(subject.as_deref(), from.as_deref(), body.as_deref()),
        });
    }
    Ok(jobs)
}
