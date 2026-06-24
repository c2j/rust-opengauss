mod cli;
mod config;
mod connection;
mod interactive;
mod output;
mod queries;
mod server;

use clap::{Parser, Subcommand};
use keyring::Entry;
use rmcp::{ServiceExt, transport::stdio};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
#[cfg(target_os = "linux")]
use tracing::debug;
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

use crate::config::{
    KEYRING_SERVICE, LazyConnectionEntry, PasswordSource, ResolvedConnection, TimeoutConfig,
    default_config_path, read_config, resolve_all_connections_lazy, resolve_env_var_connection,
    resolve_single_connection, rewrite_password_to_sentinel, store_keyring_password,
};
use crate::server::{format_error_chain, redact_url};

// ─── Configuration ─────────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(name = "gaussdb", version, about = concat!("openGauss MCP server and CLI tool — v", env!("CARGO_PKG_VERSION")))]
struct Cli {
    /// Path to config file
    #[arg(long, global = true)]
    config: Option<String>,

    /// Target connection name
    #[arg(long, global = true)]
    name: Option<String>,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Run as MCP server (default when no subcommand given)
    Mcp,

    /// Test database connectivity and exit
    Check {
        /// Show detailed connection info
        #[arg(short, long)]
        verbose: bool,
    },

    /// Store password in OS keychain (prompts interactively; reads from stdin when piped)
    StorePassword {},

    /// Execute SQL from command line
    Cli {
        /// SQL statement to execute
        #[arg(short, long)]
        sql: Option<String>,

        /// Read SQL from file (or pipe to stdin)
        #[arg(short, long)]
        file: Option<String>,

        /// Test database connectivity without executing SQL
        #[arg(long)]
        check_connection: bool,

        /// Show detailed connection info (use with --check-connection)
        #[arg(short, long)]
        verbose: bool,

        /// Output format: table, json, vertical, csv
        #[arg(long, default_value = "table")]
        format: String,

        /// Statement timeout (e.g. "30s", "5min"). Overrides config.
        #[arg(long)]
        statement_timeout: Option<String>,

        /// Connection max lifetime before reconnect (e.g. "10min").
        #[arg(long)]
        connection_max_lifetime: Option<String>,

        /// Action on timeout: "cancel" or "disconnect". Default: cancel.
        #[arg(long)]
        timeout_action: Option<String>,

        /// Enter interactive REPL mode
        #[arg(short, long)]
        interactive: bool,
    },
}

// ─── Verbose details structures ────────────────────────────────────────────────

struct VerboseDetails {
    server_version: Option<String>,
    server_version_num: Option<String>,
    protocol_version: Option<String>,
    current_user: Option<String>,
    current_database: Option<String>,
    server_addr: Option<String>,
    server_port: Option<String>,
    start_time: Option<String>,
    is_in_recovery: Option<bool>,
    ssl_is_used: Option<bool>,
    ssl_version: Option<String>,
    ssl_cipher: Option<String>,
    elapsed: Duration,
    guc_max_connections: Option<String>,
    guc_shared_buffers: Option<String>,
    guc_work_mem: Option<String>,
    guc_timezone: Option<String>,
    guc_data_directory: Option<String>,
}

struct TlsCertInfo {
    subject: String,
    issuer: String,
    valid_from: String,
    valid_to: String,
    serial: String,
}

// ─── Logging ───────────────────────────────────────────────────────────────────

fn init_logging() {
    let log_dir = dirs::data_local_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("gaussdb-mcp");

    if let Err(e) = std::fs::create_dir_all(&log_dir) {
        eprintln!(
            "warning: cannot create log dir {}: {}",
            log_dir.display(),
            e
        );
    }

    let file_appender = tracing_appender::rolling::daily(&log_dir, "gaussdb-mcp.log");
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("gaussdb_mcp=info"));

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(file_appender)
        .with_ansi(false)
        .with_target(false)
        .init();

    info!("log file: {}/gaussdb-mcp.log", log_dir.display());
}

// ─── Keyring helpers ───────────────────────────────────────────────────────────

fn check_keyring_available(username: &str) -> Result<(), String> {
    let test_key = "__gaussdb_mcp_keyring_test__";
    let entry = Entry::new(KEYRING_SERVICE, username)
        .map_err(|e| format!("keyring entry creation failed: {}", e))?;
    entry
        .set_password(test_key)
        .map_err(|e| format!("keyring write failed: {}", e))?;
    let read_back = entry
        .get_password()
        .map_err(|e| format!("keyring read-back failed: {}", e))?;
    if read_back != test_key {
        return Err("keyring read-back mismatch".to_string());
    }
    Ok(())
}

