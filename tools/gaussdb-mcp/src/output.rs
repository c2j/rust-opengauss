use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use serde_json::{Value, json};
use std::error::Error;
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
        _ => match row.try_get::<_, RawBytes>(idx) {
            Ok(RawBytes(Some(b))) => Value::String(format_unsupported_type(col_type.name(), b)),
            Ok(RawBytes(None)) => Value::Null,
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
    fn decimal_to_json_negative_beyond_i64_falls_back_to_string() {
        let d = Decimal::from_str("-99999999999999999999").unwrap();
        let v = decimal_to_json(d);
        assert_eq!(v, Value::String("-99999999999999999999".to_string()));
    }

    #[test]
    fn decimal_to_json_high_precision_fraction_uses_f64() {
        let d = Decimal::from_str("1.123456789012345").unwrap();
        let v = decimal_to_json(d);
        match v {
            Value::Number(_) => {}
            other => panic!("expected Number, got {other:?}"),
        }
    }

    #[test]
    fn format_unsupported_type_is_visible() {
        let s = format_unsupported_type("hstore", &[0x01, 0x02, 0xff]);
        assert!(s.contains("hstore"));
        assert!(s.contains("0102ff"));
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

    // Silent-NULL regression guard: types NOT in the dispatch table must
    // emit a visible placeholder, NOT Value::Null. If a future change
    // re-introduces `_ => Value::Null`, this test fails loudly.
    #[tokio::test]
    async fn unsupported_type_emits_visible_placeholder_not_null() {
        let client = connect().await;
        let v = first_value(&client, "SELECT '08:00:2b:01:02:03'::macaddr").await;
        assert_ne!(
            v,
            Value::Null,
            "unsupported type silently dropped to NULL — regression of the silent-NULL bug"
        );
        match v {
            Value::String(s) => assert!(
                s.contains("<unsupported type") || s.contains("macaddr"),
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
