//! Built-in trial mode: an in-process mock POP3 server with sample Chinese
//! mail, backed by a temporary database that is removed when the process ends.

use std::net::SocketAddr;
use std::path::Path;
use std::sync::mpsc;
use std::thread::{self, JoinHandle};

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use encoding_rs::{BIG5, GBK};
use tempfile::TempDir;
use tokio::sync::oneshot;

use crate::config::{AccountConfig, Config, Security};
use crate::error::{HeraldError, Result};
use crate::mock_pop3::{MockConfig, MockMessage, MockPop3Server};
use crate::model::SyncOptions;
use crate::store::Store;
use crate::sync::{self, EmbeddingJob};

pub const DEMO_ACCOUNT: &str = "demo";
pub const SAMPLE_MESSAGES: usize = 25;
pub const MAX_SAMPLE_MESSAGES: usize = 29;

/// Keeps the embedded POP3 server and the temporary data directory alive for
/// as long as the owning [`crate::Herald`] lives.
pub struct DemoSession {
    addr: SocketAddr,
    dir: TempDir,
    shutdown: Option<oneshot::Sender<()>>,
    worker: Option<JoinHandle<()>>,
}

impl DemoSession {
    fn start() -> Result<Self> {
        let (ready_tx, ready_rx) = mpsc::channel();
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let worker = thread::Builder::new()
            .name("herald-demo".to_string())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(err) => {
                        let _ = ready_tx.send(Err(format!("创建假邮箱运行时失败：{err}")));
                        return;
                    }
                };
                runtime.block_on(async move {
                    let server = match MockPop3Server::start(MockConfig {
                        messages: sample_messages(SAMPLE_MESSAGES),
                        ..Default::default()
                    })
                    .await
                    {
                        Ok(server) => server,
                        Err(err) => {
                            let _ = ready_tx.send(Err(format!("启动内置假邮箱失败：{err}")));
                            return;
                        }
                    };
                    let _ = ready_tx.send(Ok(server.addr()));
                    let _ = shutdown_rx.await;
                    drop(server);
                });
            })
            .map_err(HeraldError::Io)?;
        let addr = ready_rx
            .recv()
            .map_err(|_| HeraldError::Config("内置假邮箱线程意外退出".to_string()))?
            .map_err(HeraldError::Config)?;
        let dir = tempfile::Builder::new().prefix("herald-demo-").tempdir()?;
        Ok(Self {
            addr,
            dir,
            shutdown: Some(shutdown_tx),
            worker: Some(worker),
        })
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn data_dir(&self) -> &Path {
        self.dir.path()
    }
}

