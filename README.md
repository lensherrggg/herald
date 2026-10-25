# Herald（传令官）

Herald 是一个用 Rust 编写的**只读 POP3 邮箱客户端**，面向 AI Agent 与人类用户：

- **MCP server**：通过 stdio 向 Agent 暴露 7 个结构化工具，低 token、可分页、有硬上限。
- **CLI**：可脚本化，支持 `--json`。
- **TUI**：完整的终端收件箱，含中文搜索、原文查看与内置附件导出浏览器。
- **本地缓存**：UIDL 增量同步，头部、正文、附件元数据与附件内容（可选）全部落在单个 SQLite 文件中。
- **中文搜索**：jieba 预分词 + contentless FTS5；可选本地 ONNX 语义检索，两路可 RRF 混合（CLI/TUI）；支持按发件人、收件人、主题、正文单独检索。
- **本地已读状态**：只写本地 SQLite，不向服务器发送任何标记；打开邮件自动置为已读，也可手动切换。
- **便携优先**：默认把配置、数据库、模型放在可执行文件旁边；目录不可写时自动降级并明确提示。

Herald **不删除、不移动邮件，也不会修改服务器上的已读状态，也不支持 IMAP/SMTP**。已读/未读仅是本地视图。

---

## 1. 功能一览

| 能力 | 说明 |
|---|---|
| POP3 | `plain` / `stls` / `tls`，CAPA 探测，服务器无 TOP 时回退 RETR |
| 增量同步 | 服务器全量 UIDL 与本地差集，只处理新邮件；`limit` / `--all` |
| 邮件解析 | 头部、纯文本/HTML 正文、中文编码（GBK/BIG5/UTF-8 等）、附件元数据 |
| 按需正文 | 首次查看时 RETR 并缓存，之后离线读取 |
| 附件 | 元数据默认同步；内容按需拉取并存入 SQLite BLOB，默认不落盘 |
| 关键词搜索 | jieba + FTS5，FTS 无结果时回退 LIKE；可按发件人/收件人（To+Cc）/主题/正文限定 |
| 语义搜索 | 本地 `bge-small-zh-v1.5`（512 维），模型与 tokenizer 目录可分开配置，仅供 CLI/TUI |
| 已读状态 | 本地 SQLite 字段，打开即已读，可手动切换；列表显示未读圆点与未读数 |
| MCP | 7 个只读工具，列表默认 20 / 最大 100，正文 32KB 截断，原文 256KB 上限 |
| TUI | 账户切换、同步、分页排序（每页数可配置）、详情、搜索、原文、附件导出 |

---

## 2. 仓库结构

```
herald/
├── Cargo.toml                 # workspace
├── crates/
│   ├── herald-core/           # 全部业务逻辑（无入口层依赖）
│   ├── herald-cli/            # herald 命令
│   ├── herald-mcp/            # herald-mcp stdio server
│   └── herald-tui/            # TUI 库（由 herald tui 调用）
├── vendor/sqlite-vec/         # 内置 sqlite-vec（静态编译，无需系统扩展）
├── .github/workflows/ci.yml   # 三平台 fmt + clippy + test
├── DESIGN.md                  # 架构与实现说明
└── README.md
```

`herald-core` 不依赖入口层；可执行文件只有 `herald` 与 `herald-mcp`，TUI 作为库由 `herald tui` 启动，不经过 MCP。

---

## 3. 构建

要求 Rust **1.88+**（`rust-toolchain` 非必需）。ONNX 运行时由 `fastembed` 在构建时下载/链接。

```bash
cargo build --release --workspace
```

产物：

```
target/release/herald(.exe)       # CLI + TUI
target/release/herald-mcp(.exe)
```

只构建不带语义检索的 core：

```bash
cargo build -p herald-core                 # 默认 features 为空
cargo build -p herald-core --features semantic
```

CLI 与 TUI 固定启用 `semantic`；MCP 不启用，也不会加载模型。

---

## 4. 快速开始

### 4.1 生成配置

```bash
# 默认写到可执行文件所在目录的 herald.toml
./target/release/herald init

# 或指定位置
./target/release/herald init --config ./herald.toml
```

编辑 `herald.toml`：

