//! Interactive SQL REPL for gaussdb-mcp (rustyline Validator-based).

#![allow(dead_code)]

use std::borrow::Cow;
use std::path::PathBuf;

use rustyline::Editor;
use rustyline::config::Configurer;
use rustyline::error::ReadlineError;
use rustyline::highlight::Highlighter;
use rustyline::history::DefaultHistory;
use rustyline::validate::{ValidationContext, ValidationResult, Validator};
use rustyline_derive::{Completer, Helper, Hinter};

use crate::cli::{CliArgs, OutputFormat, QueryResult, execute_sql_buffered, render_result};
use crate::config::{
    TimeoutConfig, read_config, resolve_env_var_connection, resolve_single_connection,
    rewrite_password_to_sentinel, store_keyring_password,
};
use crate::connection::do_connect;
use crate::server::format_error_chain;

// ─── SqlTokenizer ───────────────────────────────────────────────────────────────

pub(crate) struct SplitResult {
    pub complete: Vec<String>,
    pub remainder: String,
}

pub(crate) struct SqlTokenizer;

impl SqlTokenizer {
    pub(crate) fn split_statements(input: &str) -> SplitResult {
        let mut complete: Vec<String> = Vec::new();
        let mut current = String::new();
        let mut in_single_quote = false;
        let mut in_double_quote = false;
        let mut in_line_comment = false;
        let mut in_block_comment_depth: u32 = 0;
        let mut dollar_quote_tag: Option<String> = None;

        let chars: Vec<char> = input.chars().collect();
        let len = chars.len();
        let mut i = 0;

        while i < len {
            let c = chars[i];

            if in_line_comment {
                if c == '\n' {
                    in_line_comment = false;
                }
                current.push(c);
                i += 1;
                continue;
            }

            if in_block_comment_depth > 0 {
                current.push(c);
                if c == '/' && i + 1 < len && chars[i + 1] == '*' {
                    in_block_comment_depth += 1;
                    i += 1;
                    current.push(chars[i]);
                } else if c == '*' && i + 1 < len && chars[i + 1] == '/' {
                    in_block_comment_depth -= 1;
                    i += 1;
                    current.push(chars[i]);
                }
                i += 1;
                continue;
            }

            if in_single_quote {
                current.push(c);
                if c == '\'' && i + 1 < len && chars[i + 1] == '\'' {
                    i += 1;
                    current.push(chars[i]);
                } else if c == '\'' {
                    in_single_quote = false;
                }
                i += 1;
                continue;
            }

            if in_double_quote {
                current.push(c);
                if c == '"' && i + 1 < len && chars[i + 1] == '"' {
                    i += 1;
                    current.push(chars[i]);
                } else if c == '"' {
                    in_double_quote = false;
                }
                i += 1;
                continue;
            }

            if let Some(ref tag) = dollar_quote_tag {
                current.push(c);
                if c == '$' {
                    if tag.is_empty() {
                        if i + 1 < len && chars[i + 1] == '$' {
                            i += 1;
                            current.push(chars[i]);
                            dollar_quote_tag = None;
                        }
                    } else {
                        let tag_chars: Vec<char> = tag.chars().collect();
                        let tag_len = tag_chars.len();
                        let mut matches = i + 1 + tag_len < len && chars[i + 1 + tag_len] == '$';
                        if matches {
                            for (k, &tc) in tag_chars.iter().enumerate() {
                                if chars[i + 1 + k] != tc {
                                    matches = false;
                                    break;
                                }
                            }
                        }
                        if matches {
                            for _ in 0..=tag_len {
                                i += 1;
                                current.push(chars[i]);
                            }
                            dollar_quote_tag = None;
                        }
                    }
                }
                i += 1;
                continue;
            }

            match c {
                '\'' => {
                    in_single_quote = true;
                    current.push(c);
                }
                '"' => {
                    in_double_quote = true;
                    current.push(c);
                }
                '$' => {
                    if i + 1 < len && chars[i + 1] == '$' {
                        dollar_quote_tag = Some(String::new());
                        current.push(c);
                        i += 1;
                        current.push(chars[i]);
                    } else if i + 1 < len
                        && (chars[i + 1].is_ascii_alphabetic() || chars[i + 1] == '_')
                    {
                        let mut j = i + 1;
                        while j < len && (chars[j].is_ascii_alphanumeric() || chars[j] == '_') {
                            j += 1;
                        }
                        if j < len && chars[j] == '$' {
                            let tag: String = chars[i + 1..j].iter().collect();
                            dollar_quote_tag = Some(tag);
                            current.extend(chars[i..=j].iter());
                            i = j;
                        } else {
                            current.push(c);
                        }
                    } else {
                        current.push(c);
                    }
                }
                '-' if i + 1 < len && chars[i + 1] == '-' => {
                    in_line_comment = true;
                    current.push(c);
                    i += 1;
                    current.push(chars[i]);
                }
                '/' if i + 1 < len && chars[i + 1] == '*' => {
                    in_block_comment_depth = 1;
                    current.push(c);
                    i += 1;
                    current.push(chars[i]);
                }
                ';' => {
                    let trimmed = current.trim().to_string();
                    if !trimmed.is_empty() {
                        complete.push(trimmed);
                    }
                    current = String::new();
                }
                _ => {
                    current.push(c);
                }
            }
            i += 1;
        }

        SplitResult {
            complete,
            remainder: current.trim_start().to_string(),
        }
    }
}

