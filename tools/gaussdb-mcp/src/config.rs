use keyring::Entry;
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::Arc;

pub(crate) const KEYRING_SERVICE: &str = "gaussdb-mcp";
pub(crate) const KEYRING_SENTINEL: &str = "keyring";

#[derive(Clone, Copy, Debug)]
pub(crate) enum PasswordSource {
    EnvVar,
    Plaintext,
    Keyring,
    None,
}

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

    pub(crate) default_connection: Option<String>,
    pub(crate) connections: Option<Vec<NamedConnection>>,
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
}

impl MultiConfig {
    pub(crate) fn resolve(self) -> Result<(Vec<NamedConnection>, Option<String>), String> {
        match self.connections {
            Some(ref conns) if !conns.is_empty() => {
                let default = self
                    .default_connection
                    .clone()
                    .or_else(|| conns.first().map(|c| c.name.clone()));
                Ok((self.connections.unwrap(), default))
            }
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
                };
                Ok((vec![single], Some("default".to_string())))
            }
        }
    }
}

pub(crate) struct ResolvedConnection {
    pub(crate) name: String,
    pub(crate) connection_url: String,
    pub(crate) config_path: Option<PathBuf>,
    pub(crate) plaintext_password: Option<String>,
    pub(crate) keyring_username: String,
    pub(crate) password_source: PasswordSource,
}

pub(crate) enum LazyConnectionEntry {
    Ready(ResolvedConnection),
    Pending {
        name: String,
        resolver: Arc<dyn (Fn() -> Result<String, String>) + Send + Sync>,
    },
}

pub(crate) fn read_keyring_password(username: &str) -> Result<String, String> {
    let entry = Entry::new(KEYRING_SERVICE, username)
        .map_err(|e| format!("keyring entry creation failed: {}", e))?;
    entry.get_password().map_err(|e| {
        format!(
            "keyring password not found for '{}'. Store it first:\n  \
             gaussdb-mcp --store-password <password> --config <path>\n  \
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

pub(crate) fn rewrite_password_to_sentinel(path: &std::path::Path) -> std::io::Result<()> {
    let content = std::fs::read_to_string(path)?;
    let mut new_content = String::new();
    let mut replaced = false;

    for line in content.lines() {
        if !replaced && line.trim().starts_with("password") {
            if line.contains('=') {
                let indent = &line[..line.find("password").unwrap_or(0)];
                new_content.push_str(&format!("{}password = \"{}\"", indent, KEYRING_SENTINEL));
                replaced = true;
            } else {
                new_content.push_str(line);
            }
        } else {
            new_content.push_str(line);
        }
        new_content.push('\n');
    }

    if content.ends_with('\n') && new_content.ends_with("\n\n") {
        new_content.pop();
    }

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

    Ok(ResolvedConnection {
        name: conn.name.clone(),
        connection_url,
        config_path,
        plaintext_password,
        keyring_username: keyring_user,
        password_source,
    })
}

pub(crate) fn build_lazy_resolver(conn: &NamedConnection) -> Result<LazyConnectionEntry, String> {
    let conn = conn.clone();
    let keyring_user = conn.keyring_username();

    let is_sentinel = conn.password.as_deref() == Some(KEYRING_SENTINEL);
    let is_plaintext = conn
        .password
        .as_ref()
        .is_some_and(|p| p != KEYRING_SENTINEL);

    if is_plaintext || conn.url.is_some() {
        let resolved = resolve_single_connection(&conn, None)?;
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

    Ok(LazyConnectionEntry::Pending { name, resolver })
}

pub(crate) fn resolve_all_connections(
    config_path: Option<PathBuf>,
) -> Result<(Vec<ResolvedConnection>, String), String> {
    if let Ok(url) = std::env::var("GAUSSDB_URL").or_else(|_| std::env::var("DATABASE_URL")) {
        let resolved = ResolvedConnection {
            name: "default".to_string(),
            connection_url: url,
            config_path: None,
            plaintext_password: None,
            keyring_username: String::new(),
            password_source: PasswordSource::EnvVar,
        };
        return Ok((vec![resolved], "default".to_string()));
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

    let (connections, default_name) = config.resolve()?;
    let default_name = default_name.unwrap_or_else(|| {
        connections
            .first()
            .map(|c| c.name.clone())
            .unwrap_or_default()
    });

    let mut resolved = Vec::with_capacity(connections.len());
    for conn in &connections {
        resolved.push(resolve_single_connection(conn, Some(config_path.clone()))?);
    }

    Ok((resolved, default_name))
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

    let (connections, default_name) = config.resolve()?;
    let default_name = default_name.unwrap_or_else(|| {
        connections
            .first()
            .map(|c| c.name.clone())
            .unwrap_or_default()
    });

    let mut entries = Vec::with_capacity(connections.len());
    for conn in &connections {
        entries.push(build_lazy_resolver(conn)?);
    }

    Ok((entries, default_name))
}
