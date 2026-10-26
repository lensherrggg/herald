# Herald 设计文档

版本：0.1.0  
对应实现：workspace `herald-core` / `herald-cli` / `herald-mcp` / `herald-tui`

---

## 1. 目标与边界

Herald 解决的是「让 Agent 稳定、受控、低 token 地读内网 POP3 邮箱」：

- 只读收信，绝不删除、标记、移动或发信。
- 本地 SQLite 缓存，UIDL 增量同步，支持离线重复读取。
- 中文友好的关键词搜索、本地语义搜索与 RRF 混合检索。
- 同一套 core 同时服务 MCP、CLI、TUI；TUI 编入 CLI 二进制，且不绕道 MCP。
- 便携部署：配置、数据库、模型默认随可执行文件目录。

明确不做：IMAP、SMTP、多用户权限、定时后台任务、MCP 附件能力。`Herald::sync` 是可被外部调度器复用的入口，但本项目不启动任何后台循环。

---

## 2. 分层与模块

```
                 ┌──────────────────────┐  ┌──────────────┐
                 │      herald-cli      │  │  herald-mcp  │
                 │ clap/json + ratatui  │  │  stdio/MCP   │
                 │  └─ herald-tui (lib) │  │              │
                 └──────────┬───────────┘  └──────┬───────┘
                            └───────────┬─────────┘
                                        ▼
                                ┌───────────────┐
                                │  herald-core  │
                                │  Herald facade│
                                └───────┬───────┘
        ┌──────────┬──────────┬───────────┼───────────┬──────────┬──────────┐
        ▼          ▼          ▼           ▼           ▼          ▼          ▼
     config     paths       pop3        parse       sync       store     search
                                                               │
                                                    SQLite + FTS5 + sqlite-vec
                                                               │
                                              semantic / models（feature = semantic）
```

### 2.1 `herald-core`

| 模块 | 职责 |
|---|---|
| `config` | TOML 反序列化、校验、账户解析、凭据来源判定、示例配置 |
| `credentials` | `cred-cmd` 执行（sh / PowerShell、超时、stdout 首行）、进程内密码缓存 |
| `paths` | 可执行文件目录/用户目录决策、目录可写探测、最终路径拼装、`describe()` |
| `error` | `HeraldError` 及稳定的 MCP 错误码映射 |
| `model` | 头/详情/摘要/附件/同步/搜索等跨层数据结构 |
| `pop3` | 只读 POP3 状态机、CAPA/UIDL/TOP/RETR、TLS/STLS、超时 |
| `tls` | 系统根证书 + 额外信任根（测试自签 CA） |
| `parse` | mail-parser 封装、8-bit 中文头兜底、正文与附件解析 |
| `store` | 事务/迁移、UIDL 去重、正文缓存、FTS、向量表、附件 BLOB、本地状态 |
| `search` | jieba 分词、FTS 查询构造、LIKE 兜底转义 |
| `sync` | UIDL 差集、邮件处理、错误隔离、embedding 文本生成 |
| `time` | 将存储的 RFC 3339 时间统一换算成北京时间（UTC+8）供 CLI/TUI 展示 |
| `service` | `Herald` 门面：打开实例、账户选择、读路径、附件导出、模型入口 |
| `semantic` | 从模型目录/Tokenizer 目录分别加载本地 ONNX、批量 embedding、归一化、按需写入 |
| `models` | HF/mirror 下载、模型与 tokenizer 分目录落盘、原子写入、文件校验、目录保护 |
| `mock_pop3` | 内置 mock POP3 server（plain；STLS/TLS 需 `test-support`） |
| `demo` | `demo = true` 时启动内置假邮箱、临时数据库、样例邮件与首次同步，进程退出自动清理 |

### 2.2 入口层

- **CLI**：唯一的人用可执行文件，clap 解析；所有命令共用 `Herald`，`--json` 便于脚本。
- **MCP**：`rust-mcp-sdk` + `#[mcp_tool]`；一个 `Mutex<Herald>` 串行化工具调用；不编译 `semantic`。
- **TUI**：`herald-tui` 只提供库，由 `herald tui` 子命令调用。`main` 保持同步，TUI 分支在创建 Tokio Runtime 之前执行，避免嵌套运行时；普通命令再各自建运行时。

---

## 3. 启动与路径解析