// ─── rustyline Helper (Validator + Highlighter) ────────────────────────────────

#[derive(Completer, Helper, Hinter)]
struct SqlHelper;

/// Uses `SqlTokenizer` to determine if the input is a complete statement
/// (all semicolons are outside quotes/comments/dollar-quotes). When incomplete,
/// rustyline inserts a newline and shows the `>` continuation prompt.
impl Validator for SqlHelper {
    fn validate(&self, ctx: &mut ValidationContext) -> rustyline::Result<ValidationResult> {
        let input = ctx.input();
        let trimmed = input.trim();
        if trimmed.is_empty() || trimmed.starts_with('.') || trimmed == "?" {
            return Ok(ValidationResult::Valid(None));
        }
        let split = SqlTokenizer::split_statements(input);
        if split.remainder.trim().is_empty() {
            Ok(ValidationResult::Valid(None))
        } else {
            Ok(ValidationResult::Incomplete)
        }
    }
}

/// Switches `$ ` → `> ` when rustyline is in continuation mode.
impl Highlighter for SqlHelper {
    fn highlight_prompt<'b, 's: 'b, 'p: 'b>(
        &'s self,
        prompt: &'p str,
        default: bool,
    ) -> Cow<'b, str> {
        if default {
            Cow::Borrowed(prompt)
        } else {
            Cow::Borrowed("> ")
        }
    }
}

// ─── OutputTarget ──────────────────────────────────────────────────────────────

enum OutputTarget {
    Stdout,
    File(std::fs::File, PathBuf),
}

// ─── Dot command handling ───────────────────────────────────────────────────────

struct DotAction {
    exit: bool,
}

struct ReplContext<'a> {
    history: &'a [String],
    output_target: &'a mut OutputTarget,
    last_result: &'a mut Option<QueryResult>,
    format: OutputFormat,
}

