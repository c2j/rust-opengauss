# Statement Timeout & Connection Lifetime Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add configurable SQL statement timeout (per-connection default + per-tool-call override) and connection max-lifetime recycling, applicable to both MCP server and CLI, with two timeout actions: `cancel` (keep connection) or `disconnect` (force reconnect).

**Architecture:** Server-side `statement_timeout` GUC as the primary timeout mechanism (avoids the well-documented tokio-postgres connection-poisoning risk from client-side `tokio::time::timeout`). Global default applied via `SET statement_timeout` after connect; per-call override via `SELECT set_config('statement_timeout', $1, true)` inside a transaction. `connection_max_lifetime` checked in `get_client_for()` before returning a client. Timeout action (`cancel`/`disconnect`) determines whether `downgrade_on_error()` is called after SQLSTATE 57014.

**Tech Stack:** Rust 1.85+, tokio-opengauss 0.7.17, clap 4 (derive), serde + toml, rmcp (MCP SDK), schemars (JSON schema for tool params).

---

## Design Decisions (confirmed with user)

| Decision | Choice | Rationale |
|---|---|---|
| Timeout mechanism | Server-side `statement_timeout` | Avoids connection poisoning (rust-postgres #1109) |
| Scope | Global per-connection **+** per-tool-call override | Flexibility for AI agents |
| Action on timeout | `cancel` (default) or `disconnect` (user-configurable) | cancel keeps connection; disconnect forces reconnect |
| Connection lifetime | `connection_max_lifetime` (new) | Forces periodic connection recycling |
| Validation | `statement_timeout ≤ connection_max_lifetime` (if both set) | Fail-fast at config load |
| EXPLAIN ANALYZE | Uses same timeout mechanism | Can override per-call via `timeout_ms` param |

---

## File Change Map

| File | Changes |
|---|---|
| `tools/gaussdb-mcp/src/config.rs` | Add `TimeoutConfig`, `TimeoutAction`, duration parser, config fields, validation |
| `tools/gaussdb-mcp/src/connection.rs` | `do_connect` accepts `Option<&TimeoutConfig>`, applies `SET statement_timeout` |
| `tools/gaussdb-mcp/src/server.rs` | `ConnectionState` carries `TimeoutConfig` + `connected_at`; tool params add `timeout_ms`; per-call override via transaction; max-lifetime check; 57014 → action |
| `tools/gaussdb-mcp/src/main.rs` | CLI args `--statement-timeout`, `--connection-max-lifetime`, `--timeout-action`; pass through |
| `tools/gaussdb-mcp/src/cli.rs` | Build `TimeoutConfig`, pass to `do_connect`, enhance error display |
| `tools/gaussdb-mcp/src/server.rs` (sqlstate map) | Add `57014` → SQLCODE mapping |
| `README.md` + `README_zh.md` | Document new options |

---

## Task 0: Duration Parsing Helper

**Files:**
- Create: `tools/gaussdb-mcp/src/duration_parse.rs`
- Modify: `tools/gaussdb-mcp/src/main.rs` (add `mod duration_parse;`)

**Why:** Need a small parser for human-friendly duration strings ("30s", "5min", "1h", "500ms", or bare seconds). Avoids adding `humantime` dependency for such a narrow use case.

**Step 1: Write the failing test**

Create `tools/gaussdb-mcp/src/duration_parse.rs`:

```rust
/// Parse a human-friendly duration string into `std::time::Duration`.
///
/// Accepted formats:
///   - Bare integer: interpreted as **seconds** (e.g. `"30"` → 30s)
///   - With unit suffix: `"500ms"`, `"30s"`, `"5m"`/`"5min"`, `"1h"`/
///     `"1hr"`, `"2d"`
///   - Case-insensitive, whitespace-trimmed
///
/// Returns `Err` with a descriptive message for invalid input.
pub(crate) fn parse_duration(s: &str) -> Result<std::time::Duration, String> {
    let trimmed = s.trim().to_lowercase();
    if trimmed.is_empty() {
        return Err("empty duration string".into());
    }

    // Try to split into numeric part + unit suffix.
    let split_at = trimmed
        .find(|c: char| c.is_alphabetic())
        .unwrap_or(trimmed.len());

    let (num_str, unit) = trimmed.split_at(split_at);
    let num: u64 = num_str.parse().map_err(|_| {
        format!("invalid duration number '{}' in '{}'", num_str, s)
    })?;

    let multiplier_ms: u64 = match unit {
        "" | "s" | "sec" | "secs" | "second" | "seconds" => num * 1000,
        "ms" | "millis" | "milliseconds" => num,
        "m" | "min" | "mins" | "minute" | "minutes" => num * 60 * 1000,
        "h" | "hr" | "hrs" | "hour" | "hours" => num * 3600 * 1000,
        "d" | "day" | "days" => num * 86400 * 1000,
        other => {
            return Err(format!(
                "unknown duration unit '{}' in '{}'. Valid: ms, s, m/min, h/hr, d",
                other, s
            ))
        }
    };

    Ok(std::time::Duration::from_millis(multiplier_ms))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn bare_integer_is_seconds() {
        assert_eq!(parse_duration("30").unwrap(), Duration::from_secs(30));
    }

    #[test]
    fn seconds_suffix() {
        assert_eq!(parse_duration("30s").unwrap(), Duration::from_secs(30));
        assert_eq!(parse_duration("30 sec").unwrap(), Duration::from_secs(30));
    }

    #[test]
    fn milliseconds_suffix() {
        assert_eq!(parse_duration("500ms").unwrap(), Duration::from_millis(500));
    }

    #[test]
    fn minutes_suffix() {
        assert_eq!(parse_duration("5m").unwrap(), Duration::from_secs(300));
        assert_eq!(parse_duration("5min").unwrap(), Duration::from_secs(300));
        assert_eq!(parse_duration("5 minutes").unwrap(), Duration::from_secs(300));
    }

    #[test]
    fn hours_suffix() {
        assert_eq!(parse_duration("1h").unwrap(), Duration::from_secs(3600));
        assert_eq!(parse_duration("2hr").unwrap(), Duration::from_secs(7200));
    }

    #[test]
    fn case_insensitive_and_whitespace_trimmed() {
        assert_eq!(parse_duration("  10S  ").unwrap(), Duration::from_secs(10));
        assert_eq!(parse_duration("3MIN").unwrap(), Duration::from_secs(180));
    }

    #[test]
    fn empty_string_errors() {
        assert!(parse_duration("").is_err());
        assert!(parse_duration("   ").is_err());
    }

    #[test]
    fn invalid_number_errors() {
        assert!(parse_duration("abc").is_err());
        assert!(parse_duration("12.5s").is_err()); // no floats
    }

    #[test]
    fn unknown_unit_errors() {
        assert!(parse_duration("10x").is_err());
    }
}
```

**Step 2: Register the module**

In `tools/gaussdb-mcp/src/main.rs`, add after line 6 (`mod server;`):

```rust
mod duration_parse;
```

**Step 3: Run tests**

```bash
cargo test -p gaussdb-mcp duration_parse -- --nocapture
```

Expected: all 9 tests pass.

**Step 4: Commit**

```bash
git add tools/gaussdb-mcp/src/duration_parse.rs tools/gaussdb-mcp/src/main.rs
git commit -m "feat(mcp): add duration_parse module for human-friendly timeout strings"
```

---

## Task 1: TimeoutConfig Struct + Validation

**Files:**
- Modify: `tools/gaussdb-mcp/src/config.rs`

**Step 1: Add TimeoutAction enum and TimeoutConfig struct**

Add at the top of `config.rs` (after `use` block, before `NamedConnection`):

```rust
use std::time::Duration;

/// What to do when a statement times out (SQLSTATE 57014).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TimeoutAction {
    /// Cancel the statement but keep the connection alive (server-side
    /// statement_timeout; connection stays valid).
    Cancel,
    /// Cancel the statement AND force reconnect on next call (drop the
    /// client from the pool so the next tool call establishes a fresh
    /// connection).
    Disconnect,
}

impl Default for TimeoutAction {
    fn default() -> Self {
        TimeoutAction::Cancel
    }
}

impl TimeoutAction {
    /// Parse from a config/CLI string. Accepts: "cancel", "disconnect"
    /// (case-insensitive).
    pub(crate) fn from_str(s: &str) -> Result<Self, String> {
        match s.trim().to_lowercase().as_str() {
            "cancel" | "keep" => Ok(TimeoutAction::Cancel),
            "disconnect" | "drop" | "reconnect" => Ok(TimeoutAction::Disconnect),
            other => Err(format!(
                "invalid timeout_action '{}'. Use 'cancel' or 'disconnect'.",
                other
            )),
        }
    }
}

/// Bundle of timeout-related settings for a single connection.
#[derive(Clone, Debug, Default)]
pub(crate) struct TimeoutConfig {
    /// Server-side statement_timeout. `None` = use server default (often
    /// unlimited).
    pub(crate) statement_timeout: Option<Duration>,
    /// Connection max lifetime — if set, the connection is recycled
    /// (forcibly reconnected) after this duration. `None` = no limit.
    pub(crate) connection_max_lifetime: Option<Duration>,
    /// What to do when statement_timeout fires.
    pub(crate) timeout_action: TimeoutAction,
}

impl TimeoutConfig {
    /// Validate that `statement_timeout <= connection_max_lifetime` when
    /// both are set. Returns `Err(message)` on violation.
    pub(crate) fn validate(&self) -> Result<(), String> {
        match (self.statement_timeout, self.connection_max_lifetime) {
            (Some(st), Some(ml)) if st > ml => Err(format!(
                "statement_timeout ({:?}) must not exceed connection_max_lifetime ({:?})",
                st, ml
            )),
            _ => Ok(()),
        }
    }

    /// Build a TimeoutConfig from raw optional string inputs (as read from
    /// TOML config or CLI args). Unset fields fall back to `base` if
    /// provided.
    pub(crate) fn from_overrides(
        statement_timeout: Option<&str>,
        connection_max_lifetime: Option<&str>,
        timeout_action: Option<&str>,
        base: Option<&TimeoutConfig>,
    ) -> Result<Self, String> {
        let parse_or_inherit = |field: Option<&str>,
                                base_val: Option<Duration>,
                                label: &str|
         -> Result<Option<Duration>, String> {
            match field {
                Some(s) => {
                    let d = crate::duration_parse::parse_duration(s)?;
                    Ok(Some(d))
                }
                None => Ok(base_val),
            }
        };

        let base_st = base.and_then(|b| b.statement_timeout);
        let base_ml = base.and_then(|b| b.connection_max_lifetime);
        let base_action = base.map(|b| b.timeout_action).unwrap_or_default();

        let st = parse_or_inherit(statement_timeout, base_st, "statement_timeout")?;
        let ml = parse_or_inherit(connection_max_lifetime, base_ml, "connection_max_lifetime")?;
        let action = match timeout_action {
            Some(s) => TimeoutAction::from_str(s)?,
            None => base_action,
        };

        let cfg = TimeoutConfig {
            statement_timeout: st,
            connection_max_lifetime: ml,
            timeout_action: action,
        };
        cfg.validate()?;
        Ok(cfg)
    }
}
```

**Step 2: Add unit tests for validation**

Append to `config.rs` (or inline at bottom):

```rust
#[cfg(test)]
mod timeout_tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn validate_ok_when_statement_le_lifetime() {
        let cfg = TimeoutConfig {
            statement_timeout: Some(Duration::from_secs(30)),
            connection_max_lifetime: Some(Duration::from_secs(600)),
            timeout_action: TimeoutAction::Cancel,
        };
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn validate_fails_when_statement_gt_lifetime() {
        let cfg = TimeoutConfig {
            statement_timeout: Some(Duration::from_secs(600)),
            connection_max_lifetime: Some(Duration::from_secs(30)),
            timeout_action: TimeoutAction::Cancel,
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn validate_ok_with_only_statement_timeout() {
        let cfg = TimeoutConfig {
            statement_timeout: Some(Duration::from_secs(30)),
            connection_max_lifetime: None,
            timeout_action: TimeoutAction::Cancel,
        };
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn timeout_action_parses_aliases() {
        assert_eq!(TimeoutAction::from_str("cancel").unwrap(), TimeoutAction::Cancel);
        assert_eq!(TimeoutAction::from_str("DISCONNECT").unwrap(), TimeoutAction::Disconnect);
        assert_eq!(TimeoutAction::from_str("keep").unwrap(), TimeoutAction::Cancel);
        assert_eq!(TimeoutAction::from_str("reconnect").unwrap(), TimeoutAction::Disconnect);
        assert!(TimeoutAction::from_str("invalid").is_err());
    }

    #[test]
    fn from_overrides_inherits_unset_fields_from_base() {
        let base = TimeoutConfig {
            statement_timeout: Some(Duration::from_secs(30)),
            connection_max_lifetime: Some(Duration::from_secs(600)),
            timeout_action: TimeoutAction::Cancel,
        };
        let cfg = TimeoutConfig::from_overrides(None, None, None, Some(&base)).unwrap();
        assert_eq!(cfg.statement_timeout, Some(Duration::from_secs(30)));
        assert_eq!(cfg.timeout_action, TimeoutAction::Cancel);
    }

    #[test]
    fn from_overrides_overrides_set_fields() {
        let base = TimeoutConfig {
            statement_timeout: Some(Duration::from_secs(30)),
            connection_max_lifetime: Some(Duration::from_secs(600)),
            timeout_action: TimeoutAction::Cancel,
        };
        let cfg = TimeoutConfig::from_overrides(Some("10s"), None, Some("disconnect"), Some(&base)).unwrap();
        assert_eq!(cfg.statement_timeout, Some(Duration::from_secs(10)));
        assert_eq!(cfg.connection_max_lifetime, Some(Duration::from_secs(600)));
        assert_eq!(cfg.timeout_action, TimeoutAction::Disconnect);
    }
}
```

**Step 3: Run tests**

```bash
cargo test -p gaussdb-mcp timeout_tests -- --nocapture
```

Expected: all 6 tests pass.

**Step 4: Commit**

```bash
git add tools/gaussdb-mcp/src/config.rs
git commit -m "feat(mcp): add TimeoutConfig, TimeoutAction, and validation logic"
```

---

## Task 2: Add Timeout Fields to NamedConnection & MultiConfig

**Files:**
- Modify: `tools/gaussdb-mcp/src/config.rs`

**Step 1: Add fields to `NamedConnection` struct** (around line 18)

Replace the `NamedConnection` struct definition with:

```rust
#[derive(Debug, Deserialize, Clone)]
pub(crate) struct NamedConnection {
    pub(crate) name: String,
    pub(crate) url: Option<String>,
    pub(crate) host: Option<String>,
    pub(crate) port: Option<u16>,
    pub(crate) user: Option<String>,
    pub(crate) password: Option<String>,
    pub(crate) dbname: Option<String>,
    pub(crate) sslmode: Option<String>,
    /// Statement timeout (e.g. "30s", "5min"). Applied via
    /// `SET statement_timeout` after connect.
    pub(crate) statement_timeout: Option<String>,
    /// Connection max lifetime before forced reconnect (e.g. "10min").
    pub(crate) connection_max_lifetime: Option<String>,
    /// Action on timeout: "cancel" (default) or "disconnect".
    #[serde(default)]
    pub(crate) timeout_action: Option<String>,
}
```

**Step 2: Add same fields to `MultiConfig`** (around line 30)

```rust
#[derive(Debug, Deserialize)]
pub(crate) struct MultiConfig {
    pub(crate) url: Option<String>,
    pub(crate) host: Option<String>,
    pub(crate) port: Option<u16>,
    pub(crate) user: Option<String>,
    pub(crate) password: Option<String>,
    pub(crate) dbname: Option<String>,
    pub(crate) sslmode: Option<String>,

    pub(crate) default_connection: Option<String>,
    pub(crate) connections: Option<Vec<NamedConnection>>,

    /// Flat-level defaults inherited by all [[connections]] that don't
    /// override them.
    pub(crate) statement_timeout: Option<String>,
    pub(crate) connection_max_lifetime: Option<String>,
    pub(crate) timeout_action: Option<String>,
}
```

**Step 3: Add a method to extract `TimeoutConfig` from a `NamedConnection`**

Add inside `impl NamedConnection { ... }`:

```rust
    /// Build a `TimeoutConfig` from this connection's settings, inheriting
    /// unset fields from `base` (typically the flat-level MultiConfig
    /// defaults).
    pub(crate) fn timeout_config(
        &self,
        base: Option<&TimeoutConfig>,
    ) -> Result<TimeoutConfig, String> {
        TimeoutConfig::from_overrides(
            self.statement_timeout.as_deref(),
            self.connection_max_lifetime.as_deref(),
            self.timeout_action.as_deref(),
            base,
        )
    }
```

**Step 4: Update `MultiConfig::resolve` to thread flat-level fields**

Replace the `_ => { ... }` arm of `MultiConfig::resolve` (around line 96-114) so the synthesized single `NamedConnection` inherits the flat-level timeout fields:

```rust
            _ => {
                if self.host.is_none() && self.user.is_none() && self.url.is_none() {
                    return Err(
                        "config must contain either [[connections]] or flat host/user fields"
                            .into(),
                    );
                }
                let single = NamedConnection {
                    name: "default".to_string(),
                    url: self.url,
                    host: self.host,
                    port: self.port,
                    user: self.user,
                    password: self.password,
                    dbname: self.dbname,
                    sslmode: self.sslmode,
                    statement_timeout: self.statement_timeout,
                    connection_max_lifetime: self.connection_max_lifetime,
                    timeout_action: self.timeout_action,
                };
                Ok((vec![single], Some("default".to_string())))
            }
```

**Step 5: Handle `resolve_all_connections` and lazy resolver paths**

Both `resolve_single_connection` (around line 225) and `build_lazy_resolver` (around line 275) currently build connection URLs. They do **not** need to change for URL building — the timeout is applied post-connect via `SET statement_timeout`, not via the connection string.

**However**, we need a way to carry the `TimeoutConfig` forward alongside the resolved connection. The cleanest approach: add an optional `timeout_config` field to `ResolvedConnection`:

```rust
pub(crate) struct ResolvedConnection {
    pub(crate) name: String,
    pub(crate) connection_url: String,
    pub(crate) config_path: Option<PathBuf>,
    pub(crate) plaintext_password: Option<String>,
    pub(crate) keyring_username: String,
    pub(crate) password_source: PasswordSource,
    /// Timeout settings parsed from config (None if unset in config).
    pub(crate) timeout_config: TimeoutConfig,
}
```

Then in `resolve_single_connection`, compute it before returning:

```rust
    let timeout_config = conn.timeout_config(None)?; // base=None at this level

    Ok(ResolvedConnection {
        name: conn.name.clone(),
        connection_url,
        config_path,
        plaintext_password,
        keyring_username: keyring_user,
        password_source,
        timeout_config,
    })
```

**Note:** If the `MultiConfig` has flat-level timeout defaults that should be inherited by `[[connections]]` entries, `resolve_all_connections` should pass the flat-level base when resolving each `NamedConnection`. Add this threading in `resolve_all_connections`:

```rust
    // Build flat-level base timeout config (if any flat timeout fields set)
    let base_tc = TimeoutConfig::from_overrides(
        config.statement_timeout.as_deref(),
        config.connection_max_lifetime.as_deref(),
        config.timeout_action.as_deref(),
        None,
    ).ok(); // ok() = silently ignore flat-level validation errors here,
            // they will be caught per-connection below.

    let mut resolved = Vec::with_capacity(connections.len());
    for conn in &connections {
        // resolve_single_connection needs a variant that accepts base_tc;
        // see Step 6 below.
        resolved.push(resolve_single_connection(conn, Some(config_path.clone()), base_tc.as_ref())?);
    }
```

**Step 6: Update `resolve_single_connection` signature to accept base timeout**

Change its signature to:

```rust
pub(crate) fn resolve_single_connection(
    conn: &NamedConnection,
    config_path: Option<PathBuf>,
    base_timeout: Option<&TimeoutConfig>,
) -> Result<ResolvedConnection, String> {
    // ... existing body ...
    let timeout_config = conn.timeout_config(base_timeout)?;
    // ... rest unchanged except use timeout_config in the returned struct ...
}
```

Also update the two existing callers of `resolve_single_connection` (in `resolve_all_connections` and `build_lazy_resolver`) to pass the appropriate base.

**Step 7: Run existing build + cargo check**

```bash
cargo check -p gaussdb-mcp
```

Expected: compiles cleanly. Fix any field-not-found errors from call sites that construct `NamedConnection` or `ResolvedConnection` literals (there are several in `config.rs` — add the new fields with appropriate values, typically `None` / `Default::default()`).

**Step 8: Commit**

```bash
git add tools/gaussdb-mcp/src/config.rs
git commit -m "feat(mcp): thread timeout config through NamedConnection/MultiConfig/ResolvedConnection"
```

---

## Task 3: Apply statement_timeout in do_connect

**Files:**
- Modify: `tools/gaussdb-mcp/src/connection.rs`

**Step 1: Change `do_connect` signature and apply SET**

Replace the entire `do_connect` function body with:

```rust
use crate::config::TimeoutConfig;
use std::sync::Arc;

pub(crate) fn needs_tls(url: &str) -> bool {
    url.split_whitespace().any(|part| {
        if let Some(val) = part.strip_prefix("sslmode=") {
            matches!(val, "require" | "verify-ca" | "verify-full")
        } else {
            false
        }
    })
}

pub(crate) async fn do_connect(
    url: &str,
    timeout_config: Option<&TimeoutConfig>,
) -> Result<
    (Arc<tokio_opengauss::Client>, tokio::task::JoinHandle<()>),
    Box<dyn std::error::Error + Send + Sync>,
> {
    let (client, connection) = if needs_tls(url) {
        let connector = native_tls::TlsConnector::builder()
            .danger_accept_invalid_certs(true)
            .danger_accept_invalid_hostnames(true)
            .build()?;
        let tls = opengauss_native_tls::MakeTlsConnector::new(connector);
        tokio_opengauss::connect(url, tls).await?
    } else {
        tokio_opengauss::connect(url, tokio_opengauss::NoTls).await?
    };

    // Apply server-side statement_timeout if configured.
    if let Some(tc) = timeout_config {
        if let Some(st) = tc.statement_timeout {
            let ms = st.as_millis() as u64;
            // SET works via simple query protocol (batch_execute).
            // Use a parameter-safe interpolation: ms is a u64 so no
            // SQL-injection risk.
            let set_sql = format!("SET statement_timeout = {}", ms);
            if let Err(e) = client.batch_execute(&set_sql).await {
                tracing::warn!(
                    "failed to apply statement_timeout={}ms for new connection: {}",
                    ms, e
                );
                // Non-fatal: continue with server default. The user will
                // see a warning in logs.
            } else {
                tracing::info!(
                    "applied statement_timeout={}ms to new connection",
                    ms
                );
            }
        }
    }

    let handle = tokio::spawn(async move {
        if let Err(e) = connection.await {
            tracing::error!("database connection lost: {}", e);
        }
    });

    Ok((Arc::new(client), handle))
}
```

**Step 2: Fix all call sites**

There are **three** call sites currently (verified via `rg -n 'do_connect\(' tools/gaussdb-mcp/src/`):

1. `cli.rs:96` — `do_connect(&target.connection_url)` in `run_cli`. Pass `None` for now (the `TimeoutConfig` will be threaded through `run_cli` in Task 8).
2. `server.rs:492` — startup probe `try_connect`. Pass `None` (probe connection is throwaway; timeout not needed for connectivity test).
3. `server.rs:597` — `connect_with_url`. This one gets the real `TimeoutConfig` in Task 4.

Note: `main.rs`'s `--check-connection` path does **not** call `do_connect` — it uses `try_connect_notls` / `try_connect_tls` which call `tokio_opengauss::connect` directly. No changes needed there.

Run the search to confirm:

```bash
rg -n 'connection::do_connect\(' tools/gaussdb-mcp/src/
```

Update `cli.rs:96` and `server.rs:492` to pass `None`:

```rust
// cli.rs:96 — before:
let (client, _handle) = match connection::do_connect(&target.connection_url).await { ... };
// After:
let (client, _handle) = match connection::do_connect(&target.connection_url, None).await { ... };

// server.rs:492 (startup probe) — same pattern, pass None.
```

Leave `server.rs:597` for Task 4.

**Step 3: Build check**

```bash
cargo check -p gaussdb-mcp
```

Expected: compiles. If main.rs has a check-connection call site, fix it now.

**Step 4: Commit**

```bash
git add tools/gaussdb-mcp/src/connection.rs tools/gaussdb-mcp/src/main.rs
git commit -m "feat(mcp): do_connect accepts TimeoutConfig and applies SET statement_timeout"
```

---

## Task 4: Thread TimeoutConfig through server.rs ConnectionState

**Files:**
- Modify: `tools/gaussdb-mcp/src/server.rs`

This is the largest task. It has several sub-steps.

**Step 1: Add imports**

At the top of `server.rs`, add:

```rust
use crate::config::{TimeoutAction, TimeoutConfig};
use std::time::Instant;
use tokio_opengauss::error::SqlState;
```

**Step 2: Extend `ConnectionState` variants**

Replace the `ConnectionState` enum (around line 393) with:

```rust
enum ConnectionState {
    Pending(ResolveFn),
    Connecting {
        url: String,
        timeout_config: TimeoutConfig,
    },
    Connected {
        client: Arc<tokio_opengauss::Client>,
        url: String,
        timeout_config: TimeoutConfig,
        connected_at: Instant,
    },
    Unavailable(String),
}
```

**Step 3: Add a `timeout_configs` map to `GaussdbMcp`**

Because `Pending` and `Unavailable` variants don't carry a URL+timeout, and `get_client_for` needs to know the timeout when transitioning states, store timeout configs separately. Add a field:

```rust
pub struct GaussdbMcp {
    connections: Arc<Mutex<HashMap<String, ConnectionState>>>,
    default_name: String,
    on_connected: HashMap<String, CallbackFn>,
    /// Timeout config per connection name. Set at construction time,
    /// used when (re)establishing connections.
    timeout_configs: HashMap<String, TimeoutConfig>,
}
```

Update all three constructors (`new_multi_disconnected`, `new_multi_lazy`, and any others) to accept and store `timeout_configs`. If a name is missing from the map, fall back to `TimeoutConfig::default()`.

**Step 4: Update `new_multi_disconnected`**

Change its signature and body to accept a `timeout_configs: HashMap<String, TimeoutConfig>`:

```rust
pub fn new_multi_disconnected(
    entries: Vec<(String, String)>,
    default_name: String,
    timeout_configs: HashMap<String, TimeoutConfig>,
) -> Self {
    let mut connections = HashMap::new();
    for (name, url) in entries {
        let tc = timeout_configs
            .get(&name)
            .cloned()
            .unwrap_or_default();
        connections.insert(
            name,
            ConnectionState::Connecting {
                url,
                timeout_config: tc,
            },
        );
    }
    Self {
        connections: Arc::new(Mutex::new(connections)),
        default_name,
        on_connected: HashMap::new(),
        timeout_configs,
    }
}
```

**Step 5: Update `new_multi_lazy`**

Change `LazyConnectionEntry::Pending { name, resolver }` insertion to carry timeout_config alongside (store in `timeout_configs` map, and in the `Pending` transition look it up there).

```rust
pub fn new_multi_lazy(
    entries: Vec<(String, ResolveFn)>,
    default_name: String,
    timeout_configs: HashMap<String, TimeoutConfig>,
) -> Self {
    let mut connections = HashMap::new();
    for (name, resolver) in entries {
        connections.insert(name, ConnectionState::Pending(resolver));
    }
    Self {
        connections: Arc::new(Mutex::new(connections)),
        default_name,
        on_connected: HashMap::new(),
        timeout_configs,
    }
}
```

**Step 6: Update `get_client_for` — handle new variant shapes + max_lifetime check**

Replace the `match conns.get(&name)` body. The key additions:
- Destructure `Connecting { url, timeout_config: _ }` and `Connected { client, url, timeout_config, connected_at }`
- Before returning a `Connected` client, check `connected_at.elapsed() >= connection_max_lifetime` (if set) → recycle

```rust
        match conns.get(&name) {
            Some(ConnectionState::Connected {
                client,
                url,
                timeout_config,
                connected_at,
            }) => {
                if !client.is_closed() {
                    // Check connection max lifetime.
                    if let Some(max_lifetime) = timeout_config.connection_max_lifetime {
                        if connected_at.elapsed() >= max_lifetime {
                            info!(
                                "connection '{}' exceeded max_lifetime ({:?}), recycling",
                                name, max_lifetime
                            );
                            let url = url.clone();
                            let tc = timeout_config.clone();
                            conns.insert(
                                name.clone(),
                                ConnectionState::Connecting {
                                    url: url.clone(),
                                    timeout_config: tc,
                                },
                            );
                            drop(conns);
                            return self.connect_with_url(name, url).await;
                        }
                    }
                    return Ok(Arc::clone(client));
                }
                // Connection is dead — downgrade for reconnect.
                info!(
                    "connection '{}' is closed, downgrading to Connecting for reconnect",
                    name
                );
                let url = url.clone();
                let tc = timeout_config.clone();
                conns.insert(
                    name.clone(),
                    ConnectionState::Connecting {
                        url: url.clone(),
                        timeout_config: tc,
                    },
                );
                drop(conns);
                self.connect_with_url(name, url).await
            }
            Some(ConnectionState::Pending(resolver)) => {
                let resolver = Arc::clone(resolver);
                drop(conns);
                let url = resolver().map_err(|e| {
                    McpError::internal_error(
                        format!(
                            "Failed to resolve database credentials for '{}': {}",
                            name, e
                        ),
                        Some(json!({
                            "connection_name": name,
                            "hint": "Check your gaussdb-mcp configuration and OS keychain access"
                        })),
                    )
                })?;
                info!(
                    "connection URL resolved for '{}', attempting database connection",
                    name
                );
                self.connect_with_url(name, url).await
            }
            Some(ConnectionState::Connecting { url, .. })
            | Some(ConnectionState::Unavailable(url)) => {
                let url = url.clone();
                drop(conns);
                info!("attempting database connection for '{}'", name);
                self.connect_with_url(name, url).await
            }
            None => {
                let available: Vec<&String> = conns.keys().collect();
                Err(McpError::invalid_request(
                    "unknown_connection",
                    Some(json!({
                        "message": format!("Connection '{}' not found", name),
                        "available_connections": available,
                        "default_connection": self.default_name,
                    })),
                ))
            }
        }
```

**Step 7: Update `connect_with_url`**

Replace `connect_with_url` body. It needs to:
1. Look up the `TimeoutConfig` for this connection name (from `self.timeout_configs`)
2. Pass it to `do_connect`
3. Store `connected_at: Instant::now()` in the `Connected` state

```rust
    async fn connect_with_url(
        &self,
        name: String,
        url: String,
    ) -> Result<Arc<tokio_opengauss::Client>, McpError> {
        // Look up timeout config; fall back to default if missing.
        let tc = self
            .timeout_configs
            .get(&name)
            .cloned()
            .unwrap_or_default();

        let result = connection::do_connect(&url, Some(&tc)).await;
        let mut conns = self.connections.lock().await;

        match result {
            Ok((client, _handle)) => {
                info!("database '{}' connected successfully", name);
                if let Some(cb) = self.on_connected.get(&name) {
                    cb();
                }
                conns.insert(
                    name,
                    ConnectionState::Connected {
                        client: Arc::clone(&client),
                        url: url.clone(),
                        timeout_config: tc,
                        connected_at: Instant::now(),
                    },
                );
                Ok(client)
            }
            Err(e) => {
                let err = connection_error(&url, e.as_ref());
                conns.insert(name, ConnectionState::Unavailable(url));
                Err(err)
            }
        }
    }
```

**Step 8: Update `downgrade_on_error` for new variant shape**

```rust
    async fn downgrade_on_error(&self, name: &str) {
        let mut conns = self.connections.lock().await;
        if let Some(ConnectionState::Connected { url, timeout_config, .. }) = conns.get(name) {
            let url = url.clone();
            let tc = timeout_config.clone();
            info!(
                "connection '{}' downgrading to Connecting after query error",
                name
            );
            conns.insert(
                name.to_string(),
                ConnectionState::Connecting {
                    url,
                    timeout_config: tc,
                },
            );
        }
    }
```

**Step 9: Build check**

```bash
cargo check -p gaussdb-mcp
```

Fix any remaining pattern-match exhaustiveness errors. The startup-probe loop (around line 480-520, which iterates and inserts `ConnectionState::Connected { client, url }`) also needs updating to include `timeout_config` and `connected_at: Instant::now()`.

**Step 10: Commit**

```bash
git add tools/gaussdb-mcp/src/server.rs
git commit -m "feat(mcp): thread TimeoutConfig through ConnectionState and apply max-lifetime recycling"
```

---

## Task 5: Per-Tool-Call Timeout Override (execute_query, get_execution_plan)

**Files:**
- Modify: `tools/gaussdb-mcp/src/server.rs`

**Step 1: Add `timeout_ms` to tool param structs**

Update `ExecuteQueryParams` (around line 374):

```rust
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ExecuteQueryParams {
    pub sql: String,
    /// Optional per-call statement timeout in milliseconds. Overrides the
    /// connection's global `statement_timeout` for this query only.
    #[serde(default)]
    pub timeout_ms: Option<u64>,
    #[serde(default)]
    pub connection_name: Option<String>,
}
```

Update `GetExecutionPlanParams` (around line 382):

```rust
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct GetExecutionPlanParams {
    pub sql: String,
    pub analyze: Option<bool>,
    pub format: Option<String>,
    /// Optional per-call statement timeout in milliseconds.
    #[serde(default)]
    pub timeout_ms: Option<u64>,
    #[serde(default)]
    pub connection_name: Option<String>,
}
```

**Step 2: Add a `query_with_optional_timeout` helper**

Inside `impl GaussdbMcp { ... }` (the non-tool-router impl block), add:

```rust
    /// Run a read-only query with an optional per-call statement timeout.
    /// When `timeout_ms` is set, wraps the query in a transaction and uses
    /// `set_config(..., true)` to set a LOCAL statement_timeout that
    /// automatically resets when the transaction commits.
    async fn query_with_optional_timeout(
        client: &tokio_opengauss::Client,
        sql: &str,
        timeout_ms: Option<u64>,
    ) -> Result<Vec<tokio_opengauss::Row>, tokio_opengauss::Error> {
        match timeout_ms {
            None => client.query(sql, &[]).await,
            Some(ms) => {
                let tx = client.transaction().await?;
                // set_config(name, value, is_local=true) — safe,
                // parameterized, transaction-scoped.
                tx.query(
                    "SELECT set_config('statement_timeout', $1, true)",
                    &[&ms.to_string()],
                )
                .await?;
                let rows = tx.query(sql, &[]).await?;
                tx.commit().await?;
                Ok(rows)
            }
        }
    }
```

**Step 3: Use it in `execute_query`**

Replace `let rows = match client.query(trimmed, &[]).await {` (line 891) with:

```rust
        let rows = match Self::query_with_optional_timeout(&client, trimmed, params.timeout_ms).await {
            Ok(rows) => rows,
            Err(e) => {
                return Err(
                    self.handle_query_error(&name, e, "execute_query", trimmed)
                        .await,
                );
            }
        };
```

**Step 4: Use it in `get_execution_plan`**

Replace `let rows = match client.query(&explain_sql, &[]).await {` (line 964) with:

```rust
        let rows = match Self::query_with_optional_timeout(&client, &explain_sql, params.timeout_ms).await {
            Ok(rows) => rows,
            Err(e) => {
                return Err(
                    self.handle_query_error(&name, e, "get_execution_plan", &explain_sql)
                        .await,
                );
            }
        };
```

**Step 5: Build check**

```bash
cargo check -p gaussdb-mcp
```

**Step 6: Commit**

```bash
git add tools/gaussdb-mcp/src/server.rs
git commit -m "feat(mcp): add per-call timeout_ms override to execute_query and get_execution_plan"
```

---

## Task 6: Timeout Action (disconnect) on SQLSTATE 57014

**Files:**
- Modify: `tools/gaussdb-mcp/src/server.rs`

**Step 1: Update `handle_query_error` to honor `timeout_action`**

Replace the current `handle_query_error` (line 640) with:

```rust
    async fn handle_query_error(
        &self,
        name: &str,
        err: tokio_opengauss::Error,
        tool: &str,
        sql: &str,
    ) -> McpError {
        // Distinguish three cases:
        //  1) SQLSTATE 57014 (QUERY_CANCELED) + action=Disconnect → force
        //     reconnect (downgrade to Connecting).
        //  2) SQLSTATE 57014 + action=Cancel → return error, keep
        //     connection (the default).
        //  3) Non-DB error (connection dropped) → always downgrade.
        let is_timeout = err
            .as_db_error()
            .is_some_and(|e| e.code() == &SqlState::QUERY_CANCELED);

        if is_timeout {
            let action = self
                .timeout_configs
                .get(name)
                .map(|tc| tc.timeout_action)
                .unwrap_or_default();
            if action == TimeoutAction::Disconnect {
                info!(
                    "connection '{}' force-disconnecting after statement timeout (action=disconnect)",
                    name
                );
                self.downgrade_on_error(name).await;
            } else {
                info!(
                    "connection '{}' statement timed out, keeping connection (action=cancel)",
                    name
                );
            }
        } else if err.as_db_error().is_none() {
            // Connection-level error (not a DB/SQL error).
            self.downgrade_on_error(name).await;
        }
        query_error(tool, sql, &err)
    }
```

**Step 2: Add SQLSTATE 57014 → SQLCODE mapping**

In `sqlstate_to_sqlcode` (around line 17), add inside the match:

```rust
        "57014" => -7,
```

(Place it alphabetically near other 57xxx codes if any, otherwise near the end before the wildcard arm.)

**Step 3: Enhance the timeout error message in `query_error`**

Find `fn query_error(` in server.rs (the free function, not the method). After building the base error JSON, add a `hint` for timeout errors:

```rust
        // If this is a statement timeout, add a helpful hint.
        let hint = if db_err.code() == &SqlState::QUERY_CANCELED {
            Some(
                "Query exceeded the configured statement_timeout. \
                 Options: increase the timeout, add timeout_ms to the tool call, \
                 or optimize the query."
                    .to_string(),
            )
        } else {
            db_err.hint().map(String::from)
        };
```

And use `hint` instead of `db_err.hint()` in the JSON output.

**Step 4: Build + lsp diagnostics**

```bash
cargo check -p gaussdb-mcp
```

**Step 5: Commit**

```bash
git add tools/gaussdb-mcp/src/server.rs
git commit -m "feat(mcp): honor timeout_action on SQLSTATE 57014; add helpful timeout hint"
```

---

## Task 7: CLI Args (main.rs)

**Files:**
- Modify: `tools/gaussdb-mcp/src/main.rs`

**Step 1: Add timeout-related CLI args to `Commands::Cli`**

Inside the `Cli { ... }` variant of `Commands` (around line 59), add:

```rust
        /// Statement timeout (e.g. "30s", "5min"). Overrides config.
        #[arg(long)]
        statement_timeout: Option<String>,

        /// Connection max lifetime before reconnect (e.g. "10min").
        #[arg(long)]
        connection_max_lifetime: Option<String>,

        /// Action on timeout: "cancel" or "disconnect". Default: cancel.
        #[arg(long)]
        timeout_action: Option<String>,
```

**Step 2: Add same args to `Commands::Mcp` (optional but useful)**

So MCP mode can override the config on the command line too:

```rust
        /// Override statement timeout for all connections.
        #[arg(long)]
        statement_timeout: Option<String>,

        /// Override connection max lifetime for all connections.
        #[arg(long)]
        connection_max_lifetime: Option<String>,

        /// Override timeout action.
        #[arg(long)]
        timeout_action: Option<String>,
```

**Step 3: Thread CLI overrides through to `run_mcp_server`**

Update `run_mcp_server` to accept the CLI overrides and apply them on top of config-derived `timeout_configs`. The cleanest approach:

1. `run_mcp_server` takes `cli_overrides: Option<CliTimeoutOverrides>` (a small struct bundling the three `Option<String>`s).
2. When building `timeout_configs: HashMap<String, TimeoutConfig>`, for each entry, call `TimeoutConfig::from_overrides(...)` with the per-connection config as the "base" and the CLI overrides as the "override" layer.

Define near the top of main.rs:

```rust
#[derive(Clone, Default)]
struct CliTimeoutOverrides {
    statement_timeout: Option<String>,
    connection_max_lifetime: Option<String>,
    timeout_action: Option<String>,
}

impl CliTimeoutOverrides {
    /// Apply these overrides on top of a per-connection TimeoutConfig.
    fn apply_over(&self, base: &TimeoutConfig) -> Result<TimeoutConfig, String> {
        TimeoutConfig::from_overrides(
            self.statement_timeout.as_deref(),
            self.connection_max_lifetime.as_deref(),
            self.timeout_action.as_deref(),
            Some(base),
        )
    }
}
```

In the MCP startup path (where `timeout_configs` is built from `ResolvedConnection.timeout_config`), apply:

```rust
let timeout_configs: HashMap<String, TimeoutConfig> = {
    let mut m = HashMap::new();
    for conn in &resolved {
        let base = conn.timeout_config.clone();
        let final_tc = cli_overrides
            .as_ref()
            .map(|ov| ov.apply_over(&base))
            .transpose()?
            .unwrap_or(base);
        m.insert(conn.name.clone(), final_tc);
    }
    m
};
```

**Step 4: Thread CLI overrides through to `cli::run_cli`**

See Task 8.

**Step 5: Build check**

```bash
cargo check -p gaussdb-mcp
```

**Step 6: Commit**

```bash
git add tools/gaussdb-mcp/src/main.rs
git commit -m "feat(mcp): add --statement-timeout, --connection-max-lifetime, --timeout-action CLI args"
```

---

## Task 8: cli.rs Pass-Through + Error Display

**Files:**
- Modify: `tools/gaussdb-mcp/src/cli.rs`

**Step 1: Update `run_cli` to accept and use a `TimeoutConfig`**

Change `run_cli`'s signature so it accepts an `Option<TimeoutConfig>` (built from CLI overrides over config defaults). Pass it to `do_connect`:

```rust
pub(crate) async fn run_cli(
    args: crate::CliArgs,           // or whatever the current arg struct is
    timeout_config: Option<TimeoutConfig>,
) -> Result<(), Box<dyn std::error::Error>> {
    // ... existing config resolution to get connection_url ...

    let (client, _handle) = do_connect(&connection_url, timeout_config.as_ref()).await?;

    // ... rest of existing logic ...
}
```

(Adapt the exact arg-passing shape to whatever `run_cli` currently takes — read the existing signature in `cli.rs` line 51+ and main.rs call site to match.)

**Step 2: Build the TimeoutConfig in main.rs before calling `run_cli`**

In the `Commands::Cli { ... }` arm of `main()`:

```rust
        // Resolve base timeout from config (if available), then apply CLI
        // overrides.
        let base_tc = resolved_first.timeout_config.clone(); // from config
        let cli_tc = CliTimeoutOverrides {
            statement_timeout: args.statement_timeout,
            connection_max_lifetime: args.connection_max_lifetime,
            timeout_action: args.timeout_action,
        };
        let timeout_config = cli_tc.apply_over(&base_tc).ok();
        cli::run_cli(args, timeout_config).await?;
```

**Step 3: Enhance `format_sql_error` for timeout messages**

In `cli.rs`, update `format_sql_error` to show a helpful hint when SQLSTATE is 57014:

```rust
fn format_sql_error(err: &tokio_opengauss::Error) -> String {
    if let Some(db_err) = err.as_db_error() {
        let sqlstate = db_err.code().code();
        let sqlcode = sqlstate_to_sqlcode(sqlstate);
        let mut msg = format!(
            "[SQLSTATE {} | SQLCODE {}] {}",
            sqlstate, sqlcode, db_err.message()
        );
        if let Some(detail) = db_err.detail() {
            msg.push_str(&format!("\nDETAIL: {}", detail));
        }
        // Special-case statement timeout for actionable guidance.
        if sqlstate == "57014" {
            msg.push_str(
                "\nHINT: Query exceeded the configured statement_timeout. \
                 Pass a larger --statement-timeout or optimize the query.",
            );
        } else if let Some(hint) = db_err.hint() {
            msg.push_str(&format!("\nHINT: {}", hint));
        }
        msg
    } else {
        format_error_chain(err)
    }
}
```

**Step 4: Build check + manual test**

```bash
cargo build -p gaussdb-mcp
# Smoke-test the CLI (requires a running openGauss/PostgreSQL):
GAUSSDB_URL="host=127.0.0.1 user=postgres password=secret dbname=postgres" \
    ./target/debug/gaussdb-mcp cli --sql "SELECT pg_sleep(10)" --statement-timeout 2s
# Expected: error after ~2s with SQLSTATE 57014 message.
```

**Step 5: Commit**

```bash
git add tools/gaussdb-mcp/src/cli.rs tools/gaussdb-mcp/src/main.rs
git commit -m "feat(mcp): CLI passes timeout config and surfaces actionable timeout errors"
```

---

## Task 9: Documentation

**Files:**
- Modify: `README.md`
- Modify: `README_zh.md`

**Step 1: Add a "Statement Timeout" section to README.md**

After the "Configuration" section, add:

````markdown
## Statement Timeout & Connection Lifetime

`gaussdb-mcp` supports configurable SQL execution timeouts to prevent
runaway queries from blocking AI assistant sessions.

### Configuration (TOML)

```toml
# Per-connection timeout settings
[[connections]]
name = "prod"
host = "192.168.1.10"
# ... other fields ...
statement_timeout = "30s"              # cancel queries running > 30s
connection_max_lifetime = "10min"      # recycle connection every 10 min
timeout_action = "cancel"              # or "disconnect" (default: cancel)
```

### CLI

```sh
gaussdb-mcp cli --sql "SELECT pg_sleep(60)" --statement-timeout 5s
gaussdb-mcp cli --sql "..." --statement-timeout 30s --timeout-action disconnect
```

### MCP Tool Per-Call Override

`execute_query` and `get_execution_plan` accept an optional `timeout_ms`
parameter that overrides the connection-level default for a single call:

```json
{ "sql": "SELECT count(*) FROM huge_table", "timeout_ms": 5000 }
```

### How It Works

- **`statement_timeout`**: Applied server-side via PostgreSQL/openGauss's
  `SET statement_timeout` GUC. On timeout the server returns SQLSTATE
  `57014` (`query_canceled`); the connection stays valid.
- **`timeout_action = "cancel"`** (default): the connection is kept; the
  next tool call reuses it.
- **`timeout_action = "disconnect"`**: on timeout, the connection is
  forcibly recycled — the next tool call establishes a fresh connection.
- **`connection_max_lifetime`**: regardless of timeouts, connections are
  recycled after this duration to avoid state drift in long-running MCP
  sessions. Must be ≥ `statement_timeout`.
````

**Step 2: Mirror the section in `README_zh.md`** (Chinese translation).

**Step 3: Commit**

```bash
git add README.md README_zh.md
git commit -m "docs: document statement_timeout, connection_max_lifetime, and timeout_action"
```

---

## Verification Checklist

After all tasks complete, verify end-to-end:

- [ ] `cargo test -p gaussdb-mcp` — all unit tests pass
- [ ] `cargo check -p gaussdb-mcp` — no warnings related to new code
- [ ] `cargo build -p gaussdb-mcp` — release build succeeds
- [ ] **CLI basic**: `gaussdb-mcp cli --sql "SELECT 1"` works as before (no regression)
- [ ] **CLI timeout**: `gaussdb-mcp cli --sql "SELECT pg_sleep(10)" --statement-timeout 2s` → error in ~2s with SQLSTATE 57014
- [ ] **CLI disconnect action**: same query with `--timeout-action disconnect` → same error, and a subsequent CLI call reconnects cleanly
- [ ] **MCP tool per-call override**: `execute_query` with `timeout_ms: 2000` on `SELECT pg_sleep(10)` → 57014 in ~2s
- [ ] **MCP connection_max_lifetime**: set `"1s"`, call `get_database_info` twice with 2s gap → logs show "exceeded max_lifetime, recycling"
- [ ] **Validation**: config with `statement_timeout = "10min"` + `connection_max_lifetime = "1min"` → startup error
- [ ] **Backward compat**: existing configs without timeout fields → no behavior change

---

## Risk Notes

1. **Transaction wrapping for per-call override**: `execute_query` currently runs the user query directly. Wrapping in a transaction changes isolation semantics for writes — but `execute_query` only allows SELECT/EXPLAIN, so this is safe. `get_execution_plan` with `analyze: true` also runs read-only analysis, safe to wrap.

2. **Concurrent tool calls sharing one connection**: the `set_config(..., true)` approach uses LOCAL scope (transaction-scoped), so concurrent transactions on the same `Client` do NOT interfere. This is safe because tokio-postgres pipelines queries but each transaction has its own scope.

3. **SET statement_timeout at connect time**: if the server rejects SET (e.g., insufficient privileges), the warning is logged but the connection proceeds with the server default. This is intentional — fail-open, not fail-closed.

4. **Existing test suite**: the `tokio-opengauss` crate has tests that already use `SqlState::QUERY_CANCELED` (see `tests/test/runtime.rs:133`), confirming the mechanism works end-to-end.

---

## Execution Notes

- Tasks 0–1 are independent and can run in parallel.
- Task 2 depends on Task 1 (needs `TimeoutConfig`).
- Task 3 depends on Task 1.
- Task 4 depends on Tasks 2 + 3.
- Tasks 5–6 depend on Task 4.
- Tasks 7–8 depend on Task 4 (for the `TimeoutConfig` plumbing).
- Task 9 is independent and can run anytime after Task 1.

**Parallelizable groups:**
- Group A: Tasks 0 + 1 (independent modules)
- Group B: Tasks 2 + 3 (after Task 1)
- Group C: Tasks 4 (after 2 + 3) — sequential, this is the critical path
- Group D: Tasks 5, 6, 7, 8 (after 4, can partially parallelize)
- Group E: Task 9 (after 1, independent of code changes)