fn read_password_secure() -> Result<String, String> {
    use std::io::IsTerminal;

    if std::io::stdin().is_terminal() {
        let pw1 = rpassword::prompt_password("Enter password: ")
            .map_err(|e| format!("failed to read password: {}", e))?;
        if pw1.is_empty() {
            return Err("password cannot be empty".to_string());
        }
        let pw2 = rpassword::prompt_password("Confirm password: ")
            .map_err(|e| format!("failed to read password: {}", e))?;
        if pw1 != pw2 {
            return Err("passwords do not match".to_string());
        }
        Ok(pw1)
    } else {
        let mut input = String::new();
        std::io::stdin()
            .read_line(&mut input)
            .map_err(|e| format!("failed to read password from stdin: {}", e))?;
        let pw = input.trim_end_matches(['\r', '\n']).to_string();
        if pw.is_empty() {
            return Err("password from stdin cannot be empty".to_string());
        }
        Ok(pw)
    }
}

fn handle_store_password(name: Option<String>, config_path: Option<String>) {
    let password = read_password_secure().unwrap_or_else(|e| {
        eprintln!("error: {}", e);
        std::process::exit(1);
    });

    let config_path = config_path
        .map(PathBuf::from)
        .or_else(default_config_path)
        .unwrap_or_else(|| {
            eprintln!("error: no config file specified and no default found");
            std::process::exit(1);
        });

    if !config_path.exists() {
        eprintln!("error: config file not found: {}", config_path.display());
        std::process::exit(1);
    }

    let content = std::fs::read_to_string(&config_path).unwrap_or_else(|e| {
        eprintln!("error: failed to read {}: {}", config_path.display(), e);
        std::process::exit(1);
    });

    let config: config::MultiConfig = toml::from_str(&content).unwrap_or_else(|e| {
        eprintln!("error: failed to parse {}: {}", config_path.display(), e);
        std::process::exit(1);
    });

    let (connections, default_name) = config.resolve().unwrap_or_else(|e| {
        eprintln!("error: {}", e);
        std::process::exit(1);
    });

    let target = if let Some(ref name) = name {
        connections
            .iter()
            .find(|c| c.name == *name)
            .unwrap_or_else(|| {
                eprintln!("error: connection '{}' not found in config", name);
                eprintln!(
                    "  available: {:?}",
                    connections.iter().map(|c| &c.name).collect::<Vec<_>>()
                );
                std::process::exit(1);
            })
    } else {
        let default = default_name.unwrap_or_else(|| {
            connections
                .first()
                .map(|c| c.name.clone())
                .unwrap_or_default()
        });
        connections
            .iter()
            .find(|c| c.name == default)
            .unwrap_or_else(|| {
                eprintln!(
                    "error: default connection '{}' not found in config",
                    default
                );
                std::process::exit(1);
            })
    };

    let keyring_user = target.keyring_username();

    if let Err(e) = store_keyring_password(&keyring_user, &password) {
        eprintln!("error: {}", e);
        std::process::exit(1);
    }

    println!(
        "Password stored in OS keychain for '{}' (connection: '{}').",
        keyring_user, target.name
    );
}

// ─── Connection Diagnostics ────────────────────────────────────────────────────

fn parse_host_port_from_url(url: &str) -> Option<(String, u16)> {
    let mut host = None;
    let mut port = None;
    for part in url.split_whitespace() {
        if let Some(v) = part.strip_prefix("host=") {
            host = Some(v.trim_matches('"').to_string());
        }
        if let Some(v) = part.strip_prefix("port=") {
            port = v.trim_matches('"').parse::<u16>().ok();
        }
    }
    match (host, port) {
        (Some(h), Some(p)) => Some((h, p)),
        (Some(h), None) => Some((h, 5432)),
        _ => None,
    }
}