fn handle_dot_command(line: &str, ctx: &mut ReplContext) -> DotAction {
    let trimmed = line.trim();
    let parts: Vec<&str> = trimmed.split_whitespace().collect();
    if parts.is_empty() {
        return DotAction { exit: false };
    }
    let cmd = parts[0].to_lowercase();

    match cmd.as_str() {
        ".help" | "?" => {
            println!(".help / ?            Show this help message");
            println!(".exit / .quit        Exit the REPL");
            println!(".history             Show SQL execution history");
            println!(".clear / .cls        Clear the terminal screen");
            println!(
                ".output [<file>]     Redirect SQL output to file (append), \
                 or back to stdout"
            );
            println!(
                ".save <file> [fmt]   Save last query result to file (table/json/vertical/csv)"
            );
            DotAction { exit: false }
        }

        ".exit" | ".quit" => DotAction { exit: true },

        ".history" => {
            for (i, entry) in ctx.history.iter().enumerate() {
                let preview: String = entry
                    .chars()
                    .map(|c| if c == '\n' { ' ' } else { c })
                    .collect();
                let preview = preview.trim();
                let display = if preview.chars().count() > 80 {
                    format!("{}…", preview.chars().take(79).collect::<String>())
                } else {
                    preview.to_string()
                };
                println!("{:4}  {}", i + 1, display);
            }
            DotAction { exit: false }
        }

        ".clear" | ".cls" => {
            use std::io::Write;
            let mut stdout = std::io::stdout();
            let _ = write!(stdout, "\x1b[2J\x1b[H");
            let _ = stdout.flush();
            DotAction { exit: false }
        }

        ".output" => {
            match parts.len() {
                1 => {
                    *ctx.output_target = OutputTarget::Stdout;
                    println!("output reset to stdout");
                }
                _ => {
                    let file_path = parts[1..].join(" ");
                    match std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(&file_path)
                    {
                        Ok(file) => {
                            *ctx.output_target =
                                OutputTarget::File(file, PathBuf::from(&file_path));
                            println!("output redirected to {} (append)", file_path);
                        }
                        Err(e) => {
                            eprintln!("error: cannot open {}: {}", file_path, e);
                        }
                    }
                }
            }
            DotAction { exit: false }
        }

        ".save" => {
            if parts.len() < 2 {
                eprintln!("error: usage: .save <file> [format]");
                return DotAction { exit: false };
            }
            let (file_path, fmt) = if parts.len() >= 3 {
                match parts[parts.len() - 1].parse::<OutputFormat>() {
                    Ok(f) => (parts[1..parts.len() - 1].join(" "), f),
                    Err(_) => (parts[1..].join(" "), ctx.format),
                }
            } else {
                (parts[1].to_string(), ctx.format)
            };
            match ctx.last_result {
                None => {
                    eprintln!("error: no previous query result to save");
                }
                Some(result) => match std::fs::File::create(&file_path) {
                    Ok(mut file) => {
                        let boxed = matches!(fmt, OutputFormat::Table);
                        if let Err(e) = render_result(result, &mut file, fmt, boxed) {
                            eprintln!("error: {}", e);
                        } else {
                            let fmt_name = match fmt {
                                OutputFormat::Table => "table",
                                OutputFormat::Json => "json",
                                OutputFormat::Vertical => "vertical",
                                OutputFormat::Csv => "csv",
                            };
                            println!(
                                "saved {} row(s) to {} ({})",
                                result.row_count, file_path, fmt_name
                            );
                        }
                    }
                    Err(e) => {
                        eprintln!("error: cannot create {}: {}", file_path, e);
                    }
                },
            }
            DotAction { exit: false }
        }

        _ => {
            eprintln!("error: unknown command '{}', type .help for list", line);
            DotAction { exit: false }
        }
    }
}

// ─── REPL loop ──────────────────────────────────────────────────────────────────

const PROMPT: &str = "$ ";

// ─── Persistent per-connection history ─────────────────────────────────────────

/// Max entries retained per connection history file. Oldest entries are dropped
/// first (rustyline `DefaultHistory` evicts in insertion order once the cap is
/// reached).
const HISTORY_MAX_ENTRIES: usize = 1000;

/// Sanitize a connection name into a filesystem-safe history file name.
///
/// Replaces any char outside `[A-Za-z0-9._-]` with `_` so that connection names
/// containing path separators (e.g. `prod/shard1`) cannot escape the history
/// directory. The empty name, `.`, and `..` collapse to `default`.
fn sanitize_history_name(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect();
    match s.as_str() {
        "" | "." | ".." => "default".to_string(),
        other => other.to_string(),
    }
}