```text
PathLayout::resolve(--config?)
  ├─ 有 --config: 配置目录 = 该文件父目录；data_dir 同目录；portable = false
  └─ 无 --config:
       exe_dir 可写？ ──是──> config/data = exe_dir；portable = true
             │
             否
             ▼
       platform_dirs()：config_dir / data_dir；记录 fallback_reason，warn 日志
```

然后：

0. 入口先解析一次 `PathLayout`，在 `data_dir/logs` 安装按天滚动的文件日志（保留 14 天）；TUI 关闭 stderr 层，CLI 保留 info、MCP 保留 warn。
1. `Config::load` 读取并校验 TOML；`demo = true` 时跳过账户校验（账户会被覆盖）。
2. `demo` 分支：在后台线程启动 mock POP3（25 封样例邮件），把账户、`database` 改写到系统临时目录；`attachments.export_dir` 仅在未配置时改写到临时目录，已配置则保留。
3. `PathLayout::complete` 得出 `database_path`、`models_dir`、`tokenizer_dir`：
   - `database` 相对路径按**配置文件目录**解析；
   - `semantic.model_path` 同样相对配置文件目录，缺省为 `models/<model_name>`；
   - `semantic.tokenizer_path` 缺省复用 `models_dir`，配置后单独解析，实现目录级拆分。
4. `Store::open` 建库/迁移。
5. 把所有配置账户 `ensure_account` 注册进数据库（因此从未同步也能 `accounts`）。
6. demo 分支在独立线程上跑首次全量同步（正文 + 附件），避免在异步调用方里嵌套 runtime；若模型文件已存在，再顺带写入 embedding。
7. 打印实际生效路径、存放模式与降级原因。

设计要点：路径决策发生在读配置之前，因此 `herald init` 也能复用；`describe()` 同时供 CLI 日志与 TUI 帮助页使用，避免入口层各自拼路径。

---

## 4. POP3 与同步

### 4.1 客户端

- 连接模式：`plain`、`stls`（连接后 STARTTLS）、`tls`（隐式 TLS）。
- TLS 使用 rustls + 系统根证书；测试可注入额外根证书。
- 连接后 `CAPA`：
  - 不支持 CAPA 不影响使用，UIDL/TOP 能力按保守值处理；
  - TOP 不可用时 `fetch_headers` 回退 `RETR` 全量再解析；
  - STLS 只有在 CAPA 声明时才尝试。
- 只实现 CAPA/UIDL/LIST/TOP/RETR/QUIT（以及认证 USER/PASS），没有修改类命令。
- 默认超时 30 秒，可按账户覆盖。

### 4.2 凭据来源

每个账户的 `cred-cmd` / `password_env` / `password` 必须且只能配一个，没有优先级叠加：

```text
credential_source()
  ├─ password       → 直接使用（已弃用；启动日志 warn）
  ├─ password_env   → 读环境变量；空/缺失即失败，不回退明文
  └─ cred-cmd       → 缓存命中？直接用
                      否则 sh -c / powershell -NoProfile -Command 执行
                           → 只取 stdout 第一行，去掉行尾 \n / \r，不 trim 空格
                           → 空输出、非零退出、超时均为结构化失败
                           → 成功且 cred_cmd_cache = true 时写入内存缓存
```

- 执行器使用同步 `std::process`，因为 `pop3_options()` 等入口是同步的；stdout/stderr 分别开线程排空，主线程轮询 `try_wait` 实现超时，超时后 kill。
- 错误码：`cred_cmd_not_found`、`cred_cmd_failed`、`cred_cmd_timeout`、`cred_cmd_empty_output`、`credential_error`；错误文本只含账户与失败类别，不含 stdout/stderr。
- 缓存键是账户名，内容只存在进程内存；MCP server 长时间运行时同一账户只执行一次命令，`cred_cmd_cache = false` 可关闭。
- MCP 启动不预先执行命令；第一次需要网络（`sync_mailbox`、`get_message` 拉正文等）才解析。失败只终止该次工具调用，server 继续运行，且向 agent 只返回「凭据获取失败，请检查配置」。
- Linux/macOS 走 `sh -c`，Windows 走 `powershell -NoProfile -Command`；Herald 不提供 TTY 透传，交互式命令必须改用非交互路径或 GUI pinentry。
- 明文 `password` 时会提示迁移：TUI 每次启动显示一次浮窗（任意键关闭），CLI 每条命令首行 stderr 输出建议（不污染 `--json` stdout），日志另记 warn。

