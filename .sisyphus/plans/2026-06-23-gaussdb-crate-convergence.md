# 设计文档:gaussdb crate 收敛为唯一对外入口

- **日期**:2026-06-23
- **状态**:已设计 + Oracle 评审通过(SHIP,无 showstopper)
- **目标**:新建 `gaussdb` 库 crate 作为唯一对外入口;其余所有 crate(`opengauss`/`tokio-opengauss`/`opengauss-protocol`/`opengauss-types`/`opengauss-derive`/`opengauss-native-tls`/`opengauss-openssl`)设为 `publish = false` 内部私有;`gaussdb-mcp` 重构为仅依赖 `gaussdb`。

---

## 1. 背景与现状

### 1.1 当前 workspace(10 个成员,无 `gaussdb` 库)

```
opengauss-protocol (leaf, 线协议)   自述"不应直接使用"
        ← opengauss-types (FromSql/ToSql/Type)   自述"通常无需直接依赖"
                ← tokio-opengauss (异步核心,真正实现)
                        ← opengauss (同步门面:包裹 tokio-opengauss,阻塞等待 future)
                ← opengauss-derive (proc-macro,自述"内部 crate")
tokio-opengauss ← opengauss-native-tls / opengauss-openssl (TLS 连接器)

gaussdb-mcp(唯一外部消费者,异步 MCP server/CLI)→ 直连 tokio-opengauss + opengauss-native-tls,
                                                    绕过 opengauss 门面(因 opengauss 同步、gaussdb-mcp 异步)

codegen(构建工具,无依赖)、opengauss-derive-test(测试) — 内部
```

### 1.2 关键事实

- `opengauss`(同步)与 `tokio-opengauss`(异步)都导出 `Client`/`Row`/`Config`/`Error`/`NoTls`,**它们是不同、不兼容的类型**(同步包裹异步)。
- `opengauss_types::{FromSql, ToSql, Type}` 是**共享**的——同步与异步路径用的是同一套类型(tokio-opengauss 经 `types` 模块 re-export)。
- 没有任何 crate 设置 `publish = false`,7 个库 crate 全部可独立发布。
- `gaussdb` 当前**只是** `gaussdb-mcp` 工具的二进制名,**不是**库 crate。

### 1.3 核心架构张力

**同步/异步二元性**——最大障碍:
- `gaussdb-mcp` 是异步工具,无法走同步的 `opengauss` 门面,这是它现在直连 `tokio-opengauss` 的根本原因。
- 若新建 `gaussdb` 要同时服务 sync 和 async 用户,**不能平铺 re-export**(重名冲突),必须用**模块隔离**。

---

## 2. 设计决策

| 问题 | 决策 | 理由 |
|---|---|---|
| **异步模块命名** | 异步表面在 crate 根(`gaussdb::Client` = 异步),同步在 `gaussdb::sync::Client` | 异步是核心(tokio-opengauss 是实现基础),gaussdb-mcp 异步,现代 Rust 异步优先。根 = 主路径 |
| **sync 默认开启?** | 否,`default = ["runtime"]`,`sync` 为 opt-in feature | 异步优先;sync wrapper 引入 tokio Runtime 依赖,纯异步用户不应承担 |
| **版本号** | `0.1.0` | 新 crate,诚实的新 semver 起点 |
| **向后兼容** | 无(`publish = false` 硬切,用户已确认) | — |

---

## 3. Oracle 评审结论

**SHIP** — 无 showstopper,核心设计成立。sync/async 合一是合法的架构选择(非错误),真实风险有 3 个,都易修。

### 3.1 逐条裁决

