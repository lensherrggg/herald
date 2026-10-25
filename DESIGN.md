# Herald 设计文档

版本：0.1.0  
对应实现：workspace `herald-core` / `herald-cli` / `herald-mcp` / `herald-tui`

---

## 1. 目标与边界

Herald 解决的是「让 Agent 稳定、受控、低 token 地读内网 POP3 邮箱」：

- 只读收信，绝不删除、标记、移动或发信。
- 本地 SQLite 缓存，UIDL 增量同步，支持离线重复读取。
- 中文友好的关键词搜索与本地语义搜索。
- 同一套 core 同时服务 MCP、CLI、TUI；CLI/TUI 不绕道 MCP。
- 便携部署：配置、数据库、模型默认随可执行文件目录。

明确不做：IMAP、SMTP、多用户权限、定时后台任务、MCP 附件能力。`Herald::sync` 是可被外部调度器复用的入口，但本项目不启动任何后台循环。

---

## 2. 分层与模块

```
                 ┌──────────────┐  ┌──────────────┐  ┌──────────────┐
                 │  herald-cli  │  │  herald-mcp  │  │  herald-tui  │
                 │  clap/json   │  │ stdio/MCP    │  │ ratatui      │
                 └──────┬───────┘  └──────┬───────┘  └──────┬───────┘
                        └─────────────────┼─────────────────┘
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
| `config` | TOML 反序列化、校验、账户解析、示例配置 |
| `paths` | 可执行文件目录/用户目录决策、目录可写探测、最终路径拼装、`describe()` |
| `error` | `HeraldError` 及稳定的 MCP 错误码映射 |
| `model` | 头/详情/摘要/附件/同步/搜索等跨层数据结构 |
| `pop3` | 只读 POP3 状态机、CAPA/UIDL/TOP/RETR、TLS/STLS、超时 |
| `tls` | 系统根证书 + 额外信任根（测试自签 CA） |
| `parse` | mail-parser 封装、8-bit 中文头兜底、正文与附件解析 |
| `store` | 事务/迁移、UIDL 去重、正文缓存、FTS、向量表、附件 BLOB、本地状态 |
| `search` | jieba 分词、FTS 查询构造、LIKE 兜底转义 |
| `sync` | UIDL 差集、邮件处理、错误隔离、embedding 文本生成 |
| `service` | `Herald` 门面：打开实例、账户选择、读路径、附件导出、模型入口 |
| `semantic` | fastembed 本地 ONNX 加载、批量 embedding、归一化、按需写入 |
| `models` | HF/mirror 下载、原子落盘、文件校验、目录保护 |
| `mock_pop3` | 仅 `test-support`：plain/STLS/TLS mock server |

### 2.2 入口层

- **CLI**：clap 解析，所有命令共用 `Herald`；`--json` 输出便于脚本。
- **MCP**：`rust-mcp-sdk` + `#[mcp_tool]`；一个 `Mutex<Herald>` 串行化工具调用；不编译 `semantic`。
- **TUI**：ratatui + crossterm，同步阻塞调用放在专用 tokio Runtime 上，UI 状态全部集中在 `App`。

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

1. `Config::load` 读取并校验 TOML。
2. `PathLayout::complete` 得出 `database_path`、`models_dir`：
   - `database` 相对路径按**配置文件目录**解析；
   - `semantic.model_path` 同样相对配置文件目录，缺省为 `models/<model_name>`。
3. `Store::open` 建库/迁移。
4. 把所有配置账户 `ensure_account` 注册进数据库（因此从未同步也能 `accounts`）。
5. 打印实际生效路径、存放模式与降级原因。

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

### 4.2 增量同步流程

```text
sync(options)
  account ← explicit / default / 唯一账户
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

---

## 5. 读取路径

| 操作 | 行为 |
|---|---|
| `list_messages` | 只查 SQLite；支持账户、from、since/until、排序（日期/发件人/主题）、升降序、分页 |
| `keyword_search` | jieba 分词 → FTS5 `MATCH`；0 结果或 FTS 错误时回退 `LIKE` |
| `semantic_search` | 查询文本 embedding → `vec0` KNN → 按账户过滤 → 距离升序 |
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
- `messages`：头部字段 + `body_text`/`body_html` + `body_cached_at`；`UNIQUE(account_id, uidl)`。
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

### 8.2 语义

- 模型：`bge-small-zh-v1.5`，CLS pooling，最大长度 512，输出 512 维，手动 L2 归一化。
- 文本：主题 + 发件人显示名 +（若已缓存）正文。
- 同步时：
  - 默认只缓存头部时生成主题级向量；
  - 缓存正文后由 `get_message`/`get_attachment` 自动升级为全文向量；
  - `sync --index` 对已缓存正文批量重建。
- 查询：embedding → 取 `k = max(limit × 10, 100)` 的近邻 → 按账户过滤 → 取前 `limit`。
- 模型缺失是预期状态：只在第一次失败时告警，不阻塞同步。
- MCP 不编译该模块，因此 Agent 无法触发模型下载或向量检索。

---

## 9. 模型下载

```text
download(options)
  model ← --model / semantic.model_name
  dest  ← --dest / semantic.model_path / models/<model>
  mirror← --mirror / HF_ENDPOINT / https://huggingface.co
  校验 model 名称（非空、不含 ".."）与 mirror 协议（http/https）
  校验 dest：不是文件；非空目录必须“看起来像模型目录”或 --force；写入探针
  for file in 清单:
     已存在且非空且未 --force → skip
     依次尝试候选远程路径（model.onnx 先试 onnx/model.onnx）
     流式写入 *.part → 成功后 rename → 失败删除 *.part
  最终复核 5 个文件都存在且非空
