use std::path::{Path, PathBuf};
use std::sync::Arc;

// ─── Re-exports from gaussdb::config ─────────────────────────────────

pub(crate) use gaussdb::config::{
    KEYRING_SENTINEL, MultiConfig, NamedConnection, PasswordSource, ResolvedConnection,
    TimeoutAction, TimeoutConfig,
};

// ─── MCP-specific constants ──────────────────────────────────────────

pub(crate) const KEYRING_SERVICE: &str = "gaussdb-mcp";

// ─── McpRawConfig wrapper (adds is_env_var) ─────────────────────────

pub(crate) struct McpRawConfig {
    pub(crate) connections: Vec<NamedConnection>,
    pub(crate) default_name: String,
    pub(crate) config_path: Option<PathBuf>,
    pub(crate) base_timeout: Option<TimeoutConfig>,
    pub(crate) is_env_var: bool,
}

// ─── Lazy connection entry (MCP-specific lifecycle) ──────────────────

pub(crate) enum LazyConnectionEntry {
    Ready(ResolvedConnection),
    Pending {
        name: String,
        resolver: Arc<dyn (Fn() -> Result<String, String>) + Send + Sync>,
        timeout_config: TimeoutConfig,
    },
}

// ─── MCP-specific config path helpers ────────────────────────────────

pub(crate) fn default_config_path() -> Option<PathBuf> {
    dirs::home_dir().map(|p| p.join(".gaussdb-mcp.toml"))
}

pub(crate) fn find_config_path(opt: Option<PathBuf>) -> Result<PathBuf, String> {
    match opt {
        Some(p) => Ok(p),
        None => match default_config_path() {
            Some(p) if p.exists() => Ok(p),
            _ => Err(
                "No connection configuration found. Use one of:\n\
                 \n\
                 \u{20} 1. Set GAUSSDB_URL or DATABASE_URL environment variable\n\
                 \u{20}    export GAUSSDB_URL=\"host=localhost user=postgres password=secret dbname=mydb\"\n\
                 \n\
                 \u{20} 2. Create ~/.gaussdb-mcp.toml config file:\n\
                 \u{20}    host = \"localhost\"\n\
                 \u{20}    user = \"postgres\"\n\
                 \u{20}    password = \"secret\"\n\
                 \u{20}    dbname = \"mydb\"\n\
                 \n\
                 \u{20} 3. Pass --config <path> to specify a config file\n\
                 \n\
                 \u{20} Password will be migrated to OS keychain on first successful connection."
                    .to_string(),
            ),
        },
    }
}

// ─── MCP-specific wrappers ───────────────────────────────────────────

pub(crate) fn read_config(config_path: Option<PathBuf>) -> Result<McpRawConfig, String> {
    if let Ok(url) = std::env::var("GAUSSDB_URL").or_else(|_| std::env::var("DATABASE_URL")) {
        let conn = NamedConnection {
            name: "default".to_string(),
            url: Some(url),
            host: None,
            port: None,
            user: None,
            password: None,
            dbname: None,
            sslmode: None,
            statement_timeout: None,
            connection_max_lifetime: None,
            timeout_action: None,
        };
        return Ok(McpRawConfig {
            connections: vec![conn],
            default_name: "default".to_string(),
            config_path: None,
            base_timeout: None,
            is_env_var: true,
        });
    }

    let config_path = find_config_path(config_path)?;
    let content = std::fs::read_to_string(&config_path).map_err(|e| {
        format!(
            "failed to read config file {}: {}",
            config_path.display(),
            e
        )
    })?;

    let config: MultiConfig = toml::from_str(&content).map_err(|e| {
        format!(
            "failed to parse config file {}: {}",
            config_path.display(),
            e
        )
    })?;

    let base_tc = TimeoutConfig::from_overrides(
        config.statement_timeout.as_deref(),
        config.connection_max_lifetime.as_deref(),
        config.timeout_action.as_deref(),
        None,
    )
    .ok();

    let (connections, default_name) = config.resolve()?;
    let default_name = default_name.unwrap_or_else(|| {
        connections
            .first()
            .map(|c| c.name.clone())
            .unwrap_or_default()
    });

    Ok(McpRawConfig {
        connections,
        default_name,
        config_path: Some(config_path),
        base_timeout: base_tc,
        is_env_var: false,
    })
}

pub(crate) fn resolve_single_connection(
    conn: &NamedConnection,
    config_path: Option<PathBuf>,
    base_tc: Option<&TimeoutConfig>,
) -> Result<ResolvedConnection, String> {
    gaussdb::config::resolve_single_connection(conn, config_path, base_tc, KEYRING_SERVICE)
        .map_err(|e| e.to_string())
}