| 审查点 | 裁决 | 关键发现 |
|---|---|---|
| Q1 sync/async 合一 | ✅ 可接受(有摩擦) | `.await`/非 `.await` 误用是真实陷阱,需文档说明 |
| Q2 glob-import 冲突 | ⚠️ Flaw(需文档) | `use gaussdb::*; use gaussdb::sync::*;` **静默遮蔽** 6 个同名类型 |
| Q3 类型 coherence | ✅ Sound | 无 orphan 问题;`FromSql`/`ToSql` **不要**提到根,留在 `types` 模块 |
| Q4 feature 矩阵 | ⚠️ Sound + 3 gap | feature unification 工作正常,但缺 `derive`;`sync` 隐式强制 `runtime` |
| Q5 WASM | ✅ Sound(当前 CI) | CI 只查 tokio-opengauss,gaussdb 不受影响;需保证 `opengauss` 声明为 `optional` |
| Q6 connect 命名 | ✅ 可接受 | 建议补 `gaussdb::sync::connect` free fn |
| Q7 文档链接 | ✅ Sound | intra-doc 用 `#method.x` 相对引用,re-export 不破坏 |
| Q8 gaussdb-mcp 完整性 | ✅ Sound | 全部 9 个 import 已覆盖,无遗漏 |
| Q9 命名冲突 | ✅ Sound | `gaussdb`(lib) 与 `gaussdb-mcp`(bin=`gaussdb`) 不冲突 |
| Q10 其他 | ⚠️ 缺 derive/integration feature/docs.rs metadata/测试策略 | — |

### 3.2 Must-Fix(已纳入设计)

| # | 问题 | 修订 |
|---|---|---|
| **MF1** | 缺 `derive` feature | tokio-opengauss 加 `derive = ["opengauss-types/derive"]`,gaussdb 透传 |
| **MF2** | glob-import 静默遮蔽 | crate 根 + sync 模块加显著文档警告 |
| **MF3** | Cargo.toml metadata 缺失 | 补全 description/repository/readme/keywords/categories/rust-version/docs.rs |
| **MF4** | 无测试策略 | 加 `tests/smoke.rs`(编译验证 + Send/Sync 断言 + integration-gated 真连)|

### 3.3 Recommended(已纳入)

- **R1** 补 `gaussdb::sync::connect` free fn(对称性)
- **R2** 文档说明 SemVer 与 tokio-opengauss 耦合
- **R3** CI wasm 注记(`sync` 不能进 wasm)

---

## 4. 最终规格

### 4.1 `crates/gaussdb/Cargo.toml`

