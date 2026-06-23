use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use serde_json::{Value, json};
use std::error::Error;
use std::net::IpAddr;
use tokio_opengauss::Row;
use tokio_opengauss::types::{FromSql, Type};

/// Generic byte-extractor that accepts ANY Postgres type so the dispatch
/// fallback can read raw bytes for types without an explicit handler.
/// Without this, `try_get::<Option<&[u8]>>` would only succeed for BYTEA
/// (OID 17) and silently drop every other unsupported type back to NULL.
struct RawBytes<'a>(Option<&'a [u8]>);

impl<'a> FromSql<'a> for RawBytes<'a> {
    fn from_sql(_: &Type, raw: &'a [u8]) -> Result<Self, Box<dyn Error + Sync + Send>> {
        Ok(RawBytes(Some(raw)))
    }

    fn from_sql_null(_: &Type) -> Result<Self, Box<dyn Error + Sync + Send>> {
        Ok(RawBytes(None))
    }

    fn accepts(_: &Type) -> bool {
        true
    }
}

pub(crate) fn format_row_value(row: &Row, idx: usize) -> Value {
    format_value_with_type(row, idx, row.columns()[idx].type_())
}

/// Type-explicit variant so streaming callers (CSV/Vertical) can hoist the
/// column-type lookup out of the inner cell loop and reuse one `&Type`
/// reference per column across all rows.
pub(crate) fn format_value_with_type(row: &Row, idx: usize, ty: &Type) -> Value {
    match *ty {
        Type::VARCHAR | Type::TEXT | Type::BPCHAR | Type::NAME | Type::UNKNOWN => {
            typed_or_raw(row, idx, ty, |r, i| {
                match r.try_get::<_, Option<String>>(i) {
                    Ok(Some(s)) => Some(Value::String(s)),
                    Ok(None) => Some(Value::Null),
                    Err(_) => None,
                }
            })
        }
        Type::INT2 => typed_or_raw(row, idx, ty, |r, i| {
            r.try_get::<_, Option<i16>>(i)
                .ok()
                .map(|v| v.map(|x| json!(x)).unwrap_or(Value::Null))
        }),
        Type::INT4 | Type::OID | Type::REGPROC | Type::REGTYPE => {
            typed_or_raw(row, idx, ty, |r, i| {
                r.try_get::<_, Option<i32>>(i)
                    .ok()
                    .map(|v| v.map(|x| json!(x)).unwrap_or(Value::Null))
            })
        }
        Type::INT8 | Type::REGCLASS => typed_or_raw(row, idx, ty, |r, i| {
            r.try_get::<_, Option<i64>>(i)
                .ok()
                .map(|v| v.map(|x| json!(x)).unwrap_or(Value::Null))
        }),
        Type::FLOAT4 => typed_or_raw(row, idx, ty, |r, i| {
            r.try_get::<_, Option<f32>>(i)
                .ok()
                .map(|v| v.map(|x| json!(x)).unwrap_or(Value::Null))
        }),
        Type::FLOAT8 => typed_or_raw(row, idx, ty, |r, i| {
            r.try_get::<_, Option<f64>>(i)
                .ok()
                .map(|v| v.map(|x| json!(x)).unwrap_or(Value::Null))
        }),
        Type::NUMERIC => typed_or_raw(row, idx, ty, |r, i| {
            match r.try_get::<_, Option<Decimal>>(i) {
                Ok(Some(d)) => Some(decimal_to_json(d)),
                Ok(None) => Some(Value::Null),
                Err(_) => None,
            }
        }),
        Type::BOOL => typed_or_raw(row, idx, ty, |r, i| match r.try_get::<_, Option<bool>>(i) {
            Ok(Some(v)) => Some(json!(v)),
            Ok(None) => Some(Value::Null),
            Err(_) => None,
        }),
        Type::BYTEA => typed_or_raw(row, idx, ty, |r, i| {
            match r.try_get::<_, Option<&[u8]>>(i) {
                Ok(Some(b)) => Some(Value::String(format!("\\x{}", hex_bytes(b)))),
                Ok(None) => Some(Value::Null),
                Err(_) => None,
            }
        }),
        Type::UUID => typed_or_raw(row, idx, ty, |r, i| {
            match r.try_get::<_, Option<uuid::Uuid>>(i) {
                Ok(Some(u)) => Some(Value::String(u.to_string())),
                Ok(None) => Some(Value::Null),
                Err(_) => None,
            }
        }),
        Type::JSON | Type::JSONB => typed_or_raw(row, idx, ty, |r, i| {
            match r.try_get::<_, Option<Value>>(i) {
                Ok(Some(v)) => Some(v),
                Ok(None) => Some(Value::Null),
                Err(_) => None,
            }
        }),
        Type::TIMESTAMP | Type::TIMESTAMPTZ => typed_or_raw(row, idx, ty, |r, i| {
            match r.try_get::<_, Option<chrono::NaiveDateTime>>(i) {
                Ok(Some(v)) => Some(Value::String(v.to_string())),
                Ok(None) => Some(Value::Null),
                Err(_) => None,
            }
        }),
        Type::DATE => typed_or_raw(row, idx, ty, |r, i| {
            match r.try_get::<_, Option<chrono::NaiveDate>>(i) {
                Ok(Some(v)) => Some(Value::String(v.to_string())),
                Ok(None) => Some(Value::Null),
                Err(_) => None,
            }
        }),
        Type::TIME | Type::TIMETZ => typed_or_raw(row, idx, ty, |r, i| {
            match r.try_get::<_, Option<chrono::NaiveTime>>(i) {
                Ok(Some(v)) => Some(Value::String(v.to_string())),
                Ok(None) => Some(Value::Null),
                Err(_) => None,
            }
        }),
        Type::INET | Type::CIDR => typed_or_raw(row, idx, ty, |r, i| {
            match r.try_get::<_, Option<IpAddr>>(i) {
                Ok(Some(ip)) => Some(Value::String(ip.to_string())),
                Ok(None) => Some(Value::Null),
                Err(_) => None,
            }
        }),
        Type::MACADDR => typed_or_raw(row, idx, ty, |r, i| match r.try_get::<_, RawBytes>(i) {
            Ok(RawBytes(Some(b))) if b.len() == 6 => Some(Value::String(format!(
                "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
                b[0], b[1], b[2], b[3], b[4], b[5]
            ))),
            Ok(RawBytes(None)) => Some(Value::Null),
            _ => None,
        }),
        Type::MACADDR8 => typed_or_raw(row, idx, ty, |r, i| match r.try_get::<_, RawBytes>(i) {
            Ok(RawBytes(Some(b))) if b.len() == 8 => Some(Value::String(format!(
                "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
                b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]
            ))),
            Ok(RawBytes(None)) => Some(Value::Null),
            _ => None,
        }),
        Type::INTERVAL => typed_or_raw(row, idx, ty, |r, i| match r.try_get::<_, RawBytes>(i) {
            Ok(RawBytes(Some(b))) => Some(Value::String(format_interval(b))),
            Ok(RawBytes(None)) => Some(Value::Null),
            _ => None,
        }),
        _ => raw_bytes_fallback(row, idx, ty),
    }
}

