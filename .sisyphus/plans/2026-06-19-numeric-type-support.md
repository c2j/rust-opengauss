# NUMERIC 类型支持 + format_row_value 类型覆盖完整修复

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

## Revision Notes (2026-06-19, post-PR#18)

PR#18 (`perf/streaming-export`) 已合并到 main：
- CSV / Vertical 已通过 `query_raw` 实现 O(1) 内存流式输出
- Table / JSON 仍是 buffered（合理取舍）

**修订范围**：
- ✅ T1-T3 核心修复不变
- ✅ T4 加入 **X2 优化**：`decimal_to_json` 用 `to_f64/to_i64/to_u64` fast path 替代 `to_string + from_str` 双解析
- ✅ T6 加入 **X4 文档化**：README 加 SQL `::text` cast 建议，大数据量导出推荐做法
- 🟡 **X1（CSV/Vertical 改用 simple_query text protocol）暂缓**：留作后续 perf PR。当前路径 CPU 开销对中小数据集可接受，38M+ 场景靠 SQL-side cast 兜底
- ✅ 实施分支：`fix/numeric-type-support`
- ✅ 实施方式：subagent-driven-development（fresh subagent per task + 两阶段评审）

**PR#18 没有触及的问题**（本次修复的核心）：
- ❌ `format_row_value` trial-and-error 模式未变 → NUMERIC 等类型仍被吞成 NULL
- ❌ NUMERIC 二进制解码仍缺失
- ❌ gaussdb-mcp 仍未启用任何 `with-*` feature
- 流式只解决了内存（O(1)），没解决 CPU（NUMERIC 转 String 仍 ~1500ns/cell）

**Goal:** 修复 gaussdb-mcp 查询输出时 NUMERIC 字段被静默丢弃为 NULL 的 bug，同时补齐其它被同样方式吞掉的类型（UUID / JSON / TIMESTAMP 等），并在底层库增加 `rust_decimal::Decimal` 对 NUMERIC 的 FromSql/ToSql 实现。

**Architecture:** 三层修复：
1. **底层库 opengauss-types**：新增 `with-rust_decimal-1` feature，内部 port rust-decimal crate 的 driver.rs 代码（不引入外部 db-tokio-postgres 依赖，与现有 bit-vec/chrono/uuid 范式一致）。
2. **中间层 tokio-opengauss**：透传 `with-rust_decimal-1` feature；添加 dev-dependency 与集成测试。
3. **应用层 gaussdb-mcp**：在 `Cargo.toml` 启用一组 `with-*` feature；将 `format_row_value` 从 trial-and-error 模式重构为基于 `column.type_()` 的 type-aware dispatch，覆盖 NUMERIC + 所有常见 GaussDB 类型，并对未识别类型给出可见的 hex 字节 fallback 而非静默 NULL。

**Tech Stack:** Rust 1.85 / edition 2024；rust_decimal 1.x；现有 opengauss-types 的 `accepts!` / `to_sql_checked!` 宏；现有 tokio-opengauss 测试套件范式（`test_type` helper）。

---

## 背景与根因

### 现象
通过 gaussdb-mcp（CLI 或 MCP server）查询 GaussDB，NUMERIC 字段返回 NULL，但实际有值。

### 根因
1. **opengauss-types 没有 NUMERIC 解码器**：`crates/opengauss-types/src/lib.rs:774` 仅 `simple_from!(f64, float8_from_sql, FLOAT8)`，`f64::accepts()` 对 `Type::NUMERIC` (OID 1700) 返回 false。opengauss-protocol 中无 `numeric_from_sql` 函数。
2. **gaussdb-mcp 的 format_row_value 是 trial-and-error 模式**：`tools/gaussdb-mcp/src/output.rs:3-22` 顺序试 `Option<&str>`、`Option<i32>`、`Option<i64>`、`Option<f64>`、`Option<bool>`、`Option<&[u8]>`，全部失败时返回 `serde_json::Value::Null`，**静默丢值**。
3. **gaussdb-mcp 没启用任何 `with-*` feature**：`tools/gaussdb-mcp/Cargo.toml:23` `tokio-opengauss = { path = "...", }` 无 features，所以即使代码写了 `try_get::<Option<Uuid>>`，因为 feature 关闭也无法编译到。
4. **同样的 bug 影响其它类型**：UUID / JSON / JSONB / TIMESTAMP / TIMESTAMPTZ / DATE / TIME / TIMETZ / INTERVAL / INET / CIDR / MACADDR / NUMERIC[] 等都会落到 Null fallback。

