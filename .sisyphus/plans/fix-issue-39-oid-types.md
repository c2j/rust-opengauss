# Fix Issue #39 — OID/REGPROC/REGTYPE Type Display

## Problem

`output.rs` groups OID-family types (OID, REGPROC, REGTYPE) with INT4 and uses `try_get::<i32>()`, but `FromSql<i32>` only `accepts(Type::INT4)`. The typed extraction fails, falls through to raw-bytes fallback, and shows `<unsupported type oid>: \x...` instead of the actual integer value.

## Root Cause (confirmed via MCP + regression tests)

### Layer 1: `crates/opengauss-types/src/lib.rs:773`
```rust
simple_from!(u32, oid_from_sql, OID);  // u32 only accepts Type::OID
```
REGPROC, REGTYPE, XID, CID, TID have NO `FromSql` implementation.

### Layer 2: `tools/gaussdb-mcp/src/output.rs:52`
```rust
Type::INT4 | Type::OID | Type::REGPROC | Type::REGTYPE => {
    r.try_get::<_, Option<i32>>(i)  // i32 rejects OID/REGPROC/REGTYPE
}
```
And line 59: REGCLASS grouped with INT8 → i64 (REGCLASS is u32 OID-alias, not i64).

### Affected types (wire format: all 4-byte unsigned big-endian)

| Type | PG OID | Current FromSql | Fix needed |
|------|--------|----------------|------------|
| OID | 26 | u32 ✅ | output.rs: use u32 not i32 |
| REGPROC | 24 | none ❌ | add to simple_from!(u32) |
| REGPROCEDURE | 2202 | none ❌ | add to simple_from!(u32) |
| REGOPER | 2203 | none ❌ | add to simple_from!(u32) |
| REGOPERATOR | 2204 | none ❌ | add to simple_from!(u32) |
| REGCLASS | 2205 | none ❌ | add to simple_from!(u32) |
| REGTYPE | 2206 | none ❌ | add to simple_from!(u32) |
| REGNAMESPACE | 4089 | none ❌ | add to simple_from!(u32) |
| REGCOLLATION | 4191 | none ❌ | add to simple_from!(u32) |
| XID | 28 | none ❌ | add to simple_from!(u32) |
| CID | 29 | none ❌ | add to simple_from!(u32) |
| TID | 27 | none ❌ | out of scope (6-byte, not 4) |

## Fix Plan

### Step 1: Widen FromSql<u32> acceptance

**File:** `crates/opengauss-types/src/lib.rs`, line 773

```rust
// Before:
simple_from!(u32, oid_from_sql, OID);
// After:
simple_from!(u32, oid_from_sql, OID,
    REGPROC, REGPROCEDURE, REGOPER, REGOPERATOR, REGCLASS, REGTYPE,
    REGNAMESPACE, REGCOLLATION, XID, CID);
```

Rationale: All REG* types are OID aliases — 4-byte unsigned integers on the wire. The `accepts!` macro already supports multiple types (`$($expected:ident),+`), and `simple_from!` passes through to it. REGPROCEDURE/REGOPER/REGOPERATOR/REGNAMESPACE/REGCOLLATION were confirmed missing by Momus review (defined in type_gen.rs with no FromSql impl, zero references in output.rs).

### Step 2: Fix output.rs `format_value_with_type()` dispatch

**File:** `tools/gaussdb-mcp/src/output.rs`, lines 47-63

Split the current combined arm into separate type-correct arms:

```rust
// INT4 → i32 (unchanged, isolated)
Type::INT4 => typed_or_raw(row, idx, ty, |r, i| {
    r.try_get::<_, Option<i32>>(i)
        .ok()
        .map(|v| v.map(|x| json!(x)).unwrap_or(Value::Null))
}),
// OID-family → u32 (all REG* types, XID, CID)
Type::OID | Type::REGPROC | Type::REGPROCEDURE | Type::REGOPER
| Type::REGOPERATOR | Type::REGCLASS | Type::REGTYPE
| Type::REGNAMESPACE | Type::REGCOLLATION
| Type::XID | Type::CID => {
    typed_or_raw(row, idx, ty, |r, i| {
        r.try_get::<_, Option<u32>>(i)
            .ok()
            .map(|v| v.map(|x| json!(x)).unwrap_or(Value::Null))
    })
},
// INT8 → i64 (REGCLASS removed from this arm)
Type::INT8 => typed_or_raw(row, idx, ty, |r, i| {
    r.try_get::<_, Option<i64>>(i)
        .ok()
        .map(|v| v.map(|x| json!(x)).unwrap_or(Value::Null))
}),
```

### Step 3: Fix output.rs `format_field_string()` dispatch

**File:** `tools/gaussdb-mcp/src/output.rs`, lines 171-187

Same split as Step 2, but returning `Option<String>` instead of `Value`.

### Step 4: Update regression test canary

**File:** `tests/regress/tests/oid_types.rs`

Invert `regproc_columns_fail_currently_bug_39` — after the fix, REGPROC columns must SUCCEED with u32:

```rust
#[tokio::test]
async fn regproc_columns_readable_as_u32() {
    // ... same SELECT, but assert result.is_ok() ...
}
```

## Verification Checklist

- [ ] `cargo check -p opengauss-types -p gaussdb-mcp -p regress`
- [ ] `cargo fmt --all -- --check`
- [ ] `RUSTFLAGS="-Dwarnings" cargo clippy --all --all-targets`
- [ ] `cargo test -p gaussdb-mcp`
- [ ] `cargo test -p regress --features integration` — all 14+1 tests pass
- [ ] MCP smoke test: `SELECT proname, pronamespace, proowner FROM pg_proc WHERE proname = 'to_number' LIMIT 1` returns integer values, not `<unsupported type>`

## Files Changed

| File | Change |
|------|--------|
| `crates/opengauss-types/src/lib.rs:773` | +10 type constants in simple_from! |
| `tools/gaussdb-mcp/src/output.rs:47-63` | Split INT4/OID-family/INT8 arms (format_value_with_type) |
| `tools/gaussdb-mcp/src/output.rs:171-187` | Same split (format_field_string) |
| `tests/regress/tests/oid_types.rs` | Invert regproc canary test + add REG* variants |

## Out of Scope

- **TID** (6-byte: block_id u32 + offset u16) — requires separate decoder, not handled by `oid_from_sql`
- **OID arrays** (`oidvector`, `oid[]`) — existing FromSql for arrays is separate, already works as `Vec<u32>`