impl Drop for DemoSession {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// Point `config` at the embedded server and a fresh temporary database.
pub fn configure(config: &mut Config) -> Result<DemoSession> {
    let session = DemoSession::start()?;
    let data_dir = session.data_dir();
    let exports = data_dir.join("exports");
    std::fs::create_dir_all(&exports)?;
    config.accounts = vec![AccountConfig {
        name: DEMO_ACCOUNT.to_string(),
        host: "127.0.0.1".to_string(),
        port: session.addr.port(),
        security: Security::Plain,
        username: "test".to_string(),
        password: Some("secret".to_string()),
        password_env: None,
        cred_cmd: None,
        cred_cmd_timeout_secs: None,
        cred_cmd_cache: true,
        timeout_secs: Some(5),
    }];
    config.default_account = Some(DEMO_ACCOUNT.to_string());
    config.database = Some(path_text(&data_dir.join("herald.db")));
    if config.attachments.export_dir.is_none() {
        config.attachments.export_dir = Some(path_text(&exports));
    }
    Ok(session)
}

/// Run the first sync on a scratch thread. Demo mode is reachable from both
/// sync and async callers, so it must not create a nested tokio runtime on the
/// calling thread.
pub fn initial_sync(config: &Config, mut store: Store) -> Result<(Store, Vec<EmbeddingJob>)> {
    let config = config.clone();
    let account = config.resolve_account(None)?.clone();
    let options = SyncOptions {
        account: None,
        limit: None,
        all: true,
        with_body: true,
        with_attachments: true,
        index: false,
        with_embeddings: false,
    };
    let (tx, rx) = mpsc::channel();
    thread::Builder::new()
        .name("herald-demo-sync".to_string())
        .spawn(move || {
            let result = (|| -> Result<Vec<EmbeddingJob>> {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()?;
                let password = account.resolve_password()?;
                let outcome = runtime
                    .block_on(sync::run(&config, &mut store, &account, &options, password))?;
                Ok(outcome.jobs)
            })();
            let _ = tx.send((store, result));
        })
        .map_err(HeraldError::Io)?;
    let (store, result) = rx
        .recv()
        .map_err(|_| HeraldError::Config("内置假邮箱同步线程意外退出".to_string()))?;
    Ok((store, result?))
}

fn path_text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// Newest-first sample mail covering encodings, HTML, attachments and
/// boundary cases. Older filler messages are prepended to reach `count`.
pub fn sample_messages(count: usize) -> Vec<MockMessage> {
    let count = count.clamp(1, MAX_SAMPLE_MESSAGES);
    let curated = curated_messages();
    if count <= curated.len() {
        return curated[curated.len() - count..].to_vec();
    }
    let mut messages = filler_messages(count - curated.len());
    messages.extend(curated);
    messages
}

/// Newest-first dates for fillers, all older than the curated set.
const FILLER_DATES: [&str; 16] = [
    "Fri, 30 Aug 2024 08:30:00 +0800",
    "Thu, 29 Aug 2024 08:30:00 +0800",
    "Wed, 28 Aug 2024 08:30:00 +0800",
    "Tue, 27 Aug 2024 08:30:00 +0800",
    "Mon, 26 Aug 2024 08:30:00 +0800",
    "Sun, 25 Aug 2024 08:30:00 +0800",
    "Sat, 24 Aug 2024 08:30:00 +0800",
    "Fri, 23 Aug 2024 08:30:00 +0800",
    "Thu, 22 Aug 2024 08:30:00 +0800",
    "Wed, 21 Aug 2024 08:30:00 +0800",
    "Tue, 20 Aug 2024 08:30:00 +0800",
    "Mon, 19 Aug 2024 08:30:00 +0800",
    "Sun, 18 Aug 2024 08:30:00 +0800",
    "Sat, 17 Aug 2024 08:30:00 +0800",
    "Fri, 16 Aug 2024 08:30:00 +0800",
    "Thu, 15 Aug 2024 08:30:00 +0800",
];

/// Older, deliberately plain mail used to fill out pages.
fn filler_messages(count: usize) -> Vec<MockMessage> {
    (0..count)
        .map(|index| {
            let n = index + 1;
            let stamp = FILLER_DATES[count - 1 - index].to_string();
            let uidl = format!("filler-{n:02}");
            let (from, subject_line, body) = match n % 4 {
                0 => (
                    "项目办公室 <pm@example.com>",
                    format!("第 {n} 期项目周报"),
                    "本周完成接口联调，遗留 2 个低优先级问题。\r\n下周计划进入回归测试。\r\n"
                        .to_string(),
                ),
                1 => (
                    "会议助手 <calendar@example.com>",
                    format!("会议提醒：每周例会（第 {n} 次）"),
                    "本周例会定于周四上午 9:30，地点 2 楼小会议室。\r\n请提前提交议题。\r\n"
                        .to_string(),
                ),
                2 => (
                    "IT 服务台 <it@example.com>",
                    format!("系统维护通知（第 {n} 期）"),
                    "周六 22:00 至 23:00 进行数据库例行维护，期间同步可能短暂中断。\r\n"
                        .to_string(),
                ),
                _ => (
                    "李四 <lisi@example.com>",
                    format!("日程变更：评审改到第 {n} 周"),
                    "原定评审因会议室冲突顺延，具体时间另行通知。\r\n".to_string(),
                ),
            };
            MockMessage::new(
                &uidl,
                Mail::new(
                    &uidl,
                    from,
                    "demo@example.com",
                    subject(&subject_line),
                    stamp,
                )
                .text("text/plain; charset=utf-8", "8bit", &body),
            )
        })
        .collect()
}

const TEXT_ATTACHMENT: &str =
    "附件说明：\r\n本文件由 Herald 演示模式生成，仅用于演示附件导出。\r\n";

/// Smallest valid PNG (1x1 transparent pixel), used as an inline image.
const PNG_1PX_B64: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR4nGP4z8AAAAMBAQDJ/pLvAAAAAElFTkSuQmCC";

/// Enough of a PDF to look like one when exported; content is irrelevant here.
const FAKE_PDF: &[u8] = b"%PDF-1.4\r\n\
1 0 obj<</Type/Catalog/Pages 2 0 R>>endobj\r\n\
2 0 obj<</Type/Pages/Kids[3 0 R]/Count 1>>endobj\r\n\
3 0 obj<</Type/Page/Parent 2 0 R/MediaBox[0 0 595 842]>>endobj\r\n\
trailer<</Root 1 0 R>>\r\n%%EOF\r\n";

/// One message per common shape: plain/HTML/alternative, UTF-8/GBK/BIG5,
/// 8bit/base64/quoted-printable, missing subject, empty body, long headers,
/// nested multiparts and attachments.
fn curated_messages() -> Vec<MockMessage> {
    let mut messages = Vec::with_capacity(12);

    messages.push(MockMessage::new(
        "demo-01",
        Mail::new(
            "demo-01",
            "王小明 <wang.xiaoming@example.com>",
            "demo@example.com, lisi@example.com",
            subject("9 月运维值班安排"),
            "Mon, 02 Sep 2024 09:10:00 +0800",
        )
        .cc("zhaoliu@example.com")
        .text(
            "text/plain; charset=utf-8",
            "8bit",
            "各位好：\r\n\r\n\
             9 月运维值班安排如下，请提前确认：\r\n\
             - 第一周：王小明\r\n\
             - 第二周：李四\r\n\
             - 第三周：赵六\r\n\r\n\
             如需调班，请在本周五前回复。\r\n",
        ),
    ));

    messages.push(MockMessage::new(
        "demo-02",
        Mail::new(
            "demo-02",
            "Herald 通知 <noreply@example.com>",
            "demo@example.com",
            rfc2047_utf8("账号安全提醒：请更新登录密码"),
            "Tue, 03 Sep 2024 10:20:00 +0800",
        )
        .text(
            "text/plain; charset=utf-8",
            "8bit",
            "检测到你的账号已连续使用同一密码 90 天。\r\n\
             请前往设置页更新密码，并开启二次验证。\r\n",
        ),
    ));

    messages.push(MockMessage::new(
        "demo-03",
        Mail::new(
            "demo-03",
            "Herald 团队 <team@example.com>",
            "demo@example.com",
            subject("Herald 0.1 发布说明（HTML 正文）"),
            "Wed, 04 Sep 2024 11:05:00 +0800",
        )
        .text(
            "text/html; charset=utf-8",
            "8bit",
            "<html><body style=\"font-family: sans-serif\">\
             <h1>Herald 0.1 发布说明</h1>\
             <ul>\
             <li>只读 POP3 增量同步</li>\
             <li>中文邮件与附件解析</li>\
             <li>关键词与语义检索</li>\
             </ul>\
             <p>详情见 <a href=\"https://example.com/herald\">发布页面</a>。</p>\
             </body></html>\r\n",
        ),
    ));

    messages.push(MockMessage::new(
        "demo-04",
        Mail::new(
            "demo-04",
            "张伟 <zhang.wei@example.com>",
            "lisi@example.com, demo@example.com",
            subject("下周三项目评审会议"),
            "Thu, 05 Sep 2024 14:00:00 +0800",
        )
        .multipart(
            "alternative",
            &[
                Part::text(
                    "text/plain; charset=utf-8",
                    "李四你好：\r\n\
                     下周三下午 2 点安排项目评审，地点在 3 楼会议室，请提前准备演示材料。\r\n",
                ),
                Part::text(
                    "text/html; charset=utf-8",
                    "<html><body><p>李四你好：</p>\
                     <p>下周三下午 <b>2 点</b>安排项目评审，地点在 3 楼会议室，请提前准备演示材料。</p>\
                     </body></html>\r\n",
                ),
            ],
        ),
    ));

    let mail = Mail::new(
        "demo-05",
        "陈静 <chen.jing@example.com>",
        "demo@example.com",
        Some(raw_8bit("供应商对账结果（GBK 编码）", GBK)),
        "Fri, 06 Sep 2024 09:45:00 +0800",
    );
    let body = raw_8bit(
        "本月供应商对账已完成，差异金额 0.00 元。\r\n请财务在 9 月 10 日前确认并归档。\r\n",
        GBK,
    );
    messages.push(MockMessage::new(
        "demo-05",
        mail.with_content("text/plain; charset=gbk", Some("8bit"), &body),
    ));

    let mail = Mail::new(
        "demo-06",
        "林志明 <lin.zhiming@example.com>",
        "demo@example.com",
        rfc2047_charset("颱風假通知（BIG5 編碼）", BIG5),
        "Sat, 07 Sep 2024 08:30:00 +0800",
    );
    let body = raw_8bit(
        "因颱風來襲，明日停止上班上課。\r\n請各單位提前完成防颱準備，並留意後續公告。\r\n",
        BIG5,
    );
    messages.push(MockMessage::new(
        "demo-06",
        mail.with_content("text/plain; charset=big5", Some("8bit"), &body),
    ));

    let mail = Mail::new(
        "demo-07",
        "数据组 <data@example.com>",
        "demo@example.com",
        subject("季度数据汇总（Base64 正文）"),
        "Sun, 08 Sep 2024 16:20:00 +0800",
    );
    let encoded = wrap_base64(
        "二季度数据汇总：\r\n\
         - 同步邮件 1,286 封\r\n\
         - 附件总量 356 MB\r\n\
         - 平均同步耗时 42 秒\r\n\r\n\
         以上数据来自监控面板，如有疑问请联系数据组。\r\n"
            .as_bytes(),
    );
    messages.push(MockMessage::new(
        "demo-07",
        mail.with_content(
            "text/plain; charset=utf-8",
            Some("base64"),
            encoded.as_bytes(),
        ),
    ));

    let mail = Mail::new(
        "demo-08",
        "客户成功 <support@example.com>",
        "demo@example.com",
        subject("客户反馈摘要（Quoted-Printable）"),
        "Mon, 09 Sep 2024 10:00:00 +0800",
    );
    let body = quoted_printable(
        "客户反馈摘要：\r\n\
         1. 界面响应速度明显提升。\r\n\
         2. 希望增加导出为 CSV 的功能。\r\n\
         3. 之前提到的编码问题（A=B 测试）已经修复。\r\n",
    );
    messages.push(MockMessage::new(
        "demo-08",
        mail.with_content(
            "text/plain; charset=utf-8",
            Some("quoted-printable"),
            body.as_bytes(),
        ),
    ));

    messages.push(MockMessage::new(
        "demo-09",
        Mail::new(
            "demo-09",
            "系统监控 <monitor@example.com>",
            "demo@example.com",
            None,
            "Tue, 10 Sep 2024 03:00:00 +0800",
        )
        .text("text/plain; charset=utf-8", "8bit", ""),
    ));

    let long_subject = "关于 2024 年第四季度信息系统安全自查工作安排及材料报送要求的补充通知（请各部门务必于 9 月 20 日前反馈）";
    let long_body = format!(
        "{}\r\n\r\ntoken: {}\r\n",
        "这是一段很长且没有换行的中文正文，用于检查终端在窄窗口下的折行表现。".repeat(4),
        "abcdefghij".repeat(12)
    );
    messages.push(MockMessage::new(
        "demo-10",
        Mail::new(
            "demo-10",
            "信息安全办公室 <security@example.com>",
            "demo@example.com",
            folded_subject(long_subject),
            "Wed, 11 Sep 2024 15:40:00 +0800",
        )
        .text("text/plain; charset=utf-8", "8bit", &long_body),
    ));

    messages.push(MockMessage::new(
        "demo-11",
        Mail::new(
            "demo-11",
            "财务共享中心 <invoice@example.com>",
            "demo@example.com",
            subject("电子发票（仅附件）"),
            "Thu, 12 Sep 2024 09:15:00 +0800",
        )
        .multipart(
            "mixed",
            &[Part::attachment(Attachment::encoded_name(
                "发票.pdf",
                "application/pdf",
                FAKE_PDF,
            ))],
        ),
    ));

    let report = Mail::new(
        "demo-12",
        "项目办公室 <pm@example.com>",
        "demo@example.com",
        subject("本周项目进度汇报（含附件）"),
        "Fri, 13 Sep 2024 17:30:00 +0800",
    );
    let binary: Vec<u8> = (0..=255).collect();
    let png = BASE64.decode(PNG_1PX_B64).expect("valid png");
    let alternative = Part::multipart(
        "alternative",
        "=herald-demo-alt",
        &[
            Part::text(
                "text/plain; charset=utf-8",
                "本周项目进度正常，测试覆盖率稳定在 82%。\r\n\
                 附件包含说明、二进制样例和内嵌图片。\r\n",
            ),
            Part::text(
                "text/html; charset=utf-8",
                "<html><body>\
                 <p>本周项目进度正常，测试覆盖率稳定在 <b>82%</b>。</p>\
                 <p><img src=\"cid:logo@herald\" alt=\"logo\" width=\"1\" height=\"1\"></p>\
                 <p>附件包含说明文件和二进制样例。</p>\
                 </body></html>\r\n",
            ),
        ],
    );
    let related = Part::multipart(
        "related",
        "=herald-demo-related",
        &[
            alternative,
            Part::attachment(Attachment::inline(
                "logo.png",
                "image/png",
                "logo@herald",
                &png,
            )),
        ],
    );
    messages.push(MockMessage::new(
        "demo-12",
        report.multipart(
            "mixed",
            &[
                related,
                Part::attachment(Attachment::file(
                    "说明.txt",
                    "text/plain; charset=utf-8",
                    TEXT_ATTACHMENT.as_bytes(),
                )),
                Part::attachment(Attachment::file(
                    "data.bin",
                    "application/octet-stream",
                    &binary,
                )),
            ],
        ),
    ));

    let recipients: Vec<String> = (1..=40)
        .map(|index| format!("User {index:02} <user{index:02}@example.com>"))
        .collect();
    let mut long_body = String::new();
    for index in 1..=80 {
        long_body.push_str(&format!(
            "第 {index:02} 段：{}这是用于验证超长正文折行、滚动与截断提示的样例段落。\r\n\r\n",
            "中文长文本测试。".repeat(20)
        ));
    }
    long_body.push_str(&format!(
        "无空格长串（用于验证折行）：{}\r\n",
        "ABCDEFGHIJ".repeat(120)
    ));
    messages.push(MockMessage::new(
        "demo-13",
        Mail::new(
            "demo-13",
            "批量通知 <bulk@example.com>",
            recipients.join(", "),
            subject("超长内容测试：40 个收件人与长正文"),
            "Sat, 14 Sep 2024 10:30:00 +0800",
        )
        .cc("Cc User 01 <cc1@example.com>, Cc User 02 <cc2@example.com>")
        .text("text/plain; charset=utf-8", "8bit", &long_body),
    ));

    messages
}

struct Mail {
    uidl: String,
    from: String,
    to: String,
    cc: Option<String>,
    subject: Option<Vec<u8>>,
    date: String,
}

impl Mail {
    fn new(
        uidl: &str,
        from: impl Into<String>,
        to: impl Into<String>,
        subject: Option<Vec<u8>>,
        date: impl Into<String>,
    ) -> Self {
        Self {
            uidl: uidl.to_string(),
            from: from.into(),
            to: to.into(),
            cc: None,
            subject,
            date: date.into(),
        }
    }