### 上游参考
- rust-postgres 也没有 rust_decimal feature（`postgres-types/Cargo.toml`）。
- 真正实现在 `rust-decimal` crate 的 `src/postgres/{common.rs, driver.rs}`，通过 `db-tokio-postgres` feature 反向实现。
- 我们的策略：把 driver.rs 的二进制解析逻辑 port 到 opengauss-types 内部，符合本仓库现有 optional-type 范式。

### NUMERIC 二进制格式（PostgreSQL/OpenGauss wire v3）

```
Header (8 bytes, big-endian):
  u16 num_groups    // base-10000 数字组数
  i16 weight        // 首组权重 (10000^weight)
  u16 sign          // 0x0000=正, 0x4000=负, 0xC000=NaN, 0xD000=+Inf, 0xF000=-Inf
  u16 dscale        // 小数点后十进制位数
Body (num_groups * 2 bytes):
  i16[num_groups]   // 每组 0..=9999，base-10000
```

例：`3950.123456` → `00 03 00 00 00 00 00 06 0F 6E 04 D2 15 E0`

---

## Task 分解

### Task 1：opengauss-types 新增 `with-rust_decimal-1` feature

**Files:**
- Modify: `crates/opengauss-types/Cargo.toml`
- Create: `crates/opengauss-types/src/rust_decimal_1.rs`
- Modify: `crates/opengauss-types/src/lib.rs` (添加 mod 声明)

**Step 1.1：Cargo.toml 增加依赖与 feature**

在 `crates/opengauss-types/Cargo.toml` 的 `[features]` 节末尾添加：
```toml
with-rust_decimal-1 = ["rust_decimal-1"]
```

在 `[dependencies]` 节末尾添加：
```toml
rust_decimal-1 = { version = "1.41", package = "rust_decimal", default-features = false, features = ["std"], optional = true }
```

> 注意：`default-features = false` 避免引入不必要的 `c-repr`/`serde` 等 feature；`std` 是必须的。

**Step 1.2：创建 `crates/opengauss-types/src/rust_decimal_1.rs`**

Port 自 rust-decimal `src/postgres/{common.rs, driver.rs}`，集成进 opengauss-types 的宏体系。文件结构：

```rust
use bytes::BytesMut;
use opengauss_protocol::types;
use std::error::Error;
use rust_decimal_1::Decimal;

use crate::{FromSql, IsNull, ToSql, Type};

// NUMERIC sign masks（来自 PostgreSQL numeric.c）
const NUMERIC_NAN: u16 = 0xC000;
const NUMERIC_PINF: u16 = 0xD000;
const NUMERIC_NINF: u16 = 0xF000;
const NUMERIC_SPECIAL: u16 = 0xC000;
const NUMERIC_NEG: u16 = 0x4000;

impl<'a> FromSql<'a> for Decimal {
    fn from_sql(_: &Type, raw: &[u8]) -> Result<Decimal, Box<dyn Error + Sync + Send>> {
        // 1. 解析 8 字节 header
        // 2. 处理 NaN / ±Inf 特殊值（返回 Error，Decimal 不支持）
        // 3. 读取所有 base-10000 数字组
        // 4. 调用 checked_from_postgres 完成 base-10000 → Decimal 转换
        // ...
    }

    fn accepts(ty: &Type) -> bool {
        matches!(*ty, Type::NUMERIC)
    }
}

impl ToSql for Decimal {
    fn to_sql(&self, _: &Type, w: &mut BytesMut) -> Result<IsNull, Box<dyn Error + Sync + Send>> {
        // 调用 to_postgres() 将 Decimal → base-10000 数字组
        // 写入 header + 数字组
        // ...
    }

    fn accepts(ty: &Type) -> bool {
        matches!(*ty, Type::NUMERIC)
    }

    to_sql_checked!();
}
```

完整代码从 rust-decimal 的 `src/postgres/driver.rs` 和 `src/postgres/common.rs` port。两个核心 helper（`checked_from_postgres`、`to_postgres`、以及它们依赖的 `mantissa_array4` / `mul_by_u32` / `div_by_u32` / `is_all_zero`）需要一并 port 或用 Decimal 公开 API 重写。