```toml
# 本地试用：true 时忽略账户，启动内置假邮箱 + 临时数据库，退出后自动清空
demo = false

default_account = "work"
# database 可省略，默认是配置文件所在目录下的 herald.db

[[accounts]]
name = "work"
host = "mail.intra.example.com"
port = 995
security = "tls"                 # plain | stls | tls
username = "alice"
# password 与 password_env 必须且只能配一个
password_env = "HERALD_WORK_PASSWORD"
# timeout_secs = 30

[sync]
default_limit = 50
auto_body_max_size = 0           # 0 = 同步时缓存全部新邮件正文

[attachments]
export_dir = "~/Downloads/herald"  # 导出浏览器默认起始目录；目录不存在时回退到上次导出目录 → Downloads → 主目录
max_attachment_size_bytes = 52428800

[semantic]
enabled = true
model_name = "bge-small-zh-v1.5"
# model_path = "models/bge-small-zh-v1.5"
# tokenizer_path = "models/bge-small-zh-v1.5-tokenizer"  # 可选；缺省与模型目录相同

[ui]
page_size = 20                   # TUI 每页邮件数（也用于搜索结果分页）
```

> `password_env` 指向的环境变量不存在或为空时会直接报错，**不会回退到明文密码**。

### 4.2 准备语义模型（可选）

```bash
# 默认 HuggingFace，可写入配置目录下 models/<model_name>/
./target/release/herald models download

# 内网镜像
./target/release/herald models download --mirror https://hf-mirror.example
# 或
HF_ENDPOINT=https://hf-mirror.example ./target/release/herald models download

# 指定目录 / 强制重下
./target/release/herald models download --dest /data/herald-models --force
```

下载清单：`model.onnx`、`tokenizer.json`、`config.json`、`special_tokens_map.json`、`tokenizer_config.json`。已存在且非空的文件默认跳过。

默认使用 `Xenova/bge-small-zh-v1.5` 的 ONNX 转换权重：BAAI 官方仓库只提供 PyTorch 权重，没有 `model.onnx`。模型本身仍是 BAAI 的 `bge-small-zh-v1.5`；也可以显式指定其他含 ONNX 文件的仓库。

下载目录规则：

- 默认把 `model.onnx` 和 4 个 tokenizer 文件放在模型目录。
- 配置了 `semantic.tokenizer_path` 时，`model.onnx` 写入模型目录，tokenizer 文件写入该目录。
- `--dest` 是显式覆盖，会把 5 个文件全部放进指定目录；它不会修改配置文件，需要时请自行同步 `model_path` / `tokenizer_path`。

模型或 tokenizer 缺失不会阻塞同步：embedding 写入会跳过并给出一次警告；关键词检索照常可用，语义检索会返回 `MODEL_ERROR`。

### 4.3 同步与查看

```bash
./target/release/herald accounts
./target/release/herald sync --all
./target/release/herald sync --with-body --with-attachments
./target/release/herald sync --index          # 为已缓存正文批量升级全文 embedding

./target/release/herald list --limit 20 --offset 0
./target/release/herald list --unread                      # 只看本地未读
./target/release/herald search 项目                            # 默认：有模型时混合
./target/release/herald search 项目进度 --mode keyword
./target/release/herald search 项目进度 --mode semantic
./target/release/herald search 项目进度 --mode hybrid
./target/release/herald search 项目进度 --semantic             # 等价于 --mode semantic
./target/release/herald search 张三 --field from
./target/release/herald search 李四 --field to                 # 同时匹配 To 与 Cc
./target/release/herald search 周报 --field subject
./target/release/herald search 下周计划 --field body
./target/release/herald show <uidl>                            # 同时标记为本地已读
./target/release/herald show <uidl> --full-body
./target/release/herald show <uidl> --raw
./target/release/herald mark <uidl>                            # 标记已读（仅本地）
./target/release/herald mark <uidl> --unread                   # 标记未读（仅本地）
./target/release/herald status

./target/release/herald export-attachment <uidl> 0 ./exports
```

全局参数：`--config <path>`、`--json`（所有命令均可脚本化读取）。

`search` 默认 `--mode auto`：`semantic.enabled` 打开且模型文件齐全时使用混合检索，否则用关键词。混合检索把关键词（bm25）与语义（向量距离）两路结果按名次做 RRF 融合（`1/(60+名次)`，按 UIDL 去重），不需要校准两种分数量纲；语义不可用时自动降级为关键词，并在命令行 stderr 或 TUI 状态栏明确提示。

`--field` 可选 `all`（默认）、`from`、`to`、`subject`、`body`。字段限定使用对相应列的 `LIKE` 子串匹配（`to` 同时匹配 To 与 Cc），不走 FTS；因为 embedding 覆盖整封邮件，字段限定检索固定使用关键词模式，`--mode semantic` 会被拒绝，混合模式则自动只走关键词并给出提示。`auto` 在字段限定时同样落到关键词。

