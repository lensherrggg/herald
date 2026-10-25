use herald_core::config::{AccountConfig, Security};
use herald_core::model::{AttachmentMeta, MessageFilter, MessageHeader, SearchField};
use herald_core::store::{Store, EMBEDDING_DIM};

fn account() -> AccountConfig {
    AccountConfig {
        name: "work".to_string(),
        host: "h".to_string(),
        port: 110,
        security: Security::Plain,
        username: "u".to_string(),
        password: Some("p".to_string()),
        password_env: None,
        cred_cmd: None,
        cred_cmd_timeout_secs: None,
        cred_cmd_cache: true,
        timeout_secs: None,
    }
}

fn header(uidl: &str, subject: &str) -> MessageHeader {
    MessageHeader {
        uidl: uidl.to_string(),
        message_id: Some(format!("<{uidl}@example.com>")),
        from: Some("张三 <zhangsan@example.com>".to_string()),
        to: vec!["alice@example.com".to_string()],
        cc: vec![],
        subject: Some(subject.to_string()),
        date: Some("2024-09-03T10:00:00+08:00".to_string()),
        size: 100,
    }
}

#[test]
fn fts_index_is_replaced_and_searchable() {
    let store = Store::open_in_memory().unwrap();
    let account_id = store.ensure_account(&account()).unwrap();
    let first = store
        .upsert_header(account_id, &header("u1", "季度项目进度"))
        .unwrap();
    let second = store
        .upsert_header(account_id, &header("u2", "午餐通知"))
        .unwrap();

    store
        .set_body(
            first,
            Some("项目进度正常"),
            None,
            &herald_core::search::index_text(
                Some("季度项目进度"),
                Some("张三"),
                Some("项目进度正常"),
            ),
        )
        .unwrap();
    store
        .set_body(second, Some("今天吃面"), None, "午餐 通知")
        .unwrap();

    let result = store.keyword_search(account_id, "项目", 10, 0).unwrap();
    assert_eq!(result.total, 1);
    assert_eq!(result.messages[0].uidl, "u1");

    let result = store.keyword_search(account_id, "正常", 10, 0).unwrap();
    assert_eq!(result.total, 1);

    // Tokens separated by a space must hit FTS rather than the LIKE fallback.
    let result = store
        .keyword_search(account_id, "项目 进度", 10, 0)
        .unwrap();
    assert_eq!(result.total, 1);

    // Replacing the body must not duplicate or keep stale FTS terms.
    store
        .set_body(first, Some("无关内容"), None, "无关 内容")
        .unwrap();
    let result = store.keyword_search(account_id, "正常", 10, 0).unwrap();
    assert_eq!(result.total, 0);

    let result = store.keyword_search(account_id, "无关", 10, 0).unwrap();
    assert_eq!(result.total, 1);
}

#[test]
fn field_scoped_search_limits_matches() {
    let store = Store::open_in_memory().unwrap();
    let account_id = store.ensure_account(&account()).unwrap();
    let first = store
        .upsert_header(account_id, &header("u1", "季度项目进度"))
        .unwrap();
    let mut second_header = header("u2", "午餐通知");
    second_header.from = Some("李四 <lisi@example.com>".to_string());
    second_header.to = vec!["bob@example.com".to_string()];
    let second = store.upsert_header(account_id, &second_header).unwrap();
    store
        .set_body(first, Some("正文提到项目"), None, "正文 提到 项目")
        .unwrap();
    store
        .set_body(second, Some("项目聚餐"), None, "项目 聚餐")
        .unwrap();

    let from = store
        .keyword_search_with(account_id, "张三", SearchField::From, 10, 0)
        .unwrap();
    assert_eq!(from.total, 1);
    assert_eq!(from.messages[0].uidl, "u1");

    let to = store
        .keyword_search_with(account_id, "bob", SearchField::To, 10, 0)
        .unwrap();
    assert_eq!(to.total, 1);
    assert_eq!(to.messages[0].uidl, "u2");

    let subject = store
        .keyword_search_with(account_id, "项目", SearchField::Subject, 10, 0)
        .unwrap();
    assert_eq!(subject.total, 1);
    assert_eq!(subject.messages[0].uidl, "u1");

    let body = store
        .keyword_search_with(account_id, "项目", SearchField::Body, 10, 0)
        .unwrap();
    assert_eq!(body.total, 2);

    let all = store
        .keyword_search_with(account_id, "项目", SearchField::All, 10, 0)
        .unwrap();
    assert_eq!(all.total, 2);
}

