use std::collections::HashSet;
use std::path::Path;
use std::sync::Once;
use std::time::Duration;

use chrono::{SecondsFormat, Utc};
use rusqlite::{params, params_from_iter, Connection, OptionalExtension, ToSql};

use crate::config::AccountConfig;
use crate::error::{HeraldError, Result};
use crate::model::{
    AccountSummary, AttachmentData, AttachmentMeta, MessageDetail, MessageFilter, MessageHeader,
    MessageSummary, SearchField, SearchResult, SyncErrorRecord, SyncStatus,
};
use crate::search;

pub const EMBEDDING_DIM: usize = 512;

type BodyRow = (i64, String, Option<String>, Option<String>, Option<String>);
const SCHEMA_VERSION: i64 = 1;

pub fn now_rfc3339() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

pub struct Store {
    conn: Connection,
    vec_enabled: bool,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        register_sqlite_vec();
        let conn = Connection::open(path)?;
        conn.busy_timeout(Duration::from_secs(5))?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA foreign_keys=ON;
             PRAGMA synchronous=NORMAL;",
        )?;
        let mut store = Self {
            conn,
            vec_enabled: false,
        };
        store.migrate()?;
        Ok(store)
    }

    pub fn open_in_memory() -> Result<Self> {
        register_sqlite_vec();
        let conn = Connection::open_in_memory()?;
        conn.execute_batch("PRAGMA foreign_keys=ON;")?;
        let mut store = Self {
            conn,
            vec_enabled: false,
        };
        store.migrate()?;
        Ok(store)
    }

    pub fn vec_enabled(&self) -> bool {
        self.vec_enabled
    }

    fn migrate(&mut self) -> Result<()> {
        self.conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS schema_version (version INTEGER NOT NULL);
            CREATE TABLE IF NOT EXISTS accounts (
                id         INTEGER PRIMARY KEY AUTOINCREMENT,
                name       TEXT NOT NULL UNIQUE,
                host       TEXT NOT NULL,
                port       INTEGER NOT NULL,
                security   TEXT NOT NULL,
                username   TEXT NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS sync_state (
                account_id     INTEGER PRIMARY KEY REFERENCES accounts(id) ON DELETE CASCADE,
                last_uidl      TEXT,
                last_sync_at   TEXT,
                total_messages INTEGER DEFAULT 0
            );
            CREATE TABLE IF NOT EXISTS messages (
                id             INTEGER PRIMARY KEY AUTOINCREMENT,
                account_id     INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
                uidl           TEXT NOT NULL,
                message_id     TEXT,
                from_addr      TEXT,
                to_addr        TEXT,
                cc_addr        TEXT,
                subject        TEXT,
                date           TEXT,
                size           INTEGER,
                body_text      TEXT,
                body_html      TEXT,
                body_cached_at TEXT,
                is_read        INTEGER NOT NULL DEFAULT 0,
                UNIQUE(account_id, uidl)
            );
            CREATE INDEX IF NOT EXISTS idx_messages_account_date
                ON messages(account_id, date DESC);
            CREATE TABLE IF NOT EXISTS attachments (
                id         INTEGER PRIMARY KEY AUTOINCREMENT,
                message_id INTEGER NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
                idx        INTEGER NOT NULL,
                filename   TEXT,
                mime_type  TEXT,
                size       INTEGER,
                content_id TEXT,
                is_inline  INTEGER DEFAULT 0,
                content    BLOB,
                UNIQUE(message_id, idx)
            );
            CREATE TABLE IF NOT EXISTS sync_errors (
                id         INTEGER PRIMARY KEY AUTOINCREMENT,
                account_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
                uidl       TEXT,
                error      TEXT NOT NULL,
                at         TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS local_state (
                key   TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
            CREATE VIRTUAL TABLE IF NOT EXISTS messages_fts USING fts5(
                tokens,
                content='',
                contentless_delete=1,
                tokenize='unicode61'
            );
            "#,
        )?;
        self.ensure_read_column()?;

        let count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM schema_version", [], |row| row.get(0))?;
        if count == 0 {
            self.conn.execute(
                "INSERT INTO schema_version(version) VALUES (?1)",
                params![SCHEMA_VERSION],
            )?;
        }

        let vec_version: rusqlite::Result<String> =
            self.conn
                .query_row("SELECT vec_version()", [], |row| row.get(0));
        if vec_version.is_ok() {
            match self.conn.execute_batch(&format!(
                "CREATE VIRTUAL TABLE IF NOT EXISTS message_embeddings USING vec0(
                    message_id INTEGER PRIMARY KEY,
                    embedding FLOAT[{EMBEDDING_DIM}]
                );"
            )) {
                Ok(()) => {
                    self.vec_enabled = true;
                }
                Err(err) => {
                    tracing::warn!(%err, "sqlite-vec 表创建失败，语义检索将不可用");
                }
            }
        } else {
            tracing::warn!("sqlite-vec 扩展不可用，语义检索将不可用");
        }
        Ok(())
    }

    pub fn ensure_account(&self, account: &AccountConfig) -> Result<i64> {
        let now = now_rfc3339();
        self.conn.execute(
            r#"
            INSERT INTO accounts(name, host, port, security, username, created_at, updated_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)
            ON CONFLICT(name) DO UPDATE SET
                host = excluded.host,
                port = excluded.port,
                security = excluded.security,
                username = excluded.username,
                updated_at = excluded.updated_at
            "#,
            params![
                &account.name,
                &account.host,
                account.port,
                account.security.as_str(),
                &account.username,
                now
            ],
        )?;
        self.account_id(&account.name)
    }

    pub fn account_id(&self, name: &str) -> Result<i64> {
        self.conn
            .query_row(
                "SELECT id FROM accounts WHERE name = ?1",
                params![name],
                |row| row.get(0),
            )
            .optional()
            .map_err(HeraldError::from)?
            .ok_or_else(|| HeraldError::NotFound(format!("账户未入库：{name}")))
    }

    pub fn list_accounts(&self) -> Result<Vec<AccountSummary>> {
        let mut stmt = self.conn.prepare(
            r#"
            SELECT a.name, a.host, a.username, s.last_sync_at,
                   COALESCE(s.total_messages, 0)
            FROM accounts a
            LEFT JOIN sync_state s ON s.account_id = a.id
            ORDER BY a.name
            "#,
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(AccountSummary {
                name: row.get(0)?,
                host: row.get(1)?,
                username: row.get(2)?,
                last_sync_at: row.get(3)?,
                total_messages: row.get(4)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn known_uidls(&self, account_id: i64) -> Result<HashSet<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT uidl FROM messages WHERE account_id = ?1")?;
        let rows = stmt.query_map(params![account_id], |row| row.get::<_, String>(0))?;
        Ok(rows.collect::<rusqlite::Result<HashSet<_>>>()?)
    }

    pub fn upsert_header(&self, account_id: i64, header: &MessageHeader) -> Result<i64> {
        let to_addr = header.to.join(", ");
        let cc_addr = header.cc.join(", ");
        let id: i64 = self.conn.query_row(
            r#"
            INSERT INTO messages(account_id, uidl, message_id, from_addr, to_addr, cc_addr,
                                 subject, date, size)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
            ON CONFLICT(account_id, uidl) DO UPDATE SET
                message_id = excluded.message_id,
                from_addr = excluded.from_addr,
                to_addr = excluded.to_addr,
                cc_addr = excluded.cc_addr,
                subject = excluded.subject,
                date = excluded.date,
                size = excluded.size
            RETURNING id
            "#,
            params![
                account_id,
                &header.uidl,
                header.message_id.as_deref(),
                header.from.as_deref(),
                to_addr,
                cc_addr,
                header.subject.as_deref(),
                header.date.as_deref(),
                header.size as i64
            ],
            |row| row.get(0),
        )?;

        let has_body: Option<String> = self.conn.query_row(
            "SELECT body_cached_at FROM messages WHERE id = ?1",
            params![id],
            |row| row.get(0),
        )?;
        if has_body.is_none() {
            let tokens =
                search::index_text(header.subject.as_deref(), header.from.as_deref(), None);
            self.replace_fts(id, &tokens)?;
        }
        Ok(id)
    }

    pub fn set_body(
        &self,
        message_id: i64,
        body_text: Option<&str>,
        body_html: Option<&str>,
        tokens: &str,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE messages SET body_text = ?1, body_html = ?2, body_cached_at = ?3 WHERE id = ?4",
            params![body_text, body_html, now_rfc3339(), message_id],
        )?;
        self.replace_fts(message_id, tokens)?;
        Ok(())
    }

    pub fn replace_fts(&self, message_id: i64, tokens: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM messages_fts WHERE rowid = ?1",
            params![message_id],
        )?;
        self.conn.execute(
            "INSERT INTO messages_fts(rowid, tokens) VALUES (?1, ?2)",
            params![message_id, tokens],
        )?;
        Ok(())
    }

    pub fn insert_attachments(
        &self,
        message_id: i64,
        attachments: &[AttachmentMeta],
    ) -> Result<()> {
        for attachment in attachments {
            self.conn.execute(
                r#"
                INSERT INTO attachments(message_id, idx, filename, mime_type, size,
                                        content_id, is_inline)
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                ON CONFLICT(message_id, idx) DO UPDATE SET
                    filename = excluded.filename,
                    mime_type = excluded.mime_type,
                    size = excluded.size,
                    content_id = excluded.content_id,
                    is_inline = excluded.is_inline
                "#,
                params![
                    message_id,
                    attachment.index as i64,
                    attachment.filename.as_deref(),
                    attachment.mime_type.as_deref(),
                    attachment.size as i64,
                    attachment.content_id.as_deref(),
                    attachment.is_inline as i64
                ],
            )?;
        }
        self.conn.execute(
            "DELETE FROM attachments WHERE message_id = ?1 AND idx >= ?2",
            params![message_id, attachments.len() as i64],
        )?;
        Ok(())
    }

    pub fn set_attachment_content(
        &self,
        message_id: i64,
        index: usize,
        content: &[u8],
    ) -> Result<()> {
        let changed = self.conn.execute(
            "UPDATE attachments SET content = ?1 WHERE message_id = ?2 AND idx = ?3",
            params![content, message_id, index as i64],
        )?;
        if changed == 0 {
            return Err(HeraldError::not_found(format!(
                "附件不存在：message {message_id} index {index}"
            )));
        }
        Ok(())
    }

    pub fn attachments_for(&self, message_id: i64) -> Result<Vec<AttachmentMeta>> {
        let mut stmt = self.conn.prepare(
            r#"
            SELECT idx, filename, mime_type, COALESCE(size, 0), content_id,
                   COALESCE(is_inline, 0), content IS NOT NULL
            FROM attachments WHERE message_id = ?1 ORDER BY idx
            "#,
        )?;
        let rows = stmt.query_map(params![message_id], |row| {
            Ok(AttachmentMeta {
                index: row.get::<_, i64>(0)? as usize,
                filename: row.get(1)?,
                mime_type: row.get(2)?,
                size: row.get::<_, i64>(3)? as u64,
                content_id: row.get(4)?,
                is_inline: row.get::<_, i64>(5)? != 0,
                has_content: row.get::<_, i64>(6)? != 0,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn attachment(&self, account_id: i64, uidl: &str, index: usize) -> Result<AttachmentData> {
        let message_id = self.message_id(account_id, uidl)?;
        let row = self
            .conn
            .query_row(
                r#"
                SELECT idx, filename, mime_type, COALESCE(size, 0), content_id,
                       COALESCE(is_inline, 0), content
                FROM attachments WHERE message_id = ?1 AND idx = ?2
                "#,
                params![message_id, index as i64],
                |row| {
                    let content: Option<Vec<u8>> = row.get(6)?;
                    Ok((
                        AttachmentMeta {
                            index: row.get::<_, i64>(0)? as usize,
                            filename: row.get(1)?,
                            mime_type: row.get(2)?,
                            size: row.get::<_, i64>(3)? as u64,
                            content_id: row.get(4)?,
                            is_inline: row.get::<_, i64>(5)? != 0,
                            has_content: content.is_some(),
                        },
                        content,
                    ))
                },
            )
            .optional()?
            .ok_or_else(|| HeraldError::not_found(format!("附件不存在：{uidl} index {index}")))?;
        match row.1 {
            Some(content) => Ok(AttachmentData {
                meta: row.0,
                content,
            }),
            None => Err(HeraldError::not_found(format!(
                "附件内容未下载：{uidl} index {index}"
            ))),
        }
    }

    pub fn message_id(&self, account_id: i64, uidl: &str) -> Result<i64> {
        self.conn
            .query_row(
                "SELECT id FROM messages WHERE account_id = ?1 AND uidl = ?2",
                params![account_id, uidl],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| HeraldError::not_found(format!("邮件不存在：{uidl}")))
    }

    pub fn get_message(&self, account_id: i64, uidl: &str) -> Result<MessageDetail> {
        let row = self
            .conn
            .query_row(
                r#"
                SELECT id, message_id, from_addr, to_addr, cc_addr, subject, date,
                       COALESCE(size, 0), body_text, body_html,
                       body_cached_at IS NOT NULL, COALESCE(is_read, 0)
                FROM messages WHERE account_id = ?1 AND uidl = ?2
                "#,
                params![account_id, uidl],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        MessageHeader {
                            uidl: uidl.to_string(),
                            message_id: row.get(1)?,
                            from: row.get(2)?,
                            to: split_addresses(row.get::<_, Option<String>>(3)?),
                            cc: split_addresses(row.get::<_, Option<String>>(4)?),
                            subject: row.get(5)?,
                            date: row.get(6)?,
                            size: row.get::<_, i64>(7)? as u64,
                        },
                        row.get::<_, Option<String>>(8)?,
                        row.get::<_, Option<String>>(9)?,
                        row.get::<_, i64>(10)? != 0,
                        row.get::<_, i64>(11)? != 0,
                    ))
                },
            )
            .optional()?
            .ok_or_else(|| HeraldError::not_found(format!("邮件不存在：{uidl}")))?;
        let attachments = self.attachments_for(row.0)?;
        Ok(MessageDetail {
            header: row.1,
            body_text: row.2,
            body_html: row.3,
            body_cached: row.4,
            attachments,
            is_read: row.5,
        })
    }

    pub fn body_is_cached(&self, account_id: i64, uidl: &str) -> Result<bool> {
        let cached: Option<i64> = self
            .conn
            .query_row(
                "SELECT body_cached_at IS NOT NULL FROM messages WHERE account_id = ?1 AND uidl = ?2",
                params![account_id, uidl],
                |row| row.get(0),
            )
            .optional()?;
        cached
            .map(|value| value != 0)
            .ok_or_else(|| HeraldError::not_found(format!("邮件不存在：{uidl}")))
    }

    pub fn set_read(&self, account_id: i64, uidl: &str, read: bool) -> Result<()> {
        let changed = self.conn.execute(
            "UPDATE messages SET is_read = ?1 WHERE account_id = ?2 AND uidl = ?3",
            params![read as i64, account_id, uidl],
        )?;
        if changed == 0 {
            return Err(HeraldError::not_found(format!("邮件不存在：{uidl}")));
        }
        Ok(())
    }

    pub fn unread_count(&self, account_id: i64) -> Result<usize> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM messages WHERE account_id = ?1 AND is_read = 0",
            params![account_id],
            |row| row.get(0),
        )?;
        Ok(count as usize)
    }

    fn ensure_read_column(&self) -> Result<()> {
        let present: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM pragma_table_info('messages') WHERE name = 'is_read'",
            [],
            |row| row.get(0),
        )?;
        if present == 0 {
            self.conn.execute_batch(
                "ALTER TABLE messages ADD COLUMN is_read INTEGER NOT NULL DEFAULT 0;",
            )?;
        }
        Ok(())
    }

    pub fn list_messages(
        &self,
        account_id: i64,
        filter: &MessageFilter,
    ) -> Result<(usize, Vec<MessageSummary>)> {
        let mut conditions = vec!["m.account_id = ?".to_string()];
        let mut values: Vec<Box<dyn ToSql>> = vec![Box::new(account_id)];
        if let Some(from) = &filter.from {
            conditions.push("m.from_addr LIKE ? ESCAPE '\\'".to_string());
            values.push(Box::new(search::like_pattern(from)));
        }
        if let Some(since) = &filter.since {
            conditions.push("datetime(m.date) >= datetime(?)".to_string());
            values.push(Box::new(since.clone()));
        }
        if let Some(until) = &filter.until {
            conditions.push("datetime(m.date) <= datetime(?)".to_string());
            values.push(Box::new(until.clone()));
        }
        if filter.unread_only {
            conditions.push("m.is_read = 0".to_string());
        }
        let where_clause = conditions.join(" AND ");

        let total: i64 = self.conn.query_row(
            &format!("SELECT COUNT(*) FROM messages m WHERE {where_clause}"),
            params_from_iter(values.iter().map(|value| value.as_ref())),
            |row| row.get(0),
        )?;

        values.push(Box::new(filter.limit as i64));
        values.push(Box::new(filter.offset as i64));
        let order = match filter.sort {
            crate::model::MessageSort::Date => "COALESCE(datetime(m.date), '')",
            crate::model::MessageSort::From => "COALESCE(lower(m.from_addr), '')",
            crate::model::MessageSort::Subject => "COALESCE(lower(m.subject), '')",
        };
        let direction = if filter.descending { "DESC" } else { "ASC" };
        let sql = format!(
            r#"
            SELECT m.uidl, m.subject, m.from_addr, m.date, COALESCE(m.size, 0),
                   COALESCE(m.is_read, 0)
            FROM messages m
            WHERE {where_clause}
            ORDER BY {order} {direction}, m.id DESC
            LIMIT ? OFFSET ?
            "#
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(
            params_from_iter(values.iter().map(|value| value.as_ref())),
            |row| {
                Ok(MessageSummary {
                    uidl: row.get(0)?,
                    subject: row.get(1)?,
                    from: row.get(2)?,
                    date: row.get(3)?,
                    size: row.get::<_, i64>(4)? as u64,
                    is_read: row.get::<_, i64>(5)? != 0,
                })
            },
        )?;
        Ok((total as usize, rows.collect::<rusqlite::Result<Vec<_>>>()?))
    }

    pub fn keyword_search(
        &self,
        account_id: i64,
        query: &str,
        limit: usize,
        offset: usize,
    ) -> Result<SearchResult> {
        self.keyword_search_with(account_id, query, SearchField::All, limit, offset)
    }

    pub fn keyword_search_with(
        &self,
        account_id: i64,
        query: &str,
        field: SearchField,
        limit: usize,
        offset: usize,
    ) -> Result<SearchResult> {
        // Field scope is a substring filter, not an FTS column query; the
        // single-column index would otherwise match other fields too.
        if field != SearchField::All {
            return self.keyword_search_like(account_id, query, field, limit, offset);
        }
        let fts_query = search::fts_query(query);
        if !fts_query.is_empty() {
            match self.keyword_search_fts(account_id, &fts_query, limit, offset) {
                Ok(result) if result.total > 0 => return Ok(result),
                Ok(_) => {}
                Err(err) => tracing::warn!(%err, "FTS 查询失败，回退 LIKE"),
            }
        }
        self.keyword_search_like(account_id, query, field, limit, offset)
    }

    fn keyword_search_fts(
        &self,
        account_id: i64,
        fts_query: &str,
        limit: usize,
        offset: usize,
    ) -> Result<SearchResult> {
        let total: i64 = self.conn.query_row(
            r#"
            SELECT COUNT(*) FROM messages_fts
            JOIN messages m ON m.id = messages_fts.rowid
            WHERE messages_fts MATCH ?1 AND m.account_id = ?2
            "#,
            params![fts_query, account_id],
            |row| row.get(0),
        )?;
        let mut stmt = self.conn.prepare(
            r#"
            SELECT m.uidl, m.subject, m.from_addr, m.date, COALESCE(m.size, 0),
                   COALESCE(m.is_read, 0)
            FROM messages_fts
            JOIN messages m ON m.id = messages_fts.rowid
            WHERE messages_fts MATCH ?1 AND m.account_id = ?2
            ORDER BY bm25(messages_fts), m.date DESC
            LIMIT ?3 OFFSET ?4
            "#,
        )?;
        let rows = stmt.query_map(
            params![fts_query, account_id, limit as i64, offset as i64],
            |row| {
                Ok(MessageSummary {
                    uidl: row.get(0)?,
                    subject: row.get(1)?,
                    from: row.get(2)?,
                    date: row.get(3)?,
                    size: row.get::<_, i64>(4)? as u64,
                    is_read: row.get::<_, i64>(5)? != 0,
                })
            },
        )?;
        Ok(SearchResult {
            total: total as usize,
            messages: rows.collect::<rusqlite::Result<Vec<_>>>()?,
            note: None,
        })
    }

    fn keyword_search_like(
        &self,
        account_id: i64,
        query: &str,
        field: SearchField,
        limit: usize,
        offset: usize,
    ) -> Result<SearchResult> {
        let pattern = search::like_pattern(query);
        let clause = match field {
            SearchField::All => {
                "(subject LIKE ?2 ESCAPE '\\' OR from_addr LIKE ?2 ESCAPE '\\' \
                 OR body_text LIKE ?2 ESCAPE '\\')"
            }
            SearchField::From => "from_addr LIKE ?2 ESCAPE '\\'",
            SearchField::To => "(to_addr LIKE ?2 ESCAPE '\\' OR cc_addr LIKE ?2 ESCAPE '\\')",
            SearchField::Subject => "subject LIKE ?2 ESCAPE '\\'",
            SearchField::Body => "body_text LIKE ?2 ESCAPE '\\'",
        };
        let total: i64 = self.conn.query_row(
            &format!("SELECT COUNT(*) FROM messages WHERE account_id = ?1 AND {clause}"),
            params![account_id, pattern],
            |row| row.get(0),
        )?;
        let mut stmt = self.conn.prepare(&format!(
            r#"
            SELECT uidl, subject, from_addr, date, COALESCE(size, 0),
                   COALESCE(is_read, 0)
            FROM messages
            WHERE account_id = ?1 AND {clause}
            ORDER BY COALESCE(datetime(date), '') DESC, id DESC
            LIMIT ?3 OFFSET ?4
            "#
        ))?;
        let rows = stmt.query_map(
            params![account_id, pattern, limit as i64, offset as i64],
            |row| {
                Ok(MessageSummary {
                    uidl: row.get(0)?,
                    subject: row.get(1)?,
                    from: row.get(2)?,
                    date: row.get(3)?,
                    size: row.get::<_, i64>(4)? as u64,
                    is_read: row.get::<_, i64>(5)? != 0,
                })
            },
        )?;
        Ok(SearchResult {
            total: total as usize,
            messages: rows.collect::<rusqlite::Result<Vec<_>>>()?,
            note: None,
        })
    }

    pub fn update_sync_state(
        &self,
        account_id: i64,
        last_uidl: Option<&str>,
        total_messages: usize,
    ) -> Result<String> {
        let synced_at = now_rfc3339();
        self.conn.execute(
            r#"
            INSERT INTO sync_state(account_id, last_uidl, last_sync_at, total_messages)
            VALUES (?1, ?2, ?3, ?4)
            ON CONFLICT(account_id) DO UPDATE SET
                last_uidl = excluded.last_uidl,
                last_sync_at = excluded.last_sync_at,
                total_messages = excluded.total_messages
            "#,
            params![account_id, last_uidl, synced_at, total_messages as i64],
        )?;
        Ok(synced_at)
    }

    pub fn sync_status(&self, account_id: i64) -> Result<SyncStatus> {
        let (last_sync_at, last_uidl, total): (Option<String>, Option<String>, i64) = self
            .conn
            .query_row(
                "SELECT last_sync_at, last_uidl, COALESCE(total_messages, 0) FROM sync_state WHERE account_id = ?1",
                params![account_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?
            .unwrap_or((None, None, 0));
        let local = self.message_count(account_id)?;
        let errors = self.error_count(account_id)?;
        Ok(SyncStatus {
            account: String::new(),
            last_sync_at,
            last_uidl,
            total_messages: total,
            local_messages: local,
            error_count: errors,
        })
    }

    pub fn record_sync_error(
        &self,
        account_id: i64,
        uidl: Option<&str>,
        error: &str,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO sync_errors(account_id, uidl, error, at) VALUES (?1, ?2, ?3, ?4)",
            params![account_id, uidl, error, now_rfc3339()],
        )?;
        self.conn.execute(
            r#"
            DELETE FROM sync_errors
            WHERE account_id = ?1 AND id NOT IN (
                SELECT id FROM sync_errors WHERE account_id = ?1 ORDER BY id DESC LIMIT 100
            )
            "#,
            params![account_id],
        )?;
        Ok(())
    }

    pub fn recent_sync_errors(
        &self,
        account_id: i64,
        limit: usize,
    ) -> Result<Vec<SyncErrorRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT uidl, error, at FROM sync_errors WHERE account_id = ?1 ORDER BY id DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![account_id, limit as i64], |row| {
            Ok(SyncErrorRecord {
                uidl: row.get(0)?,
                error: row.get(1)?,
                at: row.get(2)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn error_count(&self, account_id: i64) -> Result<i64> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM sync_errors WHERE account_id = ?1",
            params![account_id],
            |row| row.get(0),
        )?)
    }

    pub fn message_count(&self, account_id: i64) -> Result<i64> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM messages WHERE account_id = ?1",
            params![account_id],
            |row| row.get(0),
        )?)
    }

    pub fn messages_with_body(&self, account_id: i64) -> Result<Vec<BodyRow>> {
        let mut stmt = self.conn.prepare(
            r#"
            SELECT id, uidl, subject, from_addr, body_text
            FROM messages WHERE account_id = ?1 AND body_cached_at IS NOT NULL
            ORDER BY id
            "#,
        )?;
        let rows = stmt.query_map(params![account_id], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn get_local_state(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT value FROM local_state WHERE key = ?1",
                params![key],
                |row| row.get(0),
            )
            .optional()?)
    }

    pub fn set_local_state(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            r#"
            INSERT INTO local_state(key, value) VALUES (?1, ?2)
            ON CONFLICT(key) DO UPDATE SET value = excluded.value
            "#,
            params![key, value],
        )?;
        Ok(())
    }

    pub fn embedding_exists(&self, message_id: i64) -> Result<bool> {
        if !self.vec_enabled {
            return Ok(false);
        }
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM message_embeddings WHERE message_id = ?1",
            params![message_id],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    pub fn upsert_embedding(&self, message_id: i64, embedding: &[f32]) -> Result<()> {
        if !self.vec_enabled {
            return Err(HeraldError::Model("sqlite-vec 不可用".to_string()));
        }
        if embedding.len() != EMBEDDING_DIM {
            return Err(HeraldError::Model(format!(
                "embedding 维度错误：期望 {EMBEDDING_DIM}，实际 {}",
                embedding.len()
            )));
        }
        let blob = embedding_to_blob(embedding);
        self.conn.execute(
            "DELETE FROM message_embeddings WHERE message_id = ?1",
            params![message_id],
        )?;
        self.conn.execute(
            "INSERT INTO message_embeddings(message_id, embedding) VALUES (?1, ?2)",
            params![message_id, blob],
        )?;
        Ok(())
    }

    pub fn semantic_search(
        &self,
        account_id: i64,
        embedding: &[f32],
        limit: usize,
    ) -> Result<SearchResult> {
        if !self.vec_enabled {
            return Err(HeraldError::Model(
                "sqlite-vec 扩展不可用，无法进行语义检索".to_string(),
            ));
        }
        let query = serde_json::to_string(embedding)
            .map_err(|err| HeraldError::Model(format!("向量序列化失败：{err}")))?;
        let k = (limit.saturating_mul(10)).max(100) as i64;
        let mut stmt = self.conn.prepare(
            r#"
            SELECT m.uidl, m.subject, m.from_addr, m.date, COALESCE(m.size, 0),
                   COALESCE(m.is_read, 0), x.distance
            FROM (
                SELECT message_id, distance FROM message_embeddings
                WHERE embedding MATCH ?1 AND k = ?2
            ) x
            JOIN messages m ON m.id = x.message_id
            WHERE m.account_id = ?3
            ORDER BY x.distance
            LIMIT ?4
            "#,
        )?;
        let rows = stmt.query_map(params![query, k, account_id, limit as i64], |row| {
            Ok(MessageSummary {
                uidl: row.get(0)?,
                subject: row.get(1)?,
                from: row.get(2)?,
                date: row.get(3)?,
                size: row.get::<_, i64>(4)? as u64,
                is_read: row.get::<_, i64>(5)? != 0,
            })
        })?;
        let messages = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(SearchResult {
            total: messages.len(),
            messages,
            note: None,
        })
    }

    pub fn clear_embeddings(&self, account_id: i64) -> Result<()> {
        if !self.vec_enabled {
            return Ok(());
        }
        self.conn.execute(
            "DELETE FROM message_embeddings WHERE message_id IN (SELECT id FROM messages WHERE account_id = ?1)",
            params![account_id],
        )?;
        Ok(())
    }

    pub fn first_uidl(&self, account_id: i64) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT uidl FROM messages WHERE account_id = ?1 ORDER BY id LIMIT 1",
                params![account_id],
                |row| row.get(0),
            )
            .optional()?)
    }
}