async fn query_verbose_details(client: &gaussdb::Client, elapsed: Duration) -> VerboseDetails {
    async fn query_scalar(client: &gaussdb::Client, sql: &str) -> Option<String> {
        match client.query_one(sql, &[]).await {
            Ok(row) => row
                .try_get::<_, Option<&str>>(0)
                .ok()
                .flatten()
                .map(String::from),
            Err(_) => None,
        }
    }

    let server_version = query_scalar(
        client,
        "SELECT setting FROM pg_settings WHERE name = 'server_version'",
    )
    .await;
    let server_version_num = query_scalar(
        client,
        "SELECT setting FROM pg_settings WHERE name = 'server_version_num'",
    )
    .await;
    let protocol_version = query_scalar(
        client,
        "SELECT setting FROM pg_settings WHERE name = 'protocol_version'",
    )
    .await;
    let current_user = query_scalar(client, "SELECT current_user::text").await;
    let current_database = query_scalar(client, "SELECT current_database()::text").await;
    let server_addr = query_scalar(client, "SELECT inet_server_addr()::text").await;
    let server_port = query_scalar(client, "SELECT inet_server_port()::text").await;
    let start_time = query_scalar(client, "SELECT pg_postmaster_start_time()::text").await;
    let is_in_recovery_str = query_scalar(client, "SELECT pg_is_in_recovery()::text").await;
    let ssl_is_used_str = query_scalar(client, "SELECT ssl_is_used()::text").await;
    let ssl_version = query_scalar(client, "SELECT ssl_version()").await;
    let ssl_cipher = query_scalar(client, "SELECT ssl_cipher()").await;
    let guc_max_connections = query_scalar(
        client,
        "SELECT setting FROM pg_settings WHERE name = 'max_connections'",
    )
    .await;
    let guc_shared_buffers = query_scalar(
        client,
        "SELECT setting FROM pg_settings WHERE name = 'shared_buffers'",
    )
    .await;
    let guc_work_mem = query_scalar(
        client,
        "SELECT setting FROM pg_settings WHERE name = 'work_mem'",
    )
    .await;
    let guc_timezone = query_scalar(
        client,
        "SELECT setting FROM pg_settings WHERE name = 'TimeZone'",
    )
    .await;
    let guc_data_directory = query_scalar(
        client,
        "SELECT setting FROM pg_settings WHERE name = 'data_directory'",
    )
    .await;

    let is_in_recovery = is_in_recovery_str
        .as_deref()
        .map(|s| matches!(s.to_lowercase().as_str(), "true" | "t" | "yes" | "on" | "1"));
    let ssl_is_used = ssl_is_used_str
        .as_deref()
        .map(|s| matches!(s.to_lowercase().as_str(), "true" | "t" | "yes" | "on" | "1"));

    VerboseDetails {
        server_version,
        server_version_num,
        protocol_version,
        current_user,
        current_database,
        server_addr,
        server_port,
        start_time,
        is_in_recovery,
        ssl_is_used,
        ssl_version,
        ssl_cipher,
        elapsed,
        guc_max_connections,
        guc_shared_buffers,
        guc_work_mem,
        guc_timezone,
        guc_data_directory,
    }
}

fn print_verbose_details(details: &VerboseDetails) {
    eprintln!("  [verbose] Connection Details:");
    eprintln!(
        "    {:24} {}",
        "server_version",
        details.server_version.as_deref().unwrap_or("—")
    );
    eprintln!(
        "    {:24} {}",
        "server_version_num",
        details.server_version_num.as_deref().unwrap_or("—")
    );
    eprintln!(
        "    {:24} {}",
        "protocol_version",
        details.protocol_version.as_deref().unwrap_or("—")
    );
    eprintln!(
        "    {:24} {}",
        "current_user",
        details.current_user.as_deref().unwrap_or("—")
    );
    eprintln!(
        "    {:24} {}",
        "current_database",
        details.current_database.as_deref().unwrap_or("—")
    );
    eprintln!(
        "    {:24} {}",
        "server_addr",
        details.server_addr.as_deref().unwrap_or("—")
    );
    eprintln!(
        "    {:24} {}",
        "server_port",
        details.server_port.as_deref().unwrap_or("—")
    );
    eprintln!(
        "    {:24} {}",
        "server_start_time",
        details.start_time.as_deref().unwrap_or("—")
    );
    match details.is_in_recovery {
        Some(true) => eprintln!("    {:24} true  (standby / recovering)", "is_in_recovery"),
        Some(false) => eprintln!("    {:24} false (primary)", "is_in_recovery"),
        None => eprintln!("    {:24} —", "is_in_recovery"),
    }
    eprintln!(
        "    {:24} {}ms",
        "connect_time",
        details.elapsed.as_millis()
    );

    if details.ssl_is_used == Some(true) {
        eprintln!();
        eprintln!("  [verbose] TLS Session:");
        eprintln!(
            "    {:24} {}",
            "ssl_version",
            details.ssl_version.as_deref().unwrap_or("—")
        );
        eprintln!(
            "    {:24} {}",
            "ssl_cipher",
            details.ssl_cipher.as_deref().unwrap_or("—")
        );
    }

    let has_any_guc = details.guc_max_connections.is_some()
        || details.guc_shared_buffers.is_some()
        || details.guc_work_mem.is_some()
        || details.guc_timezone.is_some()
        || details.guc_data_directory.is_some();
    if has_any_guc {
        eprintln!();
        eprintln!("  [verbose] Server Configuration (GUC):");
        eprintln!(
            "    {:24} {}",
            "max_connections",
            details.guc_max_connections.as_deref().unwrap_or("—")
        );
        eprintln!(
            "    {:24} {}",
            "shared_buffers",
            details.guc_shared_buffers.as_deref().unwrap_or("—")
        );
        eprintln!(
            "    {:24} {}",
            "work_mem",
            details.guc_work_mem.as_deref().unwrap_or("—")
        );
        eprintln!(
            "    {:24} {}",
            "timezone",
            details.guc_timezone.as_deref().unwrap_or("—")
        );
        eprintln!(
            "    {:24} {}",
            "data_directory",
            details.guc_data_directory.as_deref().unwrap_or("—")
        );
    }
}

