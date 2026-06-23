use keyring::Entry;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

pub(crate) const KEYRING_SERVICE: &str = "gaussdb-mcp";
pub(crate) const KEYRING_SENTINEL: &str = "keyring";

// ─── Timeout types ─────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) enum TimeoutAction {
    #[default]
    Cancel,
    Disconnect,
}

impl TimeoutAction {
    pub(crate) fn from_str(s: &str) -> Result<Self, String> {
        match s.trim().to_lowercase().as_str() {
            "cancel" | "keep" => Ok(TimeoutAction::Cancel),
            "disconnect" | "drop" | "reconnect" => Ok(TimeoutAction::Disconnect),
            _ => Err(format!(
                "unknown timeout_action '{}': expected cancel/keep/disconnect/drop/reconnect",
                s
            )),
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct TimeoutConfig {
    pub(crate) statement_timeout: Option<Duration>,
    pub(crate) connection_max_lifetime: Option<Duration>,
    pub(crate) timeout_action: TimeoutAction,
}

pub(crate) const DEFAULT_STATEMENT_TIMEOUT_SECS: u64 = 600;

impl Default for TimeoutConfig {
    fn default() -> Self {
        TimeoutConfig {
            statement_timeout: Some(Duration::from_secs(DEFAULT_STATEMENT_TIMEOUT_SECS)),
            connection_max_lifetime: None,
            timeout_action: TimeoutAction::default(),
        }
    }
}

impl TimeoutConfig {
    /// Validate the timeout configuration.
    ///
    /// If both `statement_timeout` and `connection_max_lifetime` are set,
    /// `statement_timeout` must be ≤ `connection_max_lifetime`.
    pub(crate) fn validate(&self) -> Result<(), String> {
        match (self.statement_timeout, self.connection_max_lifetime) {
            (Some(sto), Some(cml)) if sto > cml => Err(format!(
                "statement_timeout ({:.0?}) exceeds connection_max_lifetime ({:.0?})",
                sto, cml
            )),
            _ => Ok(()),
        }
    }

    /// Build a `TimeoutConfig` from optional raw string overrides,
    /// inheriting unset fields from a base config.
    ///
    /// Calls `validate()` before returning.
    pub(crate) fn from_overrides(
        statement_timeout: Option<&str>,
        connection_max_lifetime: Option<&str>,
        timeout_action: Option<&str>,
        base: Option<&TimeoutConfig>,
    ) -> Result<Self, String> {
        let statement_timeout = match statement_timeout {
            Some(s) => {
                let d = crate::duration_parse::parse_duration(s)?;
                Some(d)
            }
            None => base
                .as_ref()
                .and_then(|b| b.statement_timeout)
                .or_else(|| Some(Duration::from_secs(DEFAULT_STATEMENT_TIMEOUT_SECS))),
        };

        let connection_max_lifetime = match connection_max_lifetime {
            Some(s) => {
                let d = crate::duration_parse::parse_duration(s)?;
                Some(d)
            }
            None => base.as_ref().and_then(|b| b.connection_max_lifetime),
        };

        let timeout_action = match timeout_action {
            Some(s) => TimeoutAction::from_str(s)?,
            None => base.map(|b| b.timeout_action).unwrap_or_default(),
        };

        let config = TimeoutConfig {
            statement_timeout,
            connection_max_lifetime,
            timeout_action,
        };

        config.validate()?;
        Ok(config)
    }
}

// ─── Connection types ───────────────────────────────────────────────

#[derive(Clone, Copy, Debug)]
pub(crate) enum PasswordSource {
    EnvVar,
    Plaintext,
    Keyring,
    None,
}

#[derive(Debug, Deserialize, Clone)]
pub(crate) struct NamedConnection {
    #[serde(skip)]
    pub(crate) name: String,
    pub(crate) url: Option<String>,
    pub(crate) host: Option<String>,
    pub(crate) port: Option<u16>,
    pub(crate) user: Option<String>,
    pub(crate) password: Option<String>,
    pub(crate) dbname: Option<String>,
    pub(crate) sslmode: Option<String>,

    /// Statement timeout (e.g. "30s", "5min"). Applied via SET statement_timeout after connect.
    pub(crate) statement_timeout: Option<String>,
    /// Connection max lifetime before forced reconnect (e.g. "10min").
    pub(crate) connection_max_lifetime: Option<String>,
    /// Action on timeout: "cancel" (default) or "disconnect".
    #[serde(default)]
    pub(crate) timeout_action: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct MultiConfig {
    pub(crate) url: Option<String>,
    pub(crate) host: Option<String>,
    pub(crate) port: Option<u16>,
    pub(crate) user: Option<String>,
    pub(crate) password: Option<String>,
    pub(crate) dbname: Option<String>,
    pub(crate) sslmode: Option<String>,

    pub(crate) statement_timeout: Option<String>,
    pub(crate) connection_max_lifetime: Option<String>,
    pub(crate) timeout_action: Option<String>,

    pub(crate) default_connection: Option<String>,
    #[serde(default)]
    pub(crate) connections: BTreeMap<String, NamedConnection>,
}

impl NamedConnection {
    pub(crate) fn keyring_username(&self) -> String {
        match (&self.user, &self.host, &self.dbname) {
            (Some(u), Some(h), Some(d)) => format!("{}@{}/{}", u, h, d),
            (Some(u), Some(h), None) => format!("{}@{}", u, h),
            (Some(u), _, _) => u.clone(),
            _ => "default".to_string(),
        }
    }

    pub(crate) fn to_connection_url(&self) -> Option<String> {
        if let Some(ref url) = self.url {
            return Some(url.clone());
        }

        if self.host.is_none() && self.user.is_none() {
            return None;
        }

        let mut parts = Vec::new();
        if let Some(ref host) = self.host {
            parts.push(format!("host={}", host));
        }
        if let Some(port) = self.port {
            parts.push(format!("port={}", port));
        }
        if let Some(ref user) = self.user {
            parts.push(format!("user={}", user));
        }
        if let Some(ref password) = self.password {
            parts.push(format!("password={}", password));
        }
        if let Some(ref dbname) = self.dbname {
            parts.push(format!("dbname={}", dbname));
        }
        if let Some(ref sslmode) = self.sslmode {
            parts.push(format!("sslmode={}", sslmode));
        }

        Some(parts.join(" "))
    }

    /// Build a TimeoutConfig from this connection's settings, inheriting
    /// unset fields from `base` (typically the flat-level MultiConfig defaults).
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
}

impl MultiConfig {
    pub(crate) fn resolve(self) -> Result<(Vec<NamedConnection>, Option<String>), String> {
        if !self.connections.is_empty() {
            let default = self
                .default_connection
                .clone()
                .or_else(|| self.connections.keys().next().cloned());
            let mut conns = Vec::with_capacity(self.connections.len());
            for (name, mut conn) in self.connections {
                conn.name = name;
                conns.push(conn);
            }
            return Ok((conns, default));
        }

        if self.host.is_none() && self.user.is_none() && self.url.is_none() {
            return Err(
                "config must contain either [connections.<name>] or flat host/user fields".into(),
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
}

pub(crate) struct ResolvedConnection {
    pub(crate) name: String,
    pub(crate) connection_url: String,
    pub(crate) config_path: Option<PathBuf>,
    pub(crate) plaintext_password: Option<String>,
    pub(crate) keyring_username: String,
    pub(crate) password_source: PasswordSource,
    /// Timeout settings parsed from config.
    pub(crate) timeout_config: TimeoutConfig,
}

pub(crate) enum LazyConnectionEntry {
    Ready(ResolvedConnection),
    Pending {
        name: String,
        resolver: Arc<dyn (Fn() -> Result<String, String>) + Send + Sync>,
        timeout_config: TimeoutConfig,
    },
}

pub(crate) fn read_keyring_password(username: &str) -> Result<String, String> {
    let entry = Entry::new(KEYRING_SERVICE, username)
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

pub(crate) fn store_keyring_password(username: &str, password: &str) -> Result<(), String> {
    let entry = Entry::new(KEYRING_SERVICE, username)
        .map_err(|e| format!("keyring entry creation failed: {}", e))?;
    entry
        .set_password(password)
        .map_err(|e| format!("keyring store failed: {}", e))?;
    let verified = entry.get_password().map_err(|e| {
        format!(
            "keyring verification failed (password was stored but cannot be read back): {}",
            e
        )
    })?;
    if verified != password {
        return Err("keyring verification failed: read-back mismatch".to_string());
    }
    Ok(())
}

fn rewrite_password_to_sentinel_content(content: &str, connection_name: &str) -> String {
    let mut new_content = String::new();
    let mut replaced = false;

    let target_bare = format!("[connections.{}]", connection_name);
    let target_quoted = format!("[connections.\"{}\"]", connection_name);
    let has_connections_section = content.contains("[connections.");

    let mut in_target_section = false;

    for line in content.lines() {
        let trimmed = line.trim();

        if trimmed.starts_with('[') {
            in_target_section = trimmed == target_bare || trimmed == target_quoted;
        }

        let should_replace = if has_connections_section {
            in_target_section
        } else {
            !replaced
        };

        if should_replace && !replaced && trimmed.starts_with("password") && trimmed.contains('=') {
            let indent = &line[..line.find("password").unwrap_or(0)];
            new_content.push_str(&format!("{}password = \"{}\"", indent, KEYRING_SENTINEL));
            replaced = true;
        } else {
            new_content.push_str(line);
        }
        new_content.push('\n');
    }

    if content.ends_with('\n') && new_content.ends_with("\n\n") {
        new_content.pop();
    }

    new_content
}

pub(crate) fn rewrite_password_to_sentinel(
    path: &std::path::Path,
    connection_name: &str,
) -> std::io::Result<()> {
    let content = std::fs::read_to_string(path)?;
    let new_content = rewrite_password_to_sentinel_content(&content, connection_name);
    std::fs::write(path, new_content)
}

pub(crate) fn default_config_path() -> Option<PathBuf> {
    dirs::home_dir().map(|p| p.join(".gaussdb-mcp.toml"))
}

pub(crate) fn find_config_path(config_path: Option<PathBuf>) -> Result<PathBuf, String> {
    match config_path {
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

pub(crate) fn resolve_single_connection(
    conn: &NamedConnection,
    config_path: Option<PathBuf>,
    base_timeout: Option<&TimeoutConfig>,
) -> Result<ResolvedConnection, String> {
    let mut conn = conn.clone();
    let keyring_user = conn.keyring_username();

    let is_sentinel = conn.password.as_deref() == Some(KEYRING_SENTINEL);
    let is_plaintext = conn
        .password
        .as_ref()
        .is_some_and(|p| p != KEYRING_SENTINEL);
    let has_no_password = conn.password.is_none();

    let password_source = if is_plaintext {
        PasswordSource::Plaintext
    } else if is_sentinel {
        PasswordSource::Keyring
    } else {
        PasswordSource::None
    };

    if is_sentinel || has_no_password {
        let pw = read_keyring_password(&keyring_user)?;
        conn.password = Some(pw);
    }

    let plaintext_password = if is_plaintext {
        conn.password.clone()
    } else {
        None
    };

    let connection_url = conn.to_connection_url().ok_or_else(|| {
        format!(
            "connection '{}' must contain either `url` or at least `host`/`user` fields",
            conn.name
        )
    })?;

    let timeout_config = conn.timeout_config(base_timeout)?;

    Ok(ResolvedConnection {
        name: conn.name.clone(),
        connection_url,
        config_path,
        plaintext_password,
        keyring_username: keyring_user,
        password_source,
        timeout_config,
    })
}

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

pub(crate) struct RawConfig {
    pub(crate) connections: Vec<NamedConnection>,
    pub(crate) default_name: String,
    pub(crate) config_path: Option<PathBuf>,
    pub(crate) base_timeout: Option<TimeoutConfig>,
    pub(crate) is_env_var: bool,
}

pub(crate) fn read_config(config_path: Option<PathBuf>) -> Result<RawConfig, String> {
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
        return Ok(RawConfig {
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

    Ok(RawConfig {
        connections,
        default_name,
        config_path: Some(config_path),
        base_timeout: base_tc,
        is_env_var: false,
    })
}

pub(crate) fn resolve_env_var_connection(url: String) -> ResolvedConnection {
    ResolvedConnection {
        name: "default".to_string(),
        connection_url: url,
        config_path: None,
        plaintext_password: None,
        keyring_username: String::new(),
        password_source: PasswordSource::EnvVar,
        timeout_config: TimeoutConfig::default(),
    }
}

pub(crate) fn resolve_all_connections_lazy(
    config_path: Option<PathBuf>,
) -> Result<(Vec<LazyConnectionEntry>, String), String> {
    if let Ok(url) = std::env::var("GAUSSDB_URL").or_else(|_| std::env::var("DATABASE_URL")) {
        let resolved = ResolvedConnection {
            name: "default".to_string(),
            connection_url: url,
            config_path: None,
            plaintext_password: None,
            keyring_username: String::new(),
            password_source: PasswordSource::EnvVar,
            timeout_config: TimeoutConfig::default(),
        };
        return Ok((
            vec![LazyConnectionEntry::Ready(resolved)],
            "default".to_string(),
        ));
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

    // Build flat-level base timeout config to inherit into named connections.
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

    let mut entries = Vec::with_capacity(connections.len());
    for conn in &connections {
        entries.push(build_lazy_resolver(
            conn,
            Some(config_path.clone()),
            base_tc.as_ref(),
        )?);
    }

    Ok((entries, default_name))
}

#[cfg(test)]
mod timeout_tests {
    use super::*;

    // ── validate() tests ─────────────────────────────────────────

    #[test]
    fn validate_ok_when_statement_le_lifetime() {
        let config = TimeoutConfig {
            statement_timeout: Some(Duration::from_secs(30)),
            connection_max_lifetime: Some(Duration::from_secs(60)),
            timeout_action: TimeoutAction::Cancel,
        };
        assert!(config.validate().is_ok());
    }

    #[test]
    fn validate_fails_when_statement_gt_lifetime() {
        let config = TimeoutConfig {
            statement_timeout: Some(Duration::from_secs(120)),
            connection_max_lifetime: Some(Duration::from_secs(60)),
            timeout_action: TimeoutAction::Cancel,
        };
        let err = config.validate().unwrap_err();
        assert!(!err.is_empty(), "error message should not be empty");
    }

    #[test]
    fn validate_ok_with_only_statement_timeout() {
        let config = TimeoutConfig {
            statement_timeout: Some(Duration::from_secs(30)),
            connection_max_lifetime: None,
            timeout_action: TimeoutAction::Cancel,
        };
        assert!(config.validate().is_ok());
    }

    // ── TimeoutAction::from_str tests ────────────────────────────

    #[test]
    fn timeout_action_parses_aliases() {
        assert_eq!(
            TimeoutAction::from_str("cancel").unwrap(),
            TimeoutAction::Cancel
        );
        assert_eq!(
            TimeoutAction::from_str("CANCEL").unwrap(),
            TimeoutAction::Cancel
        );
        assert_eq!(
            TimeoutAction::from_str("keep").unwrap(),
            TimeoutAction::Cancel
        );
        assert_eq!(
            TimeoutAction::from_str("disconnect").unwrap(),
            TimeoutAction::Disconnect
        );
        assert_eq!(
            TimeoutAction::from_str("DISCONNECT").unwrap(),
            TimeoutAction::Disconnect
        );
        assert_eq!(
            TimeoutAction::from_str("drop").unwrap(),
            TimeoutAction::Disconnect
        );
        assert_eq!(
            TimeoutAction::from_str("reconnect").unwrap(),
            TimeoutAction::Disconnect
        );
        let err = TimeoutAction::from_str("invalid").unwrap_err();
        assert!(!err.is_empty(), "error message should not be empty");
    }

    // ── from_overrides() tests ───────────────────────────────────

    #[test]
    fn from_overrides_inherits_unset_fields_from_base() {
        let base = TimeoutConfig {
            statement_timeout: Some(Duration::from_secs(30)),
            connection_max_lifetime: Some(Duration::from_secs(300)),
            timeout_action: TimeoutAction::Disconnect,
        };
        let config = TimeoutConfig::from_overrides(None, None, None, Some(&base)).unwrap();
        assert_eq!(config.statement_timeout, base.statement_timeout);
        assert_eq!(config.connection_max_lifetime, base.connection_max_lifetime);
        assert_eq!(config.timeout_action, base.timeout_action);
    }

    #[test]
    fn from_overrides_overrides_set_fields() {
        let base = TimeoutConfig {
            statement_timeout: Some(Duration::from_secs(30)),
            connection_max_lifetime: Some(Duration::from_secs(300)),
            timeout_action: TimeoutAction::Cancel,
        };
        let config = TimeoutConfig::from_overrides(
            Some("60s"),
            Some("600s"),
            Some("disconnect"),
            Some(&base),
        )
        .unwrap();
        assert_eq!(config.statement_timeout, Some(Duration::from_secs(60)));
        assert_eq!(
            config.connection_max_lifetime,
            Some(Duration::from_secs(600))
        );
        assert_eq!(config.timeout_action, TimeoutAction::Disconnect);
    }

    #[test]
    fn default_has_600s_statement_timeout() {
        let config = TimeoutConfig::default();
        assert_eq!(
            config.statement_timeout,
            Some(Duration::from_secs(DEFAULT_STATEMENT_TIMEOUT_SECS))
        );
        assert_eq!(config.connection_max_lifetime, None);
        assert_eq!(config.timeout_action, TimeoutAction::Cancel);
    }

    #[test]
    fn from_overrides_without_base_uses_600s_default() {
        let config = TimeoutConfig::from_overrides(None, None, None, None).unwrap();
        assert_eq!(
            config.statement_timeout,
            Some(Duration::from_secs(DEFAULT_STATEMENT_TIMEOUT_SECS))
        );
    }
}

#[cfg(test)]
mod connection_config_tests {
    use super::*;

    #[test]
    fn parse_table_format_connections() {
        let toml = r#"
default_connection = "dev"

[connections.dev]
host = "localhost"
user = "gaussdb"
password = "secret"
dbname = "postgres"

[connections.prod]
host = "10.0.0.1"
user = "admin"
password = "keyring"
dbname = "production"
"#;
        let config: MultiConfig = toml::from_str(toml).unwrap();
        let (conns, default) = config.resolve().unwrap();
        assert_eq!(default.as_deref(), Some("dev"));
        assert_eq!(conns.len(), 2);

        let dev = conns.iter().find(|c| c.name == "dev").unwrap();
        assert_eq!(dev.host.as_deref(), Some("localhost"));
        assert_eq!(dev.password.as_deref(), Some("secret"));

        let prod = conns.iter().find(|c| c.name == "prod").unwrap();
        assert_eq!(prod.host.as_deref(), Some("10.0.0.1"));
        assert_eq!(prod.password.as_deref(), Some("keyring"));
    }

    #[test]
    fn parse_flat_format_single_connection() {
        let toml = r#"
host = "localhost"
user = "gaussdb"
password = "secret"
dbname = "postgres"
"#;
        let config: MultiConfig = toml::from_str(toml).unwrap();
        let (conns, default) = config.resolve().unwrap();
        assert_eq!(conns.len(), 1);
        assert_eq!(conns[0].name, "default");
        assert_eq!(default.as_deref(), Some("default"));
    }

    #[test]
    fn parse_quoted_connection_name() {
        let toml = r#"
[connections."my-prod"]
host = "localhost"
"#;
        let config: MultiConfig = toml::from_str(toml).unwrap();
        let (conns, _) = config.resolve().unwrap();
        assert_eq!(conns[0].name, "my-prod");
    }

    #[test]
    fn rewrite_targets_correct_section_in_multi_connection() {
        let content = r#"
[connections.dev]
host = "localhost"
password = "already_keyring"

[connections.ogagila]
host = "localhost"
password = "Enmo@123"
"#;
        let result = rewrite_password_to_sentinel_content(content, "ogagila");
        let dev_section = result.split("[connections.ogagila]").next().unwrap();
        assert!(
            dev_section.contains("password = \"already_keyring\""),
            "dev's password must be untouched"
        );
        assert!(
            result.contains("[connections.ogagila]\nhost = \"localhost\"\npassword = \"keyring\""),
            "ogagila's password must be rewritten to sentinel"
        );
    }

    #[test]
    fn rewrite_flat_config_replaces_first_password() {
        let content = "host = \"localhost\"\npassword = \"secret\"\ndbname = \"postgres\"\n";
        let result = rewrite_password_to_sentinel_content(content, "ignored");
        assert!(result.contains("password = \"keyring\""));
        assert!(!result.contains("password = \"secret\""));
    }

    #[test]
    fn rewrite_does_not_touch_other_sections() {
        let content = r#"
[connections.dev]
password = "dev_secret"

[connections.staging]
password = "staging_secret"
"#;
        let result = rewrite_password_to_sentinel_content(content, "dev");
        assert!(result.contains("password = \"keyring\""));
        let after_keyring = result.split("password = \"keyring\"").nth(1).unwrap();
        assert!(
            after_keyring.contains("password = \"staging_secret\""),
            "staging password must remain plaintext"
        );
    }
}
