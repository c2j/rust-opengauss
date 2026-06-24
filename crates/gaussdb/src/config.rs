use keyring::Entry;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

// ─── SslMode ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum SslMode {
    #[default]
    Disable,
    Prefer,
    Require,
    VerifyCa,
    VerifyFull,
}

// ─── Error types ─────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("config file not found at {searched_path}")]
    ConfigNotFound { searched_path: PathBuf },
    #[error("failed to parse config at {path}")]
    ConfigParse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error("connection '{name}' not found; available: {available:?}")]
    ConnectionNotFound {
        name: String,
        available: Vec<String>,
    },
    #[error("keyring error for user '{username}'")]
    Keyring {
        username: String,
        #[source]
        source: keyring::Error,
    },
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Generic(String),
}

#[derive(Debug, thiserror::Error)]
pub enum ConnectError {
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[cfg(feature = "tls-native-tls")]
    #[error("TLS initialization failed")]
    Tls(#[from] native_tls::Error),
    #[error("sslmode '{sslmode:?}' requires 'tls-native-tls' feature, which is not enabled")]
    TlsFeatureMissing { sslmode: SslMode },
    #[error("connection failed: {0}")]
    Driver(Box<dyn std::error::Error + Send + Sync>),
}

// ─── Constants ───────────────────────────────────────────────────────

pub const KEYRING_SENTINEL: &str = "keyring";
pub const DEFAULT_STATEMENT_TIMEOUT_SECS: u64 = 600;

// ─── Timeout types ───────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum TimeoutAction {
    #[default]
    Cancel,
    Disconnect,
}

