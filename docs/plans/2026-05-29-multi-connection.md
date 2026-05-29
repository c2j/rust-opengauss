# Multi-Database Connection Support for gaussdb-mcp

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Support multiple named database connections in `~/.gaussdb-mcp.toml`, selectable at MCP tool invocation time via an optional `connection_name` parameter.

**Architecture:** Replace the single `ConnectionState` with a `HashMap<String, ConnectionState>`. TOML config adds `[[connections]]` array with backward compatibility for the old flat format. Each MCP tool gains an optional `connection_name` parameter that defaults to the first (or explicitly marked default) connection.

**Tech Stack:** Rust, rmcp 1.5.0, tokio-opengauss, toml 0.8, serde, keyring

---

## Task 1: Update Config Structs and TOML Parsing

**Files:**
- Modify: `tools/gaussdb-mcp/src/main.rs`

**Step 1: Add `MultiConfig` and `NamedConnection` structs**

Add after the existing `Config` struct (around line 56):

```rust
#[derive(Debug, Deserialize)]
struct NamedConnection {
    name: String,
    url: Option<String>,
    host: Option<String>,
    port: Option<u16>,
    user: Option<String>,
    password: Option<String>,
    dbname: Option<String>,
    sslmode: Option<String>,
}

#[derive(Debug, Deserialize)]
struct MultiConfig {
    // Old flat fields (backward compatible)
    url: Option<String>,
    host: Option<String>,
    port: Option<u16>,
    user: Option<String>,
    password: Option<String>,
    dbname: Option<String>,
    sslmode: Option<String>,

    // New multi-connection fields
    default_connection: Option<String>,
    connections: Option<Vec<NamedConnection>>,
}
```

**Step 2: Implement `MultiConfig::resolve` method**

This method converts both old-format and new-format configs into a `Vec<NamedConnection>`:

```rust
impl MultiConfig {
    fn resolve(self) -> Result<(Vec<NamedConnection>, Option<String>), String> {
        match self.connections {
            Some(conns) if !conns.is_empty() => {
                // New format: use [[connections]] array
                let default = self.default_connection
                    .or_else(|| conns.first().map(|c| c.name.clone()));
                Ok((conns, default))
            }
            _ => {
                // Old format: wrap flat fields into a single NamedConnection
                if self.host.is_none() && self.user.is_none() && self.url.is_none() {
                    return Err("config must contain either [[connections]] or flat host/user fields".into());
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
                };
                Ok((vec![single], Some("default".to_string())))
            }
        }
    }
}
```

**Step 3: Add `NamedConnection::to_connection_url` and `NamedConnection::keyring_username`**

These mirror the existing `Config` methods:

```rust
impl NamedConnection {
    fn keyring_username(&self) -> String {
        match (&self.user, &self.host, &self.dbname) {
            (Some(u), Some(h), Some(d)) => format!("{}@{}/{}", u, h, d),
            (Some(u), Some(h), None) => format!("{}@{}", u, h),
            (Some(u), _, _) => u.clone(),
            _ => "default".to_string(),
        }
    }

    fn to_connection_url(&self) -> Option<String> {
        if let Some(ref url) = self.url {
            return Some(url.clone());
        }
        if self.host.is_none() && self.user.is_none() {
            return None;
        }
        let mut parts = Vec::new();
        if let Some(ref host) = self.host { parts.push(format!("host={}", host)); }
        if let Some(port) = self.port { parts.push(format!("port={}", port)); }
        if let Some(ref user) = self.user { parts.push(format!("user={}", user)); }
        if let Some(ref password) = self.password { parts.push(format!("password={}", password)); }
        if let Some(ref dbname) = self.dbname { parts.push(format!("dbname={}", dbname)); }
        if let Some(ref sslmode) = self.sslmode { parts.push(format!("sslmode={}", sslmode)); }
        Some(parts.join(" "))
    }
}
```

**Step 4: Verify compilation**

Run: `cargo build -p gaussdb-mcp 2>&1 | head -30`
Expected: compiles (structs are unused but valid)

**Step 5: Commit**

```bash
git add tools/gaussdb-mcp/src/main.rs
git commit -m "feat(multi-conn): add MultiConfig and NamedConnection structs"
```

---

## Task 2: Create ResolvedConnections and ConnectionResolver

**Files:**
- Modify: `tools/gaussdb-mcp/src/main.rs`

**Step 1: Add ResolvedConnection struct**

Replaces `ResolvedConfig` for a single resolved connection entry:

```rust
struct ResolvedConnection {
    name: String,
    connection_url: String,
    config_path: Option<PathBuf>,
    plaintext_password: Option<String>,
    keyring_username: String,
    password_source: PasswordSource,
}
```

**Step 2: Add ResolvedConnections struct**

Holds all resolved connections and the default name:

```rust
struct ResolvedConnections {
    connections: Vec<ResolvedConnection>,
    default_name: String,
}
```

**Step 3: Implement resolve_connections function**

This replaces `resolve_connection_url_inner`. It reads config, calls `MultiConfig::resolve`, then for each `NamedConnection` resolves password (keyring/plaintext/none) and builds `ResolvedConnection`.

Logic for each connection mirrors the existing `resolve_connection_url_inner`:
- If `password == "keyring"` sentinel → read from keyring using `keyring_username()`
- If plaintext → store for migration
- If no password → PasswordSource::None

**Step 4: Implement resolve_connections_lazy function**

This replaces `resolve_connection_url_lazy_inner`. Returns `LazyResolvedConnections` which can defer keyring reads:

```rust
enum LazyConnection {
    Ready(ResolvedConnection),
    Pending(Arc<dyn (Fn() -> Result<String, String>) + Send + Sync>),
}

struct LazyResolvedConnections {
    connections: Vec<(String, LazyConnection)>, // (name, state)
    default_name: String,
}
```

**Step 5: Commit**

```bash
git commit -m "feat(multi-conn): add connection resolution for multiple named connections"
```

---

## Task 3: Update GaussdbMcp Server to Support Multiple Connections

**Files:**
- Modify: `tools/gaussdb-mcp/src/server.rs`

**Step 1: Change ConnectionState to be name-aware**

The existing `ConnectionState` enum stays the same. The `GaussdbMcp` struct changes:

```rust
pub struct GaussdbMcp {
    connections: Arc<Mutex<HashMap<String, ConnectionState>>>,
    default_name: String,
    on_connected: HashMap<String, Arc<dyn Fn() + Send + Sync>>,
}
```

**Step 2: Add constructors**

```rust
impl GaussdbMcp {
    pub fn new_multi_disconnected(entries: Vec<(String, String)>, default_name: String) -> Self {
        let mut connections = HashMap::new();
        for (name, url) in entries {
            connections.insert(name, ConnectionState::Connecting(url));
        }
        Self {
            connections: Arc::new(Mutex::new(connections)),
            default_name,
            on_connected: HashMap::new(),
        }
    }

    pub fn new_multi_lazy(
        entries: Vec<(String, Arc<dyn (Fn() -> Result<String, String>) + Send + Sync>)>,
        default_name: String,
    ) -> Self {
        let mut connections = HashMap::new();
        for (name, resolver) in entries {
            connections.insert(name, ConnectionState::Pending(resolver));
        }
        Self {
            connections: Arc::new(Mutex::new(connections)),
            default_name,
            on_connected: HashMap::new(),
        }
    }

    // Keep old constructors for backward compat
    pub fn new_disconnected(url: String) -> Self {
        Self::new_multi_disconnected(vec![("default".into(), url)], "default".into())
    }

    pub fn new_lazy(resolver: Arc<dyn (Fn() -> Result<String, String>) + Send + Sync>) -> Self {
        Self::new_multi_lazy(vec![("default".into(), resolver)], "default".into())
    }
}
```

**Step 3: Update get_client to accept connection_name**

```rust
async fn get_client_for(&self, connection_name: Option<&str>) -> Result<Arc<tokio_opengauss::Client>, McpError> {
    let name = connection_name.unwrap_or(&self.default_name);
    let state = self.connections.lock().await;
    match state.get(name) {
        Some(conn_state) => {
            match conn_state {
                ConnectionState::Connected(client) => Ok(Arc::clone(client)),
                ConnectionState::Pending(resolver) => {
                    let resolver = Arc::clone(resolver);
                    drop(state);
                    let url = resolver().map_err(|e| ...)?;
                    self.connect_with_url(name.to_string(), url).await
                }
                ConnectionState::Connecting(url) | ConnectionState::Unavailable(url) => {
                    let url = url.clone();
                    drop(state);
                    self.connect_with_url(name.to_string(), url).await
                }
            }
        }
        None => {
            let available: Vec<&String> = state.keys().collect();
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
}

// Backward compat: old get_client delegates to default
async fn get_client(&self) -> Result<Arc<tokio_opengauss::Client>, McpError> {
    self.get_client_for(None).await
}
```

**Step 4: Update connect_with_url to be name-aware**