/// Resolve the per-connection history file path under the app's local data dir.
///
/// Layout: `$data_local_dir/gaussdb-mcp/history/<sanitized_name>` — mirrors the
/// existing log-dir convention in `main.rs::init_logging`. Returns `None` only
/// when the platform cannot provide a local data directory.
fn history_path_for(connection_name: &str) -> Option<PathBuf> {
    let dir = dirs::data_local_dir()?.join("gaussdb-mcp").join("history");
    Some(dir.join(sanitize_history_name(connection_name)))
}

pub(crate) async fn run_interactive(args: CliArgs) -> Result<(), String> {
    let config_path = args.config_path.map(PathBuf::from);
    let raw = read_config(config_path)?;

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

    let effective_timeout = TimeoutConfig::from_overrides(
        args.statement_timeout.as_deref(),
        args.connection_max_lifetime.as_deref(),
        args.timeout_action.as_deref(),
        Some(&target.timeout_config),
    )
    .map_err(|e| format!("Invalid timeout configuration: {}", e))?;

    let (client, _handle) = do_connect(&target.connection_url, Some(&effective_timeout))
        .await
        .map_err(|e| format!("Connection failed: {}", format_error_chain(e.as_ref())))?;

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

    let db_name: String = match client.query_one("SELECT current_database()", &[]).await {
        Ok(row) => row
            .get::<_, Option<&str>>(0)
            .unwrap_or("(unknown)")
            .to_string(),
        Err(_) => "(unknown)".to_string(),
    };

    let mut rl = Editor::<SqlHelper, DefaultHistory>::new()
        .map_err(|e| format!("failed to init editor: {}", e))?;
    rl.set_helper(Some(SqlHelper));

    let _ = rl.set_max_history_size(HISTORY_MAX_ENTRIES);

    let history_path: Option<PathBuf> = if !args.no_history {
        match history_path_for(&target.name) {
            Some(p) => {
                if let Some(parent) = p.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                // Tolerate missing file on first run.
                let _ = rl.load_history(&p);
                Some(p)
            }
            None => None,
        }
    } else {
        None
    };

    println!(
        "gaussdb-mcp interactive — connected to '{}' ({}). \
         Type .help for commands, .exit to quit.",
        target.name, db_name
    );

    let mut output_target = OutputTarget::Stdout;
    let mut last_result: Option<QueryResult> = None;
    let format = args.format;

    loop {
        let input = match rl.readline(PROMPT) {
            Ok(input) => input,
            Err(ReadlineError::Interrupted) => {
                println!("^C");
                continue;
            }
            Err(ReadlineError::Eof) => {
                println!();
                break;
            }
            Err(e) => {
                if let Some(p) = &history_path {
                    let _ = rl.save_history(p);
                }
                return Err(format!("readline error: {}", e));
            }
        };

        let trimmed = input.trim();
        if trimmed.is_empty() {
            continue;
        }

        if trimmed.starts_with('.') || trimmed == "?" {
            let history_snapshot: Vec<String> = rl.history().iter().cloned().collect();
            let mut ctx = ReplContext {
                history: &history_snapshot,
                output_target: &mut output_target,
                last_result: &mut last_result,
                format,
            };
            let action = handle_dot_command(&input, &mut ctx);
            if action.exit {
                break;
            }
            continue;
        }

        let _ = rl.add_history_entry(&input);

        let split = SqlTokenizer::split_statements(&input);
        for stmt in &split.complete {
            match execute_sql_buffered(&client, stmt).await {
                Ok(query_result) => {
                    last_result = Some(query_result.clone());
                    match &mut output_target {
                        OutputTarget::Stdout => {
                            if let Err(e) =
                                render_result(&query_result, &mut std::io::stdout(), format, true)
                            {
                                eprintln!("render error: {}", e);
                            }
                        }
                        OutputTarget::File(f, _) => {
                            if let Err(e) = render_result(&query_result, f, format, true) {
                                eprintln!("render error: {}", e);
                            }
                        }
                    }
                }
                Err(e) => {
                    eprintln!("error: {}", e);
                }
            }
        }

        if !split.complete.is_empty() {
            println!();
        }
    }

    if let Some(p) = &history_path {
        let _ = rl.save_history(p);
    }
    Ok(())
}