> ⚠️ Port 时必须：
> - 用 `rust_decimal_1::` 作为 crate 别名（package rename），与 Cargo.toml 一致
> - 使用 `crate::accepts!`、`crate::to_sql_checked!` 宏（已在 lib.rs export）
> - 不要引入新的 helper 文件，所有逻辑放 `rust_decimal_1.rs` 内（保持 module 私有）
> - 处理 `NUMERIC_NAN`/`PINF`/`NINF` 时返回 `Error`（rust_decimal::Decimal 不支持）

**Step 1.3：lib.rs 注册 module**

在 `crates/opengauss-types/src/lib.rs:290` 附近（其它 `with-*` mod 声明处）添加：
```rust
#[cfg(feature = "with-rust_decimal-1")]
mod rust_decimal_1;
```

**Step 1.4：验证编译**

```sh
cargo check -p opengauss-types --features with-rust_decimal-1
```

期望：无错误。如有 unused import / 类型不匹配，按 compiler 提示修。

**Step 1.5：Commit**

```bash
git add crates/opengauss-types/Cargo.toml \
        crates/opengauss-types/src/lib.rs \
        crates/opengauss-types/src/rust_decimal_1.rs
git commit -m "feat(opengauss-types): add with-rust_decimal-1 feature for NUMERIC type support

Ports the NUMERIC binary protocol parser from the rust-decimal crate's
db-tokio-postgres driver into opengauss-types, following the same
optional-type pattern used by bit-vec/chrono/uuid/etc.

This enables FromSql/ToSql implementations for rust_decimal::Decimal
accepting Type::NUMERIC (OID 1700)."
```

---

### Task 2：tokio-opengauss 透传 feature + 集成测试

**Files:**
- Modify: `crates/tokio-opengauss/Cargo.toml`
- Modify: `crates/tokio-opengauss/src/lib.rs`（feature table 文档）
- Create: `crates/tokio-opengauss/tests/test/types/rust_decimal_1.rs`
- Modify: `crates/tokio-opengauss/tests/test/types/mod.rs`

**Step 2.1：Cargo.toml 增加转发 feature**

`crates/tokio-opengauss/Cargo.toml` 的 `[features]` 节添加：
```toml
with-rust_decimal-1 = ["opengauss-types/with-rust_decimal-1"]
```

`[dev-dependencies]` 节添加：
```toml
rust_decimal-1 = { version = "1.41", package = "rust_decimal", default-features = false, features = ["std"] }
```

**Step 2.2：lib.rs feature 文档表格更新**

在 `crates/tokio-opengauss/src/lib.rs:124` 之后追加：
```
//! | `with-rust_decimal-1` | Enable support for the `rust_decimal` crate (NUMERIC type). | [rust_decimal](https://crates.io/crates/rust_decimal) 1.0 | no |
```

**Step 2.3：测试文件 `crates/tokio-opengauss/tests/test/types/rust_decimal_1.rs`**

参考现有 `chrono_04.rs` / `uuid_1.rs` 范式：
```rust
use tokio_opengauss::types::FromSqlOwned;
use tokio_opengauss::types::ToSql;
use rust_decimal_1::Decimal;
use std::str::FromStr;

use super::test_type;

#[tokio::test]
async fn test_rust_decimal_params() {
    test_type(
        "NUMERIC",
        &[
            (Some(Decimal::from_str("3950.123456").unwrap()), "3950.123456"),
            (Some(Decimal::from_str("3950").unwrap()), "3950"),
            (Some(Decimal::from_str("0.1").unwrap()), "0.1"),
            (Some(Decimal::from_str("-100").unwrap()), "-100"),
            (Some(Decimal::from_str("119996.25").unwrap()), "119996.25"),
            (Some(Decimal::from_str("1000000").unwrap()), "1000000"),
            (Some(Decimal::from_str("9999999.99999").unwrap()), "9999999.99999"),
            (Some(Decimal::from_str("18446744073709551615").unwrap()), "18446744073709551615"),
            (Some(Decimal::from_str("-18446744073709551615").unwrap()), "-18446744073709551615"),
            (None, "NULL"),
        ],
    )
    .await;
}
```

**Step 2.4：注册测试 module**

`crates/tokio-opengauss/tests/test/types/mod.rs` 在其它 `#[cfg(feature = "with-...")]` mod 声明附近添加：
```rust
#[cfg(feature = "with-rust_decimal-1")]
mod rust_decimal_1;
```