```rust
async fn connect_with_url(&self, name: String, url: String) -> Result<Arc<tokio_opengauss::Client>, McpError> {
    let result = do_connect(&url).await;
    let mut conns = self.connections.lock().await;
    match result {
        Ok((client, _handle)) => {
            if let Some(ref cb) = self.on_connected.get(&name) {
                cb();
            }
            conns.insert(name, ConnectionState::Connected(Arc::clone(&client)));
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

**Step 5: Update try_connect to probe default connection**

The existing `try_connect` probes one connection. Update to only probe the default:

```rust
pub async fn try_connect(&self) {
    let (name, url) = {
        let state = self.connections.lock().await;
        match state.get(&self.default_name) {
            Some(ConnectionState::Connecting(url)) => (self.default_name.clone(), url.clone()),
            _ => return,
        }
    };
    // ... rest same, but insert with name
}
```

**Step 6: Commit**

```bash
git commit -m "feat(multi-conn): GaussdbMcp supports multiple named connections"
```

---

## Task 4: Update All MCP Tool Methods with connection_name Parameter

**Files:**
- Modify: `tools/gaussdb-mcp/src/server.rs`

**Step 1: Update tool parameter structs**

Add `connection_name: Option<String>` to each params struct:

```rust
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct GetTableMetadataParams {
    pub table_name: String,
    pub schema_name: Option<String>,
    pub connection_name: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ExecuteQueryParams {
    pub sql: String,
    pub connection_name: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct GetExecutionPlanParams {
    pub sql: String,
    pub analyze: Option<bool>,
    pub format: Option<String>,
    pub connection_name: Option<String>,
}
```

**Step 2: Update tool method signatures and bodies**

For tools WITHOUT explicit params (get_database_info, list_tables), add an optional parameter directly:

```rust
#[tool(description = "Get database version and server information")]
async fn get_database_info(
    &self,
    connection_name: Option<String>,
) -> Result<CallToolResult, McpError> {
    let client = self.get_client_for(connection_name.as_deref()).await?;
    // ... rest unchanged
}

#[tool(description = "List all user tables and views in the database")]
async fn list_tables(
    &self,
    connection_name: Option<String>,
) -> Result<CallToolResult, McpError> {
    let client = self.get_client_for(connection_name.as_deref()).await?;
    // ... rest unchanged
}
```

For tools WITH params structs:

```rust
#[tool(description = "Get column metadata, primary keys, and indexes for a specific table")]
async fn get_table_metadata(
    &self,
    Parameters(params): Parameters<GetTableMetadataParams>,
) -> Result<CallToolResult, McpError> {
    let conn_name = params.connection_name.as_deref();
    let client = self.get_client_for(conn_name).await?;
    // ... rest unchanged (use params.table_name, params.schema_name as before)
}

#[tool(description = "Execute a read-only SQL query (SELECT or EXPLAIN only)")]
async fn execute_query(
    &self,
    Parameters(params): Parameters<ExecuteQueryParams>,
) -> Result<CallToolResult, McpError> {
    let conn_name = params.connection_name.as_deref();
    let client = self.get_client_for(conn_name).await?;
    // ... rest unchanged
}

#[tool(description = "Get the execution plan for a SQL query")]
async fn get_execution_plan(
    &self,
    Parameters(params): Parameters<GetExecutionPlanParams>,
) -> Result<CallToolResult, McpError> {
    let conn_name = params.connection_name.as_deref();
    let client = self.get_client_for(conn_name).await?;
    // ... rest unchanged
}
```

**Step 3: Add list_connections tool**

New tool to discover available connections:

```rust
#[tool(description = "List all configured database connections")]
async fn list_connections(&self) -> Result<CallToolResult, McpError> {
    let state = self.connections.lock().await;
    let connections: Vec<serde_json::Value> = state.iter().map(|(name, conn_state)| {
        let status = match conn_state {
            ConnectionState::Connected(_) => "connected",
            ConnectionState::Connecting(_) => "connecting",
            ConnectionState::Pending(_) => "pending",
            ConnectionState::Unavailable(_) => "unavailable",
        };
        json!({
            "name": name,
            "status": status,
            "is_default": name == &self.default_name,
        })
    }).collect();
    Ok(CallToolResult::success(vec![Content::text(
        json!({
            "connections": connections,
            "default_connection": self.default_name,
        }).to_string(),
    )]))
}
```

**Step 4: Build and verify compilation**

Run: `cargo build -p gaussdb-mcp`
Expected: Clean compilation

**Step 5: Commit**

```bash
git commit -m "feat(multi-conn): add connection_name parameter to all MCP tools"
```

---

## Task 5: Update main() to Wire Multi-Connection Flow

**Files:**
- Modify: `tools/gaussdb-mcp/src/main.rs`

**Step 1: Update main() to use MultiConfig parsing**

Replace the config reading section in `main()` with `MultiConfig` parsing:

```rust
// In main(), replace resolve_connection_url_lazy() call with:
let server = match resolve_connections_lazy() {
    LazyResolvedConnections::Ready(resolved) => {
        let entries: Vec<(String, String)> = resolved.connections.iter()
            .map(|c| (c.name.clone(), c.connection_url.clone()))
            .collect();
        let server = GaussdbMcp::new_multi_disconnected(entries, resolved.default_name);

        // Set up password migration callbacks for each connection with plaintext passwords
        for conn in &resolved.connections {
            if conn.plaintext_password.is_some() {
                // ... migration callback per connection name
            }
        }

        // Probe default connection
        let probe = Arc::clone(&server);
        tokio::spawn(async move { probe.try_connect().await; });

        server
    }
    LazyResolvedConnections::Lazy { entries, default_name } => {
        GaussdbMcp::new_multi_lazy(entries, default_name)
    }
};
```

**Step 2: Update handle_store_password for multi-connection**

The `--store-password` CLI option should accept an optional `--name <connection_name>` to specify which connection's password to store.

**Step 3: Update handle_check_connection for multi-connection**

`--check-connection` should accept an optional connection name argument, or `--all` to check all.

**Step 4: Update print_help**

Add multi-connection examples to help text.

**Step 5: Commit**

```bash
git commit -m "feat(multi-conn): wire multi-connection config in main()"
```

---

## Task 6: Update on_connected Callback for Per-Connection Password Migration

**Files:**
- Modify: `tools/gaussdb-mcp/src/main.rs`
- Modify: `tools/gaussdb-mcp/src/server.rs`

**Step 1: Add set_on_connected method to GaussdbMcp**

```rust
pub fn set_on_connected(&mut self, name: String, callback: Arc<dyn Fn() + Send + Sync>) {
    self.on_connected.insert(name, callback);
}
```

**Step 2: In main(), register per-connection migration callbacks**

For each `ResolvedConnection` that has a plaintext password, register a migration callback specific to that connection's name.

**Step 3: Commit**

```bash
git commit -m "feat(multi-conn): per-connection password migration callbacks"
```

---

## Task 7: Build, Test, and Verify

**Step 1: Full build**

Run: `cargo build -p gaussdb-mcp`
Expected: Clean build, zero warnings

**Step 2: Run existing tests**

Run: `cargo test -p gaussdb-mcp`
Expected: All pass

**Step 3: Test backward compatibility**

Create a temp TOML file with old format, verify it still works:

```bash
cat > /tmp/test-old-config.toml << 'EOF'
host = "127.0.0.1"
port = 5432
user = "gaussdb"
dbname = "postgres"
EOF
./target/debug/gaussdb-mcp --config /tmp/test-old-config.toml --check-connection
```

**Step 4: Test new multi-connection format**

```bash
cat > /tmp/test-multi-config.toml << 'EOF'
default_connection = "dev"

[[connections]]
name = "dev"
host = "127.0.0.1"
user = "gaussdb"
dbname = "postgres"

[[connections]]
name = "prod"
host = "192.168.1.10"
user = "admin"
dbname = "production"
EOF
./target/debug/gaussdb-mcp --config /tmp/test-multi-config.toml --check-connection
```

**Step 5: Final commit**

```bash
git commit -m "chore: verify multi-connection builds and backward compatibility"
```

---

## Task 8: Update tools.rs (Legacy Module)

**Files:**
- Modify: `tools/gaussdb-mcp/src/tools.rs`

The `tools.rs` file contains a duplicate/legacy `Tools` struct. Check if it's used in `server.rs`. If not used (current `server.rs` has its own tool implementations), it can be left as-is or updated in a follow-up. If it IS used, apply the same `connection_name` pattern.

**Step 1: Check if tools.rs is imported**

```bash
grep -n "mod tools\|use.*tools" tools/gaussdb-mcp/src/server.rs
```

If no import found → skip this task (tools.rs is dead code).

**Step 2: If imported, update Tools struct similarly**

Add `connection_name` to all params and methods.

**Step 3: Commit**

```bash
git commit -m "feat(multi-conn): update tools.rs legacy module"
```