// ─── Unit Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_single_statement() {
        let r = SqlTokenizer::split_statements("SELECT 1;");
        assert_eq!(r.complete, vec!["SELECT 1"]);
        assert_eq!(r.remainder, "");
    }

    #[test]
    fn split_multiple_statements() {
        let r = SqlTokenizer::split_statements("SELECT 1; SELECT 2;");
        assert_eq!(r.complete, vec!["SELECT 1", "SELECT 2"]);
        assert_eq!(r.remainder, "");
    }

    #[test]
    fn split_semicolon_in_single_quotes() {
        let r = SqlTokenizer::split_statements("SELECT ';' AS a;");
        assert_eq!(r.complete, vec!["SELECT ';' AS a"]);
        assert_eq!(r.remainder, "");
    }

    #[test]
    fn split_double_quote_with_dash_dash() {
        let r = SqlTokenizer::split_statements("SELECT \"--x\"; INSERT");
        assert_eq!(r.complete, vec!["SELECT \"--x\""]);
        assert_eq!(r.remainder, "INSERT");
    }

    #[test]
    fn split_line_comment_no_semicolon() {
        let r = SqlTokenizer::split_statements("SELECT 1 -- comment\n");
        assert!(r.complete.is_empty());
        assert_eq!(r.remainder, "SELECT 1 -- comment\n");
    }

    #[test]
    fn split_block_comment_then_semicolon() {
        let r = SqlTokenizer::split_statements("/* comment */ SELECT 1;");
        assert_eq!(r.complete, vec!["/* comment */ SELECT 1"]);
        assert_eq!(r.remainder, "");
    }

    #[test]
    fn split_double_semicolons() {
        let r = SqlTokenizer::split_statements(";;");
        assert!(r.complete.is_empty());
        assert_eq!(r.remainder, "");
    }

    #[test]
    fn split_incomplete_no_semicolon() {
        let r = SqlTokenizer::split_statements("SELECT 'a'");
        assert!(r.complete.is_empty());
        assert_eq!(r.remainder, "SELECT 'a'");
    }

    #[test]
    fn split_line_comment_with_semicolon() {
        let r = SqlTokenizer::split_statements("SELECT 1 -- comment\n;");
        assert_eq!(r.complete, vec!["SELECT 1 -- comment"]);
        assert_eq!(r.remainder, "");
    }

    #[test]
    fn split_escaped_quote_in_single_quotes() {
        let r = SqlTokenizer::split_statements("SELECT 'it''s' AS msg;");
        assert_eq!(r.complete, vec!["SELECT 'it''s' AS msg"]);
        assert_eq!(r.remainder, "");
    }

    #[test]
    fn split_escaped_quote_in_double_quotes() {
        let r = SqlTokenizer::split_statements("SELECT \"\"hello\"\" AS greeting;");
        assert_eq!(r.complete, vec!["SELECT \"\"hello\"\" AS greeting"]);
        assert_eq!(r.remainder, "");
    }

    #[test]
    fn split_dollar_quote_untagged() {
        let sql = "SELECT $$hello; world$$ AS msg;";
        let r = SqlTokenizer::split_statements(sql);
        assert_eq!(r.complete, vec!["SELECT $$hello; world$$ AS msg"]);
        assert_eq!(r.remainder, "");
    }

    #[test]
    fn split_dollar_quote_tagged() {
        let sql = "SELECT $body$hello; world$body$ AS msg;";
        let r = SqlTokenizer::split_statements(sql);
        assert_eq!(r.complete, vec!["SELECT $body$hello; world$body$ AS msg"]);
        assert_eq!(r.remainder, "");
    }

    #[test]
    fn split_dollar_quote_function_definition() {
        let sql = "CREATE FUNCTION foo() RETURNS void AS $$\nBEGIN\n  RAISE NOTICE 'hi';\nEND;\n$$ LANGUAGE plpgsql;";
        let r = SqlTokenizer::split_statements(sql);
        assert_eq!(r.complete.len(), 1);
        assert_eq!(r.remainder, "");
        assert!(r.complete[0].contains("CREATE FUNCTION foo()"));
        assert!(r.complete[0].contains("LANGUAGE plpgsql"));
    }

    #[test]
    fn split_dollar_quote_incomplete() {
        let sql = "CREATE FUNCTION foo() AS $$\nBEGIN\n  SELECT 1;";
        let r = SqlTokenizer::split_statements(sql);
        assert!(r.complete.is_empty());
        assert_eq!(r.remainder, sql);
    }

    #[test]
    fn split_dollar_quote_mismatched_tags_stays_open() {
        let sql = "SELECT $a$hello$b$;";
        let r = SqlTokenizer::split_statements(sql);
        assert!(r.complete.is_empty());
        assert_eq!(r.remainder, sql);
    }

    #[test]
    fn split_positional_param_not_dollar_quote() {
        let r = SqlTokenizer::split_statements("SELECT $1, $2;");
        assert_eq!(r.complete, vec!["SELECT $1, $2"]);
        assert_eq!(r.remainder, "");
    }

    #[test]
    fn split_dollar_quote_multiple_statements() {
        let sql = "SELECT $$a$$; INSERT INTO t VALUES ($$b; c$$);";
        let r = SqlTokenizer::split_statements(sql);
        assert_eq!(r.complete.len(), 2);
        assert_eq!(r.complete[0], "SELECT $$a$$");
        assert_eq!(r.complete[1], "INSERT INTO t VALUES ($$b; c$$)");
        assert_eq!(r.remainder, "");
    }

    #[test]
    fn sanitize_keeps_safe_chars() {
        assert_eq!(sanitize_history_name("prod"), "prod");
        assert_eq!(sanitize_history_name("dev-shard1"), "dev-shard1");
        assert_eq!(sanitize_history_name("db.v2"), "db.v2");
        assert_eq!(sanitize_history_name("a_b-c.d"), "a_b-c.d");
    }

    #[test]
    fn sanitize_replaces_path_separators_and_unsafe() {
        assert_eq!(sanitize_history_name("prod/shard1"), "prod_shard1");
        assert_eq!(sanitize_history_name(r"win\path"), "win_path");
        assert_eq!(sanitize_history_name("a:b*c?d"), "a_b_c_d");
        assert_eq!(sanitize_history_name("café"), "caf_");
    }

    #[test]
    fn sanitize_reserves_dot_and_empty() {
        assert_eq!(sanitize_history_name(""), "default");
        assert_eq!(sanitize_history_name("."), "default");
        assert_eq!(sanitize_history_name(".."), "default");
    }

    #[test]
    fn sanitize_blocks_traversal_attempt() {
        let s = sanitize_history_name("../../etc/passwd");
        // Traversal requires a path separator; `..` substrings alone are just a filename.
        assert!(!s.contains('/'));
        assert!(!s.contains('\\'));
    }

    #[test]
    fn history_path_ends_with_sanitized_name_under_history_dir() {
        let p = history_path_for("prod/shard1").expect("data dir available in test env");
        assert!(p.ends_with("history/prod_shard1"));
        assert!(p.to_string_lossy().contains("gaussdb-mcp"));
    }

    #[test]
    fn history_path_empty_name_uses_default() {
        let p = history_path_for("").expect("data dir available in test env");
        assert!(p.ends_with("history/default"));
    }
}