pub(crate) fn format_field_string(row: &Row, idx: usize, ty: &Type) -> Option<String> {
    match *ty {
        Type::VARCHAR | Type::TEXT | Type::BPCHAR | Type::NAME | Type::UNKNOWN => {
            typed_or_raw_str(row, idx, ty, |r, i| {
                match r.try_get::<_, Option<String>>(i) {
                    Ok(Some(s)) => Some(Some(s)),
                    Ok(None) => Some(None),
                    Err(_) => None,
                }
            })
        }
        Type::INT2 => typed_or_raw_str(row, idx, ty, |r, i| {
            r.try_get::<_, Option<i16>>(i)
                .ok()
                .map(|v| v.map(|x| x.to_string()))
        }),
        Type::INT4 | Type::OID | Type::REGPROC | Type::REGTYPE => {
            typed_or_raw_str(row, idx, ty, |r, i| {
                r.try_get::<_, Option<i32>>(i)
                    .ok()
                    .map(|v| v.map(|x| x.to_string()))
            })
        }
        Type::INT8 | Type::REGCLASS => typed_or_raw_str(row, idx, ty, |r, i| {
            r.try_get::<_, Option<i64>>(i)
                .ok()
                .map(|v| v.map(|x| x.to_string()))
        }),
        Type::FLOAT4 => typed_or_raw_str(row, idx, ty, |r, i| {
            r.try_get::<_, Option<f32>>(i)
                .ok()
                .map(|v| v.map(|x| x.to_string()))
        }),
        Type::FLOAT8 => typed_or_raw_str(row, idx, ty, |r, i| {
            r.try_get::<_, Option<f64>>(i)
                .ok()
                .map(|v| v.map(|x| x.to_string()))
        }),
        Type::NUMERIC => typed_or_raw_str(row, idx, ty, |r, i| {
            match r.try_get::<_, Option<Decimal>>(i) {
                Ok(Some(d)) => Some(Some(d.to_string())),
                Ok(None) => Some(None),
                Err(_) => None,
            }
        }),
        Type::BOOL => typed_or_raw_str(row, idx, ty, |r, i| {
            r.try_get::<_, Option<bool>>(i)
                .ok()
                .map(|v| v.map(|x| x.to_string()))
        }),
        Type::BYTEA => typed_or_raw_str(row, idx, ty, |r, i| {
            match r.try_get::<_, Option<&[u8]>>(i) {
                Ok(Some(b)) => Some(Some(format!("\\x{}", hex_bytes(b)))),
                Ok(None) => Some(None),
                Err(_) => None,
            }
        }),
        Type::UUID => typed_or_raw_str(row, idx, ty, |r, i| {
            match r.try_get::<_, Option<uuid::Uuid>>(i) {
                Ok(Some(u)) => Some(Some(u.to_string())),
                Ok(None) => Some(None),
                Err(_) => None,
            }
        }),
        Type::JSON | Type::JSONB => typed_or_raw_str(row, idx, ty, |r, i| {
            match r.try_get::<_, Option<Value>>(i) {
                Ok(Some(v)) => Some(Some(v.to_string())),
                Ok(None) => Some(None),
                Err(_) => None,
            }
        }),
        Type::TIMESTAMP | Type::TIMESTAMPTZ => typed_or_raw_str(row, idx, ty, |r, i| match r
            .try_get::<_, Option<chrono::NaiveDateTime>>(i)
        {
            Ok(Some(v)) => Some(Some(v.to_string())),
            Ok(None) => Some(None),
            Err(_) => None,
        }),
        Type::DATE => typed_or_raw_str(row, idx, ty, |r, i| {
            match r.try_get::<_, Option<chrono::NaiveDate>>(i) {
                Ok(Some(v)) => Some(Some(v.to_string())),
                Ok(None) => Some(None),
                Err(_) => None,
            }
        }),
        Type::TIME | Type::TIMETZ => typed_or_raw_str(row, idx, ty, |r, i| {
            match r.try_get::<_, Option<chrono::NaiveTime>>(i) {
                Ok(Some(v)) => Some(Some(v.to_string())),
                Ok(None) => Some(None),
                Err(_) => None,
            }
        }),
        Type::INET | Type::CIDR => typed_or_raw_str(row, idx, ty, |r, i| {
            match r.try_get::<_, Option<IpAddr>>(i) {
                Ok(Some(ip)) => Some(Some(ip.to_string())),
                Ok(None) => Some(None),
                Err(_) => None,
            }
        }),
        Type::MACADDR => typed_or_raw_str(row, idx, ty, |r, i| match r.try_get::<_, RawBytes>(i) {
            Ok(RawBytes(Some(b))) if b.len() == 6 => Some(Some(format!(
                "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
                b[0], b[1], b[2], b[3], b[4], b[5]
            ))),
            Ok(RawBytes(None)) => Some(None),
            _ => None,
        }),
        Type::MACADDR8 => {
            typed_or_raw_str(row, idx, ty, |r, i| match r.try_get::<_, RawBytes>(i) {
                Ok(RawBytes(Some(b))) if b.len() == 8 => Some(Some(format!(
                    "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
                    b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]
                ))),
                Ok(RawBytes(None)) => Some(None),
                _ => None,
            })
        }
        Type::INTERVAL => {
            typed_or_raw_str(row, idx, ty, |r, i| match r.try_get::<_, RawBytes>(i) {
                Ok(RawBytes(Some(b))) => Some(Some(format_interval(b))),
                Ok(RawBytes(None)) => Some(None),
                _ => None,
            })
        }
        _ => raw_bytes_string_fallback(row, idx, ty),
    }
}