### 4.3 增量同步流程

```text
sync(options)
  account ← explicit / default / 唯一账户
  password ← credentials.resolve(account)   // 失败即返回结构化错误
  client.connect
  server_uidls ← UIDL 全量
  known       ← 本地该账户 UIDL 集合
  new         ← server_uidls - known，按 limit 截断（--all 忽略）
  sizes       ← LIST（失败不致命，仅用于阈值判断）

  for each new (number, uidl):
     need_body ← with_body || auto_body_max_size == 0 || size <= 阈值
     full_fetch ← need_body || with_attachments
     raw ← RETR(full_fetch) 或 TOP(headers)
     parse → upsert header
     need_body ? set_body + FTS tokens
     full_fetch ? upsert attachment metadata
     with_attachments ? 逐个校验大小并写 BLOB
     生成 embedding 文本 → jobs

     单封失败：写 sync_errors，继续下一封

  update sync_state(last_uidl, last_sync_at, total)
  QUIT
  with_embeddings ? 批量写向量（失败只告警）
```

关键性质：

- **幂等**：`UNIQUE(account_id, uidl)` + `upsert`；第二次同步 `new_count = 0`。
- **错误隔离**：单封 RETR/解析失败只记录，不中断整批。
- **超限保护**：超过 `max_attachment_size_bytes` 的附件内容不写 BLOB，并记录警告。
- **正文策略**：`auto_body_max_size = 0` 表示同步即缓存全部正文；正值表示只自动缓存小邮件，其余按需 RETR。
- **附件默认不下载**：默认同步只拿附件元数据，`--with-attachments` 或读取/导出附件时才取内容。
- **embedding 不阻塞同步**：模型缺失/向量表不可用时只提示一次，同步结果照常返回。
- **可观测**：同步开始/结束以 info 记录 `server_messages`、`known`、`new_count` 等计数；连接/认证失败记 error，单封失败记 warn，均可从本地日志回溯。

---

## 5. 读取路径

| 操作 | 行为 |
|---|---|
| `list_messages` | 只查 SQLite；支持账户、from、since/until、排序（日期/发件人/主题）、升降序、分页 |
| `keyword_search` | jieba 分词 → FTS5 `MATCH`；0 结果或 FTS 错误时回退 `LIKE` |
| `semantic_search` | 查询文本 embedding → `vec0` KNN → 按账户过滤 → 距离升序 |
| `hybrid_search` | 两路各取 `limit + offset` 条候选 → RRF 融合去重 → 排序后分页；语义不可用时降级为关键词并附 `note` |
| `get_message` | 正文已缓存则直接返回；否则 RETR、解析、写正文/附件元数据/FTS/向量，二次调用不再联网 |
| `get_raw_message` | POP3 按 UIDL 定位序号后 RETR；MCP 层截断到 256KB |
| `get_attachment` | 先查 BLOB；未命中则 RETR 整封、解析并写入所需附件内容；仅 CLI/TUI 可达 |
| `export_attachment` | 读取 BLOB → 决定目标文件/目录 → 目录目标使用 `unique_target` 避免覆盖 → 写盘 → 记录 `last_export_dir` |

正文缓存是读路径的「一次网络、多次本地」边界：MCP 重复 `get_message` 不会反复拉取；附件元数据在首次解析时补齐。

---

## 6. 存储设计

### 6.1 表

- `accounts`：配置账户的注册表，`name` 唯一。
- `sync_state`：每账户最后 UIDL、最后同步时间、邮件总数。
- `messages`：头部字段 + `body_text`/`body_html` + `body_cached_at` + 本地 `is_read`；`UNIQUE(account_id, uidl)`。
- `attachments`：`UNIQUE(message_id, idx)`；`content BLOB` 为 NULL 表示只知元数据。
- `sync_errors`：按账户记录单封失败与警告，供 `status` 查询。
- `local_state`：键值表，目前保存 `last_export_dir` 等本机状态。
- `messages_fts`：contentless FTS5（`content=''`、`contentless_delete=1`、`unicode61`），rowid = `messages.id`。
- `message_embeddings`：`vec0(message_id INTEGER PRIMARY KEY, embedding FLOAT[512])`。

### 6.2 不变量