fn print_tls_cert_info(cert: &TlsCertInfo) {
    eprintln!();
    eprintln!("  [verbose] Server Certificate:");
    eprintln!("    {:18} {}", "Subject", cert.subject);
    eprintln!("    {:18} {}", "Issuer", cert.issuer);
    eprintln!("    {:18} {}", "Serial", cert.serial);
    eprintln!("    {:18} {}", "Not Before", cert.valid_from);
    eprintln!("    {:18} {}", "Not After", cert.valid_to);
}

fn extract_tls_cert_info(host: &str, port: u16, verify: bool) -> Result<TlsCertInfo, String> {
    use std::io::{Read, Write};
    use std::net::TcpStream;

    let addr = format!("{}:{}", host, port);
    let mut stream = TcpStream::connect_timeout(
        &addr
            .parse()
            .map_err(|e| format!("invalid address '{}': {}", addr, e))?,
        Duration::from_secs(5),
    )
    .map_err(|e| format!("TCP connect to {} failed: {}", addr, e))?;

    let ssl_request: [u8; 8] = [0, 0, 0, 8, 4, 210, 22, 47];
    stream
        .write_all(&ssl_request)
        .map_err(|e| format!("SSL request write failed: {}", e))?;

    let mut buf = [0u8; 1];
    stream
        .read_exact(&mut buf)
        .map_err(|e| format!("SSL response read failed: {}", e))?;

    if buf[0] != b'S' {
        return Err("server does not support TLS".to_string());
    }

    let mut builder = native_tls::TlsConnector::builder();
    if !verify {
        builder.danger_accept_invalid_certs(true);
        builder.danger_accept_invalid_hostnames(true);
    }
    let connector = builder
        .build()
        .map_err(|e| format!("TLS connector build failed: {}", e))?;

    let tls_stream = connector
        .connect(host, stream)
        .map_err(|e| format!("TLS handshake failed: {}", e))?;

    let cert = tls_stream
        .peer_certificate()
        .map_err(|e| format!("cert extraction failed: {}", e))?
        .ok_or_else(|| "no peer certificate presented".to_string())?;

    let der = cert
        .to_der()
        .map_err(|e| format!("cert DER encoding failed: {}", e))?;

    let (_, x509) = x509_parser::parse_x509_certificate(&der)
        .map_err(|e| format!("cert parse failed: {:?}", e))?;

    let serial_hex = format!("{:x}", x509.serial);

    Ok(TlsCertInfo {
        subject: x509.subject().to_string(),
        issuer: x509.issuer().to_string(),
        valid_from: x509
            .validity()
            .not_before
            .to_rfc2822()
            .map_err(|e| e.to_string())?,
        valid_to: x509
            .validity()
            .not_after
            .to_rfc2822()
            .map_err(|e| e.to_string())?,
        serial: serial_hex,
    })
}

async fn try_connect_notls(
    url: &str,
    verbose: bool,
) -> Result<(String, Option<VerboseDetails>), gaussdb::Error> {
    let start = Instant::now();
    let (client, connection) = gaussdb::connect(url, gaussdb::NoTls).await?;
    let elapsed = start.elapsed();
    tokio::spawn(async move {
        let _ = connection.await;
    });

    let row = client.query_one("SELECT version()", &[]).await?;
    let version = row
        .get::<_, Option<&str>>(0)
        .unwrap_or("(unknown)")
        .to_string();

    let verbose_details = if verbose {
        Some(query_verbose_details(&client, elapsed).await)
    } else {
        None
    };

    Ok((version, verbose_details))
}