已读/未读只存在本地 SQLite 的 `is_read` 列。`show` 与 TUI 中按 `Enter` 打开详情会置为已读（仅预览不置读），`mark` 可显式改回；服务器端状态不受影响。旧数据库首次打开会自动补上该列。

### 4.4 TUI

```bash
./target/release/herald tui [--config <path>] [--account <name>]
```

TUI 已并入 `herald`，不再单独产出 `herald-tui` 可执行文件。

选中邮件会立即显示**本地已缓存的详情**，移动选择不访问网络；正文尚未缓存时详情区会提示按 `Enter` 从服务器拉取。原始邮件视图会把 CRLF/制表符/控制字符规范化，并限制在 256KB 内显示；查看时按 `/` 可在原文中查找关键字，输入框位于原文视图底部，边输边高亮并跳到第一处，`Enter`/`n` 向下、`N` 向上循环匹配。列表左侧 `●` 表示本地未读；预览不会改状态，按 `Enter` 打开详情才置为已读。每页邮件数由 `[ui] page_size` 控制。

所有面向人的时间（TUI 顶栏/列表/详情、CLI 的 `list`/`show`/`status`/`sync` 输出）统一换算为**北京时间（UTC+8）**显示；JSON 与 MCP 返回值仍保留原始 RFC 3339，便于程序处理。详情区按终端宽度自动换行：收件人/抄送超过 12 个时显示为“前 12 个 …（共 N 个）”，正文超过 32KB 时在顶部提示并只显示前 32KB，完整原文按 `r` 查看（最多 256KB）。

| 按键 | 作用 |
|---|---|
| `s` / `S` | 增量同步 / 同步全部新邮件 |
| `/` | 搜索；输入框内 `Tab` 循环关键词 → 混合 → 语义（默认混合，需模型就绪），`Ctrl+F` 循环全部字段 → 发件人 → 收件人 → 主题 → 正文 |
| `a` | 切换账户 |
| `o` / `O` | 切换排序字段（日期/发件人/主题）/ 反向 |
| `[` `]` / PgUp PgDn | 翻页 |
| `↑ ↓ j k` | 移动选择（详情聚焦时滚动正文） |
| `g` / `G` | 跳到首/末条 |
| `Enter` | 加载未缓存正文 / 重新拉取详情（并标记本地已读） |
| `u` | 标记已读 / 未读（仅本地） |
| `Tab` | 切换列表/详情焦点 |
| `r` | 查看原始邮件（核对 RFC822 头部、编码与 MIME 边界）；原文内按 `less` 习惯翻页：`空格`/`f` 下页、`b` 上页、`d`/`u` 半页、`↑`/`↓` 单行、`g`/`G` 首尾；`/` 查找、`Enter`/`n`/`N` 跳转匹配 |
| `e` | 导出附件 |
| `Esc` | 清除搜索 / 返回 |
| `h` | 帮助（含实际生效路径与模型状态） |
| `q` / `Ctrl+C` | 退出 |

所有单行输入框（搜索、新建目录、原文查找、附件重命名）都支持 `←`/`→` 移动光标、`Home`/`End` 跳首尾、`Backspace`/`Delete` 删除；中文按字符边界移动，不会拆开半个汉字。

附件导出使用内置文件浏览器，不调起系统 GUI：