- 邮件主键的业务身份是 `(account_id, uidl)`；本地自增 `id` 仅用于关联。
- FTS 与向量表的 rowid 必须与 `messages.id` 对齐；正文重写时先删 FTS 行再插入。
- 附件 `idx` 是解析顺序序号，导出接口以 `(uidl, index)` 定位。
- 向量维度固定 512；写入前校验长度并做 L2 归一化。
- 所有 SQL 使用参数绑定；FTS 的每个查询词单独加引号。
- `is_read` 只属于本地缓存，永远不会回写服务器；旧库打开时通过 `pragma_table_info('messages')` 检测并 `ALTER TABLE` 补列。

### 6.3 sqlite-vec 的加载方式

`sqlite-vec` 以 vendored crate 静态编译进二进制，通过 `rusqlite` 注册扩展；不需要用户安装系统库。若当前平台/链接方式导致扩展不可用：

- 启动时 `vec0` 建表失败只告警；
- `vec_enabled = false`；
- 同步跳过 embedding 写入，语义搜索返回 `MODEL_ERROR`；
- 关键词搜索完全不受影响。

这种「软失败」保证核心邮件读取不被可选的向量能力拖垮。

---

## 7. 解析与中文编码

- 使用 `mail-parser`（`full_encoding`）解析 MIME：头部、正文、HTML、内嵌图片、附件。
- 8-bit 未编码头部兜底：当主题缺失或出现替换字符 `U+FFFD` 时，直接扫描原始头部，按 UTF-8 → GB18030 → BIG5 尝试解码。
- 正文编码由 mail-parser 处理；GBK/BIG5/UTF-8 均有集成测试覆盖。
- 附件元数据：文件名、MIME、大小、`content_id`、`is_inline`。
- 附件内容读取只在需要时进行，避免把大附件常驻内存/磁盘。

---

## 8. 搜索设计

### 8.1 关键词

1. 写入：`subject + from + body` 拼接后 jieba 分词，词间空格，写入 FTS5。
2. 查询：同样分词，每个词加双引号后以空格连接（隐式 AND），避免用户输入构造 FTS 运算符。
3. 排序：`bm25(f)` 优先，其次日期倒序。
4. 兜底：FTS 无结果或执行失败时使用参数化 `LIKE`，对 `%`/`_` 转义，覆盖分词未命中的中文子串。

### 8.2 字段限定

- 入口：CLI `--field all|from|to|subject|body`，MCP `field`，TUI 输入框 `Ctrl+F` 循环。
- 实现：字段模式不走 FTS，直接对选中列做参数化 `LIKE` 子串匹配并转义 `%`/`_`；`to` 同时匹配 `to_addr` 或 `cc_addr`。
- 这样避免为每种字段维护独立 FTS 列，也不需要 schema 迁移；代价是字段模式没有分词/BM25 排序，结果按日期倒序。
- embedding 覆盖整封邮件，无法定位字段，因此字段模式固定为关键词；纯语义会返回 `UNSUPPORTED`，混合模式自动只走关键词并在 `note` 中说明。

### 8.3 本地已读状态

- 存储：`messages.is_read`，只写本地 SQLite；POP3 协议层没有已读操作。
- 置读：CLI `show`、TUI `Enter` 打开详情成功后置读；仅预览（移动选择）不置读。
- 切换：TUI `u`、CLI `mark [--unread]`；MCP 只读地返回 `is_read`，并支持 `unread_only` 过滤。
- 列表展示：TUI 行首 `▌` 与标题未读计数；CLI `list` 打印未读标记和总数。

### 8.4 语义

- 模型：`bge-small-zh-v1.5`，CLS pooling，最大长度 512，输出 512 维，手动 L2 归一化。
- 目录：`model.onnx` 从 `semantic.model_path` 读取；`tokenizer.json` 等 4 个文件从 `semantic.tokenizer_path` 读取，缺省时两者为同一目录。
- 文本：主题 + 发件人显示名 +（若已缓存）正文。
- 同步时：
  - 默认只缓存头部时生成主题级向量；
  - 缓存正文后由 `get_message`/`get_attachment` 自动升级为全文向量；
  - `sync --index` 对已缓存正文批量重建。
- 查询：embedding → 取 `k = max(limit × 10, 100)` 的近邻 → 按账户过滤 → 取前 `limit`。
- 模型缺失是预期状态：只在第一次失败时告警，不阻塞同步。
- MCP 不编译该模块，因此 Agent 无法触发模型下载或向量检索。