async fn try_connect_tls(
    url: &str,
    verify: bool,
    verbose: bool,
) -> Result<(String, Option<VerboseDetails>), Box<dyn std::error::Error + Send + Sync>> {
    let start = Instant::now();
    let mut builder = native_tls::TlsConnector::builder();
    if !verify {
        builder.danger_accept_invalid_certs(true);
        builder.danger_accept_invalid_hostnames(true);
    }
    let connector = builder.build()?;
    let tls = gaussdb::native_tls::MakeTlsConnector::new(connector);
    let (client, connection) = gaussdb::connect(url, tls).await?;
    let elapsed = start.elapsed();
    tokio::spawn(async move {
        let _ = connection.await;
    });

    let row = client.query_one("SELECT version()", &[]).await?;
    let version = row
        .get::<_, Option<&str>>(0)
        .unwrap_or("(unknown)")
        .to_string();

    let verbose_details = if verbose {
        Some(query_verbose_details(&client, elapsed).await)
    } else {
        None
    };

    Ok((version, verbose_details))
}

async fn handle_check_connection(resolved: &ResolvedConnection, verbose: bool) {
    let url = &resolved.connection_url;
    let redacted = redact_url(url);

    eprintln!("Connection: {}", resolved.name);
    eprintln!();

    match resolved.password_source {
        PasswordSource::Keyring => {
            eprintln!(
                "[Keyring] Password read from OS keychain (user: {})",
                resolved.keyring_username
            );
            let entry_result = Entry::new(KEYRING_SERVICE, &resolved.keyring_username)
                .and_then(|e| e.get_password());
            match entry_result {
                Ok(pw) => {
                    if pw.is_empty() {
                        eprintln!("  ⚠ WARNING: keyring returned empty password");
                    } else {
                        eprintln!(
                            "  ✓ Keyring accessible, password retrieved ({} chars)",
                            pw.len()
                        );
                    }
                }
                Err(e) => {
                    eprintln!("  ✗ Keyring read-back failed: {}", e);
                    eprintln!(
                        "    This means the password was already read once but keyring may be unreliable."
                    );
                    eprintln!(
                        "    Consider changing password in config from \"keyring\" back to plaintext."
                    );
                }
            }
            eprintln!();
        }
        PasswordSource::Plaintext => {
            eprintln!("[Keyring] Password from config file (plaintext)");
            match check_keyring_available(&resolved.keyring_username) {
                Ok(()) => {
                    eprintln!(
                        "  ✓ OS keychain is available — password will be migrated on first successful connection"
                    );
                }
                Err(e) => {
                    eprintln!("  ⚠ OS keychain NOT available: {}", e);
                    eprintln!("    Plaintext password will be kept in config file (no migration).");
                }
            }
            eprintln!();
        }
        PasswordSource::EnvVar => {
            eprintln!("[Keyring] Password from environment variable (no keyring involved)");
            eprintln!();
        }
        PasswordSource::None => {
            eprintln!("[Keyring] No password configured");
            eprintln!();
        }
    }

    let url_without_sslmode = url
        .split_whitespace()
        .filter(|part| !part.starts_with("sslmode="))
        .collect::<Vec<_>>()
        .join(" ");

    struct AttemptResult {
        mode: &'static str,
        success: bool,
        version: Option<String>,
        error: Option<String>,
        verbose_details: Option<VerboseDetails>,
    }

    let mut results: Vec<AttemptResult> = Vec::new();

    eprintln!("[1/3] Trying NoTls (plain TCP) → {} ...", redacted);
    match try_connect_notls(&url_without_sslmode, verbose).await {
        Ok((version, details)) => {
            eprintln!("  ✓ Connected");
            eprintln!("    {}", version);
            if let Some(ref d) = details {
                print_verbose_details(d);
            }
            results.push(AttemptResult {
                mode: "NoTls",
                success: true,
                version: Some(version),
                error: None,
                verbose_details: details,
            });
        }
        Err(e) => {
            let chain = format_error_chain(&e);
            eprintln!("  ✗ {}", chain);
            results.push(AttemptResult {
                mode: "NoTls",
                success: false,
                version: None,
                error: Some(chain),
                verbose_details: None,
            });
        }
    }

    let tls_url = format!("{} sslmode=require", url_without_sslmode);
    let host_port = parse_host_port_from_url(url);

    eprintln!("[2/3] Trying TLS (skip cert verify) → {} ...", redacted);
    match try_connect_tls(&tls_url, false, verbose).await {
        Ok((version, details)) => {
            eprintln!("  ✓ Connected");
            eprintln!("    {}", version);
            if let Some(ref d) = details {
                print_verbose_details(d);
            }
            if verbose {
                if let Some((ref host, port)) = host_port {
                    match extract_tls_cert_info(host, port, false) {
                        Ok(cert) => print_tls_cert_info(&cert),
                        Err(e) => {
                            eprintln!("  [verbose] Certificate extraction skipped: {}", e)
                        }
                    }
                }
            }
            results.push(AttemptResult {
                mode: "TLS (no verify)",
                success: true,
                version: Some(version),
                error: None,
                verbose_details: details,
            });
        }
        Err(e) => {
            let chain = format_error_chain(e.as_ref());
            eprintln!("  ✗ {}", chain);
            results.push(AttemptResult {
                mode: "TLS (no verify)",
                success: false,
                version: None,
                error: Some(chain),
                verbose_details: None,
            });
        }
    }

    eprintln!("[3/3] Trying TLS (verify cert) → {} ...", redacted);
    match try_connect_tls(&tls_url, true, verbose).await {
        Ok((version, details)) => {
            eprintln!("  ✓ Connected");
            eprintln!("    {}", version);
            if let Some(ref d) = details {
                print_verbose_details(d);
            }
            if verbose {
                if let Some((ref host, port)) = host_port {
                    match extract_tls_cert_info(host, port, true) {
                        Ok(cert) => print_tls_cert_info(&cert),
                        Err(e) => {
                            eprintln!("  [verbose] Certificate extraction skipped: {}", e)
                        }
                    }
                }
            }
            results.push(AttemptResult {
                mode: "TLS (verify)",
                success: true,
                version: Some(version),
                error: None,
                verbose_details: details,
            });
        }
        Err(e) => {
            let chain = format_error_chain(e.as_ref());
            eprintln!("  ✗ {}", chain);
            results.push(AttemptResult {
                mode: "TLS (verify)",
                success: false,
                version: None,
                error: Some(chain),
                verbose_details: None,
            });
        }
    }

    eprintln!();
    eprintln!("═══════════════════════════════════════════════════════════");
    eprintln!("  Connection Diagnostic Summary");
    eprintln!("═══════════════════════════════════════════════════════════");

    let mut any_success = false;
    for r in &results {
        if r.success {
            any_success = true;
            let elapsed_str = r
                .verbose_details
                .as_ref()
                .map(|d| format!(" ({}ms)", d.elapsed.as_millis()))
                .unwrap_or_default();
            eprintln!(
                "  {:20} ✓  {}{}",
                r.mode,
                r.version.as_deref().unwrap_or("(unknown)"),
                elapsed_str
            );
        } else {
            eprintln!(
                "  {:20} ✗  {}",
                r.mode,
                r.error.as_deref().unwrap_or("unknown")
            );
        }
    }

    eprintln!();

    if any_success {
        let working = results.iter().find(|r| r.success).unwrap();
        if let Some(ref ver) = working.version {
            eprintln!("  Database Version:");
            eprintln!("    {}", ver);
            eprintln!();
        }
        eprintln!("Recommendation: use {} mode.", working.mode);
        if working.mode != "NoTls" {
            eprintln!("  Add to config: sslmode = \"require\"");
        }
        std::process::exit(0);
    } else {
        eprintln!("All connection methods failed.");
        eprintln!();
        eprintln!("Possible causes:");
        eprintln!("  - Database server is not running or not reachable");
        eprintln!("  - Firewall blocking port 5432");
        eprintln!("  - pg_hba.conf does not allow this client IP/user");
        eprintln!("  - Wrong host, port, user, or password");
        eprintln!("  - Server requires client certificate authentication (cert mode)");
        std::process::exit(1);
    }
}

