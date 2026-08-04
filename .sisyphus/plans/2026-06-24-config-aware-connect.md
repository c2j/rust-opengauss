# 设计文档:gaussdb config-aware connect API

- **日期**:2026-06-24
- **状态**:已设计 + Oracle 评审通过(4 项裁决),待 Momus 评审
- **目标**:在 `gaussdb` facade crate 中提供 `config::resolve()` 与 `config::connect()` 高层 API,消除 `gaussdb-mcp`(权威源,909 行 config.rs)与 `ogexplain-analyzer`(消费侧,775 行 db_config.rs 镜像)之间的配置解析重复,根除 schema drift(PR #25 已发生一次)。
- **关联**:issue #30、PR #28(facade crate 收敛)、ogexplain PR #24(已迁移到 facade,差 config 层)、ogexplain PR #25(schema drift fix,本方案的直接动因)。

---

## 1. 背景与现状

### 1.1 已发生的 drift bug(最强动因)

`gaussdb-mcp` 的 `MultiConfig.connections` 从 `Vec<NamedConnection>` 改为 `BTreeMap<String, NamedConnection>`(`[[connections]]` → `[connections.<name>]`),`ogexplain-analyzer` 的镜像没跟上,导致**静默反序列化失败**。ogexplain PR #25 正在修。这不是假想风险,是已欠的债。

### 1.2 重复规模(实测)

| 位置 | 文件 | 行数 | 角色 |
|---|---|---|---|
| `tools/gaussdb-mcp/src/config.rs` | 权威源 | 909 行 | 配置解析 + keyring + 懒连接 |
| `ogexplain-analyzer/.../db_config.rs` | 镜像 | 775 行 | 配置解析 + keyring(无懒连接) |
| `tools/gaussdb-mcp/src/duration_parse.rs` | 权威源 | 149 行 | 纯 std Duration 解析,跟着搬 |

ogexplain 已在 PR #24 迁移到 `gaussdb` facade crate(用 `features = ["sync"]`),只差 config 这一层。

### 1.3 已有基础设施(为什么现在做)

- PR #28 已建立 `gaussdb` facade crate 作为唯一对外入口,其余 crate `publish = false`。
- facade 当前 `lib.rs:29` 在根 re-export `tokio_opengauss::config`(低层 Config builder)——**占用 `gaussdb::config` 命名空间**,与本方案的高层 config 模块冲突(见 §2 Q1)。
- `gaussdb-mcp/config.rs` 全部 `pub(crate)`,**零公共 API**——抽取是纯代码搬家,非 API-preserving refactor,风险低。
- config.rs **不 import `gaussdb`**;真正的 `gaussdb::connect` 在 `connection.rs`。解析层与连接层已天然分离。

---

## 2. 设计决策(Oracle 4 项裁决)

| # | 问题 | 裁决 | 关键理由 |
|---|---|---|---|
| Q1 | `gaussdb::config` 命名冲突 | **(b) 低层移入 `gaussdb::driver::*`,高层占用 `gaussdb::config`** | facade 0.1.0,唯一外部消费者 ogexplain,semver 余地大;`driver` 命名空间清晰隔离低层实现细节 |
| Q2 | feature 矩阵 | **单一 `config` feature;复用现有 `tls-native-tls`;`sync` 保持显式;keyring 在 config 内常开** | keyring 是 password 解析核心;WASM 跳过 `config`(keyring FFI 不能进 wasm32) |
| Q3 | TLS 矩阵(sync/async 共享) | **(b) `SslMode` enum + 纯解析函数,各路径各自 match** | 类型系统显式编码 sslmode 域;纯函数可单测;连接器构造因 sync/async 类型不同而分写 |
| Q4 | API 表面 | **(c) 同时暴露 `resolve()` 和 `connect()`** | ogexplain 用 `connect()`(黑盒);gaussdb-mcp 用 `resolve()`(取 ResolvedConnection)+ 自留懒连接包装 |

### 2.1 Q1 命名冲突详解

当前 `lib.rs` 把 `tokio_opengauss::{..., Config, ..., config, error, row, tls, types, ...}` 全部平铺 re-export 到 crate 根。`config` 模块(低层 Config builder)占用了 `gaussdb::config`。

**方案**:
```rust
// 新增 driver 命名空间,收容低层 re-export
pub mod driver {
    pub use tokio_opengauss::*;
}
// 根保留**选择性**便捷别名(故意不 re-export `config` 模块)
pub use driver::{
    AsyncMessage, CancelToken, Client, Column, Config, Connection, CopyInSink,
    CopyOutStream, Error, GenericClient, IsolationLevel, NoTls, Notification,
    Portal, Row, RowStream, SimpleColumn, SimpleQueryMessage, SimpleQueryRow,
    SimpleQueryStream, Socket, Statement, ToStatement, Transaction, TransactionBuilder,
    binary_copy, error, row, tls, types,
};
#[cfg(feature = "runtime")]
pub use driver::connect;

// 高层 config 模块(新)
#[cfg(feature = "config")]
pub mod config;
```

**影响**:
- `gaussdb::Client`、`gaussdb::Config`、`gaussdb::types::FromSql` 等全部不变(便捷别名)
- 唯一破坏:需要 `gaussdb::config::SslMode`(低层)的用户改走 `gaussdb::driver::config::SslMode`
- gaussdb-mcp 当前不直接用低层 config 模块项,零影响
- ogexplain 同理(它用 `gaussdb::Config` builder,在根)

### 2.2 Q2 feature 矩阵详解

```toml
# 新增 feature
config = ["dep:toml", "dep:dirs", "dep:keyring"]
```

| 启用的 feature | 可用 API |
|---|---|
| `config` | `gaussdb::config::connect_async()` → 异步 Client;`gaussdb::config::resolve()` |
| `config + sync` | 上行 + `gaussdb::config::connect()` → 同步 Client |
| `config + tls-native-tls` | connect 走 TLS 连接器;缺它则 sslmode=require 等触发运行时清晰报错 |
| `config + sync + tls-native-tls` | 双向完整 TLS 选择 |

**WASM**:`config` feature 不进 wasm32(keyring 平台 FFI)。CI 的 `cargo check --target wasm32-unknown-unknown` 只测默认 feature + TLS 变体,不含 `config`。这与现有 `sync` 不进 wasm 的处理一致。

### 2.3 Q3 TLS 矩阵详解

```rust
// gaussdb::config 模块内,纯函数可单测
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SslMode {
    Disable,
    Prefer,     // 当前等价 Require(无 fallback)
    Require,    // TLS,跳过证书校验
    VerifyCa,   // TLS,校验证书
    VerifyFull, // TLS,校验证书 + 主机名
}

fn sslmode_from_url(dsn: &str) -> SslMode;        // 从 libpq URL 解析
fn sslmode_from_value(val: Option<&str>) -> SslMode; // 从 config 字段值解析
```

`prefer` 的真正 fallback(先试 TLS,被拒再 NoTls)需要连接尝试,不是纯函数;当前映射为与 `require` 同等连接器构造(与 gaussdb-mcp 现有 `needs_tls()` 行为一致),设计文档标记为已知限制。

### 2.4 Q4 API 表面详解

```rust
// gaussdb::config 模块

/// 解析配置:TOML + 环境变量 + 连接选择 + keyring 密码获取。
/// 返回结构化连接数据,DSN 已含解析后的密码。
pub fn resolve(
    dsn: Option<&str>,
    path: Option<&Path>,
    name: Option<&str>,
) -> Result<ResolvedConnection, ConfigError>;

/// 黑盒:resolve → TLS 选择 → 驱动 connect。返回即用 Client。
#[cfg(feature = "runtime")]
pub async fn connect_async(
    dsn: Option<&str>,
    path: Option<&Path>,
    name: Option<&str>,
) -> Result<gaussdb::Client, ConnectError>;

/// 同步变体(在 config + sync 下)。
#[cfg(all(feature = "config", feature = "sync"))]
pub fn connect(
    dsn: Option<&str>,
    path: Option<&Path>,
    name: Option<&str>,
) -> Result<gaussdb::sync::Client, ConnectError>;

pub struct ResolvedConnection {
    pub name: String,
    pub connection_url: String,  // 含已解析密码
    pub sslmode: SslMode,
    pub config_path: Option<PathBuf>,
    pub timeout_config: TimeoutConfig,
    pub password_source: PasswordSource,
    pub keyring_username: String,
}
```

**消费方式**:
- **ogexplain(简单消费)**:`connect()` 一行
- **gaussdb-mcp 急切连接(明文密码)**:`resolve()` 取 DSN + 元数据,存,后续用低层 `gaussdb::connect(dsn, tls)` 连接
- **gaussdb-mcp 懒连接(keyring 密码)**:MCP **自留** `build_lazy_resolver()`(tools/gaussdb-mcp/src/config.rs:441-516,跳过 keyring 获取)。激活时调用低层 `gaussdb::connect()`

---

## 3. 最终规格

### 3.1 `crates/gaussdb/Cargo.toml`(增量)

```toml
[features]
# ...现有 feature 不变...

# 新增:高层配置解析
config = ["dep:toml", "dep:dirs", "dep:keyring"]

[dependencies]
# ...现有依赖不变...
# 新增可选依赖
toml     = { version = "0.8", optional = true }
dirs     = { version = "6",   optional = true }
keyring  = { version = "3", features = ["apple-native", "windows-native", "sync-secret-service", "crypto-rust"], optional = true }
# native-tls 已在 tls-native-tls feature 下,connect 的 TLS 路径复用

# dev-dependencies 增加(若需 tempdir 测配置文件读写)
```

### 3.2 `crates/gaussdb/src/lib.rs`(修改)

关键变化:
1. 新增 `pub mod driver { pub use tokio_opengauss::*; }`
2. 根 re-export 从 `pub use tokio_opengauss::{...}` 改为 `pub use driver::{...}`,**故意不包含 `config` 模块**
3. 同步模块同理:`pub mod sync::driver { pub use opengauss::*; }`,`pub use sync::driver::{...}` 不含 `config`
4. 新增 `#[cfg(feature = "config")] pub mod config;`

### 3.3 `crates/gaussdb/src/config.rs`(新文件,从 gaussdb-mcp 搬 + 新写)

**从 gaussdb-mcp/config.rs 搬入(改 `pub(crate)` → `pub`)**:

| 源行 | 项 | 角色 |
|---|---|---|
| 14-110 | `TimeoutAction`、`TimeoutConfig`、`DEFAULT_STATEMENT_TIMEOUT_SECS` | 纯数据 |
| 115-253 | `PasswordSource`、`NamedConnection`、`MultiConfig`、`ResolvedConnection` | 配置 schema |
| 162-217 | `keyring_username()`、`to_connection_url()`、`timeout_config()`、`MultiConfig::resolve()` | 解析逻辑 |
| 276-306 | `read_keyring_password()`、`store_keyring_password()` | keyring(参数化 service 名) |
| 308-355 | `rewrite_password_to_sentinel*` | TOML 回写 |
| 357-385 | `default_config_path()`、`find_config_path()` | 路径解析(参数化文件名) |
| 387-441 | `resolve_single_connection()` | 急切解析 |
| 518-604 | `RawConfig`、`read_config()`、`resolve_env_var_connection()` | TOML + env 读取 |

**从 gaussdb-mcp/duration_parse.rs 搬入(149 行,纯 std,14 测)**:`parse_duration()` 全模块。

**留在 gaussdb-mcp(不搬)**:
- `LazyConnectionEntry`(`Arc<dyn Fn() -> Result<String>>`,MCP 生命周期)
- `build_lazy_resolver()`(构造懒闭包)
- `resolve_all_connections_lazy()`(MCP 入口)
- `RawConfig.is_env_var` 字段(MCP 生命周期标志)
- `store_keyring_password` 的 CLI 调用入口(MCP 特有)

**新写( gaussdb-mcp 没有,ogexplain db.rs 有参考)**:
- `SslMode` enum + `sslmode_from_url()` + `sslmode_from_value()` 纯函数
- `ConfigError` / `ConnectError` 错误类型(见 §3.4)
- `resolve()` 公共 API(= `read_config` + `MultiConfig::resolve` + `resolve_single_connection`)
- `connect_async()`(= `resolve` + `SslMode` 选择 + 低层 `gaussdb::connect`)
- `connect()` 同步版(= `resolve` + `SslMode` 选择 + `gaussdb::sync::Client::connect`)

### 3.4 错误类型

```rust
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("config file not found at {searched_path}")]
    ConfigNotFound { searched_path: PathBuf },
    #[error("failed to parse config at {path}")]
    ConfigParse { path: PathBuf, #[source] source: toml::de::Error },
    #[error("connection '{name}' not found; available: {available:?}")]
    ConnectionNotFound { name: String, available: Vec<String> },
    #[error("keyring error for user '{username}'")]
    Keyring { username: String, #[source] source: keyring::Error },
}

#[derive(Debug, thiserror::Error)]
pub enum ConnectError {
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[cfg(feature = "tls-native-tls")]
    #[error("TLS initialization failed")]
    Tls(#[from] native_tls::Error),
    #[error("sslmode '{sslmode}' requires 'tls-native-tls' feature, which is not enabled")]
    TlsFeatureMissing { sslmode: SslMode },
    #[error(transparent)]
    Driver(#[from] gaussdb::Error),
}
```

### 3.5 测试策略

**从 gaussdb-mcp 搬入的 25 个单测**(11 个 config + 14 个 duration_parse),全部纯解析逻辑,跟着搬。
**新增单测**:
- `SslMode` 解析:libpq URL 各位置、config 字段值、边界值
- `resolve()`:env var 优先级、multi-config 选择、keyring sentinel
- `connect()` / `connect_async()`:WASM 编译验证 + integration-gated 真连(复用 docker compose)

---

## 4. 迁移路径(PR 拆分)

| PR | 标题 | scope | 风险 |
|---|---|---|---|
| **P1** | `refactor(gaussdb): rehome low-level re-exports under driver module` | 纯 re-export 路径调整,`driver` 命名空间建立;根便捷别名保留;**不含 config 模块** | 低(facade 0.1.0,ogexplain 改 import) |
| **P2** | `feat(gaussdb): add config-aware connect API` | 新增 `config` feature + `config.rs` + `duration_parse.rs`(从 mcp 搬,改 pub)+ `SslMode` + `resolve()` + `connect()`/`connect_async()` + 25 单测搬入 + 新单测 | 中(新代码 + 搬运) |
| **P3** | `refactor(gaussdb-mcp): consume gaussdb::config, remove duplication` | gaussdb-mcp 改为消费 `gaussdb::config::resolve()`;删除 config.rs 中已搬出的部分;**保留** LazyConnectionEntry/build_lazy_resolver/resolve_all_connections_lazy | 中(行为不变验证) |
| **P4**(ogexplain 仓库) | `refactor(cli): consume gaussdb::config::connect, delete db_config.rs` | 删除 db_config.rs(775 行)+ db.rs TLS 逻辑;改 `gaussdb = { features = ["sync", "config", "tls-native-tls"] }`;删 toml/dirs/keyring/native-tls 依赖 | 低(纯消费切换) |
| **P5** | `docs: update for config-aware connect API` | README/DeveloperGuide/CONTRIBUTION(EN+ZH)补 `config` feature 与 `gaussdb::config::connect()` 用法 | 无 |

**依赖序**:P1 → P2 → (P3 ‖ P4) → P5。P3 与 P4 可并行(不同仓库)。

---

## 5. 风险与缓解

| 风险 | 严重度 | 缓解 |
|---|---|---|
| `dirs` crate 在 wasm32 编译失败 | 低 | `config` feature 不进 wasm;CI wasm 检查不含 config(同 sync 处理) |
| keyring 在 headless CI 无法测 | 中 | keyring 单测用 `cfg(not(target_os = ...))` 门控;integration 测用 docker compose 起的真 DB + GAUSSDB_TEST_URL env var 跳过 keyring |
| `connect()` 同步/异步 TLS 连接器类型不同导致重复 | 低 | `SslMode` 纯函数共享,连接器构造各写(~10 行/路径),可接受 |
| gaussdb-mcp 懒连接行为回归 | 中 | P3 保留 `build_lazy_resolver` 原样,只改急切路径用 `resolve()`;`cargo test -p gaussdb-mcp` 70 测全过为门 |
| ogexplain 删 775 行后行为回归 | 低 | ogexplain 有自己的测试套件;P4 在 ogexplain 仓库独立 CI 验证 |

---

## 6. 验证门(每个 PR 必过)

| 门 | 命令 |
|---|---|
| 格式 | `cargo fmt --all -- --check` |
| Lint | `RUSTFLAGS="-Dwarnings" cargo clippy --all --all-targets` |
| WASM(P1/P2 后) | `cargo check --target wasm32-unknown-unknown`(不含 config) |
| 单测(P2) | `cargo test -p gaussdb`(含搬入的 25 测 + 新 SslMode 测) |
| 单测(P3) | `cargo test -p gaussdb-mcp`(70 测不回归) |
| 集成(P2) | `cargo test -p gaussdb --features integration`(docker compose,DB 真连) |
| grep 验证(P3) | `grep -r 'tokio_opengauss::config' tools/gaussdb-mcp/src` 返回 0 |

---

## 7. 未决问题(留待 Momus 或实现时定)

1. `connect_async` 还是就叫 `connect`(在 `#[cfg(feature = "runtime")]` 下)+ `connect_sync`(在 `#[cfg(feature = "sync")]` 下)?Oracle 提议 async 用 `connect_async`、sync 用 `connect`,与 lib.rs 根 `connect`=async 的现有约定冲突。**建议**:模块内 `connect_async` / `connect_sync` 命名,避免与根 `gaussdb::connect` 撞名。
2. `ResolvedConnection` 是否需要 `timeout_config` 字段?ogexplain 不用 timeout,gaussdb-mcp 用。保留字段,ogexplain 忽略。
3. `thiserror` 是否已是 workspace 依赖?若是,直接用;若否,P2 加入。