### 8.5 混合

- 入口：CLI `--mode auto|keyword|semantic|hybrid`；`auto` 在 `semantic.enabled` 且模型文件齐全时选 `hybrid`，否则 `keyword`。`--semantic` 保留为 `--mode semantic` 的简写。
- 候选：关键词与语义各取 `limit + offset` 条（RRF 只需要相对名次，分页时窗口随之扩大）。
- 融合：`score = Σ 1/(60 + 名次)`，同一 UIDL 两路得分相加；按得分降序、日期降序打破平局。
- 降级：语义侧失败（模型缺失、配置关闭、`vec0` 不可用）时保留关键词结果，并在 `SearchResult.note` 中说明原因；CLI 打到 stderr，TUI 显示在状态栏。
- `total` 是融合候选数（与纯语义模式一样，受本次窗口大小限制），不是全库精确命中数。
- TUI：`/` 打开搜索框，`Tab` 循环关键词 → 混合 → 语义；`Ctrl+F` 循环字段；默认值同样走 `auto` 规则。

---

## 9. 模型下载

```text
download(options)
  model ← --model / semantic.model_name
  (dest, tokenizer_dest) ←
      --dest ? (dest, dest)                       # 显式覆盖时全部放同一目录
      model == model_name ? (models_dir, tokenizer_dir)
                          : (config_dir/models/<model>, 同上)
  mirror← --mirror / HF_ENDPOINT / https://huggingface.co
  校验 model 名称（非空、不含 ".."）与 mirror 协议（http/https）
  校验两个目标目录：不是文件；非空目录必须包含已知模型/Tokenizer 文件或 --force；
                    分别做写入探针
  for (file, dir) in model.onnx→dest + 4 个 tokenizer 文件→tokenizer_dest:
     已存在且非空且未 --force → skip
     依次尝试候选远程路径（model.onnx 先试 onnx/model.onnx）
     流式写入 *.part → 成功后 rename → 失败删除 *.part
  最终复核 5 个文件都位于各自目标目录且非空
```

- 默认仓库为 `Xenova/bge-small-zh-v1.5`：BAAI 官方仓库只有 PyTorch 权重，没有 `model.onnx`；Xenova 提供同一模型的 ONNX 转换。
- 下载与加载解耦：`semantic.enabled = false` 不影响下载。
- 拆分与合并共存：默认单目录；配置 `tokenizer_path` 后按目录拆分；`--dest` 显式要求合并下载。
- 原子性：`.part` 临时文件避免半截模型被当作可用。
- 可观测：每个文件记录 URL/大小；报告同时给出 `dest` 与 `tokenizer_dest`，以及 skip 状态。

---

## 10. MCP 边界

- 传输：stdio，newline-delimited JSON-RPC；stdout 只走协议，日志写本地 `logs/herald.YYYY-MM-DD.log`，stderr 只保留 warn 及以上。
- 7 个工具固定注册；未知工具由 SDK 返回错误。
- 每次调用参数先反序列化；失败返回 `INVALID_ARGUMENT`。
- `limit` 用 `u32`（SDK 的 schema 宏不支持 `usize`），服务层再夹到 1–100。
- `success` 同时提供文本 JSON 与 `structured_content`；错误设置 `isError: true` 并返回 `{code,message}`。
- 附件、文件路径、模型路径、模型下载能力在 MCP 层完全不存在。
- `Mutex<Herald>` 串行化调用，避免 SQLite 连接跨线程共享与并发同步互相干扰；这也满足 `rust-mcp-sdk` 对 future `Send` 的要求。

---

## 11. TUI 设计

TUI 以 `herald-tui` 库的形式编译进 `herald`，入口为 `herald tui [--account <name>]`，不再单独生成可执行文件。ratatui + crossterm 负责渲染，UI 状态全部集中在 `App`；同步在独立线程执行，其余阻塞调用仍放在 App 持有的专用 Tokio Runtime 上。

### 11.1 状态机

```text
App
├─ 账户/列表/分页/排序/搜索条件
├─ 当前详情、原文、滚动位置
├─ Mode: Browse | Input(kind) | Accounts | Attachments | Browser | Raw | Help
├─ Focus: List | Detail
└─ Browser { dir, entries, selected, filename, filename_focus, uidl, index }
```

