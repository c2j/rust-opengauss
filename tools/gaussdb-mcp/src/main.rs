mod cli;
mod config;
mod connection;
mod output;
mod queries;
mod duration_parse;
mod server;

use clap::{Parser, Subcommand};
use keyring::Entry;
use rmcp::{ServiceExt, transport::stdio};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

use crate::config::{
    KEYRING_SERVICE, LazyConnectionEntry, PasswordSource, ResolvedConnection, default_config_path,
    resolve_all_connections, resolve_all_connections_lazy, rewrite_password_to_sentinel,
    store_keyring_password,
};
use crate::server::{format_error_chain, redact_url};

// ─── Configuration ─────────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(name = "gaussdb", version, about = concat!("openGauss MCP server and CLI tool — v", env!("CARGO_PKG_VERSION")))]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Run as MCP server (default when no subcommand)
    Mcp {
        /// Path to config file
        #[arg(long)]
        config: Option<String>,

        /// Test database connectivity and exit
        #[arg(long, num_args = 0..=1)]
        check_connection: Option<Option<String>>,

        /// Show detailed connection info
        #[arg(short, long)]
        verbose: bool,

        /// Store password in OS keychain
        #[arg(long)]
        store_password: Option<String>,

        /// Target connection name
        #[arg(long)]
        name: Option<String>,
    },

    /// Execute SQL from command line
    Cli {
        /// SQL statement to execute
        #[arg(short, long)]
        sql: Option<String>,

        /// Read SQL from file
        #[arg(short, long)]
        file: Option<String>,

        /// Path to config file
        #[arg(long)]
        config: Option<String>,

        /// Target connection name
        #[arg(long)]
        connection: Option<String>,

        /// Output format: table, json, vertical
        #[arg(long, default_value = "table")]
        format: String,
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

fn handle_store_password(password: String, name: Option<String>, config_path: Option<String>) {
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

    let (connections, _) = config.resolve().unwrap_or_else(|e| {
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
        connections.first().unwrap_or_else(|| {
            eprintln!("error: no connections defined in config");
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

async fn query_verbose_details(
    client: &tokio_opengauss::Client,
    elapsed: Duration,
) -> VerboseDetails {
    async fn query_scalar(client: &tokio_opengauss::Client, sql: &str) -> Option<String> {
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
) -> Result<(String, Option<VerboseDetails>), tokio_opengauss::Error> {
    let start = Instant::now();
    let (client, connection) = tokio_opengauss::connect(url, tokio_opengauss::NoTls).await?;
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
    let tls = opengauss_native_tls::MakeTlsConnector::new(connector);
    let (client, connection) = tokio_opengauss::connect(url, tls).await?;
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
                        "  ✓ OS keychain is available — password will be migrated on first MCP connection"
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
    let (all_resolved, default_name) = resolve_all_connections(config_path).unwrap_or_else(|e| {
        eprintln!("error: {}", e);
        std::process::exit(1);
    });

    let target = if let Some(ref name) = conn_arg {
        all_resolved
            .iter()
            .find(|c| c.name == *name)
            .unwrap_or_else(|| {
                eprintln!("error: connection '{}' not found", name);
                eprintln!(
                    "  available: {:?}",
                    all_resolved.iter().map(|c| &c.name).collect::<Vec<_>>()
                );
                std::process::exit(1);
            })
    } else {
        all_resolved
            .iter()
            .find(|c| c.name == default_name)
            .unwrap_or(&all_resolved[0])
    };

    handle_check_connection(target, verbose).await;
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

    for entry in lazy_entries {
        match entry {
            LazyConnectionEntry::Ready(resolved) => {
                let conn_name = resolved.name.clone();
                let config_path = resolved.config_path.clone();
                let plaintext_password = resolved.plaintext_password.clone();
                let keyring_username = resolved.keyring_username.clone();

                if let (Some(path), Some(plaintext)) = (&config_path, &plaintext_password) {
                    let path = path.clone();
                    let plaintext = plaintext.clone();
                    let keyring_user = keyring_username.clone();
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
                        } else if let Err(e) = rewrite_password_to_sentinel(&path) {
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
            LazyConnectionEntry::Pending { name, resolver } => {
                lazy_resolvers.push((name, resolver));
            }
        }
    }

    let mut server = if !eager_entries.is_empty() && lazy_resolvers.is_empty() {
        server::GaussdbMcp::new_multi_disconnected(eager_entries, default_name)
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
        server::GaussdbMcp::new_multi_lazy(all_lazy, default_name)
    } else {
        server::GaussdbMcp::new_multi_disconnected(Vec::new(), default_name)
    };

    for (name, cb) in callbacks_to_register {
        server.set_on_connected(name, cb);
    }

    let server = Arc::new(server);

    let probe = Arc::clone(&server);
    tokio::spawn(async move {
        probe.try_connect().await;
    });

    info!("starting MCP server on stdio");

    let service = Arc::clone(&server)
        .serve(stdio())
        .await
        .unwrap_or_else(|e| {
            error!("MCP server start failed: {}", e);
            panic!("Failed to start MCP server: {}", e);
        });

    info!("MCP server ready");

    service.waiting().await.unwrap_or_else(|e| {
        error!("MCP server error: {}", e);
        panic!("Server error: {}", e);
    });
}

// ─── Entry Point ──────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() {
    init_logging();

    let cli = Cli::parse();

    match cli.command {
        Some(Commands::Mcp {
            config,
            check_connection,
            verbose,
            store_password,
            name,
        }) => {
            if let Some(password) = store_password {
                handle_store_password(password, name, config);
                return;
            }

            if let Some(conn_arg) = check_connection {
                let config_path = config.map(PathBuf::from);
                handle_check_connection_cmd(conn_arg, verbose, config_path).await;
                return;
            }

            run_mcp_server(config).await;
        }
        Some(Commands::Cli {
            sql,
            file,
            config,
            connection,
            format,
        }) => {
            let fmt: cli::OutputFormat = format.parse().unwrap_or(cli::OutputFormat::Table);
            let args = cli::CliArgs {
                sql,
                file,
                connection_name: connection,
                config_path: config,
                format: fmt,
            };
            if let Err(e) = cli::run_cli(args).await {
                eprintln!("error: {}", e);
                std::process::exit(1);
            }
        }
        None => {
            // No subcommand = default to MCP mode (backward compat)
            run_mcp_server(None).await;
        }
    }
}