**Step 2.5：运行测试**

```sh
docker compose up -d
cargo test -p tokio-opengauss --features "integration with-rust_decimal-1" --test test -- types::rust_decimal_1
```

期望：全部通过。

**Step 2.6：Commit**

```bash
git add crates/tokio-opengauss/Cargo.toml \
        crates/tokio-opengauss/src/lib.rs \
        crates/tokio-opengauss/tests/test/types/mod.rs \
        crates/tokio-opengauss/tests/test/types/rust_decimal_1.rs
git commit -m "feat(tokio-opengauss): expose with-rust_decimal-1 feature + integration test

Adds passthrough feature for rust_decimal NUMERIC support and a
roundtrip integration test covering 10 representative decimal values."
```

---

### Task 3：gaussdb-mcp 启用 features + 重构 `format_row_value` 为 type-aware dispatch

> **本任务同时完成「方案 B」+「举一反三」**

**Files:**
- Modify: `tools/gaussdb-mcp/Cargo.toml`
- Modify: `tools/gaussdb-mcp/src/output.rs`
- Modify: `tools/gaussdb-mcp/src/cli.rs`（如有引用 format_row_value 的地方需更新签名）
- Modify: `tools/gaussdb-mcp/src/server.rs`（同上）

**Step 3.1：Cargo.toml 启用 features**

修改 `tools/gaussdb-mcp/Cargo.toml:23`：
```toml
tokio-opengauss = { version = "0.7.17", path = "../../crates/tokio-opengauss", features = [
    "with-rust_decimal-1",
    "with-serde_json-1",
    "with-uuid-1",
    "with-chrono-0_4",
] }
```

并在 `[dependencies]` 末尾添加直接依赖（output.rs 中需要用到这些类型）：
```toml
rust_decimal = { version = "1.41", default-features = false, features = ["std"] }
uuid = { version = "1", default-features = false, features = ["std"] }
chrono = { version = "0.4", default-features = false, features = ["clock", "std"] }
```

> 注意：这里用原 crate 名（`rust_decimal`、`uuid`、`chrono`），不要 rename。tokio-opengauss 内部把它们 rename 为 `rust_decimal-1` 是其内部细节；gaussdb-mcp 直接依赖时使用本名。

**Step 3.2：重构 `format_row_value` 为 type-aware dispatch**

`tools/gaussdb-mcp/src/output.rs` 完整重写。新设计：