- 同步在独立线程执行：后台通过 `Herald::open_background` 用同一配置/路径再开一个 SQLite 连接（WAL），网络、写入和 embedding 都在该线程完成；不重复启动 demo 服务器，临时库与主实例共享。UI 事件循环在同步期间改用 150ms `event::poll`，继续处理按键并轮询结果通道；完成后统一 `reload()`。后台实例的语义服务是独立的，首次需要写入 embedding 时会加载第二份模型。
- 同步期间状态栏固定带「同步中…」前缀，其他状态消息附在其后；空闲后恢复普通绿色状态栏。
- 未缓存正文、原文、导出等阻塞操作仍通过 `app.runtime.block_on` 执行；UI 线程只在事件循环中重绘。
- 切换账户/分页/排序/搜索后统一 `reload()`：重查列表、夹取页码，并对当前选择调用 `preview_selected()`。
- `preview_selected()` 只用 `get_stored_message` 读本地 SQLite（头部、已缓存正文、附件元数据），移动选择不产生网络请求；正文未缓存时显示提示，按 `Enter` 才 RETR。
- 搜索框 `Tab` 循环检索模式，`Ctrl+F` 循环字段；字段非 `all` 时强制关键词模式，与服务层规则一致。每页条数取自 `[ui] page_size`。
- 列表行首 `▌` 表示未读；`u` 直接切换本地已读状态并刷新未读计数。`Enter` 成功加载详情后自动置读。
- 时间统一按 UTC+8 展示（`time` 模块）：详情显示到秒，顶栏同步时间显示到分钟；JSON/MCP 保留原始 RFC 3339。列表按日期排序时默认分组，组标题按当前周分档（今天/昨天/本周周几/日期）；每封邮件渲染为等宽单行：未读竖条 + 固定宽度发件人 + 主题 + 右对齐时间。分组模式行尾只显示 `HH:MM`（日期已在组标题），`v` 切换为紧凑模式后去掉组标题、行尾改为本周周几/今年 `MM-DD`/往年 `YY-MM-DD`。搜索或按发件人/主题排序时自动回退紧凑模式，保证非日期顺序下不会出现交错的日期标题；主题按显示列截断并补 `…`，无色终端用 DIM 区分次要信息。
- 详情区使用 `Paragraph` 自动换行（含超长无空格 token）；正文行先按显示列宽预折行（空白与中文字符间可断行），避免“第 01 段：”这类标签被整段推到下一行。收件人/抄送超过 12 个折叠为“前 12 个 …（共 N 个）”；正文超过 32KB 时顶部提示并只渲染前 32KB，完整原文走 `r` 视图。
- 单行输入框（主搜索、新建目录、原文查找、附件重命名）统一走 `text_input` 模块维护光标：`←`/`→`、`Home`/`End` 按 UTF-8 字符边界移动，`Backspace`/`Delete` 在光标处编辑，输入框按“光标前 / 光标块 / 光标后”三段渲染。
- 宽终端（≥90 列）左右分栏；窄终端一次只显示焦点面板；极小终端显示尺寸提示。
- 底部栏按模式/焦点给出短提示，完整快捷键放在 `h` 帮助页；窄终端进一步精简。
- 启动时若检测到明文 `password`，最上层显示「安全建议」浮窗：说明可迁移到 `cred-cmd`，列出 `auth status` / `auth test` 入口；任意键只关闭浮窗，不触发该键原本的操作，下次启动仍会提示。
- 从覆盖层（原文、搜索输入、帮助、账户、附件选择、文件浏览器）返回主界面时先 `Terminal::clear()` 再整屏重绘；部分终端不会主动刷新宽字符尾格，强制重绘可清除高亮背景残留的色块。
- `NO_COLOR` 或 `TERM=dumb` 时用粗体/反色代替颜色。

### 11.3 原文视图

