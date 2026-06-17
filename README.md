# gaussdb-mcp

A standalone MCP (Model Context Protocol) server for [openGauss](https://opengauss.org/) database introspection, plus a built-in CLI for direct SQL execution. Designed for use with AI assistants like Claude, Cursor, and other MCP-compatible tools.

Built on the openGauss/PostgreSQL wire protocol (v3.0+) with **zero FFI dependencies** — no libpq, no C libraries.

## Features

- **MCP Server** — 6 tools for database introspection via MCP protocol over stdio
- **CLI Mode** — Execute SQL directly from terminal with `--sql`, `--file`, or stdin
- **Multi-connection support** — Configure multiple named databases, switch per tool call
- **OS keychain passwords** — Secure password storage via macOS Keychain / Windows Credential Manager / Linux Secret Service
- **Auto password migration** — Plaintext passwords automatically migrate to OS keychain on first connection
- **TLS support** — Automatic detection with `sslmode` (disable / require / verify-full)
- **Connection diagnostics** — `--check-connection` tests all TLS modes with verbose server info, TLS cert details, and GUC config
- **Rich error reporting** — SQLSTATE, SQLCODE, severity, detail, hint, schema, table, column context
- **File-based logging** — Daily rotating logs, no interference with stdio MCP transport
- **openGauss auth** — Supports SHA256, MD5+SHA256, SM3, SCRAM-SHA-256, and MD5 authentication

## Quick Start

### Install

```sh
# Build from source (Rust 1.85+ required)
cargo build -p gaussdb-mcp --release

# Binary available as both `gaussdb` and `gaussdb-mcp`
./target/release/gaussdb-mcp --help
```

### Connect via environment variable

```sh
export GAUSSDB_URL="host=127.0.0.1 user=gaussdb password=secret dbname=postgres"
gaussdb-mcp
```

### Use a config file

```sh
cat > ~/.gaussdb-mcp.toml << 'EOF'
host = "127.0.0.1"
port = 5432
user = "gaussdb"
password = "secret"
dbname = "postgres"
EOF
gaussdb-mcp
```

### Quick CLI query

```sh
# Execute a SELECT
gaussdb-mcp cli --sql "SELECT version()"

# Execute from file
gaussdb-mcp cli --file query.sql

# Pipe SQL via stdin
echo "SELECT count(*) FROM users" | gaussdb-mcp cli
```

## Usage Modes

### Mode 1: MCP Server (default)

```sh
gaussdb-mcp                    # default MCP mode, stdio transport
gaussdb-mcp mcp                # explicit MCP mode
gaussdb-mcp mcp --config ./prod.toml  # with custom config
```

Integrate with AI assistants (see [Integration](#integration-with-ai-assistants)).

### Mode 2: CLI Mode

```sh
gaussdb-mcp cli [OPTIONS]

OPTIONS:
    -s, --sql <SQL>         SQL statement to execute
    -f, --file <FILE>       Read SQL from file
        --config <PATH>     Path to config file
        --connection <NAME> Target connection name
        --format <FMT>      Output format: table, json, vertical [default: table]
```

**Examples:**

```sh
# Table output (default)
gaussdb-mcp cli --sql "SELECT * FROM users LIMIT 5"

# JSON output
gaussdb-mcp cli --sql "SELECT * FROM users LIMIT 5" --format json

# Vertical display (like \x in psql)
gaussdb-mcp cli --sql "SELECT * FROM users LIMIT 5" --format vertical

# DML/DDL supported
gaussdb-mcp cli --sql "INSERT INTO logs VALUES (1, 'hello')"
gaussdb-mcp cli --sql "CREATE TABLE test (id int)"

# Target a specific connection
gaussdb-mcp cli --connection prod --sql "SELECT count(*) FROM orders"
```

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

[[connections]]
name = "dev"
host = "127.0.0.1"
port = 5432
user = "gaussdb"
password = "secret"
dbname = "devdb"

[[connections]]
name = "prod"
host = "192.168.1.10"
port = 5432
user = "admin"
password = "keyring"         # stored in OS keychain
dbname = "production"

[[connections]]
name = "staging"
url = "host=10.0.0.5 user=admin password=keyring dbname=staging sslmode=require"
```

When `[[connections]]` is present, flat fields (`host`, `user`, etc.) are ignored. When absent, they wrap into a single `"default"` connection — fully backward compatible.

`default_connection` specifies the fallback when tools don't provide a `connection_name`. Defaults to the first connection.

Each connection's password can be:
- Plaintext string — migrated to OS keychain on first successful connection
- `"keyring"` — read from OS keychain (use `--store-password` to set)

## CLI Options

```
gaussdb-mcp [OPTIONS] [COMMAND]

COMMANDS:
    mcp     Run as MCP server (default)
    cli     Execute SQL from command line

MCP OPTIONS:
    --config <PATH>           Path to config file (default: ~/.gaussdb-mcp.toml)
    --check-connection [NAME] Test database connectivity and exit
    -v, --verbose             Show detailed connection info
    --store-password <PASS>   Store password in OS keychain
    --name <NAME>             Target connection name (for --store-password)

CLI OPTIONS:
    -s, --sql <SQL>            SQL statement to execute
    -f, --file <FILE>          Read SQL from file (or pipe to stdin)
    --config <PATH>            Path to config file
    --connection <NAME>        Target connection name
    --format <FMT>             Output format: table, json, vertical [default: table]
```

### Connection Diagnostics

```sh
# Check the default connection (3-pass: NoTls → TLS-skip → TLS-verify)
gaussdb-mcp --check-connection

# Check a specific named connection
gaussdb-mcp --check-connection prod --config ~/.gaussdb-mcp.toml

# Verbose output (version, GUC params, TLS cert details, timing)
gaussdb-mcp --check-connection --verbose
```

The diagnostic tool:
1. Attempts plain TCP (NoTls)
2. Attempts TLS with certificate verification skipped
3. Attempts TLS with full certificate verification
4. Reports keychain status (accessible, empty, or unavailable)
5. In verbose mode: server version, protocol version, GUC config, TLS cert chain, and timing

### Password Management

```sh
# Store password for the first/default connection
gaussdb-mcp --store-password 'MyP@ss123' --config ~/.gaussdb-mcp.toml

# Store password for a named connection
gaussdb-mcp --store-password 'Pr0dP@ss' --name prod --config ~/.gaussdb-mcp.toml

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

### Claude Desktop

Add to `claude_desktop_config.json`:

```json
{
  "mcpServers": {
    "gaussdb": {
      "command": "/path/to/gaussdb-mcp",
      "env": {
        "GAUSSDB_URL": "host=127.0.0.1 user=gaussdb password=secret dbname=postgres"
      }
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
      "command": "/path/to/gaussdb-mcp",
      "env": {
        "GAUSSDB_URL": "host=127.0.0.1 user=gaussdb password=secret dbname=postgres"
      }
    }
  }
}
```

### Multi-connection setup

```json
{
  "mcpServers": {
    "gaussdb": {
      "command": "/path/to/gaussdb-mcp",
      "args": ["mcp", "--config", "/path/to/gaussdb-mcp.toml"]
    }
  }
}
```

## Statement Timeout & Connection Lifetime

`gaussdb-mcp` supports configurable SQL execution timeouts to prevent runaway queries from blocking AI assistant sessions. Applies to both MCP server and CLI.

### Configuration (TOML)

Per-connection timeout settings in the config file:

```toml
[[connections]]
name = "prod"
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
gaussdb-mcp cli --sql "SELECT pg_sleep(60)" --statement-timeout 5s

# Force disconnect on timeout (connection recycled on next call)
gaussdb-mcp cli --sql "..." --statement-timeout 30s --timeout-action disconnect

# Set connection max lifetime
gaussdb-mcp cli --sql "..." --connection-max-lifetime 10min
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
| `statement_timeout` | Applied server-side via PostgreSQL/openGauss `SET statement_timeout` GUC. On timeout, the server returns SQLSTATE `57014` (`query_canceled`). |
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

TLS auto-detection via `--check-connection` tests all three modes.

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

Logs are written to `$XDG_DATA_HOME/gaussdb-mcp/gaussdb-mcp.log` (or `~/.local/share/gaussdb-mcp/` on Linux, `~/Library/Application Support/gaussdb-mcp/` on macOS), rotated daily. This avoids interference with the stdio-based MCP transport.

Set `RUST_LOG` for log level control:

```sh
RUST_LOG=gaussdb_mcp=debug gaussdb-mcp
```

## Project Structure

This repository is a Rust workspace containing:

- **`tools/gaussdb-mcp`** — The MCP server + CLI tool (this README's primary focus)
- **`crates/tokio-opengauss`** — Async openGauss/PostgreSQL client
- **`crates/opengauss`** — Synchronous openGauss/PostgreSQL client
- **`crates/opengauss-derive`** — Proc macros for type derivation
- **`crates/opengauss-protocol`** — Wire protocol v3.0+ implementation
- **`crates/opengauss-types`** — Type system and serialization
- **`crates/opengauss-native-tls`** — Native TLS connector for tokio-opengauss
- **`crates/opengauss-openssl`** — OpenSSL TLS connector for tokio-opengauss
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