fn split_addresses(value: Option<String>) -> Vec<String> {
    value
        .map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|part| !part.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn embedding_to_blob(embedding: &[f32]) -> Vec<u8> {
    let mut blob = Vec::with_capacity(embedding.len() * 4);
    for value in embedding {
        blob.extend_from_slice(&value.to_le_bytes());
    }
    blob
}

static VEC_INIT: Once = Once::new();

#[allow(clippy::missing_transmute_annotations)]
fn register_sqlite_vec() {
    VEC_INIT.call_once(|| unsafe {
        rusqlite::ffi::sqlite3_auto_extension(Some(std::mem::transmute(
            sqlite_vec::sqlite3_vec_init as *const (),
        )));
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn message_columns(path: &Path) -> i64 {
        let conn = rusqlite::Connection::open(path).unwrap();
        conn.query_row(
            "SELECT COUNT(*) FROM pragma_table_info('messages') WHERE name = 'is_read'",
            [],
            |row| row.get(0),
        )
        .unwrap()
    }

    #[test]
    fn legacy_database_gains_is_read_column() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("legacy.db");
        {
            let store = Store::open(&path).unwrap();
            store
                .conn
                .execute_batch("ALTER TABLE messages DROP COLUMN is_read;")
                .unwrap();
        }
        assert_eq!(message_columns(&path), 0);

        {
            let _store = Store::open(&path).unwrap();
            assert_eq!(message_columns(&path), 1);
        }

        // Re-opening an already migrated database is a no-op.
        let _store = Store::open(&path).unwrap();
        assert_eq!(message_columns(&path), 1);
    }
}