- 用于核对服务器返回的原始 RFC822：头部、编码、MIME 边界、附件头等，不经过正文解析，便于排查解析问题。
- 展示前统一换行符、展开制表符、替换其余控制字符，避免 CR/ESC 破坏终端渲染；超过 256KB 截断并标注。
- 原文按终端宽度预折行：空白处断行，中文可在字符间断行，超长 ASCII 串按宽度硬切。这样“第 01 段：”不会被拆到两行，中文长段也能填满整行；搜索跳转按同一套折行结果计算真实行号。
- 附件与内嵌资源按 MIME 解析出的原始字节范围折叠：保留段落头，正文替换为“序号 + 名称 + 类型 + 解码后大小”的一行清单；折叠发生在 256KB 截断之前，避免大附件把后续内容挤出上限。
- 全屏只读视图，保留底部栏；翻页沿用 `less` 习惯：`空格`/`f` 下页、`b` 上页、`d`/`u` 半页、`↑↓` 单行、`g/G` 首尾（`PgUp`/`PgDn` 仍可用但不作为唯一入口）。滚动位置统一夹取到 `总行数 - 可见高度`，`G` 之后可立即向上滚动。`Esc` 返回。
- `/` 在原文内查找：输入框固定在原文视图底部一行，边输边重新匹配并高亮全部命中，当前命中用另一配色标出；`Enter`/`n` 向下、`N` 向上循环，命中数实时显示为 `x/N`。滚动位置按 `Paragraph` 的实际换行行数计算，匹配被折行时也能定位到可见行；`Esc` 结束输入但保留高亮，再按 `Esc` 关闭原文视图并清空查找状态。

### 11.2 附件导出