```rust
use tokio_opengauss::Row;
use tokio_opengauss::types::Type;
use serde_json::{json, Value};

pub(crate) fn format_row_value(row: &Row, idx: usize) -> Value {
    let col_type = row.columns()[idx].type_();
    // 先处理 NULL：尝试 Option 解码任何已知类型时 None 都会自然返回 Null
    // Type-aware dispatch
    unsafe fn __unused() {} // 占位，实际代码无此行
    match *col_type {
        // ───── 字符串族 ─────
        Type::VARCHAR | Type::TEXT | Type::BPCHAR | Type::NAME | Type::UNKNOWN => {
            match row.try_get::<_, Option<String>>(idx) {
                Ok(Some(s)) => Value::String(s),
                _ => Value::Null,
            }
        }
        // ───── 整数族 ─────
        Type::INT2 => match row.try_get::<_, Option<i16>>(idx) {
            Ok(Some(v)) => json!(v),
            _ => Value::Null,
        },
        Type::INT4 | Type::OID | Type::REGPROC | Type::REGTYPE => match row.try_get::<_, Option<i32>>(idx) {
            Ok(Some(v)) => json!(v),
            _ => Value::Null,
        },
        Type::INT8 | Type::REGCLASS => match row.try_get::<_, Option<i64>>(idx) {
            Ok(Some(v)) => json!(v),
            _ => Value::Null,
        },
        // ───── 浮点族 ─────
        Type::FLOAT4 => match row.try_get::<_, Option<f32>>(idx) {
            Ok(Some(v)) => json!(v),
            _ => Value::Null,
        },
        Type::FLOAT8 => match row.try_get::<_, Option<f64>>(idx) {
            Ok(Some(v)) => json!(v),
            _ => Value::Null,
        },
        // ───── NUMERIC（核心修复）─────
        Type::NUMERIC => match row.try_get::<_, Option<rust_decimal::Decimal>>(idx) {
            Ok(Some(d)) => decimal_to_json(d),
            Ok(None) => Value::Null,
            Err(_) => Value::Null,
        },
        // ───── 布尔 ─────
        Type::BOOL => match row.try_get::<_, Option<bool>>(idx) {
            Ok(Some(v)) => json!(v),
            _ => Value::Null,
        },
        // ───── 字节 ─────
        Type::BYTEA => match row.try_get::<_, Option<&[u8]>>(idx) {
            Ok(Some(b)) => Value::String(format!("\\x{}", hex_bytes(b))),
            Ok(None) => Value::Null,
            Err(_) => Value::Null,
        },
        // ───── UUID（feature gated）─────
        #[cfg(feature = "with-uuid-1")]
        Type::UUID => match row.try_get::<_, Option<uuid::Uuid>>(idx) {
            Ok(Some(u)) => Value::String(u.to_string()),
            Ok(None) => Value::Null,
            Err(_) => Value::Null,
        },
        // ───── JSON（feature gated）─────
        #[cfg(feature = "with-serde_json-1")]
        Type::JSON | Type::JSONB => match row.try_get::<_, Option<serde_json::Value>>(idx) {
            Ok(Some(v)) => v,
            Ok(None) => Value::Null,
            Err(_) => Value::Null,
        },
        // ───── 时间族（feature gated）─────
        #[cfg(feature = "with-chrono-0_4")]
        Type::TIMESTAMP | Type::TIMESTAMPTZ => {
            match row.try_get::<_, Option<chrono::NaiveDateTime>>(idx) {
                Ok(Some(v)) => Value::String(v.to_string()),
                Ok(None) => Value::Null,
                Err(_) => Value::Null,
            }
        }
        #[cfg(feature = "with-chrono-0_4")]
        Type::DATE => match row.try_get::<_, Option<chrono::NaiveDate>>(idx) {
            Ok(Some(v)) => Value::String(v.to_string()),
            _ => Value::Null,
        },
        #[cfg(feature = "with-chrono-0_4")]
        Type::TIME | Type::TIMETZ => match row.try_get::<_, Option<chrono::NaiveTime>>(idx) {
            Ok(Some(v)) => Value::String(v.to_string()),
            _ => Value::Null,
        },
        // ───── Fallback：未知类型，尝试以 raw bytes hex 输出 ─────
        _ => match row.try_get::<_, Option<&[u8]>>(idx) {
            Ok(Some(b)) => Value::String(format!("<unsupported type {}>: \\x{}", col_type.name(), hex_bytes(b))),
            Ok(None) => Value::Null,
            Err(_) => Value::Null,
        },
    }
}

/// Decimal → JSON Value：若可无损表示为 f64 则用 Number，否则用 String 保留精度
fn decimal_to_json(d: rust_decimal::Decimal) -> Value {
    use std::str::FromStr;
    let s = d.to_string();
    if let Ok(n) = serde_json::Number::from_str(&s) {
        Value::Number(n)
    } else {
        Value::String(s)
    }
}

pub(crate) fn hex_bytes(bytes: &[u8]) -> String {
    /* 不变 */
}

pub(crate) fn format_table(...) { /* 不变 */ }
fn value_to_string(...) { /* 不变 */ }
```

**关键设计决策**：
1. **基于 `column.type_()` dispatch**：避免 trial-and-error 依赖每个 FromSql::accepts() 正确性。
2. **NUMERIC → 优先 JSON Number，超精度回退 String**：`rust_decimal::Decimal` 可保留任意精度（最大 96 位），`serde_json::Number` 仅支持 f64/i64/u64，所以超过精度必须 String fallback。当前实现先用字符串构造 Number，失败则 String。
3. **未知类型不再静默 NULL**：fallback 为 `<unsupported type NAME>: \xHEXBYTES`，让用户立即看到问题，避免再次出现「数据消失却不知道原因」。
4. **Feature gate**：UUID/JSON/时间类型用 `#[cfg(feature = "...")]` 守卫，未来若用户禁用某 feature 也能优雅编译。

> ⚠️ 注意：原 `output.rs` 中 `format_table` 和 `value_to_string` 保持不变。`cli.rs` 与 `server.rs` 中调用 `format_row_value` 的签名未变（`&Row, usize -> Value`），无需修改调用点。

