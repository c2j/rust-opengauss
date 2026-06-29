# GaussDB User Guide

Comprehensive guide for end users of `gaussdb` — the MCP server and CLI tool for openGauss/PostgreSQL databases.

## Table of Contents

- [Installation](#installation)
- [Configuration](#configuration)
- [MCP Mode (AI Assistant Integration)](#mcp-mode-ai-assistant-integration)
- [CLI Mode (Direct SQL Execution)](#cli-mode-direct-sql-execution)
  - [Interactive REPL](#interactive-repl)
- [Connection Diagnostics](#connection-diagnostics)
- [Password Management](#password-management)
- [TLS / SSL Configuration](#tls--ssl-configuration)
- [Troubleshooting](#troubleshooting)
- [FAQ](#faq)

---

## Installation

### Prerequisites

- An openGauss or PostgreSQL server (version 9.5+)
- If building from source: **Rust** 1.85+ ([rustup](https://rustup.rs/))

### Download Pre-built Binary (recommended)

1. Download the latest zip package for your platform from [GitHub Releases](https://github.com/c2j/rust-opengauss/releases)
2. Unzip:
   ```sh
   unzip gaussdb-*.zip
   ```
3. Place the binary in your PATH:
   ```sh
   # macOS / Linux
   sudo mv gaussdb /usr/local/bin/
   # Or to a user directory (no sudo required)
   mkdir -p ~/.local/bin && mv gaussdb ~/.local/bin/
   ```
4. Verify:
   ```sh
   gaussdb --help
   ```

### Build from Source

```sh
git clone https://github.com/c2j/rust-opengauss.git
cd rust-opengauss
cargo build -p gaussdb-mcp --release
```

The compiled binary is at `target/release/gaussdb`.

### Verify Installation

```sh
gaussdb --help
```

---

## Configuration

`gaussdb` supports three configuration methods (in priority order):

### 1. Environment Variable

```sh
export GAUSSDB_URL="host=127.0.0.1 user=gaussdb password=secret dbname=postgres"
export DATABASE_URL="host=127.0.0.1 user=gaussdb password=secret dbname=postgres"  # also works
```

Both `GAUSSDB_URL` and `DATABASE_URL` are accepted. `GAUSSDB_URL` takes precedence.

### 2. Config File

Default location: `~/.gaussdb.toml`

**Single connection:**

```toml
host = "127.0.0.1"
port = 5432
user = "gaussdb"
password = "secret"
dbname = "postgres"
```

**Multiple connections:**

```toml
default_connection = "development"

[connections.development]
host = "127.0.0.1"
port = 5432
user = "gaussdb"
password = "secret"
dbname = "postgres"

[connections.production]
host = "db-prod.example.com"
port = 5432
user = "admin"
password = "keyring"       # stored in OS keychain
dbname = "proddb"
sslmode = "require"
```

### 3. Custom Config Path

```sh
gaussdb mcp --config /path/to/config.toml
gaussdb cli --config /path/to/config.toml --sql "SELECT 1"
```

### Connection URL Format

Connections can be specified as a URL string or as individual fields:

```toml
# URL format (all-in-one)
url = "host=10.0.0.5 port=5432 user=admin password=secret dbname=postgres sslmode=require"

# Field format (individual)
host = "10.0.0.5"
port = 5432
user = "admin"
password = "secret"
dbname = "postgres"
sslmode = "require"
```

Both formats are equivalent. URL format takes precedence when both are present.

---

## MCP Mode (AI Assistant Integration)

### Overview

MCP mode allows AI assistants (OpenCode, OpenClaw, Lingma, Claude, Cursor, etc.) to query your openGauss/PostgreSQL database through a standardized protocol. The server runs over stdio — no network port needed.

### Starting the Server

```sh
# Start MCP server (default mode)
gaussdb

# Explicit MCP mode
gaussdb mcp

# With custom config
gaussdb mcp --config ~/my-gaussdb-config.toml
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

#### OpenCode

1. Open your OpenCode settings (`opencode.json`)
2. Add the following configuration:

```json
{
  "mcp": {
    "gaussdb": {
      "command": "/path/to/target/release/gaussdb",
      "args": ["mcp", "--config", "/path/to/gaussdb.toml"]
    }
  }
}
```

3. Restart OpenCode and find the gaussdb tools in the MCP tools list

#### OpenClaw

Add to `~/.openclaw/mcp.json`:

```json
{
  "mcpServers": {
    "gaussdb": {
      "command": "/path/to/target/release/gaussdb",
      "args": ["mcp", "--config", "/path/to/gaussdb.toml"]
    }
  }
}
```

#### Lingma (VSCode / IDEA)

Add to `.lingma/mcp.json` in your project root:

```json
{
  "mcpServers": {
    "gaussdb": {
      "command": "/path/to/target/release/gaussdb",
      "args": ["mcp", "--config", "/path/to/gaussdb.toml"]
    }
  }
}
```

#### Claude Desktop

1. Open Claude Desktop settings
2. Navigate to the "Developer" section
3. Add this to your `claude_desktop_config.json`:

```json
{
  "mcpServers": {
    "gaussdb": {
      "command": "/path/to/target/release/gaussdb",
      "args": ["mcp", "--config", "/path/to/gaussdb.toml"]
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
      "command": "/path/to/target/release/gaussdb",
      "args": ["mcp", "--config", "/path/to/gaussdb.toml"]
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
gaussdb cli --sql "SELECT version()"

# Execute from a file
gaussdb cli --file ./queries/report.sql

# Pipe SQL via stdin
cat query.sql | gaussdb cli
echo "SELECT * FROM users LIMIT 10" | gaussdb cli
```

### Output Formats

#### Table (default)

```sh
gaussdb cli --sql "SELECT id, name, email FROM users LIMIT 3"
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
gaussdb cli --sql "SELECT id, name FROM users LIMIT 2" --format json
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
gaussdb cli --sql "SELECT * FROM users LIMIT 1" --format vertical
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
gaussdb cli --sql "INSERT INTO logs (message) VALUES ('server started')"

# UPDATE
gaussdb cli --sql "UPDATE users SET active = true WHERE id = 1"

# DELETE
gaussdb cli --sql "DELETE FROM sessions WHERE created_at < now() - interval '30 days'"

# DDL
gaussdb cli --sql "CREATE TABLE metrics (id serial PRIMARY KEY, value float)"
gaussdb cli --sql "ALTER TABLE users ADD COLUMN last_login timestamptz"
```

For non-SELECT statements, the output shows the number of affected rows.

### Targeting Specific Connections

```sh
# Use a specific named connection
gaussdb cli --name production --sql "SELECT count(*) FROM orders"

# Use a custom config file with named connections
gaussdb cli --config ./prod-config.toml --name staging --sql "SELECT version()"
```

### Scripting

CLI mode is ideal for shell scripts and automation:

```sh
#!/bin/bash
# backup-verify.sh: Verify database backup

ROW_COUNT=$(gaussdb cli --name prod --format json \
    --sql "SELECT count(*) FROM important_table" | jq '.rows[0][0]')

if [ "$ROW_COUNT" -gt 0 ]; then
    echo "Backup verified: $ROW_COUNT rows in important_table"
else
    echo "ERROR: Table appears empty!" >&2
    exit 1
fi
```

---

### Interactive REPL

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
- Statements execute only on `;` outside quotes/comments (`'...'`, `"..."`, `--`, `/* ... */`)
- SQL errors print to stderr and the REPL continues
- If the server or a firewall drops an idle connection, the next statement fails and the REPL prints a hint to run `.connect`
- History deduplicates consecutive identical statements
- SQL history persists per connection name under the app data dir (`$XDG_DATA_HOME/gaussdb/history/<name>` on Linux, `~/Library/Application Support/gaussdb/history/<name>` on macOS); pass `--no-history` to opt out
- For very large exports, prefer one-shot `cli --format csv` over the REPL

---

## Connection Diagnostics

The `check` subcommand performs a comprehensive connectivity test (also available via `cli --check-connection`):

```sh
gaussdb check
gaussdb check --verbose
gaussdb check --name production --config ~/prod-config.toml
```

### What It Tests

1. **Keychain status** — Is the OS keychain accessible? Is the password stored?
2. **NoTLS (plain TCP)** — Can you connect without encryption?
3. **TLS with skip-verify** — Can you connect with encryption but relaxed cert validation?
4. **TLS with full-verify** — Can you connect with strict certificate verification?

### Sample Output

```
Connection: development

[Keyring] Password read from OS keychain (user: gaussdb@127.0.0.1/postgres)
  ✓ Keyring accessible, password retrieved (5 chars)

[1/3] Trying NoTls (plain TCP) → host=127.0.0.1 user=gaussdb password=**** dbname=postgres ...
  ✓ Connected
    (openGauss-lite 7.0.0-RC1 build 10d38387) compiled at 2025-03-21 18:40:52 commit 0 last mr  release

[2/3] Trying TLS (skip cert verify) → host=127.0.0.1 user=gaussdb ... sslmode=require ...
  ✗ TLS handshake failed: server does not support TLS

[3/3] Trying TLS (verify cert) → host=127.0.0.1 user=gaussdb ... sslmode=require ...
  ✗ TLS handshake failed: server does not support TLS

═══════════════════════════════════════════════════════════
  Connection Diagnostic Summary
═══════════════════════════════════════════════════════════
  NoTls                ✓  (openGauss-lite 7.0.0-RC1...) (15ms)
  TLS (no verify)      ✗  server does not support TLS
  TLS (verify)         ✗  server does not support TLS

  Database Version:
    (openGauss-lite 7.0.0-RC1 build 10d38387)...

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
gaussdb store-password --config ~/.gaussdb.toml

# Store password for a named connection
gaussdb store-password --name production --config ~/.gaussdb.toml

# Non-interactive (read from stdin pipe, e.g. CI/scripts):
#   printf '%s\n' "$PW" | gaussdb store-password --name production --config ~/.gaussdb.toml
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

On the **first successful MCP connection**, gaussdb:
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
[connections.secure-db]
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

Use `check` subcommand to automatically determine the correct TLS mode for your server:

```sh
gaussdb check
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
cat > ~/.gaussdb.toml << 'EOF'
host = "localhost"
user = "postgres"
password = "secret"
dbname = "postgres"
EOF

# Option 3: Pass explicit config
gaussdb --config /path/to/config.toml
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
gaussdb check --verbose

# Verify server is reachable
psql -h localhost -U postgres -d postgres -c "SELECT 1"

# Check pg_hba.conf on the server for your IP/user
```

### "Keyring password not found"

**Cause:** Config has `password = "keyring"` but no password is stored in the keychain.

**Solution:**
```sh
# Store the password (interactive prompt)
gaussdb store-password --config ~/.gaussdb.toml

# Or temporarily use plaintext (will auto-migrate)
# Edit config: change password = "keyring" to password = "YourPassword"
```

### "Only SELECT and EXPLAIN queries are allowed" (MCP mode)

**Cause:** MCP tools only support read-only queries. This is a security feature.

**Solution:** Use CLI mode for DML/DDL:
```sh
gaussdb cli --sql "INSERT INTO ..."
```

### Log file location

Logs are written to:
- **Linux**: `~/.local/share/gaussdb/gaussdb.log`
- **macOS**: `~/Library/Application Support/gaussdb/gaussdb.log`

Enable debug logging:
```sh
RUST_LOG=gaussdb_mcp=debug gaussdb
```

---

## FAQ

### What databases are supported?

openGauss and PostgreSQL 9.5+. The tool uses the standard PostgreSQL wire protocol (v3.0+).

### Can I use this without AI assistants?

Yes! Use CLI mode (`gaussdb cli`) for direct SQL execution from terminal or scripts.

### How many connections can I configure?

No hard limit. Each `[connections.<name>]` table in the config file creates one connection.

### Is my password secure?

- Config files use `"keyring"` sentinel (not plaintext) after migration
- Passwords are stored in OS-native encrypted storage
- URLs in logs/errors have passwords redacted
- Recommended: use keychain + TLS for production

### Can I use this with Docker?

Yes. See `docker-compose.yml` in the repository for a PostgreSQL test environment:
```sh
docker compose up -d
gaussdb cli --sql "SELECT version()"
```

### How does this differ from psql?

- `psql` is a full-featured interactive terminal
- `gaussdb cli` is an SQL runner with both interactive REPL (`cli -i`) and non-interactive scripting modes
- `gaussdb mcp` is an MCP server for AI assistant integration
- Both allow you to work with openGauss without installing libpq