async fn handle_check_connection_cmd(
    conn_arg: Option<String>,
    verbose: bool,
    config_path: Option<PathBuf>,
) {
    let raw = read_config(config_path).unwrap_or_else(|e| {
        eprintln!("error: {}", e);
        std::process::exit(1);
    });

    let target_name = conn_arg.as_deref().unwrap_or(&raw.default_name);

    let target_conn = match raw.connections.iter().find(|c| c.name == target_name) {
        Some(c) => c,
        None => {
            eprintln!("error: connection '{}' not found", target_name);
            eprintln!(
                "  available: {:?}",
                raw.connections.iter().map(|c| &c.name).collect::<Vec<_>>()
            );
            std::process::exit(1);
        }
    };

    let resolved = if raw.is_env_var {
        resolve_env_var_connection(target_conn.url.clone().unwrap())
    } else {
        resolve_single_connection(
            target_conn,
            raw.config_path.clone(),
            raw.base_timeout.as_ref(),
        )
        .unwrap_or_else(|e| {
            eprintln!("error: {}", e);
            std::process::exit(1);
        })
    };

    handle_check_connection(&resolved, verbose).await;
}

// ─── Process Lifecycle Helpers ───────────────────────────────────────────────

#[cfg(target_os = "linux")]
fn setup_parent_death_signal() {
    unsafe extern "C" {
        fn prctl(option: i32, arg2: i32, arg3: i32, arg4: i32, arg5: i32) -> i32;
    }
    const PR_SET_PDEATHSIG: i32 = 1;
    const SIGTERM: i32 = 15;
    // SAFETY: PR_SET_PDEATHSIG configures a per-process signal-on-parent-death
    // attribute. No UB, no allocations, async-signal-safe.
    let rc = unsafe { prctl(PR_SET_PDEATHSIG, SIGTERM, 0, 0, 0) };
    if rc != 0 {
        warn!(
            "failed to set PR_SET_PDEATHSIG: {}",
            std::io::Error::last_os_error()
        );
    } else {
        debug!("PR_SET_PDEATHSIG(SIGTERM) installed");
    }
}