fn typed_or_raw_str<F>(row: &Row, idx: usize, ty: &Type, extract: F) -> Option<String>
where
    F: Fn(&Row, usize) -> Option<Option<String>>,
{
    match extract(row, idx) {
        Some(v) => v,
        None => raw_bytes_string_fallback(row, idx, ty),
    }
}

fn raw_bytes_string_fallback(row: &Row, idx: usize, ty: &Type) -> Option<String> {
    match row.try_get::<_, RawBytes>(idx) {
        Ok(RawBytes(Some(b))) => {
            tracing::warn!(
                type_name = ty.name(),
                column_index = idx,
                bytes_len = b.len(),
                "unsupported column type, emitting hex fallback"
            );
            Some(format_unsupported_type(ty.name(), b))
        }
        Ok(RawBytes(None)) => None,
        Err(_) => None,
    }
}

/// Dispatch helper: try the typed extraction first; if it returns None
/// (meaning the typed FromSql failed), fall through to a raw-bytes
/// placeholder rather than dropping the value to NULL. This closes the
/// last loophole through which silent data loss could recur on a known
/// type whose decoder happens to reject a particular value (e.g. a
/// NUMERIC column with scale > 28 that rust_decimal cannot represent).
fn typed_or_raw<F>(row: &Row, idx: usize, ty: &Type, extract: F) -> Value
where
    F: Fn(&Row, usize) -> Option<Value>,
{
    match extract(row, idx) {
        Some(v) => v,
        None => raw_bytes_fallback(row, idx, ty),
    }
}