#[test]
fn pagination_and_filters_work() {
    let store = Store::open_in_memory().unwrap();
    let account_id = store.ensure_account(&account()).unwrap();
    for index in 1..=5 {
        let mut message = header(&format!("u{index}"), &format!("主题 {index}"));
        message.from = Some(if index % 2 == 0 {
            "李四 <lisi@example.com>".to_string()
        } else {
            "张三 <zhangsan@example.com>".to_string()
        });
        message.date = Some(format!("2024-09-0{}T10:00:00+08:00", index));
        store.upsert_header(account_id, &message).unwrap();
    }

    let (total, page) = store
        .list_messages(
            account_id,
            &MessageFilter {
                limit: 2,
                offset: 0,
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(total, 5);
    assert_eq!(page.len(), 2);
    assert_eq!(page[0].uidl, "u5");

    let (total, page) = store
        .list_messages(
            account_id,
            &MessageFilter {
                limit: 10,
                offset: 4,
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(total, 5);
    assert_eq!(page.len(), 1);

    let (total, _) = store
        .list_messages(
            account_id,
            &MessageFilter {
                limit: 10,
                offset: 0,
                from: Some("lisi".to_string()),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(total, 2);
}

#[test]
fn read_state_is_local_and_filters_unread() {
    let store = Store::open_in_memory().unwrap();
    let account_id = store.ensure_account(&account()).unwrap();
    store
        .upsert_header(account_id, &header("u1", "第一封"))
        .unwrap();
    store
        .upsert_header(account_id, &header("u2", "第二封"))
        .unwrap();

    assert_eq!(store.unread_count(account_id).unwrap(), 2);
    let (_, page) = store
        .list_messages(
            account_id,
            &MessageFilter {
                limit: 10,
                offset: 0,
                unread_only: true,
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(page.len(), 2);
    assert!(page.iter().all(|message| !message.is_read));

    store.set_read(account_id, "u1", true).unwrap();
    assert_eq!(store.unread_count(account_id).unwrap(), 1);
    let detail = store.get_message(account_id, "u1").unwrap();
    assert!(detail.is_read);

    let (total, page) = store
        .list_messages(
            account_id,
            &MessageFilter {
                limit: 10,
                offset: 0,
                unread_only: true,
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(total, 1);
    assert_eq!(page[0].uidl, "u2");

    // Toggling back to unread restores the count.
    store.set_read(account_id, "u1", false).unwrap();
    assert_eq!(store.unread_count(account_id).unwrap(), 2);

    // Unknown UIDLs are reported instead of silently ignored.
    let missing = store.set_read(account_id, "missing", true).unwrap_err();
    assert!(matches!(missing, herald_core::HeraldError::NotFound(_)));
    assert_eq!(store.unread_count(account_id).unwrap(), 2);
}

#[test]
fn attachment_metadata_and_blob_round_trip() {
    let store = Store::open_in_memory().unwrap();
    let account_id = store.ensure_account(&account()).unwrap();
    let message_id = store
        .upsert_header(account_id, &header("u1", "附件"))
        .unwrap();
    store
        .insert_attachments(
            message_id,
            &[AttachmentMeta {
                index: 0,
                filename: Some("report.pdf".to_string()),
                mime_type: Some("application/pdf".to_string()),
                size: 9,
                content_id: Some("cid".to_string()),
                is_inline: false,
                has_content: false,
            }],
        )
        .unwrap();

    let missing = store.attachment(account_id, "u1", 0).unwrap_err();
    assert!(matches!(missing, herald_core::HeraldError::NotFound(_)));

    store
        .set_attachment_content(message_id, 0, b"%PDF-1.4\n")
        .unwrap();
    let data = store.attachment(account_id, "u1", 0).unwrap();
    assert_eq!(data.content, b"%PDF-1.4\n");
    assert!(data.meta.has_content);
    assert_eq!(data.meta.filename.as_deref(), Some("report.pdf"));
}

#[test]
fn sqlite_vec_loads_and_returns_nearest_neighbour() {
    let store = Store::open_in_memory().unwrap();
    assert!(
        store.vec_enabled(),
        "sqlite-vec must be registered and vec0 available"
    );
    let account_id = store.ensure_account(&account()).unwrap();
    let first = store
        .upsert_header(account_id, &header("u1", "项目"))
        .unwrap();
    let second = store
        .upsert_header(account_id, &header("u2", "午餐"))
        .unwrap();

    let mut a = vec![0.0f32; EMBEDDING_DIM];
    a[0] = 1.0;
    let mut b = vec![0.0f32; EMBEDDING_DIM];
    b[1] = 1.0;
    store.upsert_embedding(first, &a).unwrap();
    store.upsert_embedding(second, &b).unwrap();

    let result = store.semantic_search(account_id, &a, 2).unwrap();
    assert_eq!(result.messages.len(), 2);
    assert_eq!(result.messages[0].uidl, "u1");

    // Upsert replaces instead of duplicating.
    store.upsert_embedding(first, &b).unwrap();
    let result = store.semantic_search(account_id, &b, 5).unwrap();
    assert_eq!(result.messages.len(), 2);

    store.clear_embeddings(account_id).unwrap();
    let result = store.semantic_search(account_id, &a, 5).unwrap();
    assert!(result.messages.is_empty());
}