#[cfg(not(target_os = "linux"))]
fn setup_parent_death_signal() {}

async fn await_shutdown_signal() -> &'static str {
    let ctrl_c = tokio::signal::ctrl_c();

    #[cfg(unix)]
    {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut sigterm) => {
                tokio::select! {
                    _ = ctrl_c => "SIGINT",
                    _ = sigterm.recv() => "SIGTERM",
                }
            }
            Err(e) => {
                warn!("failed to install SIGTERM handler: {e}, relying on SIGINT only");
                let _ = ctrl_c.await;
                "SIGINT"
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = ctrl_c.await;
        "SIGINT"
    }
}

#[cfg(unix)]
async fn parent_death_watchdog(interval: std::time::Duration) {
    unsafe extern "C" {
        fn getppid() -> i32;
    }
    let original_ppid = unsafe { getppid() };
    loop {
        tokio::time::sleep(interval).await;
        let current_ppid = unsafe { getppid() };
        if current_ppid != original_ppid {
            info!(
                "parent process exited (PPID {original_ppid} → {current_ppid}), \
                 initiating self-shutdown"
            );
            return;
        }
    }
}

#[cfg(not(unix))]
async fn parent_death_watchdog(_interval: std::time::Duration) {
    // Non-Unix: no portable getppid. Rely on signals + stdin EOF only.
    std::future::pending::<()>().await;
}

// ─── MCP Server ───────────────────────────────────────────────────────────────