- 单个附件直接进入浏览器；多个附件先弹选择列表。
- 内置浏览器只依赖文件系统，不调用任何 GUI 选择器；`↑↓` 选择、`Enter` 进入目录、`退格`/`←`/`h` 返回上一级（首行 `..（上一级）` 同样可进入），`d` 保存到当前目录。Windows 盘符根目录下通过 `GetLogicalDrives` 列出其他盘符，选中 `Enter` 切换。
- `/` 打开筛选框，按名称做大小写不敏感的子串过滤；ASCII 查询同时生成中文名称的拼音首字母键（保留分隔符的 skeleton 与去掉标点的 compact），因此 `xm` 与 `xmgl` 可分别命中 `项目` 与 `项目 管理`，只在名称含中文时启用。查询含路径分隔符（Unix `/`，Windows `/` 或 `\`）时改为匹配完整路径，路径中的中文同样参与首字母匹配。输入存在的绝对路径后 `Enter` 直接跳到该目录（`filter_jump_target`），因此 `/usr/local` 与 `C:\Tools` 都按各自平台的路径语义处理。切换目录时清空筛选，`..` 行始终保留。
- `z`：zoxide 风格的全局目录跳转。记录每次进入/返回的目录，按 `次数 × (0.05 + 0.5^(距今天数/7))` 计算热度（7 天半衰期），JSON 持久化在配置目录的 `dir_history.json`，最多保留 200 条。查询按空白分词，每个词都要以大小写不敏感的子序列出现在完整路径中；候选只保留仍存在的目录，并排除当前目录。空查询直接列出最常用目录。
- `d`：当前目录 + 安全化原名；核心层 `unique_target` 处理重名。
- `f`：编辑文件名，`Enter` 保存完整路径；已存在时拒绝覆盖并在浏览器内提示。
- `n`：新建目录；非法名称/路径分隔符直接拒绝。
- 起始目录：配置 → 上次成功导出的父目录（`local_state`）→ Downloads → 主目录；目录不存在时回退并显示原因。

---

## 12. 错误与降级

`HeraldError` 统一映射为稳定错误码：`CONFIG_ERROR`、`ACCOUNT_ERROR`、`auth_failed`、`credential_error`、`cred_cmd_not_found`、`cred_cmd_failed`、`cred_cmd_timeout`、`cred_cmd_empty_output`、`POP3_ERROR`、`TIMEOUT`、`TLS_ERROR`、`DB_ERROR`、`IO_ERROR`、`NOT_FOUND`、`INVALID_ARGUMENT`、`MODEL_ERROR`、`HTTP_ERROR`、`UNSUPPORTED`。

| 场景 | 策略 |
|---|---|
| exe 目录不可写 | 降级到用户目录，warn + 展示原因 |
| `cred-cmd` 失败/超时/空输出 | 返回对应结构化错误，不回退其他凭据来源 |
| 服务器不支持 CAPA | 按保守能力继续，UIDL 仍可通过 UIDL 命令获取 |
| 服务器不支持 TOP | RETR 全量取头部 |
| 单封邮件失败 | 写 `sync_errors`，继续整批 |
| 模型缺失/损坏 | 同步不失败，语义检索报 `MODEL_ERROR` |
| sqlite-vec 不可用 | 关闭语义能力，关键词与邮件读取正常 |
| 附件超限 | 跳过内容写入并记录警告 |
| 终端过小 | 显示「终端太小」提示 |
| 终端无颜色 | 改用粗体/反色 |
| 导出目标已存在 | 不覆盖，提示用户改名或换目录 |

---

## 13. 测试策略

- **单元测试**：配置校验、路径降级、分词/FTS 转义、解析编码、语义归一化、浏览器文件名与起始目录、TUI 格式化与分页。
- **凭据来源**：stdout 多行/CRLF/空白、退出码、超时、命令不存在、缓存命中与关闭、三选一校验、空 `cred-cmd`；Windows 上真实走 `powershell -NoProfile -Command` 路径。
- **集成测试（mock POP3）**：
  - `cred-cmd` 输出固定密码完成同步；缓存开启时连续两次认证只执行一次命令，关闭时执行两次；
  - `auth status` 不显示密码与命令全文，`auth test` 能完成一次真实 POP3 登录；
  - 三种连接模式与认证；
  - 增量同步、limit、无 TOP 回退、单封失败；
  - 中文搜索、字段限定、分页、本地已读状态、sqlite-vec 写入/查询；
  - 模型镜像下载、跳过/强制/目录保护、缺模型降级；
  - `demo = true` 的自动同步、临时数据库路径与退出清理；
  - CLI 真实二进制全流程（含 demo 配置的跨进程重置）；
  - MCP 真实二进制 stdio 往返，逐个验证 7 个工具与错误码；
  - TUI 使用 `TestBackend` 渲染真实邮件、详情、搜索、字段切换、已读切换、原文、附件选择、文件浏览器、新建目录、导出、窄终端与无颜色。
- **CI**：Ubuntu/macOS/Windows 三平台 `fmt + clippy + test`。
- **手工试用**：`demo = true` 会启动内置假邮箱与临时数据库，自动完成首次同步，退出即清空；`examples/mock_pop3.rs` 仍保留，用于固定端口/自定义样例数量的双终端流程。
- **未自动化**：真实终端中的键盘输入、终端模拟器差异、不同字体下的中文宽度；发布前建议按 M7 做一次人工走查。

---

## 14. 关键取舍

| 取舍 | 理由 |
|---|---|
| 只读协议子集 | 降低 Agent 误操作风险，POP3 连接天然无写操作 |
| CLI/TUI 直连 core | 少一层协议与序列化，调试简单 |
| 附件进 SQLite BLOB | 单文件实例，无目录清理与孤儿文件问题 |
| MCP 不暴露附件/语义 | 附件属人用功能；语义模型依赖内网环境，不适合作为 Agent 稳定契约 |
| 默认同步正文、不拉附件 | 正文是 Agent 核心需求，附件体积大且默认不进 Agent 视野 |
| contentless FTS | 避免正文双份存储；删除/更新由业务层显式维护 rowid |
| 向量与关键词分离 | 语义能力可关闭/失败，不影响基础搜索与同步 |
| 模型下载与加载解耦 | 支持离线分发、内网镜像与先下载后启用 |
| 便携目录 + 显式降级 | 内网分发友好，只读安装位置仍可运行 |
| 内置文件浏览器 | 跨平台一致、无 GUI 依赖、符合纯 TUI 定位 |
| `cred-cmd` 替代内置 keyring | 跨平台行为一致、核心库零密钥环依赖、天然适配 MCP 非交互场景 |
| 凭据三选一、不叠加优先级 | 行为最可预测，避免「明文悄悄兜底」 |
| 只取 stdout 第一行且不 trim 空格 | 避免破坏含空格密码，同时明确丢弃换行 |
| 默认开启会话内缓存 | 避免每次同步重复触发 GPG/DPAPI 解锁；代价是进程内存有明文 |
| MCP 启动不预执行 `cred-cmd` | 避免启动阻塞，按首次认证需要触发 |
| 不提供 TTY 透传 | 实现简单；交互式命令应由用户改用非交互路径 |

---

## 15. 后续可扩展点

- 定时同步：复用 `Herald::sync`，由外部调度器驱动；需要补充并发锁与通知机制。
- 更多模型：只要目录符合 5 文件布局与 512 维输出即可替换。
- 混合检索调权：当前 RRF 两路等权；可在有评测集后引入关键词权重或语义距离阈值。
- 附件策略：按 MIME/大小自动跳过或限制内嵌图片。