    fn cc(mut self, cc: impl Into<String>) -> Self {
        self.cc = Some(cc.into());
        self
    }

    fn headers(&self) -> Vec<u8> {
        let mut out = format!("From: {}\r\nTo: {}\r\n", self.from, self.to).into_bytes();
        if let Some(cc) = &self.cc {
            out.extend_from_slice(format!("Cc: {cc}\r\n").as_bytes());
        }
        if let Some(subject) = &self.subject {
            out.extend_from_slice(b"Subject: ");
            out.extend_from_slice(subject);
            out.extend_from_slice(b"\r\n");
        }
        out.extend_from_slice(
            format!(
                "Date: {}\r\nMessage-ID: <{}@example.com>\r\nMIME-Version: 1.0\r\n",
                self.date, self.uidl
            )
            .as_bytes(),
        );
        out
    }

    fn with_content(&self, content_type: &str, encoding: Option<&str>, body: &[u8]) -> Vec<u8> {
        let mut out = self.headers();
        out.extend_from_slice(format!("Content-Type: {content_type}\r\n").as_bytes());
        if let Some(encoding) = encoding {
            out.extend_from_slice(format!("Content-Transfer-Encoding: {encoding}\r\n").as_bytes());
        }
        out.extend_from_slice(b"\r\n");
        out.extend_from_slice(body);
        if !body.ends_with(b"\n") {
            out.extend_from_slice(b"\r\n");
        }
        out
    }