async fn run_mcp_server(config_path: Option<String>) {
    let config_path_buf = config_path.map(PathBuf::from);

    let (lazy_entries, default_name) = resolve_all_connections_lazy(config_path_buf)
        .unwrap_or_else(|e| {
            eprintln!("error: {}", e);
            std::process::exit(1);
        });

    let mut eager_entries = Vec::new();
    let mut lazy_resolvers = Vec::new();
    let mut callbacks_to_register: Vec<(String, Arc<dyn Fn() + Send + Sync>)> = Vec::new();
    let mut timeout_configs: HashMap<String, TimeoutConfig> = HashMap::new();

    for entry in lazy_entries {
        match entry {
            LazyConnectionEntry::Ready(resolved) => {
                let conn_name = resolved.name.clone();
                timeout_configs.insert(conn_name.clone(), resolved.timeout_config.clone());

                let config_path = resolved.config_path.clone();
                let plaintext_password = resolved.plaintext_password.clone();
                let keyring_username = resolved.keyring_username.clone();

                if let (Some(path), Some(plaintext)) = (&config_path, &plaintext_password) {
                    let path = path.clone();
                    let plaintext = plaintext.clone();
                    let keyring_user = keyring_username.clone();
                    let conn_name_for_cb = conn_name.clone();
                    let migrated = Arc::new(std::sync::atomic::AtomicBool::new(false));
                    let cb = Arc::new(move || {
                        if migrated.load(std::sync::atomic::Ordering::Relaxed) {
                            return;
                        }
                        migrated.store(true, std::sync::atomic::Ordering::Relaxed);
                        info!(
                            "migrating plaintext password to OS keychain for '{}'",
                            keyring_user
                        );
                        if let Err(e) = store_keyring_password(&keyring_user, &plaintext) {
                            warn!(
                                "failed to store password in keychain: {} (config file NOT modified)",
                                e
                            );
                        } else if let Err(e) =
                            rewrite_password_to_sentinel(&path, &conn_name_for_cb)
                        {
                            warn!("failed to update config file: {}", e);
                        } else {
                            info!(
                                "password migrated to OS keychain for '{}', config updated",
                                keyring_user
                            );
                        }
                    });
                    callbacks_to_register.push((conn_name.clone(), cb));
                }

                eager_entries.push((resolved.name, resolved.connection_url));
            }
            LazyConnectionEntry::Pending {
                name,
                resolver,
                timeout_config,
            } => {
                timeout_configs.insert(name.clone(), timeout_config);
                lazy_resolvers.push((name, resolver));
            }
        }
    }

    let mut server = if !eager_entries.is_empty() && lazy_resolvers.is_empty() {
        server::GaussdbMcp::new_multi_disconnected(eager_entries, default_name, timeout_configs)
    } else if !lazy_resolvers.is_empty() {
        let all_lazy = eager_entries
            .into_iter()
            .map(|(name, url)| {
                (
                    name,
                    Arc::new(move || Ok(url.clone()))
                        as Arc<dyn (Fn() -> Result<String, String>) + Send + Sync>,
                )
            })
            .chain(lazy_resolvers)
            .collect();
        server::GaussdbMcp::new_multi_lazy(all_lazy, default_name, timeout_configs)
    } else {
        server::GaussdbMcp::new_multi_disconnected(Vec::new(), default_name, HashMap::new())
    };

    for (name, cb) in callbacks_to_register {
        server.set_on_connected(name, cb);
    }

    let server = Arc::new(server);

    // Signal + parent-death watchers must be spawned before serve() so they're
    // active even during the MCP handshake (which blocks on client initialize).
    tokio::spawn(async {
        let sig = await_shutdown_signal().await;
        info!("received {sig}, shutting down");
        std::process::exit(0);
    });
    tokio::spawn(async {
        parent_death_watchdog(std::time::Duration::from_secs(5)).await;
        std::process::exit(0);
    });

    let probe = Arc::clone(&server);
    tokio::spawn(async move {
        probe.try_connect().await;
    });

    info!("starting MCP server on stdio");

    let service = match Arc::clone(&server).serve(stdio()).await {
        Ok(s) => s,
        Err(e) => {
            error!("MCP server start failed: {e}");
            std::process::exit(1);
        }
    };

    info!("MCP server ready");

    match service.waiting().await {
        Ok(reason) => info!("MCP server stopped: {reason:?}"),
        Err(e) => error!("MCP server task join error: {e}"),
    }

    info!("MCP server exiting");
    std::process::exit(0);
}

// ─── Entry Point ──────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() {
    setup_parent_death_signal();
    init_logging();

    let cli = Cli::parse();

    match cli.command {
        None | Some(Commands::Mcp) => {
            run_mcp_server(cli.config).await;
        }
        Some(Commands::Check { verbose }) => {
            let config_path = cli.config.map(PathBuf::from);
            handle_check_connection_cmd(cli.name, verbose, config_path).await;
        }
        Some(Commands::StorePassword {}) => {
            handle_store_password(cli.name, cli.config);
        }
        Some(Commands::Cli {
            sql,
            file,
            check_connection,
            verbose,
            format,
            statement_timeout,
            connection_max_lifetime,
            timeout_action,
            interactive,
        }) => {
            if check_connection {
                let config_path = cli.config.map(PathBuf::from);
                handle_check_connection_cmd(cli.name, verbose, config_path).await;
            } else if interactive {
                let fmt: cli::OutputFormat = format.parse().unwrap_or(cli::OutputFormat::Table);
                let args = cli::CliArgs {
                    sql,
                    file,
                    connection_name: cli.name,
                    config_path: cli.config,
                    format: fmt,
                    statement_timeout,
                    connection_max_lifetime,
                    timeout_action,
                };
                if let Err(e) = interactive::run_interactive(args).await {
                    eprintln!("error: {}", e);
                    std::process::exit(1);
                }
            } else {
                let fmt: cli::OutputFormat = format.parse().unwrap_or(cli::OutputFormat::Table);
                let args = cli::CliArgs {
                    sql,
                    file,
                    connection_name: cli.name,
                    config_path: cli.config,
                    format: fmt,
                    statement_timeout,
                    connection_max_lifetime,
                    timeout_action,
                };
                if let Err(e) = cli::run_cli(args).await {
                    eprintln!("error: {}", e);
                    std::process::exit(1);
                }
            }
        }
    }
}
