# 开发者指南

本指南面向希望直接使用 `rust-opengauss` 库 crate、扩展 MCP 服务器，或在线协议实现之上构建自定义工具的开发者。

## 目录

- [架构概览](#架构概览)
- [Crate 参考](#crate-参考)
  - [tokio-opengauss](#tokio-opengauss-异步客户端)
  - [opengauss](#opengauss-同步客户端)
  - [opengauss-protocol](#opengauss-protocol)
  - [opengauss-types](#opengauss-types)
  - [opengauss-derive](#opengauss-derive)
  - [opengauss-native-tls / opengauss-openssl](#opengauss-native-tls--opengauss-openssl)
  - [codegen](#codegen)
- [线协议](#线协议)
- [在您的项目中使用该库](#在您的项目中使用该库)
- [扩展 MCP 服务器](#扩展-mcp-服务器)
- [认证流程](#认证流程)
- [功能特性标志](#功能特性标志)
- [安全注意事项](#安全注意事项)

---

## 架构概览

```
┌─────────────────────────────────────────────────────┐
│                     应用层                           │
├──────────────┬──────────────────┬───────────────────┤
│  MCP 服务器   │   自定义工具      │    Web 服务       │
│ (gaussdb-mcp)│                  │                   │
└──────┬───────┴────────┬─────────┴──────────┬────────┘
       │                │                    │
       ▼                ▼                    ▼
┌──────────────┐ ┌──────────────┐  ┌──────────────────┐
│ opengauss    │ │tokio-opengauss│ │   您的代码        │
│ (同步 API)   │ │ (异步 API)   │  │                  │
└──────┬───────┘ └──────┬───────┘  └──────────────────┘
       │                │
       └────────┬───────┘
                ▼
┌──────────────────────────────────────────────────────┐
│              opengauss-protocol                        │
│   消息类型、线格式编解码                                │
│   认证：MD5、SCRAM、SHA256、SM3                        │
└──────────────────────┬───────────────────────────────┘
                       │
                       ▼
┌──────────────────────────────────────────────────────┐
│                opengauss-types                        │
│   ToSql / FromSql trait、类型映射、OID 系统            │
└──────────────────────────────────────────────────────┘
```

### 核心设计原则

1. **零 FFI** — 无 C 依赖（无需 libpq）。纯 Rust 线协议实现。
2. **可组合性** — 协议、类型和客户端位于独立的 crate 中，灵活组合。
3. **可插拔 TLS** — TLS 是抽象的；可选择 native-tls 或 openssl 连接器。
4. **PostgreSQL 兼容** — openGauss 使用 PostgreSQL 线协议 v3.0+，因此本框架同时兼容两者。

---

## Crate 参考

### tokio-opengauss（异步客户端）

**Crate**：`crates/tokio-opengauss`  
**Cargo**：`tokio-opengauss = "0.7.17"`  
**描述**：基于 tokio 构建的异步 openGauss/PostgreSQL 客户端。

#### 关键类型

| 类型 | 描述 |
|------|-------------|
| `Client` | 主异步客户端 — 查询、执行、预编译、COPY、事务 |
| `Connection` | 后台连接任务（使用 `tokio::spawn` 启动） |
| `Config` | 连接配置构建器 |
| `Statement` | 预编译语句 |
| `Portal` | 命名 Portal（游标） |
| `Row` | 查询结果行 |
| `ToStatement` | 转换为语句的 trait（字符串、预编译语句） |
| `NoTls` | 无 TLS 连接的标记类型 |

#### 基本用法

```rust
use tokio_opengauss::{Config, NoTls};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 解析连接字符串
    let config: Config = "host=localhost user=postgres password=secret dbname=mydb"
        .parse()?;

    // 连接
    let (client, connection) = config.connect(NoTls).await?;

    // 启动连接处理任务
    tokio::spawn(async move {
        if let Err(e) = connection.await {
            eprintln!("连接错误: {}", e);
        }
    });

    // 执行查询
    let rows = client
        .query("SELECT id, name FROM users WHERE active = $1", &[&true])
        .await?;

    for row in &rows {
        let id: i32 = row.get(0);
        let name: &str = row.get(1);
        println!("{}: {}", id, name);
    }

    Ok(())
}
```

#### 预编译语句

```rust
let stmt = client.prepare("INSERT INTO users (name, email) VALUES ($1, $2)").await?;
client.execute(&stmt, &[&"Alice", &"alice@example.com"]).await?;
```

#### 事务

```rust
let tx = client.transaction().await?;
tx.execute("UPDATE accounts SET balance = balance - 100 WHERE id = $1", &[&1]).await?;
tx.execute("UPDATE accounts SET balance = balance + 100 WHERE id = $1", &[&2]).await?;
tx.commit().await?;
```

#### COPY 协议

```rust
// COPY FROM
let sink = client.copy_in("COPY users (name, email) FROM STDIN").await?;
let writer = sink.into_writer();
// 向 writer 写入 CSV/TSV 数据...

// COPY TO
let rows = client.copy_out("COPY users TO STDOUT").await?;
// 从流中读取行...
```

#### 查询取消

```rust
let cancel_token = client.cancel_token();
// ...在另一个任务中：
cancel_token.cancel_query(NoTls).await?;
```

#### TLS 连接

```rust
use opengauss_native_tls::MakeTlsConnector;
use native_tls::TlsConnector;

let connector = TlsConnector::builder()
    .danger_accept_invalid_certs(true)  // 或配置正确的证书
    .build()?;
let tls = MakeTlsConnector::new(connector);

let (client, connection) = config.connect(tls).await?;
```

---

### opengauss（同步客户端）

**Crate**：`crates/opengauss`  
**Cargo**：`opengauss = "0.19.13"`  
**描述**：`tokio-opengauss` 的同步封装。当您不需要异步 I/O 时使用。

#### 关键类型

| 类型 | 描述 |
|------|-------------|
| `Client` | 同步客户端 — 查询、执行、预编译、事务 |
| `Config` | 连接配置 |
| `Transaction` | 事务句柄 |
| `BinaryCopyInWriter` | COPY FROM 写入器 |
| `BinaryCopyOutReader` | COPY TO 读取器 |

#### 基本用法

```rust
use opengauss::{Client, NoTls};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut client = Client::connect(
        "host=localhost user=postgres password=secret dbname=mydb",
        NoTls,
    )?;

    let rows = client.query("SELECT id, name FROM users", &[])?;
    for row in &rows {
        let id: i32 = row.get(0);
        let name: &str = row.get(1);
        println!("{}: {}", id, name);
    }

    Ok(())
}
```

同步客户端内部启动了一个 tokio 运行时。对于性能敏感的应用，建议直接使用 `tokio-opengauss`。

---

### opengauss-protocol

**Crate**：`crates/opengauss-protocol`  
**描述**：底层线协议实现。消息序列化/反序列化、认证握手、类型编解码。

#### 关键模块

| 模块 | 描述 |
|--------|-------------|
| `message` | 后端和前端消息类型（Startup、Query、Parse、Bind 等） |
| `authentication` | 认证消息处理（MD5、SCRAM-SHA-256、SHA256、SM3） |
| `codec` | 协议消息的二进制编解码 |

#### 协议消息类型（后端）

```rust
// 服务器发送的消息
pub enum BackendMessage {
    Authentication(AuthenticationMessage),
    BackendKeyData(BackendKeyData),
    BindComplete,
    CloseComplete,
    CommandComplete(CommandComplete),
    CopyData(CopyData),
    CopyDone,
    CopyInResponse,
    CopyOutResponse,
    CopyBothResponse,
    DataRow(DataRow),
    EmptyQueryResponse,
    ErrorResponse(ErrorResponse),
    NegotiateProtocolVersion(NegotiateProtocolVersion),  // PG18+
    NoticeResponse(NoticeResponse),
    NotificationResponse(NotificationResponse),
    ParameterDescription(ParameterDescription),
    ParameterStatus(ParameterStatus),
    ParseComplete,
    PortalSuspended,
    ReadyForQuery(ReadyForQuery),
    RowDescription(RowDescription),
}
```

#### 协议消息类型（前端）

```rust
// 发送到服务器的消息
pub enum FrontendMessage {
    Bind { portal: String, statement: String, formats: Vec<i16>, values: Vec<Option<Bytes>>, result_formats: Vec<i16> },
    CancelRequest { process_id: i32, secret_key: i32 },
    Close { variant: CloseVariant, name: String },
    CopyData(Bytes),
    CopyDone,
    CopyFail(String),
    Describe { variant: DescribeVariant, name: String },
    Execute { portal: String, max_rows: i32 },
    Flush,
    Parse { name: String, query: String, param_types: Vec<Oid> },
    PasswordMessageFamily { password: String },
    Query(String),
    SASLInitialResponse { mechanism: String, data: Bytes },
    SASLResponse(Bytes),
    Startup(Startup),
    Sync,
    Terminate,
    GssResponse(Bytes),
    SSLRequest,
}
```

#### 用法（底层）

```rust
use opengauss_protocol::message::backend::BackendMessage;
use opengauss_protocol::message::frontend::FrontendMessage;
use tokio_util::codec::Framed;

// 通常您不会直接使用此 crate，除非构建自定义驱动。
// tokio-opengauss 在内部处理协议。
```

---

### opengauss-types

**Crate**：`crates/opengauss-types`  
**描述**：类型系统 — `ToSql` 和 `FromSql` trait、OID 映射、类型转换。

#### 关键 Trait

```rust
/// 将 Rust 值转换为 PostgreSQL 参数
pub trait ToSql {
    fn to_sql(&self, ty: &Type, out: &mut BytesMut) -> Result<IsNull, Box<dyn Error + Sync + Send>>;
    fn accepts(ty: &Type) -> bool;
    fn to_sql_checked(&self, ty: &Type, out: &mut BytesMut) -> Result<IsNull, Box<dyn Error + Sync + Send>>;
}

/// 将 PostgreSQL 值转换为 Rust 类型
pub trait FromSql<'a>: Sized {
    fn from_sql(ty: &Type, raw: &'a [u8]) -> Result<Self, Box<dyn Error + Sync + Send>>;
    fn accepts(ty: &Type) -> bool;
}
```

#### 内置类型支持

| Rust 类型 | PostgreSQL 类型 |
|-----------|----------------|
| `bool` | `BOOL` |
| `i16`、`i32`、`i64` | `SMALLINT`、`INTEGER`、`BIGINT` |
| `f32`、`f64` | `REAL`、`DOUBLE PRECISION` |
| `&str`、`String` | `TEXT`、`VARCHAR`、`CHAR(n)` |
| `&[u8]`、`Vec<u8>` | `BYTEA` |
| `SystemTime` | `TIMESTAMPTZ` |
| `IpAddr` | `INET` |
| `HashMap<String, Option<String>>` | `HSTORE` |

#### 可选类型支持（功能特性标志）

在 `tokio-opengauss` 上启用功能标志：

| 功能标志 | 类型支持 |
|---------|-------------|
| `with-chrono-0_4` | `chrono::NaiveDateTime`、`chrono::DateTime<Utc>`、`chrono::NaiveDate`、`chrono::NaiveTime` |
| `with-uuid-1` | `uuid::Uuid` |
| `with-serde_json-1` | `serde_json::Value`（JSON/JSONB） |
| `with-time-0_3` | `time::Date`、`time::Time`、`time::PrimitiveDateTime`、`time::OffsetDateTime` |
| `with-geo-types-0_7` | PostGIS 几何类型 |
| `with-eui48-1` | `eui48::MacAddress`（MACADDR） |
| `with-smol_str-01` | `smol_str::SmolStr` |

---

### opengauss-derive

**Crate**：`crates/opengauss-derive`  
**描述**：`#[derive(ToSql, FromSql)]` 的过程宏。

```rust
use opengauss_types::{ToSql, FromSql};

#[derive(Debug, ToSql, FromSql)]
#[opengauss(name = "my_type")]  // 映射到自定义 PostgreSQL 类型
struct MyType {
    field1: String,
    field2: i32,
}
```

---

### opengauss-native-tls / opengauss-openssl

**描述**：`tokio-opengauss` 的 TLS 连接器实现。

- `opengauss-native-tls` 使用 `native-tls` crate（平台原生 TLS：Windows 上的 SChannel、macOS 上的 Secure Transport、Linux 上的 OpenSSL）
- `opengauss-openssl` 直接使用 `openssl` crate

两者均实现了 `tokio-opengauss::connect()` 所需的 `TlsConnect` trait。

```rust
use opengauss_native_tls::MakeTlsConnector;
use native_tls::TlsConnector;

let connector = TlsConnector::new()?;  // 或使用 builder 配置自定义选项
let tls = MakeTlsConnector::new(connector);
let (client, connection) = tokio_opengauss::connect("host=... sslmode=require", tls).await?;
```

---

### codegen

**Crate**：`tools/codegen`  
**描述**：从 PostgreSQL 目录数据生成类型映射代码的开发工具。正常使用不需要。

---

## 线协议

### 协议版本

本实现支持 PostgreSQL 线协议 **v3.0+**（openGauss 使用此协议）。最近的更新处理了 PostgreSQL 18 的 `NegotiateProtocolVersion` 消息和可变长度 `BackendKeyData`。

### 连接流程

```
客户端                          服务器
  |                               |
  |-- SSLRequest ---------------->|
  |<-- 'S'（接受）/ 'N'（拒绝）--|
  |                               |
  |-- StartupMessage ----------->|
  |   （用户、数据库、参数）       |
  |                               |
  |<-- AuthenticationXxx --------|
  |   （MD5 / SCRAM / SHA256 等）  |
  |                               |
  |-- Password / SASL ---------->|
  |                               |
  |<-- AuthenticationOk ---------|
  |<-- ParameterStatus ----------|
  |<-- BackendKeyData -----------|
  |<-- ReadyForQuery ------------|
  |                               |
  |-- Query / Parse / Bind ----->|
  |<-- RowDescription ----------|
  |<-- DataRow* ----------------|
  |<-- CommandComplete ----------|
  |<-- ReadyForQuery ------------|
```

### Startup 消息

```rust
Startup {
    version: 3,    // 协议主版本号
    version_2: 0,  // 协议次版本号
    parameters: {
        "user": "myuser",
        "database": "mydb",
        "application_name": "myapp",
        // ...任意其他参数
    },
}
```

### 扩展查询协议

扩展查询协议使用 Parse → Bind → Execute → Sync 替代简单的 Query：

```rust
// 1. Parse：预编译语句
Parse { name: "stmt1", query: "SELECT $1::int + $2::int", param_types: vec![] }

// 2. Bind：将参数绑定到语句
Bind { portal: "", statement: "stmt1", formats: vec![], values: vec![...], result_formats: vec![] }

// 3. Describe：获取结果列信息（可选）
Describe { variant: Portal, name: "" }

// 4. Execute：运行 Portal
Execute { portal: "", max_rows: 0 }

// 5. Sync：完成事务
Sync
```

---

## 在您的项目中使用该库

### 添加依赖

**异步（推荐）：**

```toml
[dependencies]
tokio = { version = "1", features = ["full"] }
tokio-opengauss = "0.7.17"
# 可选：TLS
opengauss-native-tls = "0.5.3"
native-tls = "0.2"
```

**同步：**

```toml
[dependencies]
opengauss = "0.19.13"
```

### 连接 URL

连接参数遵循 libpq 惯例：

```
host=localhost port=5432 user=postgres password=secret dbname=mydb
host=db.example.com user=admin password=secret dbname=production sslmode=require
```

或使用 `Config` 构建器：

```rust
let config = tokio_opengauss::Config::new()
    .host("localhost")
    .port(5432)
    .user("postgres")
    .password("secret")
    .dbname("mydb");
let (client, connection) = config.connect(NoTls).await?;
```

### 连接池

对于生产应用，建议使用连接池：

```toml
[dependencies]
deadpool-postgres = "0.14"  # 或 bb8-postgres、mobc-postgres
```

```rust
use deadpool_postgres::{Config, Pool};
// deadpool-postgres 可通过 Manager trait 与 tokio-opengauss 配合使用
```

### 错误处理

```rust
use tokio_opengauss::error::SqlState;

match client.query("SELECT * FROM nonexistent", &[]).await {
    Err(e) => {
        if let Some(db_err) = e.as_db_error() {
            println!("SQLSTATE: {}", db_err.code().code());
            println!("消息: {}", db_err.message());
            println!("详情: {:?}", db_err.detail());
            println!("提示: {:?}", db_err.hint());
        }
    }
    Ok(rows) => { /* 处理行 */ }
}
```

---

## 扩展 MCP 服务器

### 添加新的 MCP 工具

1. 在 `server.rs` 中定义参数结构体：

```rust
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct MyToolParams {
    pub param1: String,
    #[serde(default)]
    pub connection_name: Option<String>,
}
```

2. 在 `GaussdbMcp` 中添加工具方法：

```rust
#[tool(description = "我的工具的描述")]
async fn my_tool(
    &self,
    Parameters(params): Parameters<MyToolParams>,
) -> Result<CallToolResult, McpError> {
    let client = self.get_client_for(params.connection_name.as_deref()).await?;
    // ...实现工具逻辑
    Ok(CallToolResult::success(vec![Content::text(result.to_string())]))
}
```

3. 在 `queries.rs` 中添加任何需要的 SQL 模板。

### 添加新的 CLI 选项

1. 在 `main.rs` 的 `Commands::Cli` 枚举中添加 CLI 参数
2. 更新 `cli.rs` 中的 `cli::CliArgs`
3. 在 `cli::run_cli()` 中实现处理逻辑

### 连接状态管理

`GaussdbMcp` 服务器管理连接状态：

```
ConnectionState:
  Pending(resolver)       → 等待密钥链读取
  Connecting(url)         → 准备连接，尚未连接
  Connected(Arc<Client>)  → 活跃连接
  Unavailable(url)        → 连接失败
```

连接在首次使用时延迟建立。`try_connect()` 方法会在启动时主动探测默认连接。

### 密码解析管道

```
配置文件 → PasswordSource 检测
  ├─ Plaintext       → 立即解析，注册自动迁移回调
  ├─ "keyring"       → 通过操作系统密钥链延迟解析
  ├─ EnvVar          → URL 中已包含，无需迁移
  └─ None            → 延迟密钥链读取（用于无密码的 "trust" 认证）
```

---

## 认证流程

### 支持的认证方法

协议 crate 支持以下认证方法：

```rust
pub enum AuthenticationMessage {
    Ok,
    CleartextPassword,
    Md5Password { salt: [u8; 4] },
    ScmCredential,
    Gss,
    Sspi,
    GssContinue { data: Bytes },
    Sasl { mechanisms: Vec<String> },
    Sha256 { salt: [u8; 4], iteration_count: i32, token: Bytes, server_signature: Bytes },
    Md5Sha256 { salt: [u8; 4], iteration_count: i32, token: Bytes, server_signature: Bytes },
    Sm3 { salt: [u8; 4], iteration_count: i32, token: Bytes, server_signature: Bytes },
}
```

### SCRAM-SHA-256 流程

```
客户端                          服务器
  |                               |
  |<-- SASL(SCRAM-SHA-256) ------|
  |-- SASLInitialResponse ------>|
  |   （client-first-message）     |
  |<-- SASLResponse ------------|
  |   （server-first-message）     |
  |-- SASLResponse ------------->|
  |   （client-final-message）     |
  |<-- SASLResponse ------------|
  |   （server-final-message）     |
  |<-- AuthenticationOk ---------|
```

### openGauss SHA256 流程

```
客户端                          服务器
  |                               |
  |<-- SHA256(salt, iter, token)->|
  |-- PasswordMessage ---------->|
  |   （SHA256 哈希密码）           |
  |<-- AuthenticationOk ---------|
```

---

## 功能特性标志

### tokio-opengauss

| 功能标志 | 默认 | 描述 |
|---------|---------|-------------|
| `runtime` | 是 | 启用 tokio 运行时集成 |
| `js` | 否 | WASM/JavaScript 目标 |
| `with-chrono-0_4` | 否 | chrono DateTime/NaiveDateTime 支持 |
| `with-uuid-1` | 否 | uuid::Uuid 支持 |
| `with-serde_json-1` | 否 | serde_json JSON/JSONB 支持 |
| `with-time-0_3` | 否 | time crate 支持 |
| `with-geo-types-0_7` | 否 | PostGIS 几何类型支持 |
| `with-eui48-1` | 否 | MACADDR 支持 |
| `with-smol_str-01` | 否 | SmolStr 优化 |

### opengauss（同步）

所有功能标志转发至 `tokio-opengauss`：

```toml
[dependencies]
opengauss = { version = "0.19.13", features = ["with-chrono-0_4", "with-uuid-1"] }
```

---

## 安全注意事项

### 密码处理

- 绝不在日志中记录包含密码的连接 URL。使用 MCP 工具中的 `redact_url()`。
- 在 MCP 模式下，密码存储在操作系统密钥链中，而非配置文件。
- `"keyring"` 哨兵值防止意外的明文暴露。

### 查询安全

- MCP 服务器将 `execute_query` 限制为仅 `SELECT` 和 `EXPLAIN`（只读）。
- CLI 模式允许所有语句，但需要用户显式调用。
- 始终使用参数化查询（`$1`、`$2`）——绝不使用字符串拼接。

### TLS

- 生产环境连接应使用 `sslmode=verify-full`。
- `danger_accept_invalid_certs()` 选项仅用于开发/测试。
- 在 `--check-connection` 诊断模式下默认跳过证书验证。

### SQL 注入防护

```rust
// ✅ 正确：参数化查询
client.query("SELECT * FROM users WHERE name = $1", &[&user_input]).await?;

// ❌ 错误：字符串拼接
let sql = format!("SELECT * FROM users WHERE name = '{}'", user_input);
client.query(&sql, &[]).await?;
```
