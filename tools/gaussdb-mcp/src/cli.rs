use std::path::PathBuf;
use std::str::FromStr;

use crate::config::{resolve_all_connections, TimeoutConfig};
use crate::connection::do_connect;
use crate::output::{format_row_value, format_table};
use crate::server::{format_error_chain, sqlstate_to_sqlcode};

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

pub(crate) enum OutputFormat {
    Table,
    Json,
    Vertical,
}

impl FromStr for OutputFormat {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "table" => Ok(OutputFormat::Table),
            "json" => Ok(OutputFormat::Json),
            "vertical" => Ok(OutputFormat::Vertical),
            _ => Err(format!(
                "Unknown output format '{}'. Use table, json, or vertical.",
                s
            )),
        }
    }
}

pub(crate) struct CliArgs {
    pub sql: Option<String>,
    pub file: Option<String>,
    pub connection_name: Option<String>,
    pub config_path: Option<String>,
    pub format: OutputFormat,
    pub statement_timeout: Option<String>,
    pub connection_max_lifetime: Option<String>,
    pub timeout_action: Option<String>,
}

pub(crate) async fn run_cli(args: CliArgs) -> Result<(), String> {
    // 1. Get SQL from -c, -f, or stdin
    let sql = if let Some(s) = &args.sql {
        s.clone()
    } else if let Some(f) = &args.file {
        std::fs::read_to_string(f).map_err(|e| format!("Failed to read file '{}': {}", f, e))?
    } else {
        let mut input = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut input)
            .map_err(|e| format!("Failed to read stdin: {}", e))?
            .to_string();
        input
    };

    let sql = sql.trim().to_string();
    if sql.is_empty() {
        return Err("No SQL provided. Use -c/--sql, -f/--file, or pipe SQL to stdin.".to_string());
    }

    // 2. Load config
    let config_path = args.config_path.map(PathBuf::from);
    let (all_resolved, default_name) = resolve_all_connections(config_path)?;

    // 3. Find target connection
    let target = if let Some(ref name) = args.connection_name {
        all_resolved
            .iter()
            .find(|c| c.name == *name)
            .ok_or_else(|| format!("Connection '{}' not found", name))?
    } else {
        all_resolved
            .iter()
            .find(|c| c.name == default_name)
            .unwrap_or(&all_resolved[0])
    };

    // 4. Build effective TimeoutConfig: CLI overrides on top of config-file defaults.
    let effective_timeout = TimeoutConfig::from_overrides(
        args.statement_timeout.as_deref(),
        args.connection_max_lifetime.as_deref(),
        args.timeout_action.as_deref(),
        Some(&target.timeout_config),
    )
    .map_err(|e| format!("Invalid timeout configuration: {}", e))?;

    // 5. Connect
    let (client, _handle) = do_connect(&target.connection_url, Some(&effective_timeout))
        .await
        .map_err(|e| format!("Connection failed: {}", format_error_chain(e.as_ref())))?;

    // 5. Execute SQL
    let trimmed = sql.trim();
    let upper = trimmed.to_uppercase();

    if upper.starts_with("SELECT") || upper.starts_with("EXPLAIN") || upper.starts_with("WITH") {
        let rows = client
            .query(trimmed, &[])
            .await
            .map_err(|e| format!("Query failed: {}", format_sql_error(&e)))?;

        if rows.is_empty() {
            println!("(0 rows)");
            return Ok(());
        }

        let columns: Vec<String> = rows[0]
            .columns()
            .iter()
            .map(|c| c.name().to_string())
            .collect();

        let mut result_rows: Vec<Vec<serde_json::Value>> = Vec::new();
        for row in &rows {
            let mut result_row: Vec<serde_json::Value> = Vec::new();
            for idx in 0..row.len() {
                result_row.push(format_row_value(row, idx));
            }
            result_rows.push(result_row);
        }

        match args.format {
            OutputFormat::Table => {
                println!("{}", format_table(&columns, &result_rows));
                if rows.len() == 1 {
                    println!("(1 row)");
                } else {
                    println!("({} rows)", rows.len());
                }
            }
            OutputFormat::Json => {
                let result = serde_json::json!({
                    "columns": columns,
                    "rows": result_rows,
                    "row_count": rows.len(),
                });
                println!(
                    "{}",
                    serde_json::to_string_pretty(&result).unwrap_or_else(|_| result.to_string())
                );
            }
            OutputFormat::Vertical => {
                for (i, row) in result_rows.iter().enumerate() {
                    println!("-[ RECORD {} ]-", i + 1);
                    for (j, val) in row.iter().enumerate() {
                        let val_str = match val {
                            serde_json::Value::Null => "NULL".to_string(),
                            v => v.to_string(),
                        };
                        println!("{} | {}", columns[j], val_str);
                    }
                }
                if result_rows.len() == 1 {
                    println!("(1 row)");
                } else {
                    println!("({} rows)", result_rows.len());
                }
            }
        }
    } else {
        // DML/DDL: use execute
        let rows_affected = client
            .execute(trimmed, &[])
            .await
            .map_err(|e| format!("Execute failed: {}", format_sql_error(&e)))?;
        println!("{}", rows_affected);
    }

    Ok(())
}