fn raw_bytes_fallback(row: &Row, idx: usize, ty: &Type) -> Value {
    match row.try_get::<_, RawBytes>(idx) {
        Ok(RawBytes(Some(b))) => {
            tracing::warn!(
                type_name = ty.name(),
                column_index = idx,
                bytes_len = b.len(),
                "unsupported column type, emitting hex fallback"
            );
            Value::String(format_unsupported_type(ty.name(), b))
        }
        Ok(RawBytes(None)) => Value::Null,
        Err(_) => Value::Null,
    }
}

/// Render a NUMERIC `Decimal` as a JSON value.
///
/// Prefers `Number` when the value fits in `i64`/`u64`/`f64` without loss;
/// falls back to `String` for high-precision values that JSON numbers
/// cannot represent exactly (fractional with total significant digits
/// greater than 15, which exceeds IEEE 754 double's ~15.95 decimal-digit
/// precision; or integers outside the i64/u64 range).
fn decimal_to_json(d: Decimal) -> Value {
    // Integer fast-paths: i64/u64 give exact JSON Number representation
    // for the full machine-integer range regardless of digit count.
    if d.is_integer() {
        if let Some(i) = d.to_i64() {
            return json!(i);
        }
        if let Some(u) = d.to_u64() {
            return json!(u);
        }
        return Value::String(d.to_string());
    }
    // Fractional: guard f64 precision. Total significant digits
    // (integer digits + scale) > 15 exceeds IEEE 754 double precision
    // and would silently round-trip lose precision via f64.
    let int_digits = integer_digit_count(&d);
    let total_digits = int_digits + d.scale() as usize;
    if total_digits > 15 {
        return Value::String(d.to_string());
    }
    if let Some(f) = d.to_f64() {
        if let Some(n) = serde_json::Number::from_f64(f) {
            return Value::Number(n);
        }
    }
    Value::String(d.to_string())
}

fn integer_digit_count(d: &Decimal) -> usize {
    if d.is_zero() {
        return 1;
    }
    let s = d.abs().to_string();
    let int_part = s.split('.').next().unwrap_or("");
    int_part.len()
}

/// PostgreSQL INTERVAL binary layout: i64 microseconds + i32 days +
/// i32 months, all big-endian. Format mirrors psql's pretty-printing.
fn format_interval(b: &[u8]) -> String {
    if b.len() != 16 {
        return format!("<malformed interval: {} bytes>", b.len());
    }
    let mut buf = [0u8; 16];
    buf.copy_from_slice(b);
    let micros = i64::from_be_bytes(buf[0..8].try_into().unwrap());
    let days = i32::from_be_bytes(buf[8..12].try_into().unwrap());
    let months = i32::from_be_bytes(buf[12..16].try_into().unwrap());

    let mut parts: Vec<String> = Vec::new();
    if months != 0 {
        let years = months / 12;
        let mons = months % 12;
        if years != 0 {
            parts.push(format!("{} years", years));
        }
        if mons != 0 {
            parts.push(format!("{} mons", mons));
        }
    }
    if days != 0 {
        parts.push(format!("{} days", days));
    }
    if micros != 0 {
        let total_secs = micros.div_euclid(1_000_000);
        let frac = micros.rem_euclid(1_000_000).abs();
        let h = total_secs / 3600;
        let m = (total_secs % 3600) / 60;
        let s = total_secs % 60;
        let time_part = if frac > 0 {
            format!("{:02}:{:02}:{:02}.{:06}", h.abs(), m.abs(), s.abs(), frac)
        } else {
            format!("{:02}:{:02}:{:02}", h.abs(), m.abs(), s.abs())
        };
        parts.push(time_part);
    }
    if parts.is_empty() {
        "00:00:00".to_string()
    } else {
        parts.join(" ")
    }
}