```toml
[package]
name = "gaussdb"
version = "0.1.0"
edition = "2024"
license = "MIT OR Apache-2.0"
description = "A unified asynchronous (and optional synchronous) openGauss/PostgreSQL client"
repository = "https://github.com/c2j/rust-opengauss"
readme = "../../README.md"
keywords = ["database", "opengauss", "sql", "async", "postgres"]
categories = ["database"]
rust-version = "1.85"

[package.metadata.docs.rs]
all-features = true

[features]
default = ["runtime"]
runtime = ["tokio-opengauss/runtime"]

# 同步门面(opt-in)。注意:启用 sync 会隐式启用 tokio-opengauss 的 runtime,
# 因为同步 Client 内部持有 tokio Runtime 并阻塞调用 async Client。
sync = ["dep:opengauss"]

# TLS 连接器(命名空间隔离避免 native-tls/openssl 撞名)
tls-native-tls = ["dep:opengauss-native-tls"]
tls-openssl    = ["dep:opengauss-openssl"]

# derive 宏透传(MF1):让用户能用 #[derive(ToSql, FromSql)]
derive = ["tokio-opengauss/derive"]

# 类型扩展透传(全部 ~19 个,与 tokio-opengauss 对齐)
array-impls        = ["tokio-opengauss/array-impls"]
with-bit-vec-0_6   = ["tokio-opengauss/with-bit-vec-0_6"]
with-bit-vec-0_7   = ["tokio-opengauss/with-bit-vec-0_7"]
with-bit-vec-0_8   = ["tokio-opengauss/with-bit-vec-0_8"]
with-bit-vec-0_9   = ["tokio-opengauss/with-bit-vec-0_9"]
with-chrono-0_4    = ["tokio-opengauss/with-chrono-0_4"]
with-rust_decimal-1 = ["tokio-opengauss/with-rust_decimal-1"]
with-cidr-0_2      = ["tokio-opengauss/with-cidr-0_2"]
with-cidr-0_3      = ["tokio-opengauss/with-cidr-0_3"]
with-eui48-0_4     = ["tokio-opengauss/with-eui48-0_4"]
with-eui48-1       = ["tokio-opengauss/with-eui48-1"]
with-geo-types-0_6 = ["tokio-opengauss/with-geo-types-0_6"]
with-geo-types-0_7 = ["tokio-opengauss/with-geo-types-0_7"]
with-jiff-0_1      = ["tokio-opengauss/with-jiff-0_1"]
with-jiff-0_2      = ["tokio-opengauss/with-jiff-0_2"]
with-serde_json-1  = ["tokio-opengauss/with-serde_json-1"]
with-smol_str-01   = ["tokio-opengauss/with-smol_str-01"]
with-uuid-0_8      = ["tokio-opengauss/with-uuid-0_8"]
with-uuid-1        = ["tokio-opengauss/with-uuid-1"]
with-time-0_2      = ["tokio-opengauss/with-time-0_2"]
with-time-0_3      = ["tokio-opengauss/with-time-0_3"]

# 集成测试闸门(MF4):需要 docker compose 起的 DB
integration = ["tokio-opengauss/integration", "dep:opengauss"]

[dependencies]
tokio-opengauss = { version = "0.7.17", path = "../tokio-opengauss" }
fallible-iterator = "0.2"
# 可选,全部 dep: 形式(MF1/WASM 安全)
opengauss           = { version = "0.19.13", path = "../opengauss",           optional = true }
opengauss-native-tls = { version = "0.5.3",  path = "../opengauss-native-tls", optional = true }
opengauss-openssl    = { version = "0.5.3",  path = "../opengauss-openssl",    optional = true }
```

### 4.2 `crates/gaussdb/src/lib.rs`

```rust
//! 统一的 openGauss/PostgreSQL 客户端入口。
//!
//! # 异步与同步
//!
//! **`gaussdb::Client` 是异步的**(默认,在 crate 根)。方法返回 Future,需 `.await`。
//!
//! 若需同步 API,启用 `sync` feature 并使用 `gaussdb::sync::Client`。
//!
//! # ⚠️ 重要:不要同时 glob-import 根与 sync 模块
//!
//! `use gaussdb::*; use gaussdb::sync::*;` 会**静默遮蔽**根的异步类型
//! (`Client`/`Row`/`Error`/`Config`/`NoTls` 等),导致你拿到同步类型却在写 async 代码。
//! 请显式 import 或只用其一。
//!
//! # SemVer 耦合
//!
//! `gaussdb` 0.x.y 重新导出 `tokio-opengauss`。tokio-opengauss 或 opengauss-types
//! 的破坏性变更 ⇒ gaussdb 破坏性 bump。

pub use fallible_iterator;

// === 异步表面(主,crate 根)===
pub use tokio_opengauss::{
    AsyncMessage, CancelToken, Client, Column, Config, Connection, CopyInSink,
    CopyOutStream, Error, GenericClient, IsolationLevel, NoTls, Notification,
    Portal, Row, RowStream, SimpleColumn, SimpleQueryMessage, SimpleQueryRow,
    SimpleQueryStream, Socket, Statement, ToStatement, Transaction, TransactionBuilder,
    binary_copy, config, error, row, tls, types,
};
#[cfg(feature = "runtime")]
pub use tokio_opengauss::connect;

// === 同步表面(opt-in)===
#[cfg(feature = "sync")]
pub mod sync {
    //! 同步客户端。这些类型与 crate 根的异步类型**同名但不同类型**。
    //! 不要 `use gaussdb::sync::*` 同时又 `use gaussdb::*`。

    pub use opengauss::{
        CancelToken, Client, Config, CopyInWriter, CopyOutReader, Error, GenericClient,
        NoTls, Notifications, Row, RowIter, SimpleQueryRow, Transaction, TransactionBuilder,
        binary_copy, config, notifications,
    };

    /// 同步连接便捷函数(与根 `gaussdb::connect` 对称)。
    pub fn connect<T>(params: &str, tls: T) -> Result<Client, Error>
    where
        T: tokio_opengauss::tls::MakeTlsConnect<tokio_opengauss::Socket> + 'static + Send,
        T::TlsConnect: Send,
        T::Stream: Send,
        <T::TlsConnect as tokio_opengauss::tls::TlsConnect<tokio_opengauss::Socket>>::Future: Send,
    {
        Client::connect(params, tls)
    }
}

// === TLS(命名空间隔离)===
#[cfg(feature = "tls-native-tls")]
pub use opengauss_native_tls as native_tls;
#[cfg(feature = "tls-openssl")]
pub use opengauss_openssl as openssl;

// === 协议(最小暴露)===
pub use opengauss_protocol::Oid;
```

