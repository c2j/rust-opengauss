# GaussDB-MCP 用户手册

面向 `gaussdb-mcp` 终端用户的完整指南 — openGauss/PostgreSQL 数据库的 MCP 服务器和 CLI 工具。

## 目录

- [安装](#安装)
- [配置](#配置)
- [MCP 模式（AI 助手集成）](#mcp-模式ai-助手集成)
- [CLI 模式（直接执行 SQL）](#cli-模式直接执行-sql)
- [连接诊断](#连接诊断)
- [密码管理](#密码管理)
- [TLS / SSL 配置](#tls--ssl-配置)
- [故障排查](#故障排查)
- [常见问题](#常见问题)

---

## 安装

### 前置条件

- **Rust** 1.85+（通过 [rustup](https://rustup.rs/) 安装）
- 一个 openGauss 或 PostgreSQL 服务器（9.5+ 版本）

### 从源码构建

```sh
git clone https://github.com/c2j/rust-opengauss.git
cd rust-opengauss
cargo build -p gaussdb-mcp --release
```

编译后的二进制文件位于 `target/release/gaussdb-mcp`（也可使用 `gaussdb` 名称）。

### 验证安装

```sh
gaussdb-mcp --help
```

---

## 配置

`gaussdb-mcp` 支持三种配置方式（按优先级排列）：

### 1. 环境变量

```sh
export GAUSSDB_URL="host=127.0.0.1 user=myuser password=mypass dbname=mydb"
export DATABASE_URL="host=127.0.0.1 user=myuser password=mypass dbname=mydb"  # 同样有效
```

`GAUSSDB_URL` 和 `DATABASE_URL` 均被接受，`GAUSSDB_URL` 优先。

### 2. 配置文件

默认位置：`~/.gaussdb-mcp.toml`

**单连接：**

```toml
host = "127.0.0.1"
port = 5432
user = "myuser"
password = "mypass"
dbname = "mydb"
```

**多连接：**

```toml
default_connection = "development"

[[connections]]
name = "development"
host = "127.0.0.1"
port = 5432
user = "dev"
password = "devpass"
dbname = "devdb"

[[connections]]
name = "production"
host = "db-prod.example.com"
port = 5432
user = "admin"
password = "keyring"       # 存储在操作系统密钥链中
dbname = "proddb"
sslmode = "require"
```

### 3. 自定义配置路径

```sh
gaussdb-mcp mcp --config /path/to/config.toml
gaussdb-mcp cli --config /path/to/config.toml --sql "SELECT 1"
```

### 连接 URL 格式

连接可以指定为 URL 字符串或单独字段：

```toml
# URL 格式（一体化）
url = "host=10.0.0.5 port=5432 user=admin password=secret dbname=mydb sslmode=require"

# 字段格式（分别指定）
host = "10.0.0.5"
port = 5432
user = "admin"
password = "secret"
dbname = "mydb"
sslmode = "require"
```

两种格式等效。同时存在时，URL 格式优先。

---

## MCP 模式（AI 助手集成）

### 概述

MCP 模式允许 AI 助手（Claude、Cursor 等）通过标准化的 MCP 协议查询您的 openGauss/PostgreSQL 数据库。服务器通过 stdio 运行 — 无需网络端口。

### 启动服务器

```sh
# 启动 MCP 服务器（默认模式）
gaussdb-mcp

# 显式指定 MCP 模式
gaussdb-mcp mcp

# 使用自定义配置
gaussdb-mcp mcp --config ~/my-gaussdb-config.toml
```

服务器启动后等待 MCP 客户端通过 stdin/stdout 连接。

### 可用的 MCP 工具

| 工具 | 功能 | AI 提示示例 |
|------|-------------|-------------------|
| `list_connections` | 显示所有已配置数据库及其状态 | "有哪些数据库已连接？" |
| `get_database_info` | 获取服务器版本、编码、用户信息 | "显示数据库服务器信息" |
| `list_tables` | 列出所有表和视图 | "这个数据库有哪些表？" |
| `get_table_metadata` | 获取表的列、主键、索引信息 | "显示 users 表的 Schema" |
| `execute_query` | 执行 SELECT 或 EXPLAIN 查询 | "我们有多少活跃用户？" |
| `get_execution_plan` | 获取查询执行计划 | "解释这个慢查询的执行计划" |

### 集成示例

#### Claude Desktop

1. 打开 Claude Desktop 设置
2. 进入 "Developer" 部分
3. 在 `claude_desktop_config.json` 中添加：

```json
{
  "mcpServers": {
    "gaussdb": {
      "command": "/path/to/target/release/gaussdb-mcp",
      "args": ["mcp", "--config", "/path/to/gaussdb-mcp.toml"]
    }
  }
}
```

4. 重启 Claude Desktop
5. 在 MCP 工具列表中查看 gaussdb 工具

#### Cursor

在项目根目录的 `.cursor/mcp.json` 中添加：

```json
{
  "mcpServers": {
    "gaussdb": {
      "command": "/path/to/target/release/gaussdb-mcp",
      "args": ["mcp", "--config", "/path/to/gaussdb-mcp.toml"]
    }
  }
}
```

### 多连接使用

配置了多个连接后，AI 助手可以在它们之间切换：

```
用户："显示 production 数据库中 orders 表的 Schema"
AI：[调用 get_table_metadata，connection_name="production"]

用户："和 development 的 Schema 对比一下"
AI：[调用 get_table_metadata，connection_name="development"]
```

`list_connections` 工具可帮助 AI 发现可用的数据库。

### 错误响应

查询失败时，MCP 服务器返回丰富的错误上下文：

```json
{
  "code": -32000,
  "message": "[SQLSTATE 42P01 | SQLCODE -204] execute_query 失败: relation \"nonexistent\" 不存在",
  "data": {
    "sqlstate": "42P01",
    "sqlcode": -204,
    "severity": "ERROR",
    "message": "relation \"nonexistent\" 不存在",
    "sql": "SELECT * FROM nonexistent"
  }
}
```

这有助于 AI 助手理解并解释数据库错误。

---

## CLI 模式（直接执行 SQL）

### 概述

CLI 模式提供类似 `psql` 的接口，可直接从终端执行 SQL。支持 SELECT、DML（INSERT/UPDATE/DELETE）和 DDL（CREATE/ALTER/DROP）语句。

### 基本用法

```sh
# 执行单条查询
gaussdb-mcp cli --sql "SELECT version()"

# 从文件执行
gaussdb-mcp cli --file ./queries/report.sql

# 通过管道传入 SQL
cat query.sql | gaussdb-mcp cli
echo "SELECT * FROM users LIMIT 10" | gaussdb-mcp cli
```

### 输出格式

#### 表格（默认）

```sh
gaussdb-mcp cli --sql "SELECT id, name, email FROM users LIMIT 3"
```
```
 id | name  | email
----+-------+------------------
  1 | Alice | alice@email.com
  2 | Bob   | bob@email.com
  3 | Carol | carol@email.com
(3 rows)
```

#### JSON

```sh
gaussdb-mcp cli --sql "SELECT id, name FROM users LIMIT 2" --format json
```
```json
{
  "columns": ["id", "name"],
  "rows": [[1, "Alice"], [2, "Bob"]],
  "row_count": 2
}
```

#### 垂直格式

```sh
gaussdb-mcp cli --sql "SELECT * FROM users LIMIT 1" --format vertical
```
```
-[ RECORD 1 ]-
id    | 1
name  | Alice
email | alice@email.com
(1 row)
```

### 执行 DML/DDL

与 MCP 模式（只读）不同，CLI 模式支持写操作：

```sh
# INSERT
gaussdb-mcp cli --sql "INSERT INTO logs (message) VALUES ('服务器已启动')"

# UPDATE
gaussdb-mcp cli --sql "UPDATE users SET active = true WHERE id = 1"

# DELETE
gaussdb-mcp cli --sql "DELETE FROM sessions WHERE created_at < now() - interval '30 days'"

# DDL
gaussdb-mcp cli --sql "CREATE TABLE metrics (id serial PRIMARY KEY, value float)"
gaussdb-mcp cli --sql "ALTER TABLE users ADD COLUMN last_login timestamptz"
```

对于非 SELECT 语句，输出显示受影响的行数。

### 指定目标连接

```sh
# 使用特定的命名连接
gaussdb-mcp cli --connection production --sql "SELECT count(*) FROM orders"

# 使用包含命名连接的自定义配置文件
gaussdb-mcp cli --config ./prod-config.toml --connection staging --sql "SELECT version()"
```

### 脚本集成

CLI 模式非常适合 Shell 脚本和自动化：

```sh
#!/bin/bash
# backup-verify.sh: 验证数据库备份

ROW_COUNT=$(gaussdb-mcp cli --connection prod --format json \
    --sql "SELECT count(*) FROM important_table" | jq '.rows[0][0]')

if [ "$ROW_COUNT" -gt 0 ]; then
    echo "备份已验证：important_table 中有 $ROW_COUNT 行"
else
    echo "错误：表似乎为空！" >&2
    exit 1
fi
```

---

## 连接诊断

`--check-connection` 标志执行全面的连接测试：

```sh
gaussdb-mcp --check-connection
gaussdb-mcp --check-connection --verbose
gaussdb-mcp --check-connection production --config ~/prod-config.toml
```

### 测试内容

1. **密钥链状态** — 操作系统密钥链是否可用？密码是否已存储？
2. **NoTLS（明文 TCP）** — 能否无加密连接？
3. **TLS（跳过验证）** — 能否使用加密但放宽证书验证的连接？
4. **TLS（完整验证）** — 能否在严格证书验证下连接？

### 示例输出

```
连接: development

[Keyring] 密码从操作系统密钥链读取 (用户: dev@127.0.0.1/devdb)
  ✓ 密钥链可访问，密码已获取 (5 字符)

[1/3] 尝试 NoTls (明文 TCP) → host=127.0.0.1 user=dev password=**** dbname=devdb ...
  ✓ 已连接
    PostgreSQL 17.0 on x86_64-pc-linux-gnu, compiled by gcc...

[2/3] 尝试 TLS (跳过证书验证) → host=127.0.0.1 user=dev ... sslmode=require ...
  ✗ TLS 握手失败: 服务器不支持 TLS

[3/3] 尝试 TLS (验证证书) → host=127.0.0.1 user=dev ... sslmode=require ...
  ✗ TLS 握手失败: 服务器不支持 TLS

═══════════════════════════════════════════════════════════
  连接诊断摘要
═══════════════════════════════════════════════════════════
  NoTls                ✓  PostgreSQL 17.0... (15ms)
  TLS (不验证)          ✗  服务器不支持 TLS
  TLS (验证)            ✗  服务器不支持 TLS

  数据库版本:
    PostgreSQL 17.0 on x86_64-pc-linux-gnu...

推荐：使用 NoTls 模式。
```

### 详细模式（`-v`）

增加详细的服务器信息：
- 服务器版本及版本号
- 协议版本
- 当前用户和数据库
- 服务器地址和端口
- 启动时间
- 恢复状态（主库或备库）
- TLS 会话详情（版本、加密套件）
- 服务器配置（max_connections、shared_buffers、work_mem、timezone、data_directory）
- TLS 证书详情（颁发者、主题、序列号、有效期）
- 连接耗时

---

## 密码管理

### 存储密码

```sh
# 为配置文件中的第一个连接存储密码（交互式提示输入）
gaussdb-mcp store-password --config ~/.gaussdb-mcp.toml

# 为命名连接存储密码
gaussdb-mcp store-password --name production --config ~/.gaussdb-mcp.toml

# 非交互模式（从 stdin 管道读取，适用于 CI/脚本）：
#   printf '%s\n' "$PW" | gaussdb-mcp store-password --name production --config ~/.gaussdb-mcp.toml
```

### 工作原理

1. 密码存储在**操作系统原生密钥链**中：
   - **macOS**：钥匙串访问
   - **Windows**：凭据管理器
   - **Linux**：Secret Service（GNOME Keyring / KDE Wallet）
2. 配置文件中用 `password = "keyring"`（哨兵值）引用密码
3. 连接时从密钥链读取密码

### 自动迁移

如果配置文件中以明文密码启动：

```toml
password = "my-plaintext-password"
```

在**首次成功的 MCP 连接**时，gaussdb-mcp 会：
1. 将密码存储到操作系统密钥链
2. 将配置文件更新为 `password = "keyring"`
3. 正常继续使用该连接

此过程对用户透明 — 无需手动操作。

### 安全说明

- 迁移后配置文件**永远不**包含密码（仅含 `"keyring"` 哨兵值）
- 密码仅在活跃连接期间存在于内存中
- 环境变量密码（`GAUSSDB_URL`）永远不会写入密钥链或配置文件
- 日志和错误消息中的连接 URL 会脱敏处理（`****`）

---

## TLS / SSL 配置

### SSL 模式

| sslmode | 行为 |
|---------|----------|
| `disable` | 不使用加密（默认） |
| `require` | 要求 TLS，跳过证书验证 |
| `verify-ca` | 要求 TLS，验证 CA |
| `verify-full` | 要求 TLS，验证主机名和 CA |

### 配置示例

```toml
# 在配置文件中
[[connections]]
name = "secure-db"
host = "db.example.com"
user = "admin"
password = "keyring"
dbname = "secure"
sslmode = "verify-full"
```

```sh
# 通过环境变量
export GAUSSDB_URL="host=db.example.com user=admin password=secret dbname=secure sslmode=verify-full"
```

### 自动检测连接方式

使用 `--check-connection` 自动确定服务器的正确 TLS 模式：

```sh
gaussdb-mcp --check-connection
```

该工具测试所有三种模式并推荐应使用哪种。

---

## 故障排查

### "未找到连接配置"

**原因：** 没有配置文件，也没有环境变量。

**解决方案**（任选其一）：
```sh
# 方式一：设置环境变量
export GAUSSDB_URL="host=localhost user=postgres password=secret dbname=postgres"

# 方式二：创建配置文件
cat > ~/.gaussdb-mcp.toml << 'EOF'
host = "localhost"
user = "postgres"
password = "secret"
dbname = "postgres"
EOF

# 方式三：显式传递配置文件
gaussdb-mcp --config /path/to/config.toml
```

### "数据库连接失败"

**常见原因：**
- 数据库服务器未运行
- 主机、端口、用户或数据库名称错误
- 网络/防火墙阻止访问
- `pg_hba.conf` 不允许此客户端连接

**诊断步骤：**
```sh
# 运行连接诊断
gaussdb-mcp --check-connection --verbose

# 验证服务器是否可达
psql -h localhost -U postgres -d postgres -c "SELECT 1"

# 在服务器上检查 pg_hba.conf 中针对您的 IP/用户的条目
```

### "未找到密钥链密码"

**原因：** 配置文件中为 `password = "keyring"`，但密钥链中未存储密码。

**解决方案：**
```sh
# 存储密码（交互式提示输入）
gaussdb-mcp store-password --config ~/.gaussdb-mcp.toml

# 或临时使用明文（会自动迁移）
# 编辑配置文件：将 password = "keyring" 改为 password = "YourPassword"
```

### "仅允许 SELECT 和 EXPLAIN 查询"（MCP 模式）

**原因：** MCP 工具仅支持只读查询。这是安全特性。

**解决方案：** 使用 CLI 模式执行 DML/DDL：
```sh
gaussdb-mcp cli --sql "INSERT INTO ..."
```

### 日志文件位置

日志写入位置：
- **Linux**：`~/.local/share/gaussdb-mcp/gaussdb-mcp.log`
- **macOS**：`~/Library/Application Support/gaussdb-mcp/gaussdb-mcp.log`

启用调试日志：
```sh
RUST_LOG=gaussdb_mcp=debug gaussdb-mcp
```

---

## 常见问题

### 支持哪些数据库？

openGauss 和 PostgreSQL 9.5+。该工具使用标准的 PostgreSQL 线协议（v3.0+）。

### 可以不使用 AI 助手吗？

可以！使用 CLI 模式（`gaussdb-mcp cli`）从终端或脚本中直接执行 SQL。

### 可以配置多少个连接？

没有硬性限制。配置文件中的每个 `[[connections]]` 条目创建一个连接。

### 我的密码安全吗？

- 迁移后配置文件中使用 `"keyring"` 哨兵值（而非明文）
- 密码存储在操作系统原生的加密存储中
- 日志/错误消息中的 URL 已脱敏
- 建议：生产环境使用密钥链 + TLS

### 可以和 Docker 一起使用吗？

可以。参见仓库中的 `docker-compose.yml` 获取 PostgreSQL 测试环境：
```sh
docker compose up -d
gaussdb-mcp cli --sql "SELECT version()"
```

### 这与 psql 有什么不同？

- `psql` 是一个全功能的交互式终端
- `gaussdb-mcp cli` 是一个非交互式 SQL 执行器，适合脚本使用
- `gaussdb-mcp mcp` 是一个用于 AI 助手集成的 MCP 服务器
- 两者均无需安装 libpq 即可操作 openGauss