**Step 3.3：验证编译**

```sh
cargo check -p gaussdb-mcp
```

期望：无错误。若有未使用 import、feature 名拼写错误，按 compiler 提示修。

**Step 3.4：Commit**

```bash
git add tools/gaussdb-mcp/Cargo.toml tools/gaussdb-mcp/src/output.rs
git commit -m "fix(gaussdb-mcp): preserve NUMERIC and other unsupported types in query output

Previously, format_row_value used trial-and-error try_get across 6 Rust
types and silently returned Value::Null when none matched — causing
NUMERIC, UUID, JSON, TIMESTAMP, and other common types to be dropped
from query output.

Refactored to type-aware dispatch based on column.type_():
- NUMERIC: decoded via rust_decimal::Decimal, emitted as JSON Number
  (or String if precision exceeds f64)
- UUID/JSON/JSONB/DATE/TIME/TIMESTAMP/TIMESTAMPTZ: explicit branches
- Unknown types: visible hex-byte fallback instead of silent NULL

Enabled with-rust_decimal-1, with-serde_json-1, with-uuid-1, and
with-chrono-0_4 features on the tokio-opengauss dependency."
```

---

### Task 4：gaussdb-mcp 单元测试覆盖 format_row_value

**Files:**
- Create: `tools/gaussdb-mcp/tests/output.rs`（或 `tests/format_row_value.rs`）

> 由于 `format_row_value` 接受 `&Row` 而 `Row` 需要 DB 连接构造，单元测试较困难。**建议改方案**：把 NUMERIC → JSON 的转换逻辑（`decimal_to_json`）和 fallback 字符串构造抽成纯函数，对这些纯函数写单测；对 `format_row_value` 整体留集成测试覆盖。

**Step 4.1：抽取纯函数（若 Task 3 未抽）**

在 `output.rs` 内已抽出 `decimal_to_json`。新增：
```rust
pub(crate) fn format_unsupported_type(type_name: &str, bytes: &[u8]) -> String {
    format!("<unsupported type {}>: \\x{}", type_name, hex_bytes(bytes))
}
```

**Step 4.2：写单测**

