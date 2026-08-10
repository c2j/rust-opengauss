# gaussdb

A standalone MCP (Model Context Protocol) server for [openGauss](https://opengauss.org/) database introspection, plus a built-in CLI for direct SQL execution. Designed for use with AI assistants like OpenCode, OpenClaw, Lingma (VSCode/IDEA), Claude, Cursor, and other MCP-compatible tools.

Built on the openGauss/PostgreSQL wire protocol (v3.0+) with **zero FFI dependencies** — no libpq, no C libraries.

## Features

- **MCP Server** — 6 tools for database introspection via MCP protocol over stdio
- **CLI Mode** — Execute SQL directly from terminal with `--sql`, `--file`, or stdin
- **Interactive REPL** — `cli -i` drops into a readline-style SQL shell with history, multi-line input, and dot commands (`.help`, `.output`, `.save`, …)
- **Multi-connection support** — Configure multiple named databases, switch per tool call
- **OS keychain passwords** — Secure password storage via macOS Keychain / Windows Credential Manager / Linux Secret Service
- **Auto password migration** — Plaintext passwords automatically migrate to OS keychain on first connection
- **TLS support** — Automatic detection with `sslmode` (disable / require / verify-full)
- **Connection diagnostics** — `check` subcommand tests all TLS modes with verbose server info, TLS cert details, and GUC config
- **Rich error reporting** — SQLSTATE, SQLCODE, severity, detail, hint, schema, table, column context
- **File-based logging** — Daily rotating logs, no interference with stdio MCP transport
- **openGauss auth** — Supports SHA256, MD5+SHA256, SM3, SCRAM-SHA-256, and MD5 authentication

## Quick Start

### Install

**Download pre-built binary (recommended):**

Download the latest zip for your platform from [GitHub Releases](https://github.com/c2j/rust-opengauss/releases), unzip, and place `gaussdb` in your PATH.

```sh
unzip gaussdb-*.zip
sudo mv gaussdb /usr/local/bin/       # or ~/.local/bin/
gaussdb --help
```

**Build from source:**

```sh
# Build from source (Rust 1.85+ required)
cargo build -p gaussdb-mcp --release

# Binary available as `gaussdb`
./target/release/gaussdb --help
```

### Connect via environment variable

```sh
export GAUSSDB_URL="host=127.0.0.1 user=gaussdb password=secret dbname=postgres"
gaussdb
```

### Use a config file

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

### Quick CLI query

```sh
# Execute a SELECT
gaussdb cli --sql "SELECT version()"

# Execute from file
gaussdb cli --file query.sql

# Pipe SQL via stdin
echo "SELECT count(*) FROM users" | gaussdb cli
```

## Usage Modes

### Mode 1: MCP Server (default)

```sh
gaussdb                    # default: MCP mode, stdio transport
gaussdb mcp                 # explicit MCP mode
gaussdb mcp --config ./prod.toml  # with custom config
```

Integrate with AI assistants (see [Integration](#integration-with-ai-assistants)).

### Mode 2: CLI Mode

```sh
gaussdb cli [OPTIONS]

OPTIONS:
    -s, --sql <SQL>             SQL statement to execute
    -f, --file <FILE>           Read SQL from file
    -i, --interactive           Enter interactive REPL mode (see Mode 3 below)
        --check-connection      Test connectivity without executing SQL
    -v, --verbose               Show detailed connection info (with --check-connection)
        --name <NAME>           Target connection name
        --config <PATH>         Path to config file
        --format <FMT>          Output format: table, json, vertical, csv, parquet [default: table]
        --parquet-batch-size    Rows per RecordBatch for --format parquet [default: 65536]
        --parquet-compression   Parquet codec: none | snappy | zstd [default: snappy]
        --statement-timeout     Override config statement timeout
        --no-history            Do not read/write persistent per-connection SQL history (interactive mode)
```

**Examples:**

```sh
# Table output (default)
gaussdb cli --sql "SELECT * FROM users LIMIT 5"

# JSON output
gaussdb cli --sql "SELECT * FROM users LIMIT 5" --format json

# Vertical display (like \x in psql)
gaussdb cli --sql "SELECT * FROM users LIMIT 5" --format vertical

# CSV export (RFC 4180, streaming to stdout; redirect to a file to save)
gaussdb cli --sql "SELECT * FROM users" --format csv > users.csv

# Parquet export (columnar, compressed). Stats go to stderr.
gaussdb cli --sql "SELECT * FROM users" --format parquet > users.parquet
gaussdb cli --sql "SELECT * FROM big_table" --format parquet \
  --parquet-compression zstd --parquet-batch-size 100000 > big.parquet

# Large export tip: cast NUMERIC columns to text server-side to skip
# client-side decimal decoding and minimise CPU cost on multi-million-row
# exports. Server-side rendering is what psql / pg_dump effectively do.
gaussdb cli --sql "SELECT id, amount::text, created_at::text FROM big_table" \
  --format csv > big_table.csv

# DML/DDL supported
gaussdb cli --sql "INSERT INTO logs VALUES (1, 'hello')"
gaussdb cli --sql "CREATE TABLE test (id int)"

# Target a specific connection
gaussdb cli --name prod --sql "SELECT count(*) FROM orders"

# Check connectivity for a specific connection
gaussdb cli --check-connection --name prod
```

#### Parquet Export

`--format parquet` writes a columnar, compressed Apache Parquet file by streaming rows through Arrow `RecordBatch`es. Use it when the export is destined for OLAP / data-lake / downstream analytics tooling (DuckDB, Spark, pandas, BigQuery, etc.) — Parquet's columnar layout plus compression gives 5–10× smaller files and 10–50× faster downstream scans vs CSV.

```sh
# Default: snappy compression, 65536-row batches
gaussdb cli --sql "SELECT * FROM sales" --format parquet > sales.parquet

# Maximum compression (≈2× slower encode, ≈40% smaller than snappy on text-heavy data)
gaussdb cli --sql "SELECT * FROM sales" --format parquet --parquet-compression zstd > sales.parquet

# Tune row-group size: larger → better compression + bigger memory peak
gaussdb cli --sql "SELECT * FROM sales" --format parquet --parquet-batch-size 100000 > sales.parquet
```

Progress is printed to stderr (the parquet byte stream goes to stdout):

```
1000 rows written (22346 bytes, compression=zstd, batch_size=65536)
```

##### Type mapping (PostgreSQL → Arrow)

| PostgreSQL | Arrow | Notes |
|------------|-------|-------|
| `int2` / `int4` / `int8` | `int16` / `int32` / `int64` | direct |
| `float4` / `float8` | `float` / `double` | direct |
| `bool` | `bool` | direct |
| `bytea` | `binary` | direct |
| `numeric` | `decimal128(p, s)` | precision/scale sampled from first batch (see below) |
| `varchar` / `text` / `bpchar` / `name` | `utf8` | direct |
| `uuid` / `json` / `jsonb` | `utf8` | canonical string form |
| `date` | `date32` | days since epoch |
| `timestamp` | `timestamp[ns]` | |
| `timestamptz` | `timestamp[ns, tz=UTC]` | normalised to UTC |
| `time` / `timetz` | `time64[ns]` | `timetz` loses zone info |
| `oid` family / `xid` / `cid` | `int64` | PG `u32` widened |
| `inet` / `cidr` / `macaddr` / `interval` / custom types | `utf8` | string form, mirrors CLI text rendering |

##### NUMERIC precision sampling

The exporter inspects the first batch (default: first 65 536 rows) to derive `decimal128(precision, scale)`:

- All-NULL column → `decimal128(1, 0)`.
- `precision > 38` (Decimal128 limit) → falls back to `utf8` (string of the value).
- Otherwise `precision = max(integer_digits) + max(scale)` over the sample, `scale = max(scale)`.

The schema is fixed after the first batch, so a NUMERIC value in a *later* batch whose scale is larger than the sampled max is `rescale`d down (lossy truncation) so the encoded `i128` mantissa matches the schema — reader-side decoding stays consistent. If strict precision matters across the whole result, raise `--parquet-batch-size` so the sample sees the full range, or cast the column to `text` server-side.

##### Parquet vs CSV — when to pick which

| Dimension | CSV (server-side `COPY`) | Parquet (client-side Arrow) |
|---|---|---|
| Client CPU | near-zero (bytes piped through) | noticeable (columnar build + compress) |
| Memory | O(1) streaming | O(batch_size), bounded by `--parquet-batch-size` |
| Output size | large (uncompressed text) | 5–10× smaller (columnar + snappy/zstd) |
| Type fidelity | all-string (numbers lose precision) | native (int/float/decimal/timestamp) |
| Downstream load | slow (re-parse) | 10–50× faster (columnar scan, predicate pushdown) |
| Best for | cross-tool text interop, fastest raw export | OLAP / data lake / long-term archival |

Note on memory: parquet export streams rows from `query_raw` into one `RecordBatch` of `--parquet-batch-size` rows at a time, so peak memory is bounded regardless of total result size (measured: 10M rows × ~200 B source peaks at ~250 MB client RSS with default `batch_size=65536`). CSV is even cheaper client-side (server-side `COPY` is O(1)), but parquet's bounded memory makes it suitable for multi-million-row exports.

##### Build configuration

Parquet support is on by default. To build a leaner binary without it:

```sh
cargo build -p gaussdb-mcp --no-default-features
```

`--format parquet` then returns a clear error pointing at the missing feature.

### Mode 3: Interactive REPL

```sh
gaussdb cli -i                  # interactive REPL with default connection
gaussdb cli --interactive        # same, long form
gaussdb cli -i --name prod       # target a specific named connection
gaussdb cli -i --format json     # default format for results (table/json/vertical/csv)
```

Drops you into a readline-style SQL shell:

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

**Key bindings** (raw-mode via crossterm):

| Key | Action |
|-----|--------|
| `Enter` | Submit line (statements execute on `;`) |
| `↑` / `↓` or `Ctrl-P` / `Ctrl-N` | Navigate history |
| `←` / `→` or `Ctrl-B` / `Ctrl-F` | Move cursor |
| `Home` / `End` or `Ctrl-A` / `Ctrl-E` | Jump to line start / end |
| `Ctrl-U` / `Ctrl-K` | Delete to start / end of line |
| `Ctrl-L` | Clear screen |
| `Ctrl-C` | Abort current input (stay in REPL) |
| `Ctrl-D` | Exit on empty line / delete char on non-empty |
| `Backspace` / `Delete` | Delete previous / current char |

**Dot commands** (must be the first token on a fresh prompt):

| Command | Action |
|---------|--------|
| `.help` or `?` | Show help |
| `.exit` / `.quit` | Exit the REPL |
| `.connect [<name>]` | Reconnect after a dropped connection, or switch to connection `<name>` (omitted = current connection) |
| `.history` | Show SQL execution history (current session) |
| `.clear` / `.cls` | Clear screen |
| `.output <file>` | Redirect all subsequent SQL output to `file` (append mode) |
| `.output` | Reset SQL output back to stdout |
| `.save <file> [format]` | One-shot save of the most recent query result to `file` (overwrite; format optional, defaults to `--format`) |

```sh
# Inside the REPL:
$ .output session.log      # all queries append to session.log
$ SELECT * FROM users;
$ .output                  # back to stdout
$ .save users.csv csv      # re-render last result as CSV to users.csv
$ .exit
```

Notes:
- Statements execute only on `;` outside quotes/comments (`'...'`, `"..."`, `--`, `/* ... */`).
- SQL errors print to stderr and the REPL continues.
- If the server or a firewall drops an idle connection, the next statement fails and the REPL prints a hint to run `.connect`. `.connect` (no argument) re-establishes the current connection; `.connect <name>` switches to a different configured connection without restarting.
- History deduplicates consecutive identical statements.
- SQL history persists per connection name under the app data dir (`$XDG_DATA_HOME/gaussdb/history/<name>` on Linux, `~/Library/Application Support/gaussdb/history/<name>` on macOS); ↑/↓ reach prior sessions. Pass `--no-history` to opt out. History is stored in plaintext, so avoid running statements that embed secrets, or use `--no-history`.
- For very large exports, prefer one-shot `cli --format csv` over the REPL — the REPL buffers results to support `.save`.

## Configuration

### Single Connection (backward compatible)

```toml
host = "127.0.0.1"
port = 5432
user = "gaussdb"
password = "secret"
dbname = "postgres"
```

### Multiple Named Connections

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
password = "keyring"         # stored in OS keychain
dbname = "production"

[connections.staging]
url = "host=10.0.0.5 user=admin password=keyring dbname=staging sslmode=require"
```

When `[connections.<name>]` tables are present, flat fields (`host`, `user`, etc.) are ignored. When absent, they wrap into a single `"default"` connection — fully backward compatible.

`default_connection` specifies the fallback when tools don't provide a `connection_name`. Defaults to the first connection.

Each connection's password can be:
- Plaintext string — migrated to OS keychain on first successful connection
- `"keyring"` — read from OS keychain (use `store-password` subcommand to set)

## CLI Options

```
gaussdb [OPTIONS] [COMMAND]

COMMANDS:
    mcp             Run as MCP server (default when no subcommand given)
    check           Test database connectivity and exit
    store-password  Store password in OS keychain
    cli             Execute SQL from command line

GLOBAL OPTIONS (apply to all commands):
    --config <PATH>           Path to config file (default: ~/.gaussdb.toml)
    --name <NAME>             Target connection name

SERVE:
    (no additional options)

CHECK:
    -v, --verbose             Show detailed connection info

STORE-PASSWORD:
    <PASSWORD>                Password to store (positional argument)

CLI:
    -s, --sql <SQL>            SQL statement to execute
    -f, --file <FILE>          Read SQL from file (or pipe to stdin)
    -i, --interactive          Enter interactive REPL mode (see Mode 3)
        --check-connection     Test connectivity without executing SQL
    -v, --verbose              Show detailed connection info (with --check-connection)
        --format <FMT>         Output format: table, json, vertical, csv, parquet [default: table]
        --parquet-batch-size   Rows per RecordBatch for --format parquet [default: 65536]
        --parquet-compression   Parquet codec: none | snappy | zstd [default: snappy]
        --statement-timeout    Override config statement timeout (e.g. "30s")
        --connection-max-lifetime  Connection recycle interval (e.g. "10min")
        --timeout-action       "cancel" (default) or "disconnect"
```

### Connection Diagnostics

```sh
# Check the default connection (3-pass: NoTls → TLS-skip → TLS-verify)
gaussdb check

# Check a specific named connection
gaussdb check --name prod --config ~/.gaussdb.toml

# Verbose output (version, GUC params, TLS cert details, timing)
gaussdb check --verbose

# Also available via the cli subcommand
gaussdb cli --check-connection --name prod -v
```

The diagnostic tool:
1. Attempts plain TCP (NoTls)
2. Attempts TLS with certificate verification skipped
3. Attempts TLS with full certificate verification
4. Reports keychain status (accessible, empty, or unavailable)
5. In verbose mode: server version, protocol version, GUC config, TLS cert chain, and timing

### Password Management

```sh
# Store password for the first/default connection (interactive prompt)
gaussdb store-password --config ~/.gaussdb.toml

# Store password for a named connection
gaussdb store-password --name prod --config ~/.gaussdb.toml

# Non-interactive (read from stdin pipe, e.g. CI/scripts):
#   printf '%s\n' "$PW" | gaussdb store-password --name prod --config ~/.gaussdb.toml

# On first successful MCP connection with plaintext config password,
# auto-migration moves it to OS keychain and updates config to `password = "keyring"`
```

## MCP Tool Reference

| Tool | Description |
|------|-------------|
| `list_connections` | List all configured connections with status (connected/connecting/pending/unavailable) |
| `get_database_info` | Server version, encoding, collation, start time, current user, server address |
| `list_tables` | All user tables and views with schema, type, size, and comments |
| `get_table_metadata` | Columns (name, type, nullable, default, comment), primary keys, and indexes |
| `execute_query` | Execute read-only SELECT or EXPLAIN queries |
| `get_execution_plan` | EXPLAIN or EXPLAIN ANALYZE with TEXT/JSON/YAML/XML format |

All tools accept an optional `connection_name` parameter to target a specific database. When omitted, `default_connection` is used.

### Tool Parameters

**`list_connections`** — no parameters. Returns connection list with status and default indicator.

**`get_database_info`**
| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `connection_name` | string | no | Target connection name |

**`list_tables`**
| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `connection_name` | string | no | Target connection name |

**`get_table_metadata`**
| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `table_name` | string | yes | Table name |
| `schema_name` | string | no | Schema name (default: public) |
| `connection_name` | string | no | Target connection name |

**`execute_query`**
| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `sql` | string | yes | SQL query (SELECT or EXPLAIN only) |
| `timeout_ms` | number | no | Per-call statement timeout in milliseconds (overrides connection default) |
| `connection_name` | string | no | Target connection name |

**`get_execution_plan`**
| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `sql` | string | yes | SQL query to explain |
| `analyze` | boolean | no | Run EXPLAIN ANALYZE (default: false) |
| `format` | string | no | Output format: TEXT, JSON, YAML, XML (default: TEXT) |
| `timeout_ms` | number | no | Per-call statement timeout in milliseconds (overrides connection default) |
| `connection_name` | string | no | Target connection name |

### Error Response Format

When a database error occurs, MCP tools return structured error data:

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

## Integration with AI Assistants

### OpenCode

Add to `opencode.json`:

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

Or via environment variable:

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

Add to `~/.openclaw/mcp.json`:

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

### Lingma (VSCode / IDEA)

Add to `.lingma/mcp.json` in your project root:

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

Add to `claude_desktop_config.json`:

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

Add to `.cursor/mcp.json`:

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

## Statement Timeout & Connection Lifetime

`gaussdb` supports configurable SQL execution timeouts to prevent runaway queries from blocking AI assistant sessions. Applies to both MCP server and CLI.

### Configuration (TOML)

Per-connection timeout settings in the config file:

```toml
[connections.prod]
host = "192.168.1.10"
user = "admin"
password = "keyring"
dbname = "production"

statement_timeout = "30s"              # cancel queries running longer than 30s
connection_max_lifetime = "10min"      # recycle connection every 10 minutes
timeout_action = "cancel"              # "cancel" (default) or "disconnect"
```

Or as flat-level defaults inherited by all connections:

```toml
statement_timeout = "30s"
connection_max_lifetime = "10min"
timeout_action = "cancel"
```

Accepted duration formats: bare integers (seconds), or with suffix: `500ms`, `30s`, `5min`, `1h`, `2d`.

### CLI

```sh
# Override statement timeout for a single CLI invocation
gaussdb cli --sql "SELECT pg_sleep(60)" --statement-timeout 5s

# Force disconnect on timeout (connection recycled on next call)
gaussdb cli --sql "..." --statement-timeout 30s --timeout-action disconnect

# Set connection max lifetime
gaussdb cli --sql "..." --connection-max-lifetime 10min
```

### MCP Tool Per-Call Override

`execute_query` and `get_execution_plan` accept an optional `timeout_ms` parameter that overrides the connection-level default for a single call:

```json
{
  "sql": "SELECT count(*) FROM huge_table",
  "timeout_ms": 5000
}
```

If `timeout_ms` is omitted, the connection's global `statement_timeout` is used.

### How It Works

| Setting | Behavior |
|---------|----------|
| `statement_timeout` | Applied server-side via PostgreSQL/openGauss `SET statement_timeout` GUC. On timeout, the server returns SQLSTATE `57014` (`query_canceled`). **Default: 600s** when not configured. |
| `timeout_action = "cancel"` (default) | The connection is kept; the next tool call reuses it. |
| `timeout_action = "disconnect"` | On timeout, the connection is forcibly recycled — the next tool call establishes a fresh connection. |
| `connection_max_lifetime` | Connections are recycled after this duration regardless of timeouts, preventing state drift in long-running MCP sessions. Must be ≥ `statement_timeout` (validated at startup). |

### Validation

If both `statement_timeout` and `connection_max_lifetime` are set, `statement_timeout` must not exceed `connection_max_lifetime`. The server fails fast at startup with a clear error message if this constraint is violated.

## TLS Support

`sslmode=` parameter in connection URLs or config files:

```sh
# Disable TLS (default)
GAUSSDB_URL="host=127.0.0.1 user=gaussdb dbname=postgres sslmode=disable"

# Require TLS, skip certificate verification
GAUSSDB_URL="host=127.0.0.1 user=gaussdb dbname=postgres sslmode=require"

# Require TLS with full certificate verification
GAUSSDB_URL="host=db.example.com user=gaussdb dbname=postgres sslmode=verify-full"
```

TLS auto-detection via `check` subcommand tests all three modes.

## Authentication

Supports openGauss-specific authentication methods in addition to standard PostgreSQL auth:

| Method | Description |
|--------|-------------|
| SHA256 Password | openGauss RFC 5802-based SHA256 authentication |
| MD5 + SHA256 | Combined MD5/SHA256 authentication |
| SM3 Password | Chinese national standard (SM3) |
| SCRAM-SHA-256 | Standard SCRAM authentication |
| MD5 Password | Legacy MD5 authentication |
| Cleartext Password | Plaintext (use with TLS) |

## Logging

Logs are written to `$XDG_DATA_HOME/gaussdb/gaussdb.log` (or `~/.local/share/gaussdb/` on Linux, `~/Library/Application Support/gaussdb/` on macOS), rotated daily. This avoids interference with the stdio-based MCP transport.

Set `RUST_LOG` for log level control:

```sh
RUST_LOG=gaussdb_mcp=debug gaussdb
```

## Project Structure

This repository is a Rust workspace containing:

- **`crates/gaussdb`** — Unified public entry point for external consumers. `gaussdb::Client` is async by default (re-exports tokio-opengauss), `gaussdb::sync::Client` is available via the `sync` feature, and config-aware connections (`gaussdb::config::connect_async` and `gaussdb::config::connect_sync`) are available via the `config` feature. TLS and types are exposed through `gaussdb::native_tls`, `gaussdb::openssl`, and `gaussdb::types`. Low-level driver building blocks live under `gaussdb::driver`.
- **`tools/gaussdb-mcp`** — The MCP server + CLI tool (this README's primary focus)
- **`crates/tokio-opengauss`** — Async openGauss/PostgreSQL client (internal building block, `publish = false`)
- **`crates/opengauss`** — Synchronous openGauss/PostgreSQL client (internal building block, `publish = false`)
- **`crates/opengauss-derive`** — Proc macros for type derivation (internal building block, `publish = false`)
- **`crates/opengauss-protocol`** — Wire protocol v3.0+ implementation (internal building block, `publish = false`)
- **`crates/opengauss-types`** — Type system and serialization (internal building block, `publish = false`)
- **`crates/opengauss-native-tls`** — Native TLS connector for tokio-opengauss (internal building block, `publish = false`)
- **`crates/opengauss-openssl`** — OpenSSL TLS connector for tokio-opengauss (internal building block, `publish = false`)
- **`tools/codegen`** — Code generation from PostgreSQL catalog data

See [docs/DeveloperGuide.md](docs/DeveloperGuide.md) for library usage and [CONTRIBUTION.md](CONTRIBUTION.md) for contributing.

## Documentation

| Document | English | 中文 |
|----------|---------|------|
| User Guide | [docs/UserGuide.md](docs/UserGuide.md) | [docs/UserGuide_zh.md](docs/UserGuide_zh.md) |
| Developer Guide | [docs/DeveloperGuide.md](docs/DeveloperGuide.md) | [docs/DeveloperGuide_zh.md](docs/DeveloperGuide_zh.md) |
| Contribution Guide | [CONTRIBUTION.md](CONTRIBUTION.md) | [CONTRIBUTION_zh.md](CONTRIBUTION_zh.md) |
| README | [README.md](README.md) | [README_zh.md](README_zh.md) |

## License

Licensed under either of [Apache License, Version 2.0](license/LICENSE-APACHE) or [MIT license](license/LICENSE-MIT) at your option.