    fn text(&self, content_type: &str, encoding: &str, body: &str) -> Vec<u8> {
        self.with_content(content_type, Some(encoding), body.as_bytes())
    }

    fn multipart(&self, subtype: &str, parts: &[Part]) -> Vec<u8> {
        let boundary = "=herald-demo-outer";
        let body = render_multipart(boundary, parts);
        self.with_content(
            &format!("multipart/{subtype}; boundary=\"{boundary}\""),
            None,
            body.as_bytes(),
        )
    }
}

struct Part {
    headers: String,
    body: String,
}

impl Part {
    fn text(content_type: &str, body: &str) -> Self {
        Self {
            headers: format!("Content-Type: {content_type}\r\nContent-Transfer-Encoding: 8bit"),
            body: body.to_string(),
        }
    }

    fn attachment(attachment: Attachment) -> Self {
        let mut headers = format!(
            "Content-Type: {}\r\nContent-Disposition: {}\r\nContent-Transfer-Encoding: base64",
            attachment.content_type, attachment.disposition
        );
        if let Some(content_id) = attachment.content_id {
            headers.push_str(&format!("\r\nContent-ID: <{content_id}>"));
        }
        Self {
            headers,
            body: attachment.encoded,
        }
    }

    fn multipart(subtype: &str, boundary: &str, parts: &[Part]) -> Self {
        Self {
            headers: format!("Content-Type: multipart/{subtype}; boundary=\"{boundary}\""),
            body: render_multipart(boundary, parts),
        }
    }
}

fn render_multipart(boundary: &str, parts: &[Part]) -> String {
    let mut out = String::new();
    for part in parts {
        out.push_str("--");
        out.push_str(boundary);
        out.push_str("\r\n");
        out.push_str(&part.headers);
        out.push_str("\r\n\r\n");
        out.push_str(&part.body);
        out.push_str("\r\n");
    }
    out.push_str("--");
    out.push_str(boundary);
    out.push_str("--\r\n");
    out
}

struct Attachment {
    content_type: String,
    disposition: String,
    encoded: String,
    content_id: Option<&'static str>,
}

impl Attachment {
    fn file(filename: &str, mime: &'static str, data: &[u8]) -> Self {
        Self {
            content_type: format!("{mime}; name=\"{filename}\""),
            disposition: format!("attachment; filename=\"{filename}\""),
            encoded: wrap_base64(data),
            content_id: None,
        }
    }

