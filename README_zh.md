# gaussdb

一个独立的 MCP (Model Context Protocol) 服务器，用于 [openGauss](https://opengauss.org/) 数据库内省，同时内置 CLI 模式支持直接 SQL 执行。专为 OpenCode、OpenClaw、通义灵码（VSCode/IDEA）、Claude、Cursor 等 AI 助手及 MCP 兼容工具设计。

基于 openGauss/PostgreSQL 线协议 (v3.0+) 构建，**零 FFI 依赖** — 无需 libpq，无需 C 库。

## 特性

- **MCP 服务器** — 通过 MCP 协议提供 6 个数据库内省工具
- **CLI 模式** — 从终端直接执行 SQL，支持 `--sql`、`--file` 和标准输入
- **交互式 REPL** — `cli -i` 进入类 readline 的 SQL Shell，支持历史记录、多行输入和点命令（`.help`、`.output`、`.save` 等）
- **多连接支持** — 在单个 TOML 文件中配置多个命名数据库连接，按工具调用切换
- **操作系统密钥链密码** — 通过 macOS 钥匙串 / Windows 凭据管理器 / Linux Secret Service 安全管理密码
- **密码自动迁移** — 首次连接成功时，明文密码自动迁移至操作系统密钥链
- **TLS 支持** — 通过 `sslmode` 参数自动检测 TLS 模式 (disable / require / verify-full)
- **连接诊断** — `check` 子命令测试全部 TLS 模式，提供详细的服务器信息、TLS 证书详情和 GUC 配置
- **丰富的错误报告** — SQLSTATE、SQLCODE、严重等级、详细信息、提示、模式、表、列上下文
- **文件日志** — 每日滚动日志，不干扰 stdio MCP 传输
- **openGauss 认证** — 支持 SHA256、MD5+SHA256、SM3、SCRAM-SHA-256 和 MD5 认证

## 快速开始

### 安装

**下载预编译版本（推荐）：**

从 [GitHub Releases](https://github.com/c2j/rust-opengauss/releases) 下载对应平台的 zip 包，解压后将 `gaussdb` 放入 PATH 即可。

```sh
unzip gaussdb-*.zip
sudo mv gaussdb /usr/local/bin/       # 或放到 ~/.local/bin/
gaussdb --help
```

**从源码构建：**

```sh
# 从源码构建 (需要 Rust 1.85+)
cargo build -p gaussdb-mcp --release

# 二进制文件名为 gaussdb
./target/release/gaussdb --help
```

### 通过环境变量连接

```sh
export GAUSSDB_URL="host=127.0.0.1 user=gaussdb password=secret dbname=postgres"
gaussdb
```

### 使用配置文件

```sh
cat > ~/.gaussdb.toml << 'EOF'
host = "127.0.0.1"
port = 5432
user = "gaussdb"
password = "secret"
dbname = "postgres"
EOF
gaussdb
```

### 快速 CLI 查询

```sh
# 执行 SELECT 查询
gaussdb cli --sql "SELECT version()"

# 从文件执行 SQL
gaussdb cli --file query.sql

# 通过管道输入 SQL
echo "SELECT count(*) FROM users" | gaussdb cli
```

## 使用模式

### 模式一：MCP 服务器（默认）

```sh
gaussdb                    # 默认: MCP 模式，stdio 传输
gaussdb mcp                 # 显式指定 MCP 模式
gaussdb mcp --config ./prod.toml  # 使用自定义配置
```

与 AI 助手集成（参见[与 AI 助手集成](#与-ai-助手集成)）。

### 模式二：CLI 模式

```sh
gaussdb cli [OPTIONS]

OPTIONS:
    -s, --sql <SQL>             SQL 语句
    -f, --file <FILE>           从文件读取 SQL
    -i, --interactive           进入交互式 REPL 模式（见下方模式三）
        --check-connection      测试连接而不执行 SQL
    -v, --verbose               显示详细连接信息（配合 --check-connection）
        --name <NAME>           目标连接名称
        --config <PATH>         配置文件路径
        --format <FMT>          输出格式: table, json, vertical, csv [默认: table]
        --statement-timeout     覆盖配置中的语句超时时间
        --no-history            不读写持久化的 SQL 历史记录（交互模式）
```

**示例：**

```sh
# 表格输出（默认）
gaussdb cli --sql "SELECT * FROM users LIMIT 5"

# JSON 输出
gaussdb cli --sql "SELECT * FROM users LIMIT 5" --format json

# 垂直显示（类似 psql 的 \x）
gaussdb cli --sql "SELECT * FROM users LIMIT 5" --format vertical

# CSV 导出（RFC 4180，流式输出到 stdout；重定向到文件即可保存）
gaussdb cli --sql "SELECT * FROM users" --format csv > users.csv

# 支持 DML/DDL 语句
gaussdb cli --sql "INSERT INTO logs VALUES (1, 'hello')"
gaussdb cli --sql "CREATE TABLE test (id int)"

# 指定连接
gaussdb cli --name prod --sql "SELECT count(*) FROM orders"

# 检查指定连接的连通性
gaussdb cli --check-connection --name prod
```

### 模式三：交互式 REPL

```sh
gaussdb cli -i                  # 使用默认连接进入交互式 REPL
gaussdb cli --interactive        # 同上，完整写法
gaussdb cli -i --name prod       # 指定命名连接
gaussdb cli -i --format json     # 默认输出格式 (table/json/vertical/csv)
```

进入类似 readline 的 SQL Shell：

```
gaussdb interactive — connected to 'dev'
  host 127.0.0.1:5432  user gaussdb  database postgres
end SQL with ';' + Enter to execute (multi-line ok) · .help · .connect · .exit
$ SELECT 1,
> 2,
> 3;
┌──────────┬──────────┬──────────┐
│ ?column? │ ?column? │ ?column? │
├──────────┼──────────┼──────────┤
│ 1        │ 2        │ 3        │
└──────────┴──────────┴──────────┘
(1 row)
$
```

**快捷键**（基于 crossterm 原始模式）：

| 按键 | 功能 |
|------|------|
| `Enter` | 提交当前行（遇到 `;` 时执行） |
| `↑` / `↓` 或 `Ctrl-P` / `Ctrl-N` | 浏览历史 |
| `←` / `→` 或 `Ctrl-B` / `Ctrl-F` | 移动光标 |
| `Home` / `End` 或 `Ctrl-A` / `Ctrl-E` | 跳转到行首/行尾 |
| `Ctrl-U` / `Ctrl-K` | 删除到行首/行尾 |
| `Ctrl-L` | 清屏 |
| `Ctrl-C` | 取消当前输入（保留在 REPL 中） |
| `Ctrl-D` | 空行时退出 / 非空行时删除字符 |
| `Backspace` / `Delete` | 删除前一个/当前字符 |

**点命令**（必须在命令行的开头输入）：

| 命令 | 功能 |
|------|------|
| `.help` 或 `?` | 显示帮助 |
| `.exit` / `.quit` | 退出 REPL |
| `.connect [<name>]` | 断连后重连，或切换到连接 `<name>`（省略参数为当前连接） |
| `.history` | 显示当前会话的 SQL 执行历史 |
| `.clear` / `.cls` | 清屏 |
| `.output <file>` | 将所有后续 SQL 输出重定向到文件（追加模式） |
| `.output` | 将 SQL 输出恢复到 stdout |
| `.save <file> [format]` | 将最近一次查询结果单独保存到文件（覆盖；format 可选，默认与 `--format` 一致） |

```sh
# 在 REPL 中：
$ .output session.log      # 所有查询追加到 session.log
$ SELECT * FROM users;
$ .output                  # 恢复到 stdout
$ .save users.csv csv      # 将最近一次结果导出为 CSV
$ .exit
```

说明：
- 仅当遇到语句外（引号/注释外）的 `;` 时执行 SQL（支持 `'...'`、`"..."`、`--`、`/* ... */`）
- SQL 错误会输出到 stderr，REPL 继续运行
- 如果服务器或防火墙断开空闲连接，下次语句将失败，REPL 会提示运行 `.connect`。`.connect`（无参数）重建当前连接；`.connect <name>` 切换到其他已配置连接，无需重启
- 连续相同的语句会被历史去重
- SQL 历史按连接名持久化存储在应用数据目录下（Linux: `$XDG_DATA_HOME/gaussdb/history/<name>`，macOS: `~/Library/Application Support/gaussdb/history/<name>`）；`↑`/`↓` 可访问之前会话的历史。使用 `--no-history` 可禁用。历史以明文存储，避免在语句中嵌入密码等敏感信息，或使用 `--no-history`
- 超大导出建议使用一次性 `cli --format csv` 而非 REPL，因为 REPL 会缓冲结果以支持 `.save`

## 配置

### 单连接（向后兼容）

```toml
host = "127.0.0.1"
port = 5432
user = "gaussdb"
password = "secret"
dbname = "postgres"
```

### 多命名连接

```toml
default_connection = "dev"

[connections.dev]
host = "127.0.0.1"
port = 5432
user = "gaussdb"
password = "secret"
dbname = "postgres"

[connections.prod]
host = "192.168.1.10"
port = 5432
user = "admin"
password = "keyring"         # 存储在操作系统密钥链中
dbname = "production"

[connections.staging]
url = "host=10.0.0.5 user=admin password=keyring dbname=staging sslmode=require"
```

当存在 `[connections.<name>]` 时，顶层的 `host`、`user` 等字段将被忽略。当不存在时，这些字段将包装为单个 `"default"` 连接 — 完全向后兼容。

`default_connection` 指定在工具未提供 `connection_name` 时使用的默认连接，默认为第一个连接。

每个连接的密码可以是：
- 明文字符串 — 首次成功连接时自动迁移至操作系统密钥链
- `"keyring"` — 从操作系统密钥链读取（使用 `store-password` 子命令设置）

## CLI 选项

```
gaussdb [OPTIONS] [COMMAND]

COMMANDS:
    mcp             作为 MCP 服务器运行（默认，无子命令时）
    check           测试数据库连接并退出
    store-password  将密码存储到操作系统密钥链
    cli             从命令行执行 SQL

全局选项（适用于所有子命令）:
    --config <PATH>           配置文件路径 (默认: ~/.gaussdb.toml)
    --name <NAME>             目标连接名称

SERVE:
    （无额外选项）

CHECK:
    -v, --verbose             显示详细连接信息

STORE-PASSWORD:
    <PASSWORD>                要存储的密码（位置参数）

CLI:
    -s, --sql <SQL>            要执行的 SQL 语句
    -f, --file <FILE>          从文件读取 SQL (或管道传入 stdin)
    -i, --interactive          进入交互式 REPL 模式
        --check-connection     测试连接而不执行 SQL
    -v, --verbose              显示详细连接信息（配合 --check-connection）
        --format <FMT>         输出格式: table, json, vertical, csv [默认: table]
        --statement-timeout    覆盖配置中的语句超时 (如 "30s")
        --connection-max-lifetime  连接回收间隔 (如 "10min")
        --timeout-action       "cancel" (默认) 或 "disconnect"
        --no-history           不读写持久化的 SQL 历史记录（交互模式）
```

### 连接诊断

```sh
# 检查默认连接 (三阶段: NoTls → TLS-skip → TLS-verify)
gaussdb check

# 检查特定命名连接
gaussdb check --name prod --config ~/.gaussdb.toml

# 详细输出 (版本、GUC 参数、TLS 证书详情、耗时)
gaussdb check --verbose

# 也可以通过 cli 子命令使用
gaussdb cli --check-connection --name prod -v
```

诊断工具会：
1. 尝试纯 TCP (NoTls)
2. 尝试 TLS（跳过证书验证）
3. 尝试 TLS（完整证书验证）
4. 报告密钥链状态（可用、空或不可用）
5. 详细模式下：服务器版本、协议版本、GUC 配置、TLS 证书链和耗时

### 密码管理

```sh
# 为第一个/默认连接存储密码（交互式提示输入）
gaussdb store-password --config ~/.gaussdb.toml

# 为命名连接存储密码
gaussdb store-password --name prod --config ~/.gaussdb.toml

# 非交互模式（从 stdin 管道读取，适用于 CI/脚本）：
#   printf '%s\n' "$PW" | gaussdb store-password --name prod --config ~/.gaussdb.toml

# 首次成功 MCP 连接时，配置文件中的明文密码会自动迁移
# 至操作系统密钥链，配置文件更新为 password = "keyring"
```

## MCP 工具参考

| 工具 | 描述 |
|------|-------------|
| `list_connections` | 列出所有配置的连接及其状态（connected/connecting/pending/unavailable） |
| `get_database_info` | 服务器版本、编码、排序规则、启动时间、当前用户、服务器地址 |
| `list_tables` | 所有用户表和视图，包含模式、类型、大小和注释 |
| `get_table_metadata` | 列（名称、类型、可空、默认值、注释）、主键和索引 |
| `execute_query` | 执行只读 SELECT 或 EXPLAIN 查询 |
| `get_execution_plan` | EXPLAIN 或 EXPLAIN ANALYZE，支持 TEXT/JSON/YAML/XML 格式 |

所有工具都接受可选的 `connection_name` 参数以指定目标数据库。不提供时使用 `default_connection`。

### 工具参数

**`list_connections`** — 无参数。返回连接列表，包含状态和默认指示器。

**`get_database_info`**
| 参数 | 类型 | 必填 | 描述 |
|-----------|------|----------|-------------|
| `connection_name` | string | 否 | 目标连接名称 |

**`list_tables`**
| 参数 | 类型 | 必填 | 描述 |
|-----------|------|----------|-------------|
| `connection_name` | string | 否 | 目标连接名称 |

**`get_table_metadata`**
| 参数 | 类型 | 必填 | 描述 |
|-----------|------|----------|-------------|
| `table_name` | string | 是 | 表名 |
| `schema_name` | string | 否 | 模式名称 (默认: public) |
| `connection_name` | string | 否 | 目标连接名称 |

**`execute_query`**
| 参数 | 类型 | 必填 | 描述 |
|-----------|------|----------|-------------|
| `sql` | string | 是 | SQL 查询 (仅限 SELECT 或 EXPLAIN) |
| `timeout_ms` | number | 否 | 单次调用的语句超时（毫秒），覆盖连接默认值 |
| `connection_name` | string | 否 | 目标连接名称 |

**`get_execution_plan`**
| 参数 | 类型 | 必填 | 描述 |
|-----------|------|----------|-------------|
| `sql` | string | 是 | 要解释的 SQL 查询 |
| `analyze` | boolean | 否 | 运行 EXPLAIN ANALYZE (默认: false) |
| `format` | string | 否 | 输出格式: TEXT, JSON, YAML, XML (默认: TEXT) |
| `timeout_ms` | number | 否 | 单次调用的语句超时（毫秒），覆盖连接默认值 |
| `connection_name` | string | 否 | 目标连接名称 |

### 错误响应格式

当数据库发生错误时，MCP 工具返回结构化的错误数据：

```json
{
  "sqlstate": "42P01",
  "sqlcode": -204,
  "severity": "ERROR",
  "message": "relation \"users\" does not exist",
  "detail": null,
  "hint": null,
  "schema": null,
  "table": null,
  "column": null,
  "constraint": null,
  "position": null,
  "sql": "SELECT * FROM users"
}
```

## 与 AI 助手集成

### OpenCode

在 `opencode.json` 中添加：

```json
{
  "mcp": {
    "gaussdb": {
      "command": "/path/to/gaussdb",
      "args": ["mcp", "--config", "/path/to/gaussdb.toml"]
    }
  }
}
```

或通过环境变量：

```json
{
  "mcp": {
    "gaussdb": {
      "command": "/path/to/gaussdb",
      "env": {
        "GAUSSDB_URL": "host=127.0.0.1 user=gaussdb password=secret dbname=postgres"
      }
    }
  }
}
```

### OpenClaw

在 `~/.openclaw/mcp.json` 中添加：

```json
{
  "mcpServers": {
    "gaussdb": {
      "command": "/path/to/gaussdb",
      "args": ["mcp", "--config", "/path/to/gaussdb.toml"]
    }
  }
}
```

### 通义灵码（VSCode / IDEA）

在项目根目录的 `.lingma/mcp.json` 中添加：

```json
{
  "mcpServers": {
    "gaussdb": {
      "command": "/path/to/gaussdb",
      "args": ["mcp", "--config", "/path/to/gaussdb.toml"]
    }
  }
}
```

### Claude Desktop

在 `claude_desktop_config.json` 中添加：

```json
{
  "mcpServers": {
    "gaussdb": {
      "command": "/path/to/gaussdb",
      "args": ["mcp", "--config", "/path/to/gaussdb.toml"]
    }
  }
}
```

### Cursor

在 `.cursor/mcp.json` 中添加：

```json
{
  "mcpServers": {
    "gaussdb": {
      "command": "/path/to/gaussdb",
      "args": ["mcp", "--config", "/path/to/gaussdb.toml"]
    }
  }
}
```

## 语句超时与连接生命周期

`gaussdb` 支持可配置的 SQL 执行超时，防止失控查询阻塞 AI 助手会话。对 MCP 服务端和 CLI 均生效。

### 配置（TOML）

在配置文件中为每个连接设置超时参数：

```toml
[connections.prod]
host = "192.168.1.10"
user = "admin"
password = "keyring"
dbname = "production"

statement_timeout = "30s"              # 取消运行超过 30 秒的查询
connection_max_lifetime = "10min"      # 每 10 分钟回收连接
timeout_action = "cancel"              # "cancel"（默认）或 "disconnect"
```

也可作为全局默认值，被所有连接继承：

```toml
statement_timeout = "30s"
connection_max_lifetime = "10min"
timeout_action = "cancel"
```

支持的时长格式：纯整数（秒），或带后缀：`500ms`、`30s`、`5min`、`1h`、`2d`。

### CLI

```sh
# 为单次 CLI 调用覆盖语句超时
gaussdb cli --sql "SELECT pg_sleep(60)" --statement-timeout 5s

# 超时后强制断开连接（下次调用时重建连接）
gaussdb cli --sql "..." --statement-timeout 30s --timeout-action disconnect

# 设置连接最大生命周期
gaussdb cli --sql "..." --connection-max-lifetime 10min
```

### MCP 工具单次调用覆盖

`execute_query` 和 `get_execution_plan` 接受可选的 `timeout_ms` 参数，为单次调用覆盖连接级默认值：

```json
{
  "sql": "SELECT count(*) FROM huge_table",
  "timeout_ms": 5000
}
```

如省略 `timeout_ms`，则使用连接的全局 `statement_timeout`。

### 工作原理

| 设置 | 行为 |
|------|------|
| `statement_timeout` | 通过 PostgreSQL/openGauss 的 `SET statement_timeout` GUC 在服务端应用。超时后服务端返回 SQLSTATE `57014`（`query_canceled`）。**默认：600 秒**（未配置时）。 |
| `timeout_action = "cancel"`（默认）| 连接保留，下次工具调用继续使用。 |
| `timeout_action = "disconnect"` | 超时后强制回收连接，下次工具调用建立新连接。 |
| `connection_max_lifetime` | 无论是否超时，连接在此时长后强制回收，防止长期运行的 MCP 会话中状态漂移。必须 ≥ `statement_timeout`（启动时校验）。 |

### 校验规则

同时设置 `statement_timeout` 和 `connection_max_lifetime` 时，`statement_timeout` 不得超过 `connection_max_lifetime`。启动时违反此约束将立即报错退出。

## TLS 支持

连接 URL 或配置文件中的 `sslmode=` 参数：

```sh
# 禁用 TLS (默认)
GAUSSDB_URL="host=127.0.0.1 user=gaussdb dbname=postgres sslmode=disable"

# 要求 TLS，跳过证书验证
GAUSSDB_URL="host=127.0.0.1 user=gaussdb dbname=postgres sslmode=require"

# 要求 TLS，完整证书验证
GAUSSDB_URL="host=db.example.com user=gaussdb dbname=postgres sslmode=verify-full"
```

通过 `check` 子命令进行 TLS 自动检测，测试全部三种模式。

## 认证

除标准 PostgreSQL 认证外，还支持 openGauss 特有的认证方法：

| 方法 | 描述 |
|--------|-------------|
| SHA256 密码 | openGauss 基于 RFC 5802 的 SHA256 认证 |
| MD5 + SHA256 | MD5/SHA256 组合认证 |
| SM3 密码 | 中国国家标准 (SM3) |
| SCRAM-SHA-256 | 标准 SCRAM 认证 |
| MD5 密码 | 传统 MD5 认证 |
| 明文密码 | 纯文本（建议配合 TLS 使用） |

## 日志

日志写入 `$XDG_DATA_HOME/gaussdb/gaussdb.log`（Linux 上为 `~/.local/share/gaussdb/`，macOS 上为 `~/Library/Application Support/gaussdb/`），每日滚动。这避免了对 stdio MCP 传输的干扰。

通过 `RUST_LOG` 控制日志级别：

```sh
RUST_LOG=gaussdb_mcp=debug gaussdb
```

## 项目结构

本仓库是一个 Rust 工作区，包含：

- **`crates/gaussdb`** — 面向外部用户的统一公开入口。默认提供异步 `gaussdb::Client`（重新导出 tokio-opengauss），通过 `sync` 特性可使用同步 `gaussdb::sync::Client`，通过 `config` 特性可使用配置感知连接（`gaussdb::config::connect_async` 与 `gaussdb::config::connect_sync`）。TLS 与类型分别通过 `gaussdb::native_tls`、`gaussdb::openssl` 和 `gaussdb::types` 暴露，底层驱动构建块位于 `gaussdb::driver` 下。
- **`tools/gaussdb-mcp`** — MCP 服务器 + CLI 工具（本文档的重点）
- **`crates/tokio-opengauss`** — 异步 openGauss/PostgreSQL 客户端（内部构建模块，`publish = false`）
- **`crates/opengauss`** — 同步 openGauss/PostgreSQL 客户端（内部构建模块，`publish = false`）
- **`crates/opengauss-derive`** — 类型派生的过程宏（内部构建模块，`publish = false`）
- **`crates/opengauss-protocol`** — 线协议 v3.0+ 实现（内部构建模块，`publish = false`）
- **`crates/opengauss-types`** — 类型系统及序列化（内部构建模块，`publish = false`）
- **`crates/opengauss-native-tls`** — tokio-opengauss 的 Native TLS 连接器（内部构建模块，`publish = false`）
- **`crates/opengauss-openssl`** — tokio-opengauss 的 OpenSSL TLS 连接器（内部构建模块，`publish = false`）
- **`tools/codegen`** — 从 PostgreSQL 目录数据生成代码

详见 [docs/DeveloperGuide.md](docs/DeveloperGuide.md)（库使用）和 [CONTRIBUTION.md](CONTRIBUTION.md)（贡献指南）。

## 文档

| 文档 | English | 中文 |
|----------|---------|------|
| 用户手册 | [docs/UserGuide.md](docs/UserGuide.md) | [docs/UserGuide_zh.md](docs/UserGuide_zh.md) |
| 开发者指南 | [docs/DeveloperGuide.md](docs/DeveloperGuide.md) | [docs/DeveloperGuide_zh.md](docs/DeveloperGuide_zh.md) |
| 贡献指南 | [CONTRIBUTION.md](CONTRIBUTION.md) | [CONTRIBUTION_zh.md](CONTRIBUTION_zh.md) |
| README | [README.md](README.md) | [README_zh.md](README_zh.md) |

## 许可证

您可以选择 [Apache License, Version 2.0](license/LICENSE-APACHE) 或 [MIT license](license/LICENSE-MIT) 任一许可证。