pub(crate) fn store_keyring_password(username: &str, password: &str) -> Result<(), String> {
    gaussdb::config::store_keyring_password(username, password, KEYRING_SERVICE)
        .map_err(|e| e.to_string())
}

pub(crate) fn read_keyring_password(username: &str) -> Result<String, String> {
    let entry = keyring::Entry::new(KEYRING_SERVICE, username)
        .map_err(|e| format!("keyring entry creation failed: {}", e))?;
    entry.get_password().map_err(|e| {
        format!(
            "keyring password not found for '{}'. Store it first:\n  \
             gaussdb-mcp store-password <password> --config <path>\n  \
             or set password in config file as plaintext (will be migrated automatically).\n  \
             Keyring error: {}",
            username, e
        )
    })
}

pub(crate) fn rewrite_password_to_sentinel(
    config_path: &Path,
    connection_name: &str,
) -> Result<(), String> {
    gaussdb::config::rewrite_password_to_sentinel(config_path, connection_name)
        .map_err(|e| e.to_string())
}

pub(crate) fn resolve_env_var_connection(url: String) -> ResolvedConnection {
    gaussdb::config::resolve_env_var_connection(url)
}

// ─── MCP-specific lifecycle ──────────────────────────────────────────

pub(crate) fn build_lazy_resolver(
    conn: &NamedConnection,
    config_path: Option<PathBuf>,
    base_timeout: Option<&TimeoutConfig>,
) -> Result<LazyConnectionEntry, String> {
    let conn = conn.clone();
    let keyring_user = conn.keyring_username();

    let is_sentinel = conn.password.as_deref() == Some(KEYRING_SENTINEL);
    let is_plaintext = conn
        .password
        .as_ref()
        .is_some_and(|p| p != KEYRING_SENTINEL);

    if is_plaintext || conn.url.is_some() {
        let resolved = resolve_single_connection(&conn, config_path, base_timeout)?;
        return Ok(LazyConnectionEntry::Ready(resolved));
    }

    let password_source = if is_sentinel {
        PasswordSource::Keyring
    } else {
        PasswordSource::None
    };

    if conn.host.is_none() && conn.user.is_none() {
        return Err(format!(
            "connection '{}' must contain either `url` or at least `host`/`user` fields",
            conn.name
        ));
    }

    let host = conn.host.clone();
    let port = conn.port;
    let user = conn.user.clone();
    let dbname = conn.dbname.clone();
    let sslmode = conn.sslmode.clone();
    let name = conn.name.clone();

    let resolver = Arc::new(move || {
        let password = match password_source {
            PasswordSource::Keyring => Some(read_keyring_password(&keyring_user)?),
            PasswordSource::None => None,
            _ => unreachable!(),
        };

        let mut parts = Vec::new();
        if let Some(ref h) = host {
            parts.push(format!("host={}", h));
        }
        if let Some(p) = port {
            parts.push(format!("port={}", p));
        }
        if let Some(ref u) = user {
            parts.push(format!("user={}", u));
        }
        if let Some(pw) = password {
            parts.push(format!("password={}", pw));
        }
        if let Some(ref d) = dbname {
            parts.push(format!("dbname={}", d));
        }
        if let Some(ref s) = sslmode {
            parts.push(format!("sslmode={}", s));
        }

        Ok(parts.join(" "))
    });

    let timeout_config = conn.timeout_config(base_timeout)?;
    Ok(LazyConnectionEntry::Pending {
        name,
        resolver,
        timeout_config,
    })
}

pub(crate) fn resolve_all_connections_lazy(
    config_path: Option<PathBuf>,
) -> Result<(Vec<LazyConnectionEntry>, String), String> {
    if let Ok(url) = std::env::var("GAUSSDB_URL").or_else(|_| std::env::var("DATABASE_URL")) {
        let resolved = gaussdb::config::resolve_env_var_connection(url);
        return Ok((
            vec![LazyConnectionEntry::Ready(resolved)],
            "default".to_string(),
        ));
    }

    let raw = read_config(config_path)?;
    let mut entries = Vec::with_capacity(raw.connections.len());
    for conn in &raw.connections {
        entries.push(build_lazy_resolver(
            conn,
            raw.config_path.clone(),
            raw.base_timeout.as_ref(),
        )?);
    }

    Ok((entries, raw.default_name))
}
