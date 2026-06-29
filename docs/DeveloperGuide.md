# Developer Guide

This guide is for developers who want to use the `gaussdb` public crate, extend the MCP server, or build custom tools on top of the wire protocol implementation. External projects should depend on `gaussdb`; the other workspace crates are internal (`publish = false`) and documented here for contributors.

## Table of Contents

- [Architecture Overview](#architecture-overview)
- [Crate Reference](#crate-reference)
  - [gaussdb](#gaussdb-public-entry-point)
  - [tokio-opengauss](#tokio-opengauss-async-client)
  - [opengauss](#opengauss-sync-client)
  - [opengauss-protocol](#opengauss-protocol)
  - [opengauss-types](#opengauss-types)
  - [opengauss-derive](#opengauss-derive)
  - [opengauss-native-tls / opengauss-openssl](#opengauss-native-tls--opengauss-openssl)
  - [codegen](#codegen)
- [Wire Protocol](#wire-protocol)
- [Using the Library in Your Project](#using-the-library-in-your-project)
- [Extending the MCP Server](#extending-the-mcp-server)
- [Authentication Flow](#authentication-flow)
- [Feature Flags](#feature-flags)
- [Security Considerations](#security-considerations)

---

## Architecture Overview

```
┌─────────────────────────────────────────────────────────────────┐
│                         Applications                             │
├──────────────┬──────────────────┬───────────────────────────────┤
│  MCP Server  │   Custom Tools   │  Web Services / Your Code     │
│ (gaussdb-mcp)│                  │                               │
└──────┬───────┴────────┬─────────┴────────────────┬──────────────┘
       │                │                        │
       ▼                ▼                        ▼
┌─────────────────────────────────────────────────────────────────┐
│                          gaussdb                                 │
│              Public facade (async + sync APIs)                   │
└──────────────────────────────┬──────────────────────────────────┘
                               │
       ┌───────────────────────┴───────────────────────┐
       ▼                                               ▼
┌──────────────┐                          ┌──────────────────┐
│tokio-opengauss│                         │ opengauss        │
│ (async core) │   (internal crates)      │ (sync wrapper)   │
└──────┬───────┘                          └────────┬─────────┘
       │                                           │
       └───────────────────┬───────────────────────┘
                           ▼
┌──────────────────────────────────────────────────────────────────┐
│              opengauss-protocol                                    │
│   Message types, wire format encoding/decoding                    │
│   Authentication: MD5, SCRAM, SHA256, SM3                         │
└──────────────────────┬───────────────────────────────────────────┘
                       │
                       ▼
┌──────────────────────────────────────────────────────────────────┐
│                opengauss-types                                     │
│   ToSql / FromSql traits, type mapping, OID system                │
└──────────────────────────────────────────────────────────────────┘
```

### Key Design Principles

1. **Zero FFI**: No C dependencies (no libpq). Pure Rust wire protocol implementation.
2. **Composability**: Protocol, types, and client are separate crates for flexible composition.
3. **TLS pluggable**: TLS is abstracted; choose native-tls or openssl connector.
4. **PostgreSQL compatible**: openGauss uses the PostgreSQL wire protocol v3.0+, so this works with both.

---

## Crate Reference

### gaussdb (Public Entry Point)

**Crate**: `crates/gaussdb`  
**Cargo**: `gaussdb = "0.1.0"`  
**Description**: The single public entry point for external consumers. `gaussdb` re-exports the async surface from `tokio-opengauss` at the crate root by default, exposes a synchronous API under `gaussdb::sync` when the `sync` feature is enabled, and provides config-aware connections through `gaussdb::config` when the `config` feature is enabled. Low-level driver building blocks are available under `gaussdb::driver`.

> **Note**: All other workspace crates (`opengauss`, `tokio-opengauss`, `opengauss-protocol`, `opengauss-types`, `opengauss-derive`, `opengauss-native-tls`, `opengauss-openssl`) are now `publish = false` internal crates. They remain workspace members for layering, but external projects should depend only on `gaussdb`.

#### Key Types

| Type | Description |
|------|-------------|
| `Client` | Async client: query, execute, prepare, copy, transactions |
| `sync::Client` | Sync client: same API shape, runs on an internal tokio Runtime |
| `Config` | Connection configuration builder (also available as `gaussdb::driver::config`) |
| `config::connect_async` / `config::connect_sync` | Config-aware connection helpers (requires `config` feature) |
| `config::resolve` | Resolve config, keyring, and TLS without connecting (requires `config` feature) |
| `Statement` | Prepared statement |
| `Row` | Query result row |
| `NoTls` | Marker type for no-TLS connections |
| `types::ToSql` / `types::FromSql` | Type conversion traits (shared across sync/async) |

#### Basic Usage (Async)

```rust
use gaussdb::{Config, NoTls};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Parse connection string
    let config: Config = "host=localhost user=gaussdb password=secret dbname=postgres"
        .parse()?;

    // Connect
    let (client, connection) = config.connect(NoTls).await?;

    // Spawn the connection handler
    tokio::spawn(async move {
        if let Err(e) = connection.await {
            eprintln!("connection error: {}", e);
        }
    });

    // Execute a query
    let rows = client.query("SELECT id, name FROM users WHERE active = $1", &[&true]).await?;

    for row in &rows {
        let id: i32 = row.get(0);
        let name: &str = row.get(1);
        println!("{}: {}", id, name);
    }

    Ok(())
}
```

#### Sync Usage

Enable the `sync` feature:

```toml
[dependencies]
gaussdb = { version = "0.1.0", features = ["sync"] }
```

```rust
use gaussdb::sync::{Client, NoTls};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut client = Client::connect(
        "host=localhost user=gaussdb password=secret dbname=postgres",
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

The sync client internally spawns a tokio runtime and blocks on async futures. For performance-critical applications, prefer the async API.

#### Config-aware Connection

Enable the `config` feature for high-level, file and keychain based connection resolution:

```toml
[dependencies]
gaussdb = { version = "0.1.0", features = ["config", "sync", "tls-native-tls"] }
```

```rust
use std::path::Path;

// Synchronous: resolves config, keyring password, and TLS into a connected Client
let client = gaussdb::config::connect_sync(
    None,                        // optional dsn override
    Some(Path::new("./my.toml")), // optional config file path
    Some("prod"),                // optional connection name
)?;

// Asynchronous equivalent
let client = gaussdb::config::connect_async(None, Some(Path::new("./my.toml")), Some("prod")).await?;

// Resolve without connecting
let resolved = gaussdb::config::resolve(None, Some(Path::new("./my.toml")), Some("prod"))?;
// resolved.connection_url, resolved.sslmode, resolved.timeout_config, ...
```

The `config` feature reads TOML config files, OS keychain entries, and DSN overrides, then selects the correct TLS mode based on `sslmode`:

- `Disable` → NoTls
- `Prefer` / `Require` → TLS, skip certificate verification
- `VerifyCa` → TLS, verify cert
- `VerifyFull` → TLS, verify cert + hostname

Low-level configuration building blocks (for example `Config`, `SslMode`, `Host`) are re-exported under `gaussdb::driver::config`.

---

### tokio-opengauss (Async Client)

**Crate**: `crates/tokio-opengauss`  
**Cargo**: internal crate (`publish = false`)  
**Description**: Asynchronous openGauss/PostgreSQL client built on tokio. This section is kept for internal contributors; external users should use `gaussdb` instead.

#### Key Types

| Type | Description |
|------|-------------|
| `Client` | Main async client: query, execute, prepare, copy, transactions |
| `Connection` | Background connection task (spawn with `tokio::spawn`) |
| `Config` | Connection configuration builder |
| `Statement` | Prepared statement |
| `Portal` | Named portal (cursor) |
| `Row` | Query result row |
| `ToStatement` | Trait for converting to statement (strings, prepared) |
| `NoTls` | Marker type for no-TLS connections |

#### Basic Usage

```rust
use tokio_opengauss::{Config, NoTls};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Parse connection string
    let config: Config = "host=localhost user=gaussdb password=secret dbname=postgres"
        .parse()?;

    // Connect
    let (client, connection) = config.connect(NoTls).await?;

    // Spawn the connection handler
    tokio::spawn(async move {
        if let Err(e) = connection.await {
            eprintln!("connection error: {}", e);
        }
    });

    // Execute a query
    let rows = client.query("SELECT id, name FROM users WHERE active = $1", &[&true]).await?;

    for row in &rows {
        let id: i32 = row.get(0);
        let name: &str = row.get(1);
        println!("{}: {}", id, name);
    }

    Ok(())
}
```

#### Prepared Statements

```rust
let stmt = client.prepare("INSERT INTO users (name, email) VALUES ($1, $2)").await?;
client.execute(&stmt, &[&"Alice", &"alice@example.com"]).await?;
```

#### Transactions

```rust
let tx = client.transaction().await?;
tx.execute("UPDATE accounts SET balance = balance - 100 WHERE id = $1", &[&1]).await?;
tx.execute("UPDATE accounts SET balance = balance + 100 WHERE id = $1", &[&2]).await?;
tx.commit().await?;
```

#### COPY Protocol

```rust
// COPY FROM
let sink = client.copy_in("COPY users (name, email) FROM STDIN").await?;
let writer = sink.into_writer();
// Write CSV/TSV data to writer...

// COPY TO
let rows = client.copy_out("COPY users TO STDOUT").await?;
// Read rows from the stream...
```

#### Query Cancellation

```rust
let cancel_token = client.cancel_token();
// ... in another task:
cancel_token.cancel_query(NoTls).await?;
```

#### TLS Connections

```rust
use opengauss_native_tls::MakeTlsConnector;
use native_tls::TlsConnector;

let connector = TlsConnector::builder()
    .danger_accept_invalid_certs(true)  // or configure proper certs
    .build()?;
let tls = MakeTlsConnector::new(connector);

let (client, connection) = config.connect(tls).await?;
```

---

### opengauss (Sync Client)

**Crate**: `crates/opengauss`  
**Cargo**: internal crate (`publish = false`)  
**Description**: Synchronous wrapper around `tokio-opengauss`. The public sync API is exposed through `gaussdb::sync` (requires the `sync` feature). This section is kept for internal contributors; external users should use `gaussdb` with `features = ["sync"]`.

#### Key Types

| Type | Description |
|------|-------------|
| `Client` | Synchronous client: query, execute, prepare, transactions |
| `Config` | Connection configuration |
| `Transaction` | Transaction handle |
| `BinaryCopyInWriter` | COPY FROM writer |
| `BinaryCopyOutReader` | COPY TO reader |

#### Basic Usage

```rust
use gaussdb::sync::{Client, NoTls};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut client = Client::connect(
        "host=localhost user=gaussdb password=secret dbname=postgres",
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

The sync client internally spawns a tokio runtime. For performance-critical applications, prefer the async API.

---

### opengauss-protocol

**Crate**: `crates/opengauss-protocol`  
**Description**: Low-level wire protocol implementation. Message serialization/deserialization, authentication handshake, type encoding/decoding.

#### Key Modules

| Module | Description |
|--------|-------------|
| `message` | Backend and Frontend message types (Startup, Query, Parse, Bind, etc.) |
| `authentication` | Auth message handling (MD5, SCRAM-SHA-256, SHA256, SM3) |
| `codec` | Binary encoding/decoding of protocol messages |

#### Protocol Message Types (Backend)

```rust
// Messages sent FROM the server
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

#### Protocol Message Types (Frontend)

```rust
// Messages sent TO the server
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

#### Usage (Low-Level)

```rust
use opengauss_protocol::message::backend::BackendMessage;
use opengauss_protocol::message::frontend::FrontendMessage;
use tokio_util::codec::Framed;

// Usually you don't use this directly unless building a custom driver.
// tokio-opengauss handles the protocol internally.
```

---

### opengauss-types

**Crate**: `crates/opengauss-types`  
**Description**: Type system: `ToSql` and `FromSql` traits, OID mappings, type conversion.

#### Key Traits

```rust
/// Convert a Rust value to a PostgreSQL parameter
pub trait ToSql {
    fn to_sql(&self, ty: &Type, out: &mut BytesMut) -> Result<IsNull, Box<dyn Error + Sync + Send>>;
    fn accepts(ty: &Type) -> bool;
    fn to_sql_checked(&self, ty: &Type, out: &mut BytesMut) -> Result<IsNull, Box<dyn Error + Sync + Send>>;
}

/// Convert a PostgreSQL value to a Rust type
pub trait FromSql<'a>: Sized {
    fn from_sql(ty: &Type, raw: &'a [u8]) -> Result<Self, Box<dyn Error + Sync + Send>>;
    fn accepts(ty: &Type) -> bool;
}
```

#### Built-in Type Support

| Rust Type | PostgreSQL Type |
|-----------|----------------|
| `bool` | `BOOL` |
| `i16`, `i32`, `i64` | `SMALLINT`, `INTEGER`, `BIGINT` |
| `f32`, `f64` | `REAL`, `DOUBLE PRECISION` |
| `&str`, `String` | `TEXT`, `VARCHAR`, `CHAR(n)` |
| `&[u8]`, `Vec<u8>` | `BYTEA` |
| `SystemTime` | `TIMESTAMPTZ` |
| `IpAddr` | `INET` |
| `HashMap<String, Option<String>>` | `HSTORE` |

#### Optional Type Support (Feature Flags)

Enable with feature flags on `gaussdb`:

| Feature | Type Support |
|---------|-------------|
| `with-chrono-0_4` | `chrono::NaiveDateTime`, `chrono::DateTime<Utc>`, `chrono::NaiveDate`, `chrono::NaiveTime` |
| `with-uuid-1` | `uuid::Uuid` |
| `with-serde_json-1` | `serde_json::Value` (JSON/JSONB) |
| `with-time-0_3` | `time::Date`, `time::Time`, `time::PrimitiveDateTime`, `time::OffsetDateTime` |
| `with-geo-types-0_7` | PostGIS geometry types |
| `with-eui48-1` | `eui48::MacAddress` (MACADDR) |
| `with-smol_str-01` | `smol_str::SmolStr` |

---

### opengauss-derive

**Crate**: `crates/opengauss-derive`  
**Description**: Proc macros for `#[derive(ToSql, FromSql)]`.

```rust
use gaussdb::types::{ToSql, FromSql};

#[derive(Debug, ToSql, FromSql)]
#[opengauss(name = "my_type")]  // maps to a custom PostgreSQL type
struct MyType {
    field1: String,
    field2: i32,
}
```

---

### opengauss-native-tls / opengauss-openssl

**Description**: TLS connector implementations used internally by `tokio-opengauss`. External users consume these through the `gaussdb` facade.

- `gaussdb::native_tls::MakeTlsConnector` (feature `tls-native-tls`) uses the `native-tls` crate (platform-native TLS: SChannel on Windows, Secure Transport on macOS, OpenSSL on Linux)
- `gaussdb::openssl::MakeTlsConnector` (feature `tls-openssl`) uses the `openssl` crate directly

Both implement the `TlsConnect` trait expected by `gaussdb::connect()`.

```rust
use gaussdb::native_tls::MakeTlsConnector;
use native_tls::TlsConnector;

let connector = TlsConnector::new()?;  // or builder with custom options
let tls = MakeTlsConnector::new(connector);
let (client, connection) = gaussdb::connect("host=... sslmode=require", tls).await?;
```

---

### codegen

**Crate**: `tools/codegen`  
**Description**: Dev tool for generating type mapping code from PostgreSQL catalog data. Not needed for normal usage.

---

## Wire Protocol

### Protocol Version

This implementation supports the PostgreSQL wire protocol **v3.0+** (which openGauss uses). Recent additions handle the PostgreSQL 18 `NegotiateProtocolVersion` message and variable-length `BackendKeyData`.

### Connection Flow

```
Client                          Server
  |                               |
  |-- SSLRequest ---------------->|
  |<-- 'S' (accept) / 'N' ------|
  |                               |
  |-- StartupMessage ----------->|
  |   (user, database, params)    |
  |                               |
  |<-- AuthenticationXxx --------|
  |   (MD5 / SCRAM / SHA256 etc.) |
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

### Startup Message

```rust
Startup {
    version: 3,    // Protocol major version
    version_2: 0,  // Protocol minor version
    parameters: {
        "user": "myuser",
        "database": "mydb",
        "application_name": "myapp",
        // ... any other parameters
    },
}
```

### Extended Query Protocol

The extended query protocol uses Parse → Bind → Execute → Sync instead of simple Query:

```rust
// 1. Parse: prepare a statement
Parse { name: "stmt1", query: "SELECT $1::int + $2::int", param_types: vec![] }

// 2. Bind: bind parameters to the statement
Bind { portal: "", statement: "stmt1", formats: vec![], values: vec![...], result_formats: vec![] }

// 3. Describe: get result column info (optional)
Describe { variant: Portal, name: "" }

// 4. Execute: run the portal
Execute { portal: "", max_rows: 0 }

// 5. Sync: complete the transaction
Sync
```

---

## Using the Library in Your Project

### Adding Dependencies

**Async (recommended):**

```toml
[dependencies]
tokio = { version = "1", features = ["full"] }
gaussdb = "0.1.0"
# Optional TLS
#gaussdb = { version = "0.1.0", features = ["tls-native-tls"] }
# Optional type extensions:
#   with-chrono-0_4, with-uuid-1, with-serde_json-1, with-rust_decimal-1, etc.
```

**Sync:**

```toml
[dependencies]
gaussdb = { version = "0.1.0", features = ["sync"] }
# With TLS and type extensions:
#gaussdb = { version = "0.1.0", features = ["tls-native-tls", "sync", "with-chrono-0_4"] }
```

### Connection URLs

Connection parameters follow the libpq convention:

```
host=localhost port=5432 user=gaussdb password=secret dbname=postgres
host=db.example.com user=admin password=secret dbname=production sslmode=require
```

Or use `Config` builder:

```rust
let config = gaussdb::Config::new()
    .host("localhost")
    .port(5432)
    .user("postgres")
    .password("secret")
    .dbname("mydb");
let (client, connection) = config.connect(NoTls).await?;
```

### Connection Pooling

For production applications, use a connection pool:

```toml
[dependencies]
deadpool-postgres = "0.14"  # or bb8-postgres, mobc-postgres
```

```rust
use deadpool_postgres::{Config, Pool};
// deadpool-postgres can work with gaussdb through the Manager trait
```

### Error Handling

```rust
use gaussdb::error::SqlState;

match client.query("SELECT * FROM nonexistent", &[]).await {
    Err(e) => {
        if let Some(db_err) = e.as_db_error() {
            println!("SQLSTATE: {}", db_err.code().code());
            println!("Message: {}", db_err.message());
            println!("Detail: {:?}", db_err.detail());
            println!("Hint: {:?}", db_err.hint());
        }
    }
    Ok(rows) => { /* process rows */ }
}
```

---

## Extending the MCP Server

### Adding a New MCP Tool

1. Define parameter struct in `server.rs`:

```rust
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct MyToolParams {
    pub param1: String,
    #[serde(default)]
    pub connection_name: Option<String>,
}
```

2. Add tool method to `GaussdbMcp`:

```rust
#[tool(description = "Description of my tool")]
async fn my_tool(
    &self,
    Parameters(params): Parameters<MyToolParams>,
) -> Result<CallToolResult, McpError> {
    let client = self.get_client_for(params.connection_name.as_deref()).await?;
    // ... implement tool logic
    Ok(CallToolResult::success(vec![Content::text(result.to_string())]))
}
```

3. Add any needed SQL templates to `queries.rs`.

### Adding New CLI Options

1. Add CLI arguments to the `Commands::Cli` enum in `main.rs`
2. Update `cli::CliArgs` in `cli.rs`
3. Implement the handling in `cli::run_cli()`

### Connection State Management

The `GaussdbMcp` server manages connection states:

```
ConnectionState:
  Pending(resolver)       → waiting for keychain read
  Connecting(url)         → ready to connect, not yet connected
  Connected(Arc<Client>)  → active connection
  Unavailable(url)        → connection failed
```

Connections are lazily established on first use. The `try_connect()` method eagerly probes the default connection at startup.

### Password Resolution Pipeline

```
Config File → PasswordSource detection
  ├─ Plaintext       → immediate resolution, auto-migrate callback registered
  ├─ "keyring"       → lazy resolution via OS keychain
  ├─ EnvVar          → already in URL, no migration
  └─ None            → lazy keychain read (for password-less "trust" auth)
```

---

## Authentication Flow

### Supported Methods

The protocol crate supports these authentication methods:

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

### SCRAM-SHA-256 Flow

```
Client                          Server
  |                               |
  |<-- SASL(SCRAM-SHA-256) ------|
  |-- SASLInitialResponse ------>|
  |   (client-first-message)      |
  |<-- SASLResponse ------------|
  |   (server-first-message)      |
  |-- SASLResponse ------------->|
  |   (client-final-message)      |
  |<-- SASLResponse ------------|
  |   (server-final-message)      |
  |<-- AuthenticationOk ---------|
```

### openGauss SHA256 Flow

```
Client                          Server
  |                               |
  |<-- SHA256(salt, iter, token)->|
  |-- PasswordMessage ---------->|
  |   (SHA256 hashed password)    |
  |<-- AuthenticationOk ---------|
```

---

## Feature Flags

### gaussdb

`gaussdb` forwards feature flags to `tokio-opengauss`, which in turn forwards them to `opengauss-types`:

| Feature | Default | Description |
|---------|---------|-------------|
| `sync` | No | Enable `gaussdb::sync` synchronous API |
| `config` | No | Config-aware connect API (`gaussdb::config`); brings `toml`, `dirs`, and `keyring` dependencies |
| `tls-native-tls` | No | Enable `gaussdb::native_tls::MakeTlsConnector` |
| `tls-openssl` | No | Enable `gaussdb::openssl::MakeTlsConnector` |
| `derive` | No | Enable `#[derive(ToSql, FromSql)]` via `opengauss-derive` |
| `runtime` | Yes | Forwarded to `tokio-opengauss` |
| `with-chrono-0_4` | No | Forwarded to `tokio-opengauss` / `opengauss-types` |
| `with-uuid-1` | No | Forwarded to `tokio-opengauss` / `opengauss-types` |
| `with-serde_json-1` | No | Forwarded to `tokio-opengauss` / `opengauss-types` |
| `with-time-0_3` | No | Forwarded to `tokio-opengauss` / `opengauss-types` |
| `with-geo-types-0_7` | No | Forwarded to `tokio-opengauss` / `opengauss-types` |
| `with-eui48-1` | No | Forwarded to `tokio-opengauss` / `opengauss-types` |
| `with-smol_str-01` | No | Forwarded to `tokio-opengauss` / `opengauss-types` |

### tokio-opengauss

Internal crate (`publish = false`). Features are listed here for contributors; end users enable them through `gaussdb`.

| Feature | Default | Description |
|---------|---------|-------------|
| `runtime` | Yes | Enable tokio runtime integration |
| `js` | No | WASM/JavaScript target |
| `with-chrono-0_4` | No | chrono DateTime/NaiveDateTime support |
| `with-uuid-1` | No | uuid::Uuid support |
| `with-serde_json-1` | No | serde_json JSON/JSONB support |
| `with-time-0_3` | No | time crate support |
| `with-geo-types-0_7` | No | PostGIS geometry support |
| `with-eui48-1` | No | MACADDR support |
| `with-smol_str-01` | No | SmolStr optimization |

### opengauss (sync)

Internal crate (`publish = false`). The sync API is exposed publicly through `gaussdb::sync` with `features = ["sync"]`:

```toml
[dependencies]
gaussdb = { version = "0.1.0", features = ["sync", "with-chrono-0_4", "with-uuid-1"] }
```

---

## Security Considerations

### Password Handling

- Never log connection URLs with passwords. Use `redact_url()` from the MCP tool.
- In MCP mode, passwords are stored in the OS keychain, not in config files.
- The `"keyring"` sentinel prevents accidental plaintext exposure.

### Query Safety

- The MCP server restricts `execute_query` to `SELECT` and `EXPLAIN` only (read-only).
- CLI mode allows all statements but requires explicit user invocation.
- Always use parameterized queries (`$1`, `$2`); never string interpolation.

### TLS

- Production connections should use `sslmode=verify-full`.
- The `danger_accept_invalid_certs()` option is only for development/testing.
- Certificate verification is skipped by default in `--check-connection` mode (for diagnostics).

### SQL Injection Prevention

```rust
// ✅ GOOD: parameterized
client.query("SELECT * FROM users WHERE name = $1", &[&user_input]).await?;

// ❌ BAD: string interpolation
let sql = format!("SELECT * FROM users WHERE name = '{}'", user_input);
client.query(&sql, &[]).await?;
```
