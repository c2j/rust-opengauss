use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use serde_json::{Value, json};
use tokio_opengauss::Row;
use tokio_opengauss::types::Type;

pub(crate) fn format_row_value(row: &Row, idx: usize) -> Value {
    let col_type = row.columns()[idx].type_();
    match *col_type {
        Type::VARCHAR | Type::TEXT | Type::BPCHAR | Type::NAME | Type::UNKNOWN => {
            match row.try_get::<_, Option<String>>(idx) {
                Ok(Some(s)) => Value::String(s),
                Ok(None) => Value::Null,
                Err(_) => Value::Null,
            }
        }
        Type::INT2 => match row.try_get::<_, Option<i16>>(idx) {
            Ok(Some(v)) => json!(v),
            _ => Value::Null,
        },
        Type::INT4 | Type::OID | Type::REGPROC | Type::REGTYPE => {
            match row.try_get::<_, Option<i32>>(idx) {
                Ok(Some(v)) => json!(v),
                _ => Value::Null,
            }
        }
        Type::INT8 | Type::REGCLASS => match row.try_get::<_, Option<i64>>(idx) {
            Ok(Some(v)) => json!(v),
            _ => Value::Null,
        },
        Type::FLOAT4 => match row.try_get::<_, Option<f32>>(idx) {
            Ok(Some(v)) => json!(v),
            _ => Value::Null,
        },
        Type::FLOAT8 => match row.try_get::<_, Option<f64>>(idx) {
            Ok(Some(v)) => json!(v),
            _ => Value::Null,
        },
        Type::NUMERIC => match row.try_get::<_, Option<Decimal>>(idx) {
            Ok(Some(d)) => decimal_to_json(d),
            Ok(None) => Value::Null,
            Err(_) => Value::Null,
        },
        Type::BOOL => match row.try_get::<_, Option<bool>>(idx) {
            Ok(Some(v)) => json!(v),
            _ => Value::Null,
        },
        Type::BYTEA => match row.try_get::<_, Option<&[u8]>>(idx) {
            Ok(Some(b)) => Value::String(format!("\\x{}", hex_bytes(b))),
            Ok(None) => Value::Null,
            Err(_) => Value::Null,
        },
        Type::UUID => match row.try_get::<_, Option<uuid::Uuid>>(idx) {
            Ok(Some(u)) => Value::String(u.to_string()),
            Ok(None) => Value::Null,
            Err(_) => Value::Null,
        },
        Type::JSON | Type::JSONB => match row.try_get::<_, Option<Value>>(idx) {
            Ok(Some(v)) => v,
            Ok(None) => Value::Null,
            Err(_) => Value::Null,
        },
        Type::TIMESTAMP | Type::TIMESTAMPTZ => {
            match row.try_get::<_, Option<chrono::NaiveDateTime>>(idx) {
                Ok(Some(v)) => Value::String(v.to_string()),
                Ok(None) => Value::Null,
                Err(_) => Value::Null,
            }
        }
        Type::DATE => match row.try_get::<_, Option<chrono::NaiveDate>>(idx) {
            Ok(Some(v)) => Value::String(v.to_string()),
            Ok(None) => Value::Null,
            Err(_) => Value::Null,
        },
        Type::TIME | Type::TIMETZ => match row.try_get::<_, Option<chrono::NaiveTime>>(idx) {
            Ok(Some(v)) => Value::String(v.to_string()),
            Ok(None) => Value::Null,
            Err(_) => Value::Null,
        },
        _ => match row.try_get::<_, Option<&[u8]>>(idx) {
            Ok(Some(b)) => Value::String(format_unsupported_type(col_type.name(), b)),
            Ok(None) => Value::Null,
            Err(_) => Value::Null,
        },
    }
}

/// Render a NUMERIC `Decimal` as a JSON value.
///
/// Prefers `Number` when the value fits in `i64`/`u64`/`f64` without loss;
/// falls back to `String` for high-precision values that JSON numbers
/// cannot represent exactly.
fn decimal_to_json(d: Decimal) -> Value {
    if d.is_integer() {
        if let Some(i) = d.to_i64() {
            return json!(i);
        }
        if let Some(u) = d.to_u64() {
            return json!(u);
        }
        return Value::String(d.to_string());
    }
    if let Some(f) = d.to_f64() {
        if let Some(n) = serde_json::Number::from_f64(f) {
            return Value::Number(n);
        }
    }
    Value::String(d.to_string())
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
    fn format_unsupported_type_is_visible() {
        let s = format_unsupported_type("hstore", &[0x01, 0x02, 0xff]);
        assert!(s.contains("hstore"));
        assert!(s.contains("0102ff"));
    }
}