```

- 下载与加载解耦：`semantic.enabled = false` 不影响下载。
- 原子性：`.part` 临时文件避免半截模型被当作可用。
- 可观测：每个文件记录 URL/大小；报告含 skip 状态。

---

## 10. MCP 边界

- 传输：stdio，newline-delimited JSON-RPC；stdout 只走协议，日志走 stderr。
- 7 个工具固定注册；未知工具由 SDK 返回错误。
- 每次调用参数先反序列化；失败返回 `INVALID_ARGUMENT`。
- `limit` 用 `u32`（SDK 的 schema 宏不支持 `usize`），服务层再夹到 1–100。
- `success` 同时提供文本 JSON 与 `structured_content`；错误设置 `isError: true` 并返回 `{code,message}`。
- 附件、文件路径、模型路径、模型下载能力在 MCP 层完全不存在。
- `Mutex<Herald>` 串行化调用，避免 SQLite 连接跨线程共享与并发同步互相干扰；这也满足 `rust-mcp-sdk` 对 future `Send` 的要求。

---

## 11. TUI 设计

### 11.1 状态机

```text
App
├─ 账户/列表/分页/排序/搜索条件
├─ 当前详情、原文、滚动位置
├─ Mode: Browse | Input(kind) | Accounts | Attachments | Browser | Raw | Help
├─ Focus: List | Detail
└─ Browser { dir, entries, selected, filename, filename_focus, uidl, index }
```

- 同步、详情、原文、导出等阻塞操作通过 `app.runtime.block_on` 执行；UI 线程只在事件循环中重绘。
- 切换账户/分页/排序/搜索后统一 `reload()`：重查列表、夹取页码、清理已不可见的详情。
- 宽终端（≥90 列）左右分栏；窄终端一次只显示焦点面板；极小终端显示尺寸提示。
- `NO_COLOR` 或 `TERM=dumb` 时用粗体/反色代替颜色。

### 11.2 附件导出

- 单个附件直接进入浏览器；多个附件先弹选择列表。
- 内置浏览器只依赖文件系统，不调用任何 GUI 选择器。
- `d`：当前目录 + 安全化原名；核心层 `unique_target` 处理重名。
- `f`：编辑文件名，`Enter` 保存完整路径；已存在时拒绝覆盖并在浏览器内提示。
- `n`：新建目录；非法名称/路径分隔符直接拒绝。
- 起始目录：配置 → 上次成功导出的父目录（`local_state`）→ Downloads → 主目录；目录不存在时回退并显示原因。

---

## 12. 错误与降级

`HeraldError` 统一映射为稳定错误码：`CONFIG_ERROR`、`ACCOUNT_ERROR`、`AUTH_ERROR`、`POP3_ERROR`、`TIMEOUT`、`TLS_ERROR`、`DB_ERROR`、`IO_ERROR`、`NOT_FOUND`、`INVALID_ARGUMENT`、`MODEL_ERROR`、`HTTP_ERROR`、`UNSUPPORTED`。

| 场景 | 策略 |
|---|---|
| exe 目录不可写 | 降级到用户目录，warn + 展示原因 |
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
- **集成测试（mock POP3）**：
  - 三种连接模式与认证；
  - 增量同步、limit、无 TOP 回退、单封失败；
  - 中文搜索、分页、sqlite-vec 写入/查询；
  - 模型镜像下载、跳过/强制/目录保护、缺模型降级；
  - CLI 真实二进制全流程；
  - MCP 真实二进制 stdio 往返，逐个验证 7 个工具与错误码；
  - TUI 使用 `TestBackend` 渲染真实邮件、详情、搜索、原文、附件选择、文件浏览器、新建目录、导出、窄终端与无颜色。
- **CI**：Ubuntu/macOS/Windows 三平台 `fmt + clippy + test`。
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

---

## 15. 后续可扩展点

- 定时同步：复用 `Herald::sync`，由外部调度器驱动；需要补充并发锁与通知机制。
- 更多模型：只要目录符合 5 文件布局与 512 维输出即可替换。
- 混合检索：关键词候选 + 向量重排，目前保持两条独立路径以降低复杂度。
- 附件策略：按 MIME/大小自动跳过或限制内嵌图片。