/// Build a visible placeholder for unsupported column types so silent data
/// loss (the previous behaviour, returning `Null`) cannot recur.
fn format_unsupported_type(type_name: &str, bytes: &[u8]) -> String {
    format!("<unsupported type {}>: \\x{}", type_name, hex_bytes(bytes))
}

pub(crate) fn hex_bytes(bytes: &[u8]) -> String {
    let mut result = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        result.push_str(&format!("{:02x}", b));
    }
    result
}

/// Format query results as a text table for CLI output.
pub(crate) fn format_table(columns: &[String], rows: &[Vec<Value>]) -> String {
    if columns.is_empty() {
        return String::new();
    }

    let mut col_widths: Vec<usize> = columns.iter().map(|c| c.chars().count()).collect();
    for row in rows {
        for (i, val) in row.iter().enumerate() {
            if i < col_widths.len() {
                let val_str = value_to_string(val);
                col_widths[i] = col_widths[i].max(val_str.chars().count());
            }
        }
    }

    let sep: String = col_widths
        .iter()
        .map(|w| "-".repeat(*w))
        .collect::<Vec<_>>()
        .join("-+-");

    let header: String = columns
        .iter()
        .enumerate()
        .map(|(i, c)| format!("{:width$}", c, width = col_widths[i]))
        .collect::<Vec<_>>()
        .join(" | ");

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

/// Format query results as a table using Unicode box-drawing characters.
/// This is the default format for interactive REPL mode.
#[allow(dead_code)]
pub(crate) fn format_table_boxed(columns: &[String], rows: &[Vec<Value>]) -> String {
    if columns.is_empty() {
        return String::new();
    }

    let mut col_widths: Vec<usize> = columns.iter().map(|c| c.chars().count()).collect();
    for row in rows {
        for (i, val) in row.iter().enumerate() {
            if i < col_widths.len() {
                let val_str = value_to_string(val);
                col_widths[i] = col_widths[i].max(val_str.chars().count());
            }
        }
    }

    let make_cells = |values: &[String]| -> Vec<String> {
        values
            .iter()
            .enumerate()
            .map(|(i, s)| format!("{:width$}", s, width = col_widths[i]))
            .collect()
    };

    // Top border:  ┌───┬───┐
    let top: String = {
        let parts: Vec<String> = col_widths.iter().map(|w| "─".repeat(w + 2)).collect();
        format!("┌{}┐", parts.join("┬"))
    };

    // Header separator:  ├───┼───┤
    let header_sep: String = {
        let parts: Vec<String> = col_widths.iter().map(|w| "─".repeat(w + 2)).collect();
        format!("├{}┤", parts.join("┼"))
    };

    // Bottom border:  └───┴───┘
    let bottom: String = {
        let parts: Vec<String> = col_widths.iter().map(|w| "─".repeat(w + 2)).collect();
        format!("└{}┘", parts.join("┴"))
    };

    let format_row = |cells: &[String]| -> String {
        let inner = cells.join(" │ ");
        format!("│ {} │", inner)
    };

    let mut result = String::new();
    result.push_str(&top);
    result.push('\n');

    let header_cells = make_cells(columns);
    result.push_str(&format_row(&header_cells));
    result.push('\n');

    result.push_str(&header_sep);
    result.push('\n');

    for row in rows {
        let cell_strs: Vec<String> = row.iter().map(value_to_string).collect();
        let row_cells = make_cells(&cell_strs);
        result.push_str(&format_row(&row_cells));
        result.push('\n');
    }

    result.push_str(&bottom);
    result.push('\n');

    result
}

fn value_to_string(val: &Value) -> String {
    match val {
        Value::Null => "NULL".to_string(),
        Value::String(s) => s.clone(),
        v => v.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal::Decimal;
    use std::str::FromStr;

    #[test]
    fn decimal_to_json_positive_integer() {
        let d = Decimal::from_str("3950").unwrap();
        let v = decimal_to_json(d);
        assert_eq!(v, json!(3950i64));
    }

    #[test]
    fn decimal_to_json_negative_integer() {
        let d = Decimal::from_str("-100").unwrap();
        let v = decimal_to_json(d);
        assert_eq!(v, json!(-100i64));
    }

    #[test]
    fn decimal_to_json_fraction_fits_f64() {
        let d = Decimal::from_str("3950.123456").unwrap();
        let v = decimal_to_json(d);
        let expected = serde_json::Number::from_f64(3950.123456).unwrap();
        assert_eq!(v, Value::Number(expected));
    }

    #[test]
    fn decimal_to_json_u64_max_as_number() {
        let d = Decimal::from_str("18446744073709551615").unwrap();
        let v = decimal_to_json(d);
        assert_eq!(v, json!(u64::MAX));
    }

    #[test]
    fn decimal_to_json_beyond_u64_falls_back_to_string() {
        let d = Decimal::from_str("18446744073709551616").unwrap();
        let v = decimal_to_json(d);
        assert_eq!(v, Value::String("18446744073709551616".to_string()));
    }

    #[test]
    fn decimal_to_json_negative_beyond_i64_falls_back_to_string() {
        let d = Decimal::from_str("-99999999999999999999").unwrap();
        let v = decimal_to_json(d);
        assert_eq!(v, Value::String("-99999999999999999999".to_string()));
    }

    #[test]
    fn decimal_to_json_normal_precision_fraction_uses_number() {
        let d = Decimal::from_str("1.12345678901234").unwrap();
        let v = decimal_to_json(d);
        assert!(matches!(v, Value::Number(_)), "got {v:?}");
    }

    #[test]
    fn decimal_to_json_high_precision_fraction_falls_back_to_string() {
        let d = Decimal::from_str("1.123456789012345").unwrap();
        let v = decimal_to_json(d);
        match v {
            Value::String(s) => assert_eq!(s, "1.123456789012345"),
            other => panic!("expected String fallback, got {other:?}"),
        }
    }

    // Regression guard: integer part with trailing zeros must NOT be
    // under-counted. Previously integer_digit_count stripped trailing
    // zeros from the integer part, so 10000000000000000.1 was reported
    // as 2 significant digits (instead of 18) and silently lost precision
    // via f64 round-trip.
    #[test]
    fn decimal_to_json_integer_with_trailing_zeros_preserves_precision() {
        let d = Decimal::from_str("10000000000000000.1").unwrap();
        let v = decimal_to_json(d);
        match v {
            Value::String(s) => assert_eq!(s, "10000000000000000.1"),
            other => panic!("expected String fallback, got {other:?}"),
        }
    }

    #[test]
    fn integer_digit_count_no_trailing_zero_stripping() {
        assert_eq!(integer_digit_count(&Decimal::from_str("1000").unwrap()), 4);
        assert_eq!(
            integer_digit_count(&Decimal::from_str("1000000.5").unwrap()),
            7
        );
        assert_eq!(integer_digit_count(&Decimal::from_str("1.5").unwrap()), 1);
        assert_eq!(integer_digit_count(&Decimal::from_str("0.5").unwrap()), 1);
    }

    #[test]
    fn format_unsupported_type_is_visible() {
        let s = format_unsupported_type("hstore", &[0x01, 0x02, 0xff]);
        assert!(s.contains("hstore"));
        assert!(s.contains("0102ff"));
    }

    #[test]
    fn format_interval_zero() {
        let bytes = [0u8; 16];
        assert_eq!(format_interval(&bytes), "00:00:00");
    }

    #[test]
    fn format_interval_one_day() {
        let mut bytes = [0u8; 16];
        bytes[8..12].copy_from_slice(&1i32.to_be_bytes());
        assert_eq!(format_interval(&bytes), "1 days");
    }

    #[test]
    fn format_interval_hours_minutes_seconds() {
        let micros: i64 = (2 * 3600 + 3 * 60 + 4) * 1_000_000;
        let mut bytes = [0u8; 16];
        bytes[0..8].copy_from_slice(&micros.to_be_bytes());
        assert_eq!(format_interval(&bytes), "02:03:04");
    }

    #[test]
    fn format_interval_malformed() {
        let bytes = [0u8; 8];
        let s = format_interval(&bytes);
        assert!(s.contains("malformed"));
    }

    #[test]
    fn boxed_empty_columns_returns_empty() {
        let result = format_table_boxed(&[], &[]);
        assert_eq!(result, "");
    }

    #[test]
    fn boxed_single_cell() {
        let columns = vec!["val".to_string()];
        let rows = vec![vec![Value::String("x".to_string())]];
        let result = format_table_boxed(&columns, &rows);
        let expected = "\
┌─────┐\n\
│ val │\n\
├─────┤\n\
│ x   │\n\
└─────┘\n";
        assert_eq!(result, expected);
    }

    #[test]
    fn boxed_multiple_columns_rows() {
        let columns = vec!["a".to_string(), "b".to_string()];
        let rows = vec![
            vec![
                Value::String("1".to_string()),
                Value::String("2".to_string()),
            ],
            vec![
                Value::String("3".to_string()),
                Value::String("4".to_string()),
            ],
        ];
        let result = format_table_boxed(&columns, &rows);
        let expected = "\
┌───┬───┐\n\
│ a │ b │\n\
├───┼───┤\n\
│ 1 │ 2 │\n\
│ 3 │ 4 │\n\
└───┴───┘\n";
        assert_eq!(result, expected);
    }

    #[test]
    fn boxed_null_value_renders_as_null() {
        let columns = vec!["name".to_string(), "status".to_string()];
        let rows = vec![vec![Value::String("alice".to_string()), Value::Null]];
        let result = format_table_boxed(&columns, &rows);
        // "name"(4) vs "alice"(5) → col_width[0]=5; "status"(6) vs "NULL"(4) → col_width[1]=6
        let expected = "\
┌───────┬────────┐\n\
│ name  │ status │\n\
├───────┼────────┤\n\
│ alice │ NULL   │\n\
└───────┴────────┘\n";
        assert_eq!(result, expected);
    }

    #[test]
    fn boxed_unicode_content() {
        let columns = vec!["姓名".to_string()];
        let rows = vec![vec![Value::String("张三".to_string())]];
        let result = format_table_boxed(&columns, &rows);
        // "姓名"/"张三" each → 2 chars → col_width = 2, dash run = 2+2 = 4
        let expected = "\
┌────┐\n\
│ 姓名 │\n\
├────┤\n\
│ 张三 │\n\
└────┘\n";
        assert_eq!(result, expected);
    }

    #[test]
    fn boxed_consistent_with_format_table_semantics() {
        let columns = vec!["col".to_string(), "other".to_string()];
        let rows = vec![vec![
            Value::String("hello".to_string()),
            Value::String("world".to_string()),
        ]];
        let ascii = format_table(&columns, &rows);
        let boxed = format_table_boxed(&columns, &rows);

        // Both should use the same column widths: "col"(3) vs "hello"(5) → 5, "other"(5) vs "world"(5) → 5
        // ASCII separator: "-----" + "-+-" + "-----"
        let ascii_sep: Vec<&str> = ascii.lines().nth(1).unwrap().split("-+-").collect();
        let ascii_widths: Vec<usize> = ascii_sep.iter().map(|s| s.chars().count()).collect();

        // Boxed header separator: "├─────┼─────┤"
        let boxed_sep = boxed.lines().nth(2).unwrap();
        // Strip ├ ┤ ┼ to get dash runs (use chars() for safe UTF-8 indexing)
        let boxed_inner: String = boxed_sep
            .chars()
            .skip(1)
            .take(boxed_sep.chars().count() - 2)
            .collect();
        let boxed_dashes: Vec<&str> = boxed_inner.split("┼").collect();
        let boxed_widths: Vec<usize> = boxed_dashes.iter().map(|s| s.chars().count()).collect();

        // Boxed dash runs should be 2 longer than ASCII dash runs
        assert_eq!(ascii_widths.len(), boxed_widths.len());
        for (aw, bw) in ascii_widths.iter().zip(boxed_widths.iter()) {
            assert_eq!(*bw, *aw + 2, "boxed dash run should be ASCII + 2");
        }
    }
}

#[cfg(all(test, feature = "integration"))]
mod integration_tests {
    use super::*;
    use tokio_opengauss::{Client, Row, tls::NoTls};

    async fn connect() -> Client {
        let url = std::env::var("GAUSSDB_TEST_URL").unwrap_or_else(|_| {
            "host=127.0.0.1 port=5432 user=gaussdb password=Gaussdb@123 dbname=postgres".to_string()
        });
        let (client, connection) = tokio_opengauss::connect(&url, NoTls)
            .await
            .expect("DB connect failed; set GAUSSDB_TEST_URL or run docker");
        tokio::spawn(async move {
            let _ = connection.await;
        });
        client
    }

    async fn first_value(client: &Client, sql: &str) -> Value {
        let rows = client.query(sql, &[]).await.expect("query failed");
        format_row_value(&rows[0], 0)
    }

    // Original bug regression guard: NUMERIC columns must NOT be silently
    // dropped to JSON null. This is the user-visible bug that motivated the
    // entire fix.
    #[tokio::test]
    async fn numeric_value_is_not_null_regression() {
        let client = connect().await;
        let v = first_value(&client, "SELECT 123.456::numeric").await;
        assert_ne!(
            v,
            Value::Null,
            "NUMERIC silently dropped to NULL — regression"
        );
    }

    #[tokio::test]
    async fn numeric_positive_value_preserved() {
        let client = connect().await;
        let v = first_value(&client, "SELECT 123.456::numeric").await;
        let expected = serde_json::Number::from_f64(123.456).unwrap();
        assert_eq!(v, Value::Number(expected));
    }

    #[tokio::test]
    async fn numeric_negative_value_preserved() {
        let client = connect().await;
        let v = first_value(&client, "SELECT -99.5::numeric").await;
        let expected = serde_json::Number::from_f64(-99.5).unwrap();
        assert_eq!(v, Value::Number(expected));
    }

    #[tokio::test]
    async fn numeric_null_stays_json_null() {
        let client = connect().await;
        let v = first_value(&client, "SELECT NULL::numeric").await;
        assert_eq!(v, Value::Null);
    }

    #[tokio::test]
    async fn numeric_huge_integer_falls_back_to_string() {
        let client = connect().await;
        let v = first_value(&client, "SELECT 18446744073709551616::numeric").await;
        match v {
            Value::String(s) => assert!(s.contains("18446744073709551616")),
            other => panic!("expected String for huge NUMERIC, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn uuid_column_preserved() {
        let client = connect().await;
        let v = first_value(
            &client,
            "SELECT 'a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11'::uuid",
        )
        .await;
        match v {
            Value::String(s) => assert_eq!(s, "a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11"),
            other => panic!("expected UUID String, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn json_column_preserved() {
        let client = connect().await;
        let v = first_value(&client, "SELECT '{\"k\": 1}'::jsonb").await;
        assert_ne!(v, Value::Null, "JSON/JSONB silently dropped to NULL");
        match v {
            Value::Object(_) | Value::Number(_) | Value::String(_) => {}
            other => panic!("expected JSON value, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn timestamp_column_preserved() {
        let client = connect().await;
        let v = first_value(&client, "SELECT '2026-01-01 12:00:00'::timestamp").await;
        match v {
            Value::String(s) => assert!(s.starts_with("2026-01-01")),
            other => panic!("expected TIMESTAMP String, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn interval_column_preserved() {
        let client = connect().await;
        let v = first_value(&client, "SELECT '1 day 02:03:04'::interval").await;
        match v {
            Value::String(s) => {
                assert!(s.contains("1 day") || s.contains("days"), "got: {s}");
                assert!(s.contains("02:03:04"), "got: {s}");
            }
            other => panic!("expected INTERVAL String, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn inet_column_preserved() {
        let client = connect().await;
        let v = first_value(&client, "SELECT '192.168.1.1'::inet").await;
        assert_eq!(v, Value::String("192.168.1.1".to_string()));
    }

    #[tokio::test]
    async fn macaddr_column_preserved() {
        let client = connect().await;
        let v = first_value(&client, "SELECT '08:00:2b:01:02:03'::macaddr").await;
        match v {
            Value::String(s) => assert!(s.contains("08:00:2b:01:02:03"), "got: {s}"),
            other => panic!("expected MACADDR String, got {other:?}"),
        }
    }

    // Silent-NULL regression guard: types NOT in the dispatch table must
    // emit a visible placeholder, NOT Value::Null. Uses POINT (geometry,
    // no handler) as the unsupported-type probe. If a future change
    // re-introduces `_ => Value::Null`, this test fails loudly.
    #[tokio::test]
    async fn unsupported_type_emits_visible_placeholder_not_null() {
        let client = connect().await;
        let v = first_value(&client, "SELECT '(1,2)'::point").await;
        assert_ne!(
            v,
            Value::Null,
            "unsupported type silently dropped to NULL — regression of the silent-NULL bug"
        );
        match v {
            Value::String(s) => assert!(
                s.contains("<unsupported type") || s.contains("point"),
                "expected visible placeholder, got: {s}"
            ),
            other => panic!("expected String placeholder, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn mixed_row_all_columns_preserved() {
        let client = connect().await;
        let rows = client
            .query(
                "SELECT 1::int AS i, 2.5::numeric AS n, 'x'::text AS t, NULL::numeric AS nn",
                &[],
            )
            .await
            .unwrap();
        let row: &Row = &rows[0];
        let i = format_row_value(row, 0);
        let n = format_row_value(row, 1);
        let t = format_row_value(row, 2);
        let nn = format_row_value(row, 3);
        assert_eq!(i, json!(1));
        let expected_n = serde_json::Number::from_f64(2.5).unwrap();
        assert_eq!(n, Value::Number(expected_n));
        assert_eq!(t, Value::String("x".to_string()));
        assert_eq!(nn, Value::Null);
    }
}
