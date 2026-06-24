# Contribution Guide

Thank you for your interest in contributing to `rust-opengauss`! This document covers everything you need to know to contribute effectively.

## Table of Contents

- [Code of Conduct](#code-of-conduct)
- [Development Setup](#development-setup)
- [Project Structure](#project-structure)
- [Building and Testing](#building-and-testing)
- [Code Style](#code-style)
- [Pull Request Process](#pull-request-process)
- [Commit Conventions](#commit-conventions)
- [Testing with Docker](#testing-with-docker)
- [Release Process](#release-process)

---

## Code of Conduct

Be respectful, constructive, and collaborative. Assume good faith in all interactions.

---

## Development Setup

### Prerequisites

- **Rust** 1.85+ (install via [rustup](https://rustup.rs/))
- **Docker** (optional, for integration testing with PostgreSQL)
- **Git**

### Clone and Setup

```sh
git clone https://github.com/c2j/rust-opengauss.git
cd rust-opengauss
```

### Install Rust Toolchain

```sh
rustup default stable
rustup component add rustfmt clippy
```

---

## Project Structure

```
rust-opengauss/
├── crates/
│   ├── gaussdb/                 # Public facade crate (re-exports async/sync/config clients)
│   │   └── src/
│   │       └── lib.rs          # Async API at root; sync API behind `sync` feature; config API behind `config` feature
│   │
│   ├── opengauss/              # Synchronous client library
│   │   └── src/
│   │       ├── client.rs       # Sync client API
│   │       ├── connection.rs   # Sync connection management
│   │       ├── transaction.rs  # Transaction support
│   │       ├── binary_copy.rs  # Binary COPY protocol
│   │       ├── copy_in_writer.rs
│   │       ├── copy_out_reader.rs
│   │       ├── row_iter.rs     # Row iterator
│   │       └── notifications.rs # LISTEN/NOTIFY support
│   │
│   ├── tokio-opengauss/         # Async client library (tokio-based)
│   │   └── src/
│   │       ├── client.rs       # Async client API
│   │       ├── connection.rs   # Async connection management
│   │       ├── connect.rs      # Connection establishment
│   │       ├── connect_socket.rs # TCP socket connection
│   │       ├── connect_tls.rs  # TLS connection
│   │       ├── query.rs        # Query execution
│   │       ├── prepare.rs      # Prepared statements
│   │       ├── statement.rs    # Statement types
│   │       ├── portal.rs       # Portal (cursor) support
│   │       ├── transaction.rs  # Transaction support
│   │       ├── copy_in.rs      # COPY FROM
│   │       ├── copy_out.rs     # COPY TO
│   │       ├── cancel_query.rs # Query cancellation
│   │       ├── tls.rs          # TLS abstractions
│   │       ├── types.rs        # Type mapping
│   │       ├── socket.rs       # Socket abstraction
│   │       ├── keepalive.rs    # TCP keepalive
│   │       └── error/          # Error types and SQLSTATE mapping
│   │
│   ├── opengauss-derive/        # Proc-macro crate for #[derive(ToSql, FromSql)]
│   │   └── src/
│   │       └── lib.rs
│   │
│   ├── opengauss-protocol/      # Wire protocol v3.0+ implementation
│   │   └── src/
│   │       ├── message/        # Protocol message types (Backend, Frontend)
│   │       └── authentication/ # Auth message handling (MD5, SCRAM, SHA256)
│   │
│   ├── opengauss-types/         # Type system: ToSql / FromSql traits, type mapping
│   │   └── src/
│   │       ├── lib.rs
│   │       └── type_gen.rs     # Generated type definitions
│   │
│   ├── opengauss-native-tls/    # native-tls connector for tokio-opengauss
│   │   └── src/
│   │       └── lib.rs
│   │
│   └── opengauss-openssl/       # openssl connector for tokio-opengauss
│       └── src/
│           └── lib.rs
│
├── tools/
│   ├── gaussdb-mcp/             # The MCP server + CLI tool
│   │   └── src/
│   │       ├── main.rs         # Entry point, logging, diagnostics, CLI/MCP dispatch
│   │       ├── server.rs       # MCP tool implementations, SQLSTATE mapping, error handling
│   │       ├── config.rs       # Config parsing, keychain, URL resolution
│   │       ├── cli.rs          # CLI mode implementation (SQL executor)
│   │       ├── connection.rs   # TLS detection, connection establishment
│   │       ├── output.rs       # Output formatting (table, JSON)
│   │       └── queries.rs      # SQL query templates for introspection
│   │
│   └── codegen/                 # Code generation tools
│       └── src/
│           └── main.rs
│
├── tests/                       # Integration tests
│   └── opengauss-derive-test/
│
├── docker/
│   ├── docker-compose.yml      # PostgreSQL test environment
│   └── sql_setup.sh            # Test user/extension setup
│
├── lib/
│   └── opengauss/              # Legacy (pre-workspace) directory
│
├── license/                    # License files
│   ├── LICENSE-APACHE
│   └── LICENSE-MIT
│
├── Cargo.toml                  # Workspace definition
├── Cross.toml                  # Cross-compilation config
└── rustfmt.toml                # Formatting configuration
```

`gaussdb` is the only public library crate. All other crates under `crates/` are `publish = false` internal workspace members.

### Crate Dependency Graph

```
external consumers
  └─ gaussdb (public facade)
      ├─ tokio-opengauss (async client, publish = false internal)
      │   └─ opengauss-types (type system, publish = false internal)
      │       └─ opengauss-protocol (wire protocol, publish = false internal)
      ├─ opengauss (sync client, publish = false internal)
      │   └─ tokio-opengauss (shared via internal wrapper)
      ├─ opengauss-native-tls (native-tls connector, publish = false internal)
      │   └─ native-tls
      └─ opengauss-openssl (openssl connector, publish = false internal)
          └─ openssl

gaussdb-mcp (tool)
  └─ gaussdb (public facade)

opengauss-derive (proc macros, publish = false internal)
  └─ standalone, re-exports ToSql/FromSql derives

codegen (dev tool)
  └─ standalone, generates type mapping code
```

---

## Building and Testing

### Build

```sh
# Build everything
cargo build

# Build only the MCP tool
cargo build -p gaussdb-mcp

# Release build
cargo build --release
```

### Run Tests

```sh
# Run all tests (requires a running PostgreSQL instance)
cargo test

# Run specific crate tests
cargo test -p tokio-opengauss
cargo test -p opengauss

# Run tests with output
cargo test -- --nocapture
```

### Lint and Format

```sh
# Check formatting
cargo fmt --check

# Apply formatting
cargo fmt

# Run clippy
cargo clippy -- -D warnings

# Clippy for specific crate
cargo clippy -p gaussdb-mcp -- -D warnings
```

### Build Verification

Before submitting a PR, ensure:

```sh
cargo fmt --check         # No formatting issues
cargo clippy -- -D warnings  # No clippy warnings
cargo build --release     # Clean release build
cargo test               # All tests pass
```

---

## Code Style

### Rust

We follow standard Rust conventions enforced by `rustfmt` and `clippy`.

**rustfmt.toml:**
```toml
edition = "2024"
```

### Key Conventions

1. **Error handling**: Use structured error types. The MCP tool maps SQLSTATEs to SQLCODEs for rich error context.
2. **Async**: The MCP tool and `tokio-opengauss` are async (tokio); the MCP tool reaches `tokio-opengauss` through the `gaussdb` facade. The sync `opengauss` crate wraps the async client.
3. **Configuration**: Use serde + TOML for config. Sensitive values (passwords) go through the OS keychain.
4. **Logging**: Use `tracing` crate. Logs go to files, not stderr, to avoid MCP stdio interference.
5. **No `unsafe`** without strong justification and thorough review.

### MCP Tool Conventions

- Tool implementations in `server.rs` use the `#[tool]` macro from `rmcp`
- All tools return structured JSON via `CallToolResult::success()`
- Errors return `McpError::internal_error()` with detailed JSON `data` payloads
- SQL queries are parameterized via `gaussdb::Client::query(sql, &[])` for external consumers; internal crates still use `tokio_opengauss::Client::query(sql, &[])`
- Connection state management uses `Arc<Mutex<HashMap<String, ConnectionState>>>`

### Library Conventions

- Public API is exposed through `gaussdb`, which re-exports the tokio-opengauss/opengauss convention from the original `tokio-postgres` crate
- Type system uses `ToSql` and `FromSql` traits
- Feature flags control optional type support (chrono, uuid, serde_json, etc.)

---

## Pull Request Process

### Before You Start

1. **Search existing issues** — Check if your idea/issue is already being discussed
2. **Open an issue first** for major features or changes to get early feedback
3. **Discuss the approach** before investing significant time

### Creating a PR

1. Fork the repository
2. Create a feature branch: `git checkout -b feat/my-feature`
3. Make your changes following the code style above
4. Add tests for new functionality
5. Ensure all checks pass: `cargo fmt --check && cargo clippy && cargo build && cargo test`
6. Push and create a Pull Request

### PR Checklist

- [ ] Code follows `rustfmt` formatting (`cargo fmt --check`)
- [ ] No clippy warnings (`cargo clippy -- -D warnings`)
- [ ] Builds cleanly (`cargo build --release`)
- [ ] Tests pass (`cargo test`)
- [ ] New features include tests
- [ ] Documentation is updated (README, relevant docs/)
- [ ] Commit messages follow conventions (below)

### PR Review

- All PRs require at least one review before merging
- CI must pass (build, tests, clippy, fmt)
- Keep PRs focused — one feature/fix per PR
- Respond to review feedback promptly

---

## Commit Conventions

We use conventional commits with these prefixes:

| Prefix | Use For |
|--------|---------|
| `feat:` | New features |
| `fix:` | Bug fixes |
| `chore:` | Maintenance, version bumps, deps |
| `docs:` | Documentation changes |
| `style:` | Formatting, whitespace, semicolons |
| `refactor:` | Code restructuring without behavior changes |
| `test:` | Adding or modifying tests |
| `perf:` | Performance improvements |

**Scope** (optional but encouraged for multi-crate changes):

```
feat(gaussdb-mcp): add cli subcommand for SQL execution
fix(protocol): handle variable-length BackendKeyData in PG18
chore(gaussdb-mcp): bump version to 0.2.3
```

---

## Testing with Docker

A Docker Compose setup is provided for PostgreSQL integration testing:

```sh
# Start PostgreSQL test instance
docker compose up -d

# The instance is configured with:
# - Port: 5433
# - Users: pass_user, md5_user, scram_user, ssl_user (all with password 'password')
# - TLS certificates (self-signed)
# - Extensions: hstore, citext, ltree
# - pg_hba.conf: auth methods for testing

# Run tests
cargo test

# Stop when done
docker compose down
```

### Test User Credentials

| User | Password | Auth Method |
|------|----------|-------------|
| `postgres` | `postgres` | trust |
| `pass_user` | `password` | password (cleartext) |
| `md5_user` | `password` | md5 |
| `scram_user` | `password` | scram-sha-256 |
| `ssl_user` | (none) | trust (SSL required) |

---

## Release Process

Releases are managed by the maintainer. The process:

1. Update version in `tools/gaussdb-mcp/Cargo.toml`
2. Update changelog (if exists)
3. Create a version bump commit: `chore(gaussdb-mcp): bump to x.y.z`
4. Tag: `git tag gaussdb-mcp-vx.y.z`
5. Push tags: `git push --tags`

Crate versions follow semver:
- `opengauss` / `tokio-opengauss`: follow upstream (tokio-postgres) versioning
- `gaussdb`: 0.x.y independent semver, coupled to tokio-opengauss (breaking changes in tokio-opengauss ⇒ breaking bump in gaussdb)
- `gaussdb-mcp`: independent semver (currently 0.5.0); tags use `gaussdb-mcp-v<version>`

---

## Getting Help

- **Issues**: Open a [GitHub issue](https://github.com/c2j/rust-opengauss/issues) for bugs or feature requests
- **Discussions**: Use [GitHub Discussions](https://github.com/c2j/rust-opengauss/discussions) for questions
- **Documentation**: Check [docs/](docs/) for detailed guides
