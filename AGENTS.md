# AGENTS.md

Pre-commit and CI requirements for this repository.

## Pre-commit Checklist (MANDATORY)

All checks below must pass locally before committing. CI runs the same commands.

### 1. Formatting

```sh
cargo fmt --all -- --check
```

If this fails, run `cargo fmt --all` and re-stage.

> Note: `rustfmt.toml` contains `imports_granularity = "Preserve"` (nightly-only).
> On stable, this setting is ignored with a warning — that warning is expected and harmless.

### 2. Lint

```sh
RUSTFLAGS="-Dwarnings" cargo clippy --all --all-targets
```

CI uses `RUSTFLAGS: -Dwarnings`, so any clippy warning is a hard error.

### 3. Tests

```sh
cargo test -p gaussdb-mcp          # MCP tool unit tests (no DB required)
```

> Full `cargo test --all` requires a running openGauss/PostgreSQL instance via `docker compose up -d`.
> Run it locally if available; otherwise the `-p gaussdb-mcp` subset is sufficient for pre-commit.

## CI Pipeline

Workflow: `.github/workflows/ci.yml`

| Job | Command | Notes |
|-----|---------|-------|
| rustfmt | `cargo fmt --all -- --check` | Stable toolchain |
| clippy | `cargo clippy --all --all-targets` | `RUSTFLAGS: -Dwarnings` |
| check-wasm32 | `cargo check --target wasm32-unknown-unknown` | tokio-opengauss WASM compat |
| test | `cargo test --all` + feature variants | Requires Docker DB |

CI triggers on push to `main` and PRs targeting `main`.

## Commit Style

- Semantic: `type(scope): message` (e.g., `fix(gaussdb-mcp): ...`, `feat(mcp): ...`, `chore: ...`)
- English only

## Release / Tag Convention

- Tags: `gaussdb-mcp-v<version>` (e.g., `gaussdb-mcp-v0.4.1`)
- Version source: `tools/gaussdb-mcp/Cargo.toml`
- Also update the hardcoded `version` in `tools/gaussdb-mcp/src/server.rs` (tool_handler attribute) and `Cargo.lock`