- 目录列表选择保存目录，`d` 用附件原名保存到当前目录（重名自动加序号，不覆盖）；
- `Enter` 进入选中目录或选中文件；`退格` / `←` / `h` 返回上一级，列表首行的 `..（上一级）` 也可用 `Enter` 进入；
- `/` 打开筛选框，边输入边按名称过滤（含文件）；查询带路径分隔符时按完整路径匹配，输入存在的绝对路径（macOS/Linux 如 `/usr/local`，Windows 如 `C:\Tools`）后 `Enter` 直接跳转；
- 到盘符根目录（如 `D:\`）后，列表会列出其他可用盘符（如 `C:\`），选中后 `Enter` 即可切换；
- `f` 编辑文件名后 `Enter` 保存为指定路径；
- `n` 在当前目录新建子目录；
- 起始目录优先级：`attachments.export_dir` → 上次导出目录 → 系统 Downloads → 用户主目录；
- 终端小于 `40×8` 时显示提示，不崩溃；设置 `NO_COLOR=1` 或 `TERM=dumb` 时关闭颜色。

#### 用假邮件试用（无需真实邮箱）

在 `herald.toml` 里加一行 `demo = true` 即可。启动时会在进程内起一个假 POP3 服务，注入 25 封中文样例邮件，使用系统临时目录里的独立数据库并自动完成首次同步；**进程退出后临时数据库与假邮箱一起清空**，下次启动又是干净状态。账户名固定为 `demo`。

```toml
demo = true
```

```bash
./target/debug/herald.exe --config D:/herald-demo.toml tui
# 或者先看看同步结果
./target/debug/herald.exe --config D:/herald-demo.toml list --limit 5
./target/debug/herald.exe --config D:/herald-demo.toml search 评审 --field subject
```

`demo = true` 时配置文件里的 `[[accounts]]` 会被忽略，因此可以只写这一行；`database` 会被临时数据库覆盖，`attachments.export_dir` 若已配置则保留（便于导出到固定目录），未配置时默认指到临时目录。语义检索仍按 `[semantic]` 的配置读取模型目录；模型文件存在时，首次自动同步会顺带写入向量，语义/混合检索可直接使用。首次使用前需下载模型（约 95 MB，可重复执行；这里走 `hf-mirror.com`，能直连 HuggingFace 时去掉 `--mirror`）：

```bash
./target/debug/herald.exe --config D:/herald-demo.toml models download --mirror https://hf-mirror.com
./target/debug/herald.exe --config D:/herald-demo.toml search 团队开会讨论了什么 --mode semantic
```

每次启动都会重新同步并重建索引；25 封邮件在本地 CPU 上通常一两秒内完成。模型未下载时不做 embedding，关键词搜索照常可用，混合检索会自动退化为关键词并在状态栏提示。

默认 25 封样例邮件（含分页）。内容覆盖：纯文本、HTML、multipart/alternative、UTF-8/GBK/BIG5、8bit/base64/quoted-printable、无主题、空正文、折叠长主题，以及 PDF/文本/二进制/内嵌图片附件。最新一封“超长内容测试：40 个收件人与长正文”专用于验证详情换行、收件人折叠和正文截断（40 个收件人、正文约 40KB）；“本周项目进度汇报”带 `说明.txt`、`data.bin` 和内嵌 `logo.png`；“电子发票（仅附件）”只有 `发票.pdf`。可搜索“对账”或“评审”，或在 TUI 里选中邮件按 `e` 试导出。

需要固定端口、自定义账号或调整样例数量时，仍可单独运行 mock 服务（终端 A 启动、终端 B 使用生成的配置，语义索引可跨进程保留）：

```bash
cargo run -p herald-core --features test-support --example mock_pop3 -- --dir D:/herald-demo
```

可选参数：`--messages <1-29>`、`--port <port>`（默认自动分配）、`--no-top` 模拟不支持 TOP 的服务器。

### 4.5 MCP

```json
{
  "mcpServers": {
    "herald": {
      "command": "D:/Code/herald/target/release/herald-mcp.exe",
      "args": ["--config", "D:/Code/herald/target/release/herald.toml"]
    }
  }
}
```

也可以用环境变量 `HERALD_CONFIG` 指定配置。传输仅支持 **stdio**；日志写 stderr（默认 `warn`，可用 `HERALD_LOG` 调整），不会污染 JSON-RPC 通道。

#### 工具

| 工具 | 输入 | 输出 |
|---|---|---|
| `list_accounts` | 无 | `{ accounts: [...] }` |
| `sync_mailbox` | `account?`, `limit?`, `with_body?` | `{ account, fetched, new_count, last_uidl, synced_at, ... }` |
| `list_messages` | `account?`, `limit?=20`, `offset?`, `from?`, `since?`, `until?`, `unread_only?` | `{ total, messages }`（含 `is_read`） |
| `get_message` | `account?`, `uidl`, `include_full_body?=false` | 结构化邮件（含 `is_read`）；**无 attachments 字段** |
| `get_raw_message` | `account?`, `uidl` | `{ uidl, raw, truncated }` |
| `search_messages` | `account?`, `query`, `field?`(all/from/to/subject/body), `limit?=20`, `offset?` | `{ total, messages }` |
| `get_sync_status` | `account?` | `{ account, last_sync_at, last_uidl, total_messages }` |

硬性约束：

- 列表 `limit` 合法范围 1–100，默认 20；越界返回 `INVALID_ARGUMENT`。
- `get_message` 默认正文截断 **32KB**，`include_full_body: true` 可关闭。
- `get_raw_message` 最大 **256KB**，超出时 `truncated: true`。
- `sync_mailbox` 不支持附件参数，也不会构建 embedding。
- 错误以 `{ "code": "...", "message": "..." }` 返回，`isError: true`；不 panic。

---

## 5. 路径与便携模式

| 内容 | 默认位置 |
|---|---|
| 配置 | 可执行文件目录 / `herald.toml` |
| 数据库 | 配置所在目录 / `herald.db`（可由 `database` 覆盖，相对路径按配置文件目录解析） |
| 模型 | 配置所在目录 / `models/<model_name>/`（可由 `semantic.model_path` 覆盖） |
| Tokenizer | 默认与模型目录相同（可由 `semantic.tokenizer_path` 单独覆盖） |
| 上次导出目录 | 数据库内 `local_state` 表 |

如果可执行文件目录不可写，Herald 自动降级到用户目录，并在启动日志中打印原因：

- Linux：`~/.config/herald/`、`~/.local/share/herald/`
- macOS：`~/Library/Application Support/herald/`
- Windows：`%APPDATA%\herald\`

`herald tui` 的 `h` 帮助页会显示实际生效的配置/数据库/模型/Tokenizer 路径及其存放模式；CLI 启动日志同样输出这些信息。

---

## 6. 配置校验要点

- `password` 与 `password_env` 必须且只能配置一个。
- `name` 唯一，`host` / `username` 非空，`port` 必填。
- 多个账户且没有 `default_account` 时，必须显式传 `account`。
- `sync.default_limit > 0`；`auto_body_max_size = 0` 表示不限。
- `semantic.enabled = false` 时 CLI/TUI 不加载模型；`models download` 仍可独立执行。
- `semantic.model_path` / `semantic.tokenizer_path` 的相对路径均按配置文件所在目录解析；`tokenizer_path` 缺省时复用模型目录。
- 单附件大小上限默认 50MB，超限内容不写入 BLOB。

---

## 7. 安全边界

- 密码只参与认证，不写日志；仓库不包含任何真实账户配置。
- 仅使用 POP3 只读命令（CAPA/UIDL/TOP/RETR/QUIT）。
- SQLite 全部参数化查询；FTS 查询逐词加引号，避免注入 FTS 运算符。
- MCP 不暴露附件、文件系统路径、模型路径，也不能触发附件下载或模型下载。
- 附件内容默认只进 SQLite BLOB；只有显式导出才写文件。
- 模型下载会拒绝非 http(s) 源、拒绝覆盖非模型目录（除非 `--force`）、写入前检查目标目录可写。
- 路径降级绝不静默，必须记录并展示实际路径与原因。

---

## 8. 测试

```bash
# 全 workspace（包含 CLI/TUI 的 semantic + mock POP3 集成测试）
cargo test --workspace