### 4.3 `crates/tokio-opengauss/Cargo.toml` 增量(MF1)

`[features]` 段新增一行:

```toml
derive = ["opengauss-types/derive"]
```

### 4.4 内部 crate 全部 `publish = false`

以下 crate 的 `[package]` 段各加一行 `publish = false`:

- `crates/opengauss`
- `crates/tokio-opengauss`
- `crates/opengauss-protocol`
- `crates/opengauss-types`
- `crates/opengauss-derive`
- `crates/opengauss-native-tls`
- `crates/opengauss-openssl`
- `tools/codegen`(本就内部)
- `tests/opengauss-derive-test`(本就内部)

### 4.5 根 `Cargo.toml` workspace members

```toml
members = [
    "crates/gaussdb",          # ← 新增
    "crates/opengauss",
    "crates/opengauss-derive",
    "crates/opengauss-native-tls",
    "crates/opengauss-openssl",
    "crates/opengauss-protocol",
    "crates/opengauss-types",
    "crates/tokio-opengauss",
    "tests/opengauss-derive-test",
    "tools/codegen",
    "tools/gaussdb-mcp",
]
```

### 4.6 `tools/gaussdb-mcp` 迁移

**Cargo.toml**(替换两行直连依赖为一行):

```toml
# 删除:
# tokio-opengauss = { path = "../../crates/tokio-opengauss", features = [...] }
# opengauss-native-tls = { path = "../../crates/opengauss-native-tls" }

# 改为:
gaussdb = { path = "../../crates/gaussdb", features = [
    "tls-native-tls",
    "with-rust_decimal-1", "with-serde_json-1", "with-uuid-1", "with-chrono-0_4",
] }
```

**源码 import 替换**(机械替换,涉及 cli.rs / output.rs / server.rs / connection.rs / main.rs):

| 旧 | 新 |
|---|---|
| `use tokio_opengauss::{Client, connect, NoTls, Error, Row}` | `use gaussdb::{Client, connect, NoTls, Error, Row}` |
| `use tokio_opengauss::types::{ToSql, FromSql, Type}` | `use gaussdb::types::{ToSql, FromSql, Type}` |
| `use tokio_opengauss::error::SqlState` | `use gaussdb::error::SqlState` |
| `use opengauss_native_tls::MakeTlsConnector` | `use gaussdb::native_tls::MakeTlsConnector` |

`native_tls::TlsConnector`(直接来自 `native-tls` crate)不变。

### 4.7 `crates/gaussdb/tests/smoke.rs`(MF4)