impl std::str::FromStr for TimeoutAction {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
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
pub struct TimeoutConfig {
    pub statement_timeout: Option<Duration>,
    pub connection_max_lifetime: Option<Duration>,
    pub timeout_action: TimeoutAction,
}

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
    pub fn validate(&self) -> Result<(), String> {
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
    pub fn from_overrides(
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
            Some(s) => s.parse::<TimeoutAction>()?,
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

// ─── Password type ───────────────────────────────────────────────────

#[derive(Clone, Copy, Debug)]
pub enum PasswordSource {
    EnvVar,
    Plaintext,
    Keyring,
    None,
}

// ─── Connection types ────────────────────────────────────────────────

#[derive(Debug, Deserialize, Clone)]
pub struct NamedConnection {
    #[serde(skip)]
    pub name: String,
    pub url: Option<String>,
    pub host: Option<String>,
    pub port: Option<u16>,
    pub user: Option<String>,
    pub password: Option<String>,
    pub dbname: Option<String>,
    pub sslmode: Option<String>,

    /// Statement timeout (e.g. "30s", "5min"). Applied via SET statement_timeout after connect.
    pub statement_timeout: Option<String>,
    /// Connection max lifetime before forced reconnect (e.g. "10min").
    pub connection_max_lifetime: Option<String>,
    /// Action on timeout: "cancel" (default) or "disconnect".
    #[serde(default)]
    pub timeout_action: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct MultiConfig {
    pub url: Option<String>,
    pub host: Option<String>,
    pub port: Option<u16>,
    pub user: Option<String>,
    pub password: Option<String>,
    pub dbname: Option<String>,
    pub sslmode: Option<String>,

    pub statement_timeout: Option<String>,
    pub connection_max_lifetime: Option<String>,
    pub timeout_action: Option<String>,

    pub default_connection: Option<String>,
    #[serde(default)]
    pub connections: BTreeMap<String, NamedConnection>,
}

#[derive(Debug)]
pub struct ResolvedConnection {
    pub name: String,
    pub connection_url: String,
    pub config_path: Option<PathBuf>,
    pub plaintext_password: Option<String>,
    pub keyring_username: String,
    pub password_source: PasswordSource,
    pub sslmode: SslMode,
    pub timeout_config: TimeoutConfig,
}

pub struct RawConfig {
    pub connections: Vec<NamedConnection>,
    pub default_name: String,
    pub config_path: Option<PathBuf>,
    pub base_timeout: Option<TimeoutConfig>,
}

// ─── NamedConnection impls ───────────────────────────────────────────

impl NamedConnection {
    pub fn keyring_username(&self) -> String {
        match (&self.user, &self.host, &self.dbname) {
            (Some(u), Some(h), Some(d)) => format!("{}@{}/{}", u, h, d),
            (Some(u), Some(h), None) => format!("{}@{}", u, h),
            (Some(u), _, _) => u.clone(),
            _ => "default".to_string(),
        }
    }

    pub fn to_connection_url(&self) -> Option<String> {
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
    pub fn timeout_config(&self, base: Option<&TimeoutConfig>) -> Result<TimeoutConfig, String> {
        TimeoutConfig::from_overrides(
            self.statement_timeout.as_deref(),
            self.connection_max_lifetime.as_deref(),
            self.timeout_action.as_deref(),
            base,
        )
    }
}

// ─── MultiConfig impls ───────────────────────────────────────────────

impl MultiConfig {
    pub fn resolve(self) -> Result<(Vec<NamedConnection>, Option<String>), String> {
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

// ─── SslMode helpers ─────────────────────────────────────────────────

pub fn sslmode_from_url(dsn: &str) -> SslMode {
    // Try URI query params first (postgres://user:pass@host/db?sslmode=require)
    if let Some(query) = dsn.split('?').nth(1) {
        for param in query.split('&') {
            if let Some(val) = param.strip_prefix("sslmode=") {
                return sslmode_from_value(Some(val));
            }
        }
    }

    // Try space-separated key=value pairs (host=localhost sslmode=require)
    for part in dsn.split_whitespace() {
        if let Some(val) = part.strip_prefix("sslmode=") {
            return sslmode_from_value(Some(val));
        }
    }

    SslMode::Disable
}

pub fn sslmode_from_value(val: Option<&str>) -> SslMode {
    match val.map(|s| s.trim().to_lowercase()).as_deref() {
        Some("disable") => SslMode::Disable,
        Some("prefer") => SslMode::Prefer,
        Some("require") => SslMode::Require,
        Some("verify-ca") | Some("verify_ca") => SslMode::VerifyCa,
        Some("verify-full") | Some("verify_full") => SslMode::VerifyFull,
        _ => SslMode::Disable,
    }
}

// ─── Keyring helpers ─────────────────────────────────────────────────

fn read_keyring_password(username: &str, service: &str) -> Result<String, ConfigError> {
    let entry = Entry::new(service, username).map_err(|e| ConfigError::Keyring {
        username: username.to_string(),
        source: e,
    })?;
    entry.get_password().map_err(|e| {
        ConfigError::Generic(format!(
            "keyring entry not found for user '{}'; ensure password is stored: {}",
            username, e
        ))
    })
}

pub fn store_keyring_password(
    username: &str,
    password: &str,
    service: &str,
) -> Result<(), ConfigError> {
    let entry = Entry::new(service, username).map_err(|e| ConfigError::Keyring {
        username: username.to_string(),
        source: e,
    })?;
    entry
        .set_password(password)
        .map_err(|e| ConfigError::Keyring {
            username: username.to_string(),
            source: e,
        })?;
    let verified = entry.get_password().map_err(|e| {
        ConfigError::Generic(format!(
            "keyring verification failed (password was stored but cannot be read back): {}",
            e
        ))
    })?;
    if verified != password {
        return Err(ConfigError::Generic(
            "keyring verification failed: read-back mismatch".to_string(),
        ));
    }
    Ok(())
}

// ─── Password rewrite helpers ────────────────────────────────────────

pub fn rewrite_password_to_sentinel_content(content: &str, connection_name: &str) -> String {
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

pub fn rewrite_password_to_sentinel(
    path: &std::path::Path,
    connection_name: &str,
) -> std::io::Result<()> {
    let content = std::fs::read_to_string(path)?;
    let new_content = rewrite_password_to_sentinel_content(&content, connection_name);
    std::fs::write(path, new_content)
}

// ─── Config path resolution ──────────────────────────────────────────

fn default_config_path(filename: &str) -> Option<PathBuf> {
    dirs::home_dir().map(|p| {
        let mut path = p;
        path.push(format!(".{}", filename));
        path
    })
}

fn find_config_path(config_path: Option<PathBuf>, filename: &str) -> Result<PathBuf, ConfigError> {
    match config_path {
        Some(p) => Ok(p),
        None => match default_config_path(filename) {
            Some(p) if p.exists() => Ok(p),
            _ => Err(ConfigError::ConfigNotFound {
                searched_path: default_config_path(filename)
                    .unwrap_or_else(|| PathBuf::from(filename)),
            }),
        },
    }
}

// ─── Single connection resolution ────────────────────────────────────

pub fn resolve_single_connection(
    conn: &NamedConnection,
    config_path: Option<PathBuf>,
    base_timeout: Option<&TimeoutConfig>,
    service: &str,
) -> Result<ResolvedConnection, ConfigError> {
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
        let pw = read_keyring_password(&keyring_user, service)?;
        conn.password = Some(pw);
    }

    let plaintext_password = if is_plaintext {
        conn.password.clone()
    } else {
        None
    };

    let connection_url = conn.to_connection_url().ok_or_else(|| {
        ConfigError::Generic(format!(
            "connection '{}' must contain either `url` or at least `host`/`user` fields",
            conn.name
        ))
    })?;

    let sslmode = conn
        .sslmode
        .as_deref()
        .map_or(SslMode::Disable, |v| sslmode_from_value(Some(v)));

    let timeout_config = conn
        .timeout_config(base_timeout)
        .map_err(|e| ConfigError::Generic(format!("invalid timeout config: {}", e)))?;

    Ok(ResolvedConnection {
        name: conn.name.clone(),
        connection_url,
        config_path,
        plaintext_password,
        keyring_username: keyring_user,
        password_source,
        sslmode,
        timeout_config,
    })
}

// ─── Config reading ──────────────────────────────────────────────────

pub fn read_config(config_path: Option<PathBuf>, _service: &str) -> Result<RawConfig, ConfigError> {
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
        });
    }

    let config_path = find_config_path(config_path, "gaussdb.toml")?;
    let content = std::fs::read_to_string(&config_path)?;

    let config: MultiConfig = toml::from_str(&content).map_err(|e| ConfigError::ConfigParse {
        path: config_path.clone(),
        source: e,
    })?;

    let base_tc = TimeoutConfig::from_overrides(
        config.statement_timeout.as_deref(),
        config.connection_max_lifetime.as_deref(),
        config.timeout_action.as_deref(),
        None,
    )
    .ok();

    let (connections, default_name) = config.resolve().map_err(ConfigError::Generic)?;
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
    })
}

/// Resolve an env-var-style connection URL into a `ResolvedConnection`.
///
/// Parses sslmode from the URL string and sets password source to `EnvVar`.
pub fn resolve_env_var_connection(url: String) -> ResolvedConnection {
    let sslmode = sslmode_from_url(&url);
    ResolvedConnection {
        name: "default".to_string(),
        connection_url: url,
        config_path: None,
        plaintext_password: None,
        keyring_username: String::new(),
        password_source: PasswordSource::EnvVar,
        sslmode,
        timeout_config: TimeoutConfig::default(),
    }
}

// ─── Public resolve API ──────────────────────────────────────────────

/// Resolve a connection from a DSN, config file path, and optional connection name.
///
/// If `dsn` is provided, it is used as a direct connection URL (bypasses config files).
/// Otherwise, the config file at `path` (or the default location) is read. If `name` is
/// provided, that named connection is selected; otherwise the default connection is used.
pub fn resolve(
    dsn: Option<&str>,
    path: Option<&Path>,
    name: Option<&str>,
) -> Result<ResolvedConnection, ConfigError> {
    if let Some(url) = dsn {
        return Ok(resolve_env_var_connection(url.to_string()));
    }

    let raw = read_config(path.map(|p| p.to_path_buf()), "gaussdb")?;
    let target_name = name.unwrap_or(&raw.default_name).to_string();
    let conn = raw
        .connections
        .iter()
        .find(|c| c.name == target_name)
        .ok_or_else(|| ConfigError::ConnectionNotFound {
            name: target_name,
            available: raw.connections.iter().map(|c| c.name.clone()).collect(),
        })?;

    resolve_single_connection(
        conn,
        raw.config_path.clone(),
        raw.base_timeout.as_ref(),
        "gaussdb",
    )
}

// ─── connect_async ───────────────────────────────────────────────────

/// Connect to the database asynchronously.
///
/// Resolves the connection using [`resolve`], then selects the appropriate TLS
/// connector based on the `SslMode`. If `sslmode` is `Disable`, `NoTls` is used.
/// Otherwise, a `native-tls` connector is built with the correct certificate
/// verification settings.
///
/// Requires the `runtime` feature (enabled by default).
#[cfg(feature = "runtime")]
pub async fn connect_async(
    dsn: Option<&str>,
    path: Option<&Path>,
    name: Option<&str>,
) -> Result<crate::Client, ConnectError> {
    let resolved = resolve(dsn, path, name)?;

    match resolved.sslmode {
        SslMode::Disable => {
            let mut config: crate::Config = resolved
                .connection_url
                .parse()
                .map_err(|e: crate::Error| ConnectError::Driver(e.into()))?;
            config.ssl_mode(crate::driver::config::SslMode::Disable);
            let (client, connection) = config
                .connect(crate::NoTls)
                .await
                .map_err(|e: crate::Error| ConnectError::Driver(e.into()))?;
            tokio::spawn(connection);
            Ok(client)
        }
        _ => {
            #[cfg(not(feature = "tls-native-tls"))]
            {
                let _ = resolved;
                return Err(ConnectError::TlsFeatureMissing {
                    sslmode: resolved.sslmode,
                });
            }

            #[cfg(feature = "tls-native-tls")]
            {
                let accept_invalid_certs =
                    matches!(resolved.sslmode, SslMode::Prefer | SslMode::Require);
                let accept_invalid_hostnames = matches!(
                    resolved.sslmode,
                    SslMode::Prefer | SslMode::Require | SslMode::VerifyCa
                );

                let mut builder = native_tls::TlsConnector::builder();
                if accept_invalid_certs {
                    builder.danger_accept_invalid_certs(true);
                }
                if accept_invalid_hostnames {
                    builder.danger_accept_invalid_hostnames(true);
                }
                let connector = builder.build()?;
                let tls = crate::native_tls::MakeTlsConnector::new(connector);

                let mut config: crate::Config = resolved
                    .connection_url
                    .parse()
                    .map_err(|e: crate::Error| ConnectError::Driver(e.into()))?;
                config.ssl_mode(crate::driver::config::SslMode::Require);
                let (client, connection) = config
                    .connect(tls)
                    .await
                    .map_err(|e| ConnectError::Driver(e.into()))?;
                tokio::spawn(connection);
                Ok(client)
            }
        }
    }
}

// ─── connect_sync ────────────────────────────────────────────────────

/// Connect to the database synchronously.
///
/// Resolves the connection using [`resolve`], then selects the appropriate TLS
/// connector based on the `SslMode`. If `sslmode` is `Disable`, `NoTls` is used.
/// Otherwise, a `native-tls` connector is built with the correct certificate
/// verification settings.
///
/// Requires the `sync` feature.
#[cfg(feature = "sync")]
pub fn connect_sync(
    dsn: Option<&str>,
    path: Option<&Path>,
    name: Option<&str>,
) -> Result<crate::sync::Client, ConnectError> {
    let resolved = resolve(dsn, path, name)?;

    match resolved.sslmode {
        SslMode::Disable => {
            let mut async_config: crate::Config = resolved
                .connection_url
                .parse()
                .map_err(|e: crate::Error| ConnectError::Driver(e.into()))?;
            async_config.ssl_mode(crate::driver::config::SslMode::Disable);
            let sync_config: crate::sync::Config = async_config.into();
            let client = sync_config
                .connect(crate::NoTls)
                .map_err(|e: crate::sync::Error| ConnectError::Driver(e.into()))?;
            Ok(client)
        }
        _ => {
            #[cfg(not(feature = "tls-native-tls"))]
            {
                let _ = resolved;
                return Err(ConnectError::TlsFeatureMissing {
                    sslmode: resolved.sslmode,
                });
            }

            #[cfg(feature = "tls-native-tls")]
            {
                let accept_invalid_certs =
                    matches!(resolved.sslmode, SslMode::Prefer | SslMode::Require);
                let accept_invalid_hostnames = matches!(
                    resolved.sslmode,
                    SslMode::Prefer | SslMode::Require | SslMode::VerifyCa
                );

                let mut builder = native_tls::TlsConnector::builder();
                if accept_invalid_certs {
                    builder.danger_accept_invalid_certs(true);
                }
                if accept_invalid_hostnames {
                    builder.danger_accept_invalid_hostnames(true);
                }
                let connector = builder.build()?;
                let tls = crate::native_tls::MakeTlsConnector::new(connector);

                let mut async_config: crate::Config = resolved
                    .connection_url
                    .parse()
                    .map_err(|e: crate::Error| ConnectError::Driver(e.into()))?;
                async_config.ssl_mode(crate::driver::config::SslMode::Require);
                let sync_config: crate::sync::Config = async_config.into();
                let client = sync_config
                    .connect(tls)
                    .map_err(|e: crate::sync::Error| ConnectError::Driver(e.into()))?;
                Ok(client)
            }
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod timeout_tests {
    use super::*;
    use std::str::FromStr;

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

#[cfg(test)]
mod sslmode_tests {
    use super::*;

    #[test]
    fn sslmode_from_url_libpq_format_all_modes() {
        assert_eq!(
            sslmode_from_url("host=localhost sslmode=disable"),
            SslMode::Disable
        );
        assert_eq!(
            sslmode_from_url("host=localhost sslmode=prefer"),
            SslMode::Prefer
        );
        assert_eq!(
            sslmode_from_url("host=localhost sslmode=require"),
            SslMode::Require
        );
        assert_eq!(
            sslmode_from_url("host=localhost sslmode=verify-ca"),
            SslMode::VerifyCa
        );
        assert_eq!(
            sslmode_from_url("host=localhost sslmode=verify-full"),
            SslMode::VerifyFull
        );
    }

    #[test]
    fn sslmode_from_url_absent_defaults_disable() {
        assert_eq!(
            sslmode_from_url("host=localhost user=admin"),
            SslMode::Disable
        );
        assert_eq!(sslmode_from_url(""), SslMode::Disable);
    }

    #[test]
    fn sslmode_from_url_query_format() {
        assert_eq!(
            sslmode_from_url("postgres://user:pass@host/db?sslmode=require"),
            SslMode::Require
        );
        assert_eq!(
            sslmode_from_url("postgres://user@host/db?sslmode=verify-full&connect_timeout=10"),
            SslMode::VerifyFull
        );
        assert_eq!(
            sslmode_from_url("postgres://user@host/db?sslmode=disable"),
            SslMode::Disable
        );
    }

    #[test]
    fn sslmode_from_value_variations() {
        assert_eq!(sslmode_from_value(None), SslMode::Disable);
        assert_eq!(sslmode_from_value(Some("disable")), SslMode::Disable);
        assert_eq!(sslmode_from_value(Some("Disable")), SslMode::Disable);
        assert_eq!(sslmode_from_value(Some("DISABLE")), SslMode::Disable);
        assert_eq!(sslmode_from_value(Some("prefer")), SslMode::Prefer);
        assert_eq!(sslmode_from_value(Some("require")), SslMode::Require);
        assert_eq!(sslmode_from_value(Some("verify-ca")), SslMode::VerifyCa);
        assert_eq!(sslmode_from_value(Some("verify_ca")), SslMode::VerifyCa);
        assert_eq!(sslmode_from_value(Some("verify-full")), SslMode::VerifyFull);
        assert_eq!(sslmode_from_value(Some("verify_full")), SslMode::VerifyFull);
        assert_eq!(sslmode_from_value(Some("unknown")), SslMode::Disable);
        assert_eq!(sslmode_from_value(Some("")), SslMode::Disable);
    }

    #[test]
    fn resolve_env_var_parses_sslmode() {
        let resolved =
            resolve_env_var_connection("host=localhost user=admin sslmode=require".to_string());
        assert_eq!(resolved.sslmode, SslMode::Require);
        assert_eq!(resolved.password_source as u8, PasswordSource::EnvVar as u8);
        assert_eq!(resolved.name, "default");

        let resolved2 = resolve_env_var_connection("host=localhost user=admin".to_string());
        assert_eq!(resolved2.sslmode, SslMode::Disable);
    }

    #[test]
    fn resolve_env_var_connection_basic() {
        let resolved =
            resolve_env_var_connection("host=localhost user=test password=secret".to_string());
        assert_eq!(resolved.name, "default");
        assert!(resolved.plaintext_password.is_none());
        assert!(resolved.keyring_username.is_empty());
        assert!(resolved.config_path.is_none());
    }
}

#[cfg(test)]
mod resolve_tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn resolve_with_toml_config_file() {
        let dir = std::env::temp_dir().join(format!("gaussdb_test_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let config_path = dir.join("test_config.toml");

        let toml = r#"
host = "testhost"
port = 5432
user = "testuser"
password = "testpass"
dbname = "testdb"
"#;
        let mut file = std::fs::File::create(&config_path).unwrap();
        file.write_all(toml.as_bytes()).unwrap();

        let result = resolve(None, Some(&config_path), None);
        assert!(result.is_ok(), "resolve failed: {:?}", result.err());
        let resolved = result.unwrap();
        assert_eq!(resolved.name, "default");
        assert!(resolved.connection_url.contains("host=testhost"));
        assert!(resolved.connection_url.contains("user=testuser"));
        assert!(resolved.connection_url.contains("password=testpass"));
        assert!(resolved.connection_url.contains("dbname=testdb"));
        assert_eq!(resolved.sslmode, SslMode::Disable);
        assert!(resolved.config_path.is_some());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_with_named_connection() {
        let dir = std::env::temp_dir().join(format!("gaussdb_test_named_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let config_path = dir.join("test_named.toml");

        let toml = r#"
default_connection = "primary"

[connections.primary]
host = "primary_host"
user = "admin"
password = "secret"
dbname = "prod"

[connections.secondary]
host = "secondary_host"
user = "readonly"
password = "readonly_pass"
dbname = "prod"
"#;
        let mut file = std::fs::File::create(&config_path).unwrap();
        file.write_all(toml.as_bytes()).unwrap();

        let result = resolve(None, Some(&config_path), Some("secondary"));
        assert!(result.is_ok(), "resolve failed: {:?}", result.err());
        let resolved = result.unwrap();
        assert_eq!(resolved.name, "secondary");
        assert!(resolved.connection_url.contains("host=secondary_host"));
        assert!(resolved.connection_url.contains("user=readonly"));

        // Default name should resolve to "primary"
        let result_default = resolve(None, Some(&config_path), None);
        assert!(result_default.is_ok());
        assert_eq!(result_default.unwrap().name, "primary");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_with_dsn_direct() {
        let result = resolve(
            Some(
                "host=direct.host user=direct_user password=direct_pass dbname=direct_db sslmode=require",
            ),
            None,
            None,
        );
        assert!(result.is_ok(), "resolve failed: {:?}", result.err());
        let resolved = result.unwrap();
        assert_eq!(resolved.sslmode, SslMode::Require);
        assert!(resolved.connection_url.contains("host=direct.host"));
        assert!(resolved.config_path.is_none());
    }

    #[test]
    fn resolve_connection_not_found_error() {
        let dir = std::env::temp_dir().join(format!("gaussdb_test_err_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let config_path = dir.join("test_err.toml");

        let toml = r#"
[connections.dev]
host = "localhost"
user = "dev_user"
"#;
        let mut file = std::fs::File::create(&config_path).unwrap();
        file.write_all(toml.as_bytes()).unwrap();

        let result = resolve(None, Some(&config_path), Some("nonexistent"));
        assert!(result.is_err());
        match result.unwrap_err() {
            ConfigError::ConnectionNotFound { name, available } => {
                assert_eq!(name, "nonexistent");
                assert_eq!(available, vec!["dev".to_string()]);
            }
            e => panic!("expected ConnectionNotFound, got: {:?}", e),
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_config_not_found_error() {
        let result = resolve(
            None,
            Some(&PathBuf::from("/nonexistent/path/config.toml")),
            None,
        );
        assert!(result.is_err());
        match result.unwrap_err() {
            ConfigError::Io(_) => {} // File not found is an IO error
            _ => panic!("expected Io error"),
        }
    }
}
