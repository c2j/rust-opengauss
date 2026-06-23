# GaussDB-MCP User Guide

Comprehensive guide for end users of `gaussdb-mcp` — the MCP server and CLI tool for openGauss/PostgreSQL databases.

## Table of Contents

- [Installation](#installation)
- [Configuration](#configuration)
- [MCP Mode (AI Assistant Integration)](#mcp-mode-ai-assistant-integration)
- [CLI Mode (Direct SQL Execution)](#cli-mode-direct-sql-execution)
- [Connection Diagnostics](#connection-diagnostics)
- [Password Management](#password-management)
- [TLS / SSL Configuration](#tls--ssl-configuration)
- [Troubleshooting](#troubleshooting)
- [FAQ](#faq)

---

## Installation

### Prerequisites

- **Rust** 1.85+ ([rustup](https://rustup.rs/))
- An openGauss or PostgreSQL server (version 9.5+)

### Build from Source

```sh
git clone https://github.com/c2j/rust-opengauss.git
cd rust-opengauss
cargo build -p gaussdb-mcp --release
```

The compiled binary is at `target/release/gaussdb-mcp` (also available as `gaussdb`).

### Verify Installation

```sh
gaussdb-mcp --help
```

---

## Configuration

`gaussdb-mcp` supports three configuration methods (in priority order):

### 1. Environment Variable

```sh
export GAUSSDB_URL="host=127.0.0.1 user=myuser password=mypass dbname=mydb"
export DATABASE_URL="host=127.0.0.1 user=myuser password=mypass dbname=mydb"  # also works
```

Both `GAUSSDB_URL` and `DATABASE_URL` are accepted. `GAUSSDB_URL` takes precedence.

### 2. Config File

Default location: `~/.gaussdb-mcp.toml`

**Single connection:**

```toml
host = "127.0.0.1"
port = 5432
user = "myuser"
password = "mypass"
dbname = "mydb"
```

**Multiple connections:**

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
password = "keyring"       # stored in OS keychain
dbname = "proddb"
sslmode = "require"
```

### 3. Custom Config Path

```sh
gaussdb-mcp mcp --config /path/to/config.toml
gaussdb-mcp cli --config /path/to/config.toml --sql "SELECT 1"
```

### Connection URL Format

Connections can be specified as a URL string or as individual fields:

```toml
# URL format (all-in-one)
url = "host=10.0.0.5 port=5432 user=admin password=secret dbname=mydb sslmode=require"

# Field format (individual)
host = "10.0.0.5"
port = 5432
user = "admin"
password = "secret"
dbname = "mydb"
sslmode = "require"
```

Both formats are equivalent. URL format takes precedence when both are present.

---

## MCP Mode (AI Assistant Integration)

### Overview

MCP mode allows AI assistants (Claude, Cursor, etc.) to query your openGauss/PostgreSQL database through a standardized protocol. The server runs over stdio — no network port needed.

### Starting the Server

```sh
# Start MCP server (default mode)
gaussdb-mcp

# Explicit MCP mode
gaussdb-mcp mcp

# With custom config
gaussdb-mcp mcp --config ~/my-gaussdb-config.toml
```

The server starts and waits for MCP client connections via stdin/stdout.

### Available MCP Tools

| Tool | What It Does | Example AI Prompt |
|------|-------------|-------------------|
| `list_connections` | Show all configured databases and their status | "What databases are connected?" |
| `get_database_info` | Get server version, encoding, user | "Show me the database server info" |
| `list_tables` | List all tables and views | "What tables are in this database?" |
| `get_table_metadata` | Get columns, PKs, indexes for a table | "Show me the schema of the users table" |
| `execute_query` | Run SELECT or EXPLAIN queries | "How many active users do we have?" |
| `get_execution_plan` | Get query execution plan | "Explain the query plan for this slow query" |

### Integration Examples

#### Claude Desktop

1. Open Claude Desktop settings
2. Navigate to the "Developer" section
3. Add this to your `claude_desktop_config.json`:

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

4. Restart Claude Desktop
5. Look for the gaussdb tools in the MCP tools list

#### Cursor

Add to `.cursor/mcp.json` in your project root:

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

### Multi-connection Usage

When you have multiple connections configured, AI assistants can switch between them:

```
User: "Show me the schema of the orders table in production"
AI: [calls get_table_metadata with connection_name="production"]

User: "Compare that with the development schema"
AI: [calls get_table_metadata with connection_name="development"]
```

The `list_connections` tool helps the AI discover available databases.

### Error Responses

When queries fail, the MCP server returns rich error context:

```json
{
  "code": -32000,
  "message": "[SQLSTATE 42P01 | SQLCODE -204] execute_query failed: relation \"nonexistent\" does not exist",
  "data": {
    "sqlstate": "42P01",
    "sqlcode": -204,
    "severity": "ERROR",
    "message": "relation \"nonexistent\" does not exist",
    "sql": "SELECT * FROM nonexistent"
  }
}
```

This helps AI assistants understand and explain database errors.

---

## CLI Mode (Direct SQL Execution)

### Overview

CLI mode provides a `psql`-like interface for executing SQL directly from the terminal. It supports SELECT, DML (INSERT/UPDATE/DELETE), and DDL (CREATE/ALTER/DROP) statements.

### Basic Usage

```sh
# Execute a single query
gaussdb-mcp cli --sql "SELECT version()"

# Execute from a file
gaussdb-mcp cli --file ./queries/report.sql

# Pipe SQL via stdin
cat query.sql | gaussdb-mcp cli
echo "SELECT * FROM users LIMIT 10" | gaussdb-mcp cli
```

### Output Formats

#### Table (default)

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

#### Vertical

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

### Running DML/DDL

Unlike MCP mode (read-only), CLI mode supports write operations:

```sh
# INSERT
gaussdb-mcp cli --sql "INSERT INTO logs (message) VALUES ('server started')"

# UPDATE
gaussdb-mcp cli --sql "UPDATE users SET active = true WHERE id = 1"

# DELETE
gaussdb-mcp cli --sql "DELETE FROM sessions WHERE created_at < now() - interval '30 days'"

# DDL
gaussdb-mcp cli --sql "CREATE TABLE metrics (id serial PRIMARY KEY, value float)"
gaussdb-mcp cli --sql "ALTER TABLE users ADD COLUMN last_login timestamptz"
```

For non-SELECT statements, the output shows the number of affected rows.

### Targeting Specific Connections

```sh
# Use a specific named connection
gaussdb-mcp cli --connection production --sql "SELECT count(*) FROM orders"

# Use a custom config file with named connections
gaussdb-mcp cli --config ./prod-config.toml --connection staging --sql "SELECT version()"
```

### Scripting

CLI mode is ideal for shell scripts and automation:

```sh
#!/bin/bash
# backup-verify.sh: Verify database backup

ROW_COUNT=$(gaussdb-mcp cli --connection prod --format json \
    --sql "SELECT count(*) FROM important_table" | jq '.rows[0][0]')

if [ "$ROW_COUNT" -gt 0 ]; then
    echo "Backup verified: $ROW_COUNT rows in important_table"
else
    echo "ERROR: Table appears empty!" >&2
    exit 1
fi
```

---

## Connection Diagnostics

The `--check-connection` flag performs a comprehensive connectivity test:

```sh
gaussdb-mcp --check-connection
gaussdb-mcp --check-connection --verbose
gaussdb-mcp --check-connection production --config ~/prod-config.toml
```

### What It Tests

1. **Keychain status** — Is the OS keychain accessible? Is the password stored?
2. **NoTLS (plain TCP)** — Can you connect without encryption?
3. **TLS with skip-verify** — Can you connect with encryption but relaxed cert validation?
4. **TLS with full-verify** — Can you connect with strict certificate verification?

### Sample Output

```
Connection: development

[Keyring] Password read from OS keychain (user: dev@127.0.0.1/devdb)
  ✓ Keyring accessible, password retrieved (5 chars)

[1/3] Trying NoTls (plain TCP) → host=127.0.0.1 user=dev password=**** dbname=devdb ...
  ✓ Connected
    PostgreSQL 17.0 on x86_64-pc-linux-gnu, compiled by gcc...

[2/3] Trying TLS (skip cert verify) → host=127.0.0.1 user=dev ... sslmode=require ...
  ✗ TLS handshake failed: server does not support TLS

[3/3] Trying TLS (verify cert) → host=127.0.0.1 user=dev ... sslmode=require ...
  ✗ TLS handshake failed: server does not support TLS

═══════════════════════════════════════════════════════════
  Connection Diagnostic Summary
═══════════════════════════════════════════════════════════
  NoTls                ✓  PostgreSQL 17.0... (15ms)
  TLS (no verify)      ✗  server does not support TLS
  TLS (verify)         ✗  server does not support TLS

  Database Version:
    PostgreSQL 17.0 on x86_64-pc-linux-gnu...

Recommendation: use NoTls mode.
```

### Verbose Mode (`-v`)

Adds detailed server information:
- Server version and version number
- Protocol version
- Current user and database
- Server address and port
- Start time
- Recovery status (primary or standby)
- TLS session details (version, cipher)
- Server configuration (max_connections, shared_buffers, work_mem, timezone, data_directory)
- TLS certificate details (subject, issuer, serial, validity period)
- Connection timing

---

## Password Management

### Storing Passwords

```sh
# Store password for first connection in config (interactive prompt)
gaussdb-mcp store-password --config ~/.gaussdb-mcp.toml

# Store password for a named connection
gaussdb-mcp store-password --name production --config ~/.gaussdb-mcp.toml

# Non-interactive (read from stdin pipe, e.g. CI/scripts):
#   printf '%s\n' "$PW" | gaussdb-mcp store-password --name production --config ~/.gaussdb-mcp.toml
```

### How It Works

1. Passwords are stored in the **OS-native keychain**:
   - **macOS**: Keychain Access
   - **Windows**: Credential Manager
   - **Linux**: Secret Service (GNOME Keyring / KDE Wallet)
2. Config files reference passwords with `password = "keyring"` (a sentinel value)
3. On connection, the password is read from the keychain

### Auto-Migration

If you start with a plaintext password in your config:

```toml
password = "my-plaintext-password"
```

On the **first successful MCP connection**, gaussdb-mcp:
1. Stores the password in the OS keychain
2. Updates the config file to `password = "keyring"`
3. Continues using the connection normally

This is transparent — no manual steps needed.

### Security Notes

- Config files **never** contain passwords after migration (only `"keyring"` sentinel)
- Passwords are only in memory during active connections
- Environment variable passwords (`GAUSSDB_URL`) are never written to the keychain or config
- Connection URLs in logs and error messages have passwords redacted (`****`)

---

## TLS / SSL Configuration

### SSL Modes

| sslmode | Behavior |
|---------|----------|
| `disable` | No encryption (default) |
| `require` | Require TLS, skip certificate verification |
| `verify-ca` | Require TLS, verify CA |
| `verify-full` | Require TLS, verify hostname and CA |

### Configuration Examples

```toml
# In config file
[[connections]]
name = "secure-db"
host = "db.example.com"
user = "admin"
password = "keyring"
dbname = "secure"
sslmode = "verify-full"
```

```sh
# Via environment variable
export GAUSSDB_URL="host=db.example.com user=admin password=secret dbname=secure sslmode=verify-full"
```

### Connection Detection

Use `--check-connection` to automatically determine the correct TLS mode for your server:

```sh
gaussdb-mcp --check-connection
```

The tool tests all three modes and recommends which to use.

---

## Troubleshooting

### "No connection configuration found"

**Cause:** No config file, no environment variable.

**Solution** (choose one):
```sh
# Option 1: Set environment variable
export GAUSSDB_URL="host=localhost user=postgres password=secret dbname=postgres"

# Option 2: Create config file
cat > ~/.gaussdb-mcp.toml << 'EOF'
host = "localhost"
user = "postgres"
password = "secret"
dbname = "postgres"
EOF

# Option 3: Pass explicit config
gaussdb-mcp --config /path/to/config.toml
```

### "Database connection failed"

**Common causes:**
- Database server is not running
- Wrong host, port, user, or database name
- Network/firewall blocking access
- `pg_hba.conf` doesn't allow this client

**Diagnostic steps:**
```sh
# Run connection diagnostics
gaussdb-mcp --check-connection --verbose

# Verify server is reachable
psql -h localhost -U postgres -d postgres -c "SELECT 1"

# Check pg_hba.conf on the server for your IP/user
```

### "Keyring password not found"

**Cause:** Config has `password = "keyring"` but no password is stored in the keychain.

**Solution:**
```sh
# Store the password (interactive prompt)
gaussdb-mcp store-password --config ~/.gaussdb-mcp.toml

# Or temporarily use plaintext (will auto-migrate)
# Edit config: change password = "keyring" to password = "YourPassword"
```

### "Only SELECT and EXPLAIN queries are allowed" (MCP mode)

**Cause:** MCP tools only support read-only queries. This is a security feature.

**Solution:** Use CLI mode for DML/DDL:
```sh
gaussdb-mcp cli --sql "INSERT INTO ..."
```

### Log file location

Logs are written to:
- **Linux**: `~/.local/share/gaussdb-mcp/gaussdb-mcp.log`
- **macOS**: `~/Library/Application Support/gaussdb-mcp/gaussdb-mcp.log`

Enable debug logging:
```sh
RUST_LOG=gaussdb_mcp=debug gaussdb-mcp
```

---

## FAQ

### What databases are supported?

openGauss and PostgreSQL 9.5+. The tool uses the standard PostgreSQL wire protocol (v3.0+).

### Can I use this without AI assistants?

Yes! Use CLI mode (`gaussdb-mcp cli`) for direct SQL execution from terminal or scripts.

### How many connections can I configure?

No hard limit. Each `[[connections]]` entry in the config file creates one connection.

### Is my password secure?

- Config files use `"keyring"` sentinel (not plaintext) after migration
- Passwords are stored in OS-native encrypted storage
- URLs in logs/errors have passwords redacted
- Recommended: use keychain + TLS for production

### Can I use this with Docker?

Yes. See `docker-compose.yml` in the repository for a PostgreSQL test environment:
```sh
docker compose up -d
gaussdb-mcp cli --sql "SELECT version()"
```

### How does this differ from psql?

- `psql` is a full-featured interactive terminal
- `gaussdb-mcp cli` is a non-interactive SQL runner suitable for scripting
- `gaussdb-mcp mcp` is an MCP server for AI assistant integration
- Both allow you to work with openGauss without installing libpq