在 `output.rs` 底部加 `#[cfg(test)] mod tests { ... }`：
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal::Decimal;
    use std::str::FromStr;

    #[test]
    fn decimal_to_json_small_number() {
        let d = Decimal::from_str("3950.123456").unwrap();
        let v = decimal_to_json(d);
        assert_eq!(v, serde_json::json!(3950.123456));
    }

    #[test]
    fn decimal_to_json_huge_number_falls_back_to_string() {
        // 超出 f64 精度，应当输出 String 保留精度
        let d = Decimal::from_str("18446744073709551615").unwrap();
        let v = decimal_to_json(d);
        assert_eq!(v, serde_json::Value::String("18446744073709551615".to_string()));
    }

    #[test]
    fn decimal_to_json_negative() {
        let d = Decimal::from_str("-123.456").unwrap();
        let v = decimal_to_json(d);
        assert_eq!(v, serde_json::json!(-123.456));
    }

    #[test]
    fn format_unsupported_type_visible() {
        let s = format_unsupported_type("hstore", &[0x01, 0x02, 0xff]);
        assert!(s.contains("hstore"));
        assert!(s.contains("0102ff"));
    }
}
```

**Step 4.3：运行单测**

```sh
cargo test -p gaussdb-mcp --lib output
```

期望：全部通过。

**Step 4.4：Commit**

```bash
git add tools/gaussdb-mcp/src/output.rs
git commit -m "test(gaussdb-mcp): unit tests for decimal_to_json and unsupported-type fallback"
```

---

### Task 5：集成测试 — 端到端 NUMERIC 查询

**Files:**
- Modify: `tools/gaussdb-mcp/tests/`（若已有 integration test 目录）
- 或 Create: `tools/gaussdb-mcp/tests/numeric_e2e.rs`

> 由于 gaussdb-mcp 当前可能没有 DB-backed 集成测试范式，这一步可改为「手工验证脚本」放 README。若用户已用 `docker compose up -d` 起 DB，建议加自动测试。

**Step 5.1：手工验证脚本（最小成本）**

写一个 `tests/numeric_smoke.sh` 或文档片段：
```sh
docker compose up -d
cargo build -p gaussdb-mcp --release
./target/release/gaussdb cli --sql "SELECT 123.456::numeric AS n, 'abc'::text AS t, NULL::numeric AS nn" --format json
# 期望输出：n=123.456 (Number), t="abc" (String), nn=null (Null)
```

**Step 5.2：Commit**

```bash
git add tools/gaussdb-mcp/tests/numeric_smoke.sh
git commit -m "test(gaussdb-mcp): add NUMERIC smoke test script"
```

---

### Task 6：文档更新 + 最终验证

**Step 6.1：更新 gaussdb-mcp README**

在 `tools/gaussdb-mcp/README.md` 的 Features 节或新增 "Supported Types" 节，列出：
- ✅ NUMERIC（via rust_decimal，保留精度）
- ✅ UUID / JSON / JSONB / TIMESTAMP / DATE / TIME / TIMETZ / TIMESTAMPTZ
- ✅ INT2/4/8 / FLOAT4/8 / BOOL / BYTEA / TEXT/VARCHAR/BPCHAR/NAME
- ⚠️ 未知类型可见 hex fallback（不再静默 NULL）

**Step 6.2：更新 tokio-opengauss lib.rs feature 表**

已在 Task 2.2 完成。

**Step 6.3：CI 验证**

```sh
cargo fmt --all -- --check
RUSTFLAGS="-Dwarnings" cargo clippy --all --all-targets
cargo test -p gaussdb-mcp
cargo test -p tokio-opengauss --features "integration with-rust_decimal-1" --test test -- types::rust_decimal_1
```

**Step 6.4：Final commit（如有文档变更）**

```bash
git add tools/gaussdb-mcp/README.md
git commit -m "docs(gaussdb-mcp): document supported types in query output"
```

---

## 风险与开放问题

1. **`rust_decimal::Decimal` 精度边界**：Decimal 最大 96 位（约 28-29 十进制位），GaussDB NUMERIC 可达 131072 位。理论上超精度值会被 `checked_from_postgres` 拒绝返回 Err。**对策**：超精度时 fallback 为 String（解析 raw bytes 直接渲染），不在此版本实现，先标 known limitation。

2. **OpenGauss 与 PostgreSQL NUMERIC 二进制格式是否一致**：根据现有 `tokio-opengauss` 假设 wire protocol v3 兼容，应当一致。集成测试（Task 2.5）是关键验证点。若 OpenGauss 在 NUMERIC 上有扩展（如货币类型 MONEY 内部表示不同），需追加测试 case。

3. **rust_decimal 版本对齐**：Task 1 与 Task 2 都用 `1.41`，与 `rust_decimal-1` alias 一致；gaussdb-mcp Task 3 也用 `1.41`。需保证三处版本同步，否则 cargo 会编译两个版本的 rust_decimal。

4. **feature 编译矩阵**：CI 需新增 `--features with-rust_decimal-1` 与 `--all-features` 的 build/test。建议在 `.github/workflows/ci.yml` 加一条 job，但本计划不强制（视仓库 CI 策略）。

5. **是否要支持 NUMERIC[] 数组**：当前计划不覆盖。`array-impls` feature + `with-rust_decimal-1` 理论上能 work，但需测试。若用户有 NUMERIC[] 列需求，留作后续工作。

6. **decimal_to_json 性能**：`Decimal::to_string()` + `serde_json::Number::from_str()` 是双次解析。NUMERIC 列高频出现时可能成为瓶颈，但这是工具层、单次查询通常 rows 数有限，先 profile 后优化。

---

## 验收标准

- [ ] `cargo check -p opengauss-types --features with-rust_decimal-1` 通过
- [ ] `cargo check -p gaussdb-mcp` 通过（含新 features）
- [ ] `cargo test -p tokio-opengauss --features "integration with-rust_decimal-1" --test test -- types::rust_decimal_1` 全部通过
- [ ] `cargo test -p gaussdb-mcp --lib output` 全部通过
- [ ] 手工：`gaussdb cli --sql "SELECT 123.456::numeric" --format json` 输出 `123.456` 而非 `null`
- [ ] 手工：`gaussdb cli --sql "SELECT 'a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11'::uuid"` 输出 UUID 字符串而非 `null`
- [ ] `cargo fmt --all -- --check` 通过
- [ ] `RUSTFLAGS="-Dwarnings" cargo clippy --all --all-targets` 通过
