#![allow(dead_code)]

use std::io::Write;
use std::path::PathBuf;
use std::str::FromStr;

use crate::config::{
    TimeoutConfig, read_config, resolve_env_var_connection, resolve_single_connection,
    rewrite_password_to_sentinel, store_keyring_password,
};
use crate::connection::do_connect;
use crate::output::{format_field_string, format_row_value, format_table};
use crate::server::{format_error_chain, sqlstate_to_sqlcode};
use futures_util::StreamExt;
use gaussdb::types::ToSql;
use serde_json::Value;

pub(crate) fn format_sql_error(err: &gaussdb::Error) -> String {
    if let Some(db_err) = err.as_db_error() {
        let sqlstate = db_err.code().code();
        let sqlcode = sqlstate_to_sqlcode(sqlstate);
        let mut msg = format!(
            "[SQLSTATE {} | SQLCODE {}] {}",
            sqlstate,
            sqlcode,
            db_err.message()
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

#[derive(Debug, Clone, Copy)]
#[allow(dead_code)]
pub(crate) enum ResultKind {
    Query,
    Execute,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub(crate) struct QueryResult {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Value>>,
    pub row_count: usize,
    pub affected: u64,
    pub kind: ResultKind,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum OutputFormat {
    Table,
    Json,
    Vertical,
    Csv,
}

impl FromStr for OutputFormat {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "table" => Ok(OutputFormat::Table),
            "json" => Ok(OutputFormat::Json),
            "vertical" => Ok(OutputFormat::Vertical),
            "csv" => Ok(OutputFormat::Csv),
            _ => Err(format!(
                "Unknown output format '{}'. Use table, json, vertical, or csv.",
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
    pub no_history: bool,
}

// ---- helpers for interactive mode ----

#[allow(dead_code)]
fn value_to_compact_string(v: &Value) -> String {
    match v {
        Value::Null => "NULL".to_string(),
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        other => other.to_string(),
    }
}

fn strip_leading_comments(sql: &str) -> &str {
    let trimmed = sql.trim_start();
    let bytes = trimmed.as_bytes();
    if bytes.len() >= 2 && &bytes[..2] == b"--" {
        match trimmed.find('\n') {
            Some(pos) => strip_leading_comments(&trimmed[pos + 1..]),
            None => "",
        }
    } else if bytes.len() >= 2 && &bytes[..2] == b"/*" {
        let mut depth: usize = 1;
        let mut i = 2;
        while i + 1 < bytes.len() && depth > 0 {
            if &bytes[i..i + 2] == b"/*" {
                depth += 1;
                i += 2;
            } else if &bytes[i..i + 2] == b"*/" {
                depth -= 1;
                i += 2;
            } else {
                i += 1;
            }
        }
        if depth == 0 {
            strip_leading_comments(&trimmed[i..])
        } else {
            ""
        }
    } else {
        trimmed
    }
}

#[allow(dead_code)]
pub(crate) async fn execute_sql_buffered(
    client: &gaussdb::Client,
    sql: &str,
) -> Result<QueryResult, gaussdb::Error> {
    let trimmed = sql.trim();
    let stripped = strip_leading_comments(trimmed);
    let upper = stripped.to_uppercase();

    if upper.starts_with("SELECT") || upper.starts_with("EXPLAIN") || upper.starts_with("WITH") {
        let rows = client.query(trimmed, &[]).await?;

        if rows.is_empty() {
            return Ok(QueryResult {
                columns: vec![],
                rows: vec![],
                row_count: 0,
                affected: 0,
                kind: ResultKind::Query,
            });
        }

        let columns: Vec<String> = rows[0]
            .columns()
            .iter()
            .map(|c| c.name().to_string())
            .collect();

        let mut result_rows: Vec<Vec<Value>> = Vec::with_capacity(rows.len());
        for row in &rows {
            let mut result_row: Vec<Value> = Vec::with_capacity(columns.len());
            for idx in 0..row.len() {
                result_row.push(format_row_value(row, idx));
            }
            result_rows.push(result_row);
        }

        Ok(QueryResult {
            columns,
            rows: result_rows,
            row_count: rows.len(),
            affected: 0,
            kind: ResultKind::Query,
        })
    } else {
        let affected = client.execute(trimmed, &[]).await?;

        Ok(QueryResult {
            columns: vec![],
            rows: vec![],
            row_count: 0,
            affected,
            kind: ResultKind::Execute,
        })
    }
}

#[allow(dead_code)]
pub(crate) fn render_result(
    result: &QueryResult,
    writer: &mut dyn std::io::Write,
    format: OutputFormat,
    boxed: bool,
) -> Result<(), String> {
    match result.kind {
        ResultKind::Execute => {
            writeln!(writer, "{}", result.affected).map_err(|e| format!("write error: {}", e))?;
        }
        ResultKind::Query => {
            if result.columns.is_empty() {
                writeln!(writer, "(0 rows)").map_err(|e| format!("write error: {}", e))?;
                return Ok(());
            }

            match format {
                OutputFormat::Table => {
                    let table_str = if boxed {
                        crate::output::format_table_boxed(&result.columns, &result.rows)
                    } else {
                        format_table(&result.columns, &result.rows)
                    };
                    writeln!(writer, "{}", table_str).map_err(|e| format!("write error: {}", e))?;
                    let count_label = if result.row_count == 1 {
                        "1 row".to_string()
                    } else {
                        format!("{} rows", result.row_count)
                    };
                    writeln!(writer, "({})", count_label)
                        .map_err(|e| format!("write error: {}", e))?;
                }
                OutputFormat::Json => {
                    let v = serde_json::json!({
                        "columns": result.columns,
                        "rows": result.rows,
                        "row_count": result.row_count,
                    });
                    writeln!(
                        writer,
                        "{}",
                        serde_json::to_string_pretty(&v).unwrap_or_else(|_| v.to_string())
                    )
                    .map_err(|e| format!("write error: {}", e))?;
                }
                OutputFormat::Vertical => {
                    for (row_idx, row) in result.rows.iter().enumerate() {
                        writeln!(writer, "-[ RECORD {} ]-", row_idx + 1)
                            .map_err(|e| format!("write error: {}", e))?;
                        for (col_idx, col_name) in result.columns.iter().enumerate() {
                            let val_str = value_to_compact_string(&row[col_idx]);
                            writeln!(writer, "{} | {}", col_name, val_str)
                                .map_err(|e| format!("write error: {}", e))?;
                        }
                    }
                    let count_label = if result.row_count == 1 {
                        "1 row".to_string()
                    } else {
                        format!("{} rows", result.row_count)
                    };
                    writeln!(writer, "({})", count_label)
                        .map_err(|e| format!("write error: {}", e))?;
                }
                OutputFormat::Csv => {
                    let mut csv_writer = csv::Writer::from_writer(&mut *writer);
                    csv_writer
                        .write_record(&result.columns)
                        .map_err(|e| format!("write error: {}", e))?;
                    for row in &result.rows {
                        let str_row: Vec<String> =
                            row.iter().map(value_to_compact_string).collect();
                        csv_writer
                            .write_record(&str_row)
                            .map_err(|e| format!("write error: {}", e))?;
                    }
                    csv_writer
                        .flush()
                        .map_err(|e| format!("write error: {}", e))?;
                }
            }
        }
    }
    Ok(())
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

    // 2. Load config (without resolving all connection passwords)
    let config_path = args.config_path.map(PathBuf::from);
    let raw = read_config(config_path)?;

    // 3. Find target connection by name, resolve only that one
    let target_name = args.connection_name.as_deref().unwrap_or(&raw.default_name);

    let target_conn = raw
        .connections
        .iter()
        .find(|c| c.name == target_name)
        .ok_or_else(|| {
            format!(
                "Connection '{}' not found. Available: {:?}",
                target_name,
                raw.connections.iter().map(|c| &c.name).collect::<Vec<_>>()
            )
        })?;

    let target = if raw.is_env_var {
        resolve_env_var_connection(target_conn.url.clone().unwrap())
    } else {
        resolve_single_connection(
            target_conn,
            raw.config_path.clone(),
            raw.base_timeout.as_ref(),
        )?
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

    // 6. Migrate plaintext password to OS keychain on successful connection
    if let (Some(path), Some(plaintext)) = (&target.config_path, &target.plaintext_password) {
        match store_keyring_password(&target.keyring_username, plaintext) {
            Ok(()) => {
                if let Err(e) = rewrite_password_to_sentinel(path, &target.name) {
                    eprintln!(
                        "warning: password stored in keychain but failed to update config: {}",
                        e
                    );
                }
            }
            Err(e) => {
                eprintln!("warning: failed to migrate password to keychain: {}", e);
            }
        }
    }

    // 7. Execute SQL
    let trimmed = sql.trim();
    let upper = trimmed.to_uppercase();

    if upper.starts_with("SELECT") || upper.starts_with("EXPLAIN") || upper.starts_with("WITH") {
        let no_params: [&(dyn ToSql + Sync); 0] = [];
        match args.format {
            OutputFormat::Csv => {
                // Server-side CSV via COPY — zero per-row client processing.
                let inner = trimmed.trim_end_matches(|c: char| c == ';' || c.is_whitespace());
                let copy_sql = format!("COPY ({}) TO STDOUT WITH (FORMAT CSV, HEADER true)", inner);
                let mut stream = std::pin::pin!(
                    client
                        .copy_out(&copy_sql)
                        .await
                        .map_err(|e| format!("COPY failed: {}", format_sql_error(&e)))?
                );
                let stdout = std::io::stdout();
                let mut out = stdout.lock();
                while let Some(chunk_result) = stream.next().await {
                    let chunk = chunk_result
                        .map_err(|e| format!("COPY failed: {}", format_sql_error(&e)))?;
                    out.write_all(&chunk)
                        .map_err(|e| format!("write error: {}", e))?;
                }
                out.flush().map_err(|e| format!("flush error: {}", e))?;
            }
            OutputFormat::Vertical => {
                // O(1) memory: stream rows via query_raw instead of buffering.
                let mut stream = std::pin::pin!(
                    client
                        .query_raw(trimmed, no_params)
                        .await
                        .map_err(|e| format!("Query failed: {}", format_sql_error(&e)))?
                );
                let mut count = 0usize;
                let mut columns: Option<Vec<String>> = None;
                let mut cached_types: Vec<gaussdb::types::Type> = Vec::new();

                while let Some(row_result) = stream.next().await {
                    let row = row_result
                        .map_err(|e| format!("Query failed: {}", format_sql_error(&e)))?;
                    if columns.is_none() {
                        columns =
                            Some(row.columns().iter().map(|c| c.name().to_string()).collect());
                        cached_types = row.columns().iter().map(|c| c.type_().clone()).collect();
                    }
                    let cols = columns.as_ref().unwrap();
                    count += 1;
                    println!("-[ RECORD {} ]-", count);
                    for (idx, col_name) in cols.iter().enumerate() {
                        let ty = &cached_types[idx];
                        let val_str = format_field_string(&row, idx, ty)
                            .unwrap_or_else(|| "NULL".to_string());
                        println!("{} | {}", col_name, val_str);
                    }
                }
                if count == 0 {
                    println!("(0 rows)");
                } else if count == 1 {
                    println!("(1 row)");
                } else {
                    println!("({} rows)", count);
                }
            }
            OutputFormat::Table | OutputFormat::Json => {
                // Buffered: table needs two passes (column widths), json needs
                // {columns, rows, row_count}. Both inherently require full result
                // set — impractical for very large exports, use csv/vertical instead.
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
                            serde_json::to_string_pretty(&result)
                                .unwrap_or_else(|_| result.to_string())
                        );
                    }
                    _ => unreachable!(),
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

#[cfg(test)]
mod tests_new {
    use super::*;

    #[test]
    fn strip_leading_comments_plain() {
        assert_eq!(strip_leading_comments("SELECT 1"), "SELECT 1");
    }

    #[test]
    fn strip_leading_comments_line_comment() {
        assert_eq!(
            strip_leading_comments("-- list users\nSELECT * FROM users"),
            "SELECT * FROM users"
        );
    }

    #[test]
    fn strip_leading_comments_block_comment() {
        assert_eq!(strip_leading_comments("/* hint */ SELECT 1"), "SELECT 1");
    }

    #[test]
    fn strip_leading_comments_nested_block() {
        assert_eq!(
            strip_leading_comments("/* outer /* inner */ rest */ SELECT 1"),
            "SELECT 1"
        );
    }

    #[test]
    fn strip_leading_comments_multiple() {
        assert_eq!(
            strip_leading_comments("-- first\n-- second\n/* block */\nSELECT 1"),
            "SELECT 1"
        );
    }

    #[test]
    fn strip_leading_comments_unclosed_block() {
        assert_eq!(strip_leading_comments("/* unfinished"), "");
    }

    #[test]
    fn value_to_compact_string_null_and_string() {
        assert_eq!(value_to_compact_string(&Value::Null), "NULL");
        assert_eq!(
            value_to_compact_string(&Value::String("hello".into())),
            "hello"
        );
        assert_eq!(value_to_compact_string(&Value::Bool(true)), "true");
        assert_eq!(
            value_to_compact_string(&Value::Number(serde_json::Number::from(42))),
            "42"
        );
    }

    #[test]
    fn render_result_execute_writes_affected_count() {
        let result = QueryResult {
            columns: vec![],
            rows: vec![],
            row_count: 0,
            affected: 3,
            kind: ResultKind::Execute,
        };
        let mut buf: Vec<u8> = Vec::new();
        render_result(&result, &mut buf, OutputFormat::Table, false).unwrap();
        let output = String::from_utf8(buf).unwrap();
        assert_eq!(output, "3\n");
    }

    #[test]
    fn render_result_empty_query_prints_zero_rows() {
        let result = QueryResult {
            columns: vec![],
            rows: vec![],
            row_count: 0,
            affected: 0,
            kind: ResultKind::Query,
        };
        let mut buf: Vec<u8> = Vec::new();
        render_result(&result, &mut buf, OutputFormat::Table, false).unwrap();
        let output = String::from_utf8(buf).unwrap();
        assert_eq!(output, "(0 rows)\n");
    }

    #[test]
    fn render_result_table_boxed_uses_box_chars() {
        let result = QueryResult {
            columns: vec!["a".into(), "b".into()],
            rows: vec![vec![Value::String("1".into()), Value::String("2".into())]],
            row_count: 1,
            affected: 0,
            kind: ResultKind::Query,
        };
        let mut buf: Vec<u8> = Vec::new();
        render_result(&result, &mut buf, OutputFormat::Table, true).unwrap();
        let output = String::from_utf8(buf).unwrap();
        assert!(output.contains('│'), "boxed table should contain │");
        assert!(output.contains('─'), "boxed table should contain ─");
    }

    #[test]
    fn render_result_table_ascii_uses_pipe_chars() {
        let result = QueryResult {
            columns: vec!["a".into(), "b".into()],
            rows: vec![vec![Value::String("1".into()), Value::String("2".into())]],
            row_count: 1,
            affected: 0,
            kind: ResultKind::Query,
        };
        let mut buf: Vec<u8> = Vec::new();
        render_result(&result, &mut buf, OutputFormat::Table, false).unwrap();
        let output = String::from_utf8(buf).unwrap();
        assert!(output.contains('|'), "ascii table should contain |");
        assert!(output.contains('-'), "ascii table should contain -");
        assert!(
            !output.contains('│'),
            "ascii table should NOT contain box-drawing │"
        );
    }

    #[test]
    fn render_result_csv_header_and_rows() {
        let result = QueryResult {
            columns: vec!["a".into(), "b".into()],
            rows: vec![vec![Value::String("1".into()), Value::String("2".into())]],
            row_count: 1,
            affected: 0,
            kind: ResultKind::Query,
        };
        let mut buf: Vec<u8> = Vec::new();
        render_result(&result, &mut buf, OutputFormat::Csv, false).unwrap();
        let output = String::from_utf8(buf).unwrap();
        // Normalize CRLF -> LF (csv crate emits CRLF by default).
        let output = output.replace("\r\n", "\n");
        let lines: Vec<&str> = output.lines().collect();
        assert_eq!(lines.len(), 2, "CSV should have header + 1 data row");
        assert_eq!(lines[0], "a,b");
        assert_eq!(lines[1], "1,2");
    }

    #[test]
    fn render_result_vertical_format() {
        let result = QueryResult {
            columns: vec!["col".into()],
            rows: vec![vec![Value::String("val".into())]],
            row_count: 1,
            affected: 0,
            kind: ResultKind::Query,
        };
        let mut buf: Vec<u8> = Vec::new();
        render_result(&result, &mut buf, OutputFormat::Vertical, false).unwrap();
        let output = String::from_utf8(buf).unwrap();
        assert!(
            output.contains("-[ RECORD 1 ]-"),
            "vertical should contain RECORD marker"
        );
        assert!(
            output.contains("col | val"),
            "vertical should show column | value"
        );
        assert!(output.contains("(1 row)"), "vertical should show row count");
    }
}