```rust
// 编译验证 + Send/Sync 断言(始终运行)
fn _assert_send<T: Send>() {}
fn _assert_sync<T: Sync>() {}

#[test]
fn reexports_compile() {
    _assert_send::<gaussdb::Client>();
    _assert_sync::<gaussdb::Row>();
    let _: fn(&str, gaussdb::NoTls) -> _ = gaussdb::connect;
}

#[cfg(feature = "integration")]
#[tokio::test]
async fn smoke_connect() {
    let url = std::env::var("GAUSSDB_TEST_URL")
        .unwrap_or("host=127.0.0.1 user=gaussdb dbname=postgres".into());
    let (client, conn) = gaussdb::connect(&url, gaussdb::NoTls).await.unwrap();
    tokio::spawn(async move { let _ = conn.await; });
    let row = client.query_one("SELECT 1", &[]).await.unwrap();
    let _: i32 = row.get(0);
}
```

> 不复制 tokio-opengauss 的庞大集成测试套件——真实测试留在原 crate。

### 4.8 CI wasm 注记(R3)

`.github/workflows/ci.yml` 的 check-wasm32 保留现状(只查 tokio-opengauss)。如要加 gaussdb:

```yaml
cargo check --target wasm32-unknown-unknown -p gaussdb --no-default-features
```

`sync` feature 不能进 wasm(opengauss 用 `tokio::runtime::Runtime`/`std::net`)。

---

## 5. 不改动的部分

- 所有 `crates/*` 内部跨 crate import(tokio→protocol/types、opengauss→tokio、TLS→tokio)是正确的内部分层,**保持原样**。
- `opengauss-derive-test` 直连 `opengauss_types` 是有意为之(测试 derive 宏),保持。
- `codegen` 无 workspace 依赖,不受影响。

---

## 6. 工作量估算

| 改动 | 文件数 | 工作量 |
|---|---|---|
| 新建 `crates/gaussdb/{Cargo.toml,src/lib.rs,tests/smoke.rs}` | 3 新建 | 中 |
| tokio-opengauss 加 `derive` feature 透传 | 1 | 极小 |
| 6 内部 crate 加 `publish = false` | 6 | 极小 |
| 根 workspace members 加 `crates/gaussdb` | 1 | 极小 |
| gaussdb-mcp Cargo.toml + 5 源文件 import 替换 | 6 | 中 |
| (可选)CI wasm 加 gaussdb check | 1 | 极小 |

**Oracle 估约半天。**

---

## 7. 验收标准

- [ ] `cargo build -p gaussdb --all-features` 成功
- [ ] `cargo build -p gaussdb-mcp` 成功(gaussdb-mcp 仅依赖 gaussdb)
- [ ] `cargo test -p gaussdb`(smoke 编译测试)通过
- [ ] `cargo fmt --all -- --check` 通过
- [ ] `RUSTFLAGS="-Dwarnings" cargo clippy --all --all-targets` 通过
- [ ] `cargo check --target wasm32-unknown-unknown -p gaussdb --no-default-features` 通过(若加 CI)
- [ ] gaussdb-mcp 中无任何 `tokio_opengauss::` 或 `opengauss_native_tls::` 直接 import
- [ ] 7 个内部 crate 的 Cargo.toml 均含 `publish = false`
- [ ] `gaussdb::derive` feature 启用时,`#[derive(gaussdb::types::ToSql)]` 可用

---

## 8. 风险与缓解

| 风险 | 缓解 |
|---|---|
| 用户误用 `.await`(sync)或漏 `.await`(async) | crate 根 + sync 模块文档显著警示(MF2) |
| `use gaussdb::*` + `use gaussdb::sync::*` 静默遮蔽 | 同上文档警示;`sync` 默认关闭降低概率 |
| WASM 在 `sync` 开启时失败 | `opengauss` 声明为 optional dep;CI wasm 不查 `sync` |
| derive 链路断裂 | MF1:tokio-opengauss 加 `derive` 透传 |
| SemVer 混淆(gaussdb 0.1 vs tokio-opengauss 0.7) | lib.rs 文档说明耦合规则(R2) |
