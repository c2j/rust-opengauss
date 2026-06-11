use tokio_opengauss::Row;

pub(crate) fn format_row_value(row: &Row, idx: usize) -> serde_json::Value {
    if let Ok(v) = row.try_get::<_, Option<&str>>(idx) {
        return serde_json::Value::from(v.map(String::from));
    }
    if let Ok(v) = row.try_get::<_, Option<i32>>(idx) {
        return serde_json::json!(v);
    }
    if let Ok(v) = row.try_get::<_, Option<i64>>(idx) {
        return serde_json::json!(v);
    }
    if let Ok(v) = row.try_get::<_, Option<f64>>(idx) {
        return serde_json::json!(v);
    }
    if let Ok(v) = row.try_get::<_, Option<bool>>(idx) {
        return serde_json::json!(v);
    }
    if let Ok(v) = row.try_get::<_, Option<&[u8]>>(idx) {
        return serde_json::json!(v.map(|b| format!("\\x{}", hex_bytes(b))));
    }
    serde_json::Value::Null
}

pub(crate) fn hex_bytes(bytes: &[u8]) -> String {
    let mut result = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        result.push_str(&format!("{:02x}", b));
    }
    result
}

/// Format query results as a text table for CLI output.
pub(crate) fn format_table(columns: &[String], rows: &[Vec<serde_json::Value>]) -> String {
    if columns.is_empty() {
        return String::new();
    }

    // Calculate column widths
    let mut col_widths: Vec<usize> = columns.iter().map(|c| c.len()).collect();
    for row in rows {
        for (i, val) in row.iter().enumerate() {
            if i < col_widths.len() {
                let val_str = value_to_string(val);
                col_widths[i] = col_widths[i].max(val_str.len());
            }
        }
    }

    // Build separator
    let sep: String = col_widths
        .iter()
        .map(|w| "-".repeat(*w))
        .collect::<Vec<_>>()
        .join("-+-");

    // Build header
    let header: String = columns
        .iter()
        .enumerate()
        .map(|(i, c)| format!("{:width$}", c, width = col_widths[i]))
        .collect::<Vec<_>>()
        .join(" | ");

    // Build rows
    let mut result = String::new();
    result.push_str(&header);
    result.push('\n');
    result.push_str(&sep);
    result.push('\n');

    for row in rows {
        let line: String = row
            .iter()
            .enumerate()
            .map(|(i, val)| {
                let val_str = value_to_string(val);
                format!("{:width$}", val_str, width = col_widths[i])
            })
            .collect::<Vec<_>>()
            .join(" | ");
        result.push_str(&line);
        result.push('\n');
    }

    result
}

fn value_to_string(val: &serde_json::Value) -> String {
    match val {
        serde_json::Value::Null => "NULL".to_string(),
        serde_json::Value::String(s) => s.clone(),
        v => v.to_string(),
    }
}