    /// Chinese filename as an RFC 2047 encoded word, the form most mail
    /// clients emit and the one mail-parser decodes in MIME parameters.
    fn encoded_name(filename: &str, mime: &'static str, data: &[u8]) -> Self {
        let encoded = format!("=?UTF-8?B?{}?=", BASE64.encode(filename));
        Self {
            content_type: format!("{mime}; name=\"{encoded}\""),
            disposition: format!("attachment; filename=\"{encoded}\""),
            encoded: wrap_base64(data),
            content_id: None,
        }
    }

    fn inline(filename: &str, mime: &'static str, content_id: &'static str, data: &[u8]) -> Self {
        Self {
            content_type: format!("{mime}; name=\"{filename}\""),
            disposition: format!("inline; filename=\"{filename}\""),
            encoded: wrap_base64(data),
            content_id: Some(content_id),
        }
    }
}

fn wrap_base64(data: &[u8]) -> String {
    let encoded = BASE64.encode(data);
    encoded
        .as_bytes()
        .chunks(76)
        .map(|chunk| std::str::from_utf8(chunk).expect("base64 is ASCII"))
        .collect::<Vec<_>>()
        .join("\r\n")
}

fn subject(text: &str) -> Option<Vec<u8>> {
    Some(text.as_bytes().to_vec())
}

fn rfc2047_utf8(text: &str) -> Option<Vec<u8>> {
    Some(format!("=?UTF-8?B?{}?=", BASE64.encode(text)).into_bytes())
}

fn rfc2047_charset(text: &str, encoding: &'static encoding_rs::Encoding) -> Option<Vec<u8>> {
    let (bytes, _, _) = encoding.encode(text);
    Some(format!("=?{}?B?{}?=", encoding.name(), BASE64.encode(&bytes)).into_bytes())
}

fn folded_subject(text: &str) -> Option<Vec<u8>> {
    let chars: Vec<char> = text.chars().collect();
    let middle = chars.len() / 2;
    let head: String = chars[..middle].iter().collect();
    let tail: String = chars[middle..].iter().collect();
    Some(
        format!(
            "=?UTF-8?B?{}?=\r\n =?UTF-8?B?{}?=",
            BASE64.encode(head),
            BASE64.encode(tail)
        )
        .into_bytes(),
    )
}

/// Raw 8-bit header value (no encoded-word), which exercises the GBK/BIG5
/// fallback used when mail-parser leaves the header undecoded.
fn raw_8bit(text: &str, encoding: &'static encoding_rs::Encoding) -> Vec<u8> {
    let (bytes, _, had_errors) = encoding.encode(text);
    assert!(
        !had_errors,
        "sample text cannot be represented in {}",
        encoding.name()
    );
    bytes.into_owned()
}

fn quoted_printable(text: &str) -> String {
    let mut out = String::new();
    let mut column = 0;
    for byte in text.bytes() {
        match byte {
            b'\r' => continue,
            b'\n' => {
                out.push_str("\r\n");
                column = 0;
                continue;
            }
            _ => {}
        }
        let token = if (0x20..=0x7e).contains(&byte) && byte != b'=' {
            (byte as char).to_string()
        } else {
            format!("={byte:02X}")
        };
        if column + token.len() > 72 {
            out.push_str("=\r\n");
            column = 0;
        }
        out.push_str(&token);
        column += token.len();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_includes_long_detail_case() {
        let messages = sample_messages(SAMPLE_MESSAGES);
        assert_eq!(messages.len(), SAMPLE_MESSAGES);
        let raw = &messages
            .iter()
            .find(|message| message.uidl == "demo-13")
            .expect("demo-13")
            .raw;
        let parsed = crate::parse::parse_message(raw, "demo-13", 0, false).expect("parse");
        assert_eq!(parsed.header.to.len(), 40);
        assert!(parsed.header.to[39].contains("user40@example.com"));
        assert_eq!(parsed.header.cc.len(), 2);
        let body = parsed.body_text.expect("body");
        assert!(body.len() > 32 * 1024, "body is {} bytes", body.len());
        assert!(body.contains(&"ABCDEFGHIJ".repeat(120)));
    }
}