# 只测 core（可选语义/测试支持）
cargo test -p herald-core
cargo test -p herald-core --features semantic,test-support

cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
```

覆盖范围：

- POP3：plain / STLS / TLS、TOP 与无 TOP 回退、认证失败、三种模式端到端。
- 同步：UIDL 增量（二次同步 `new_count = 0`）、limit、单封失败不阻塞、正文阈值。
- 解析：GBK/BIG5/UTF-8 头部与正文、HTML、多附件、内嵌图片元数据。
- 搜索：中文 FTS、分页、LIKE 回退、sqlite-vec KNN。
- MCP：真实二进制 stdio 往返，逐个调用 7 个工具并校验错误码。
- 模型：本地 HTTP 镜像模拟 HF 目录结构、跳过/强制/非模型目录保护。
- TUI：TestBackend 渲染列表、详情、附件选择、文件浏览器、新建目录、导出、搜索、原文、窄终端与无颜色降级。

> TUI 的真实终端交互仍建议人工走查一次（自动化测试不覆盖键盘焦点与终端差异本身）。

CI 在 Ubuntu / macOS / Windows 上执行 `fmt + clippy + test`，见 [.github/workflows/ci.yml](.github/workflows/ci.yml)。

---

## 9. 已知限制

- 不支持 IMAP、SMTP 发信、删除/标记/移动邮件。
- 不启用定时后台同步（仅保留同步接口）。
- MCP 不暴露附件，也不提供语义/混合检索（未编译语义能力）。
- 语义模型不随仓库分发，需要单独下载或离线拷贝。
- 单实例内账户共享一个 SQLite 数据库；未实现多用户权限。

---

## 10. 设计文档

实现细节、数据模型、同步/搜索流程、降级策略与关键取舍见 [DESIGN.md](DESIGN.md)。
