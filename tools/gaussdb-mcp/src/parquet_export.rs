//! Parquet export pipeline.
//!
//! Streams rows from a `query_raw` cursor into columnar Arrow `RecordBatch`es
//! of configurable size, then writes them through `ArrowWriter` with the
//! chosen compression. NUMERIC columns are sampled against the first batch
//! to infer Decimal128 (precision, scale); values whose inferred precision
//! exceeds the Decimal128 limit (38 digits) fall back to Utf8.

use std::io::Write;
use std::sync::Arc;

use arrow::array::{
    ArrayRef, BinaryBuilder, BooleanBuilder, Date32Builder, Decimal128Builder, Float32Builder,
    Float64Builder, Int16Builder, Int32Builder, Int64Builder, RecordBatch, StringBuilder,
    Time64NanosecondBuilder, TimestampNanosecondBuilder,
};
use arrow::datatypes::{DataType, Field, Schema, TimeUnit};
use chrono::Timelike;
use futures_util::StreamExt;
use gaussdb::Row;
use gaussdb::types::{ToSql, Type};
use parquet::arrow::ArrowWriter;
use parquet::basic::{Compression, ZstdLevel};
use parquet::file::properties::WriterProperties;
use rust_decimal::Decimal;

use crate::cli::format_sql_error;
use crate::output::format_field_string;

/// Wrapper around `Write` that counts total bytes flushed, so the parquet
/// exporter can report an accurate file size even when the underlying writer
/// is a pipe / stdout with no random access.
struct CountingWriter<W> {
    inner: W,
    bytes: usize,
}

impl<W: Write> Write for CountingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.bytes += n;
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

pub const DEFAULT_BATCH_SIZE: usize = 65_536;

#[derive(Debug, Clone)]
pub struct ParquetOpts {
    pub batch_size: usize,
    pub compression: Compression,
}

impl Default for ParquetOpts {
    fn default() -> Self {
        Self {
            batch_size: DEFAULT_BATCH_SIZE,
            compression: Compression::SNAPPY,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParquetCompression {
    None,
    Snappy,
    Zstd,
}

impl ParquetCompression {
    pub fn parse(s: &str) -> Result<Self, String> {
        match s.to_lowercase().as_str() {
            "none" | "uncompressed" => Ok(Self::None),
            "snappy" => Ok(Self::Snappy),
            "zstd" => Ok(Self::Zstd),
            other => Err(format!(
                "unknown parquet compression '{other}'. Use none|snappy|zstd."
            )),
        }
    }

    pub fn to_parquet(self) -> Compression {
        match self {
            Self::None => Compression::UNCOMPRESSED,
            Self::Snappy => Compression::SNAPPY,
            Self::Zstd => Compression::ZSTD(ZstdLevel::default()),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ExportStats {
    pub rows: usize,
    pub bytes: usize,
}

/// Map a PG type to the Arrow column type used for parquet encoding.
///
/// NUMERIC returns Utf8 here as a placeholder; the actual NUMERIC encoding
/// (Decimal128 vs Utf8) is decided later by sampling the first batch.
fn pg_type_to_arrow(ty: &Type) -> DataType {
    match *ty {
        Type::INT2 => DataType::Int16,
        Type::INT4 => DataType::Int32,
        Type::INT8 => DataType::Int64,
        Type::FLOAT4 => DataType::Float32,
        Type::FLOAT8 => DataType::Float64,
        Type::BOOL => DataType::Boolean,
        Type::BYTEA => DataType::Binary,
        Type::DATE => DataType::Date32,
        Type::TIMESTAMP => DataType::Timestamp(TimeUnit::Nanosecond, None),
        // TIMESTAMPTZ falls through to Utf8: gaussdb's FromSql for
        // chrono::NaiveDateTime only handles TIMESTAMP, so a binary
        // timestamptz value can't be decoded client-side. Mirrors the
        // CLI text-rendering fallback in `output.rs`.
        Type::TIMESTAMPTZ => DataType::Utf8,
        Type::TIME | Type::TIMETZ => DataType::Time64(TimeUnit::Nanosecond),
        Type::NUMERIC => DataType::Utf8,
        // OID family: PG stores these as u32; widen to Int64 to avoid u32 wrap.
        Type::OID
        | Type::REGPROC
        | Type::REGPROCEDURE
        | Type::REGOPER
        | Type::REGOPERATOR
        | Type::REGCLASS
        | Type::REGTYPE
        | Type::REGNAMESPACE
        | Type::REGCOLLATION
        | Type::XID
        | Type::CID => DataType::Int64,
        Type::VARCHAR
        | Type::TEXT
        | Type::BPCHAR
        | Type::NAME
        | Type::UNKNOWN
        | Type::UUID
        | Type::JSON
        | Type::JSONB
        | Type::INET
        | Type::CIDR
        | Type::MACADDR
        | Type::MACADDR8
        | Type::INTERVAL => DataType::Utf8,
        _ => DataType::Utf8,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NumericStrategy {
    Decimal128 { precision: u8, scale: i8 },
    Utf8,
}

fn integer_digits(d: &Decimal) -> u32 {
    if d.is_zero() {
        return 1;
    }
    let s = d.abs().to_string();
    let int_part = s.split('.').next().unwrap_or("");
    int_part.len() as u32
}

/// Pure sampling core: given the non-null decimals observed in the first
/// batch, pick Decimal128(precision, scale) or Utf8 fallback. Extracted from
/// `sample_numeric` so it can be unit-tested without a live `Row`.
fn sample_numeric_from_decimals(values: &[Option<Decimal>]) -> NumericStrategy {
    let mut max_int_digits: u32 = 0;
    let mut max_scale: u32 = 0;
    let mut non_null = false;
    for d in values.iter().flatten() {
        non_null = true;
        max_int_digits = max_int_digits.max(integer_digits(d));
        max_scale = max_scale.max(d.scale());
    }
    if !non_null {
        return NumericStrategy::Decimal128 {
            precision: 1,
            scale: 0,
        };
    }
    let precision = max_int_digits.saturating_add(max_scale);
    if precision == 0 || precision > 38 {
        return NumericStrategy::Utf8;
    }
    NumericStrategy::Decimal128 {
        precision: precision as u8,
        scale: max_scale as i8,
    }
}

fn sample_numeric(rows: &[Row], idx: usize) -> NumericStrategy {
    let values: Vec<Option<Decimal>> = rows
        .iter()
        .map(|r| r.try_get::<_, Option<Decimal>>(idx).ok().flatten())
        .collect();
    sample_numeric_from_decimals(&values)
}

fn build_schema(col_names: &[String], col_types: &[Type], sample_rows: &[Row]) -> Schema {
    let fields: Vec<Field> = col_names
        .iter()
        .enumerate()
        .map(|(i, name)| {
            let arrow_ty = if matches!(col_types[i], Type::NUMERIC) {
                match sample_numeric(sample_rows, i) {
                    NumericStrategy::Decimal128 { precision, scale } => {
                        DataType::Decimal128(precision, scale)
                    }
                    NumericStrategy::Utf8 => DataType::Utf8,
                }
            } else {
                pg_type_to_arrow(&col_types[i])
            };
            Field::new(name, arrow_ty, true)
        })
        .collect();
    Schema::new(fields)
}

fn build_record_batch(
    schema: &Schema,
    col_types: &[Type],
    rows: &[Row],
) -> Result<RecordBatch, String> {
    let arrays: Vec<ArrayRef> = schema
        .fields()
        .iter()
        .enumerate()
        .map(|(i, field)| build_array(field.data_type(), &col_types[i], i, rows))
        .collect::<Result<_, _>>()?;
    RecordBatch::try_new(Arc::new(schema.clone()), arrays)
        .map_err(|e| format!("record batch build: {e}"))
}

fn build_array(dt: &DataType, pg_ty: &Type, idx: usize, rows: &[Row]) -> Result<ArrayRef, String> {
    let arr: ArrayRef = match dt {
        DataType::Int16 => Arc::new(build_int16(rows, idx)),
        DataType::Int32 => Arc::new(build_int32(rows, idx)),
        DataType::Int64 => Arc::new(build_int64(rows, idx, pg_ty)),
        DataType::Float32 => Arc::new(build_float32(rows, idx)),
        DataType::Float64 => Arc::new(build_float64(rows, idx)),
        DataType::Boolean => Arc::new(build_boolean(rows, idx)),
        DataType::Binary => Arc::new(build_binary(rows, idx)),
        DataType::Utf8 => Arc::new(build_utf8(rows, idx, pg_ty)),
        DataType::Date32 => Arc::new(build_date32(rows, idx)),
        DataType::Timestamp(TimeUnit::Nanosecond, _) => {
            Arc::new(build_timestamp_nanos(rows, idx, dt.clone())?)
        }
        DataType::Time64(TimeUnit::Nanosecond) => Arc::new(build_time64_nanos(rows, idx)),
        DataType::Decimal128(p, s) => Arc::new(build_decimal128(rows, idx, *p, *s)?),
        other => {
            return Err(format!(
                "unsupported arrow data type in parquet export: {other:?}"
            ));
        }
    };
    Ok(arr)
}

fn build_int16(rows: &[Row], idx: usize) -> arrow::array::Int16Array {
    let mut b = Int16Builder::with_capacity(rows.len());
    for r in rows {
        match r.try_get::<_, Option<i16>>(idx) {
            Ok(Some(v)) => b.append_value(v),
            _ => b.append_null(),
        }
    }
    b.finish()
}

fn build_int32(rows: &[Row], idx: usize) -> arrow::array::Int32Array {
    let mut b = Int32Builder::with_capacity(rows.len());
    for r in rows {
        match r.try_get::<_, Option<i32>>(idx) {
            Ok(Some(v)) => b.append_value(v),
            _ => b.append_null(),
        }
    }
    b.finish()
}

fn build_int64(rows: &[Row], idx: usize, pg_ty: &Type) -> arrow::array::Int64Array {
    let mut b = Int64Builder::with_capacity(rows.len());
    for r in rows {
        match *pg_ty {
            Type::INT8 => match r.try_get::<_, Option<i64>>(idx) {
                Ok(Some(v)) => b.append_value(v),
                _ => b.append_null(),
            },
            // OID family: u32 → i64
            _ => match r.try_get::<_, Option<u32>>(idx) {
                Ok(Some(v)) => b.append_value(v as i64),
                _ => b.append_null(),
            },
        }
    }
    b.finish()
}

fn build_float32(rows: &[Row], idx: usize) -> arrow::array::Float32Array {
    let mut b = Float32Builder::with_capacity(rows.len());
    for r in rows {
        match r.try_get::<_, Option<f32>>(idx) {
            Ok(Some(v)) => b.append_value(v),
            _ => b.append_null(),
        }
    }
    b.finish()
}

fn build_float64(rows: &[Row], idx: usize) -> arrow::array::Float64Array {
    let mut b = Float64Builder::with_capacity(rows.len());
    for r in rows {
        match r.try_get::<_, Option<f64>>(idx) {
            Ok(Some(v)) => b.append_value(v),
            _ => b.append_null(),
        }
    }
    b.finish()
}

fn build_boolean(rows: &[Row], idx: usize) -> arrow::array::BooleanArray {
    let mut b = BooleanBuilder::with_capacity(rows.len());
    for r in rows {
        match r.try_get::<_, Option<bool>>(idx) {
            Ok(Some(v)) => b.append_value(v),
            _ => b.append_null(),
        }
    }
    b.finish()
}

fn build_binary(rows: &[Row], idx: usize) -> arrow::array::BinaryArray {
    let mut b = BinaryBuilder::with_capacity(rows.len(), rows.len() * 8);
    for r in rows {
        match r.try_get::<_, Option<&[u8]>>(idx) {
            Ok(Some(v)) => b.append_value(v),
            _ => b.append_null(),
        }
    }
    b.finish()
}

fn build_utf8(rows: &[Row], idx: usize, pg_ty: &Type) -> arrow::array::StringArray {
    let mut b = StringBuilder::new();
    for r in rows {
        match format_field_string(r, idx, pg_ty) {
            Some(s) => b.append_value(s),
            None => b.append_null(),
        }
    }
    b.finish()
}

fn build_date32(rows: &[Row], idx: usize) -> arrow::array::Date32Array {
    let epoch = chrono::NaiveDate::from_ymd_opt(1970, 1, 1).unwrap_or_else(|| {
        // SAFETY: 1970-01-01 is always representable.
        unreachable!("epoch is in range")
    });
    let mut b = Date32Builder::with_capacity(rows.len());
    for r in rows {
        match r.try_get::<_, Option<chrono::NaiveDate>>(idx) {
            Ok(Some(d)) => {
                let days = (d - epoch).num_days() as i32;
                b.append_value(days);
            }
            _ => b.append_null(),
        }
    }
    b.finish()
}

fn build_timestamp_nanos(
    rows: &[Row],
    idx: usize,
    data_type: DataType,
) -> Result<arrow::array::TimestampNanosecondArray, String> {
    let mut b = TimestampNanosecondBuilder::with_capacity(rows.len()).with_data_type(data_type);
    for r in rows {
        match r.try_get::<_, Option<chrono::NaiveDateTime>>(idx) {
            Ok(Some(ndt)) => match ndt.and_utc().timestamp_nanos_opt() {
                Some(nanos) => b.append_value(nanos),
                None => b.append_null(),
            },
            _ => b.append_null(),
        }
    }
    Ok(b.finish())
}

fn build_time64_nanos(rows: &[Row], idx: usize) -> arrow::array::Time64NanosecondArray {
    let mut b = Time64NanosecondBuilder::with_capacity(rows.len());
    for r in rows {
        match r.try_get::<_, Option<chrono::NaiveTime>>(idx) {
            Ok(Some(t)) => {
                let secs =
                    (t.hour() as i64 * 3600) + (t.minute() as i64 * 60) + (t.second() as i64);
                let nanos = secs * 1_000_000_000 + (t.nanosecond() as i64);
                b.append_value(nanos);
            }
            _ => b.append_null(),
        }
    }
    b.finish()
}

fn build_decimal128(
    rows: &[Row],
    idx: usize,
    precision: u8,
    scale: i8,
) -> Result<arrow::array::Decimal128Array, String> {
    let mut b = Decimal128Builder::with_capacity(rows.len())
        .with_precision_and_scale(precision, scale)
        .map_err(|e| format!("decimal schema (p={precision}, s={scale}): {e}"))?;
    let target_scale = scale.max(0) as u32;
    for r in rows {
        match r.try_get::<_, Option<Decimal>>(idx) {
            Ok(Some(mut d)) => {
                // Normalise to schema scale: pad with zeros when value scale
                // is smaller; truncate (lossy) when larger. Without this,
                // a value whose scale differs from the schema's would be
                // decoded at the wrong magnitude.
                if d.scale() != target_scale {
                    d.rescale(target_scale);
                }
                b.append_value(d.mantissa());
            }
            _ => b.append_null(),
        }
    }
    Ok(b.finish())
}

/// Stream `sql` through `query_raw`, accumulate rows into columnar
/// `RecordBatch`es of `opts.batch_size`, and write them to `writer` as
/// Parquet using `opts.compression`.
///
/// Memory is bounded by `opts.batch_size` rows (one `RecordBatch` in
/// flight at a time). A zero-row result writes nothing.
pub async fn export_parquet<W>(
    client: &gaussdb::Client,
    sql: &str,
    writer: W,
    opts: &ParquetOpts,
) -> Result<ExportStats, String>
where
    W: Write + Send + Sync,
{
    let no_params: [&(dyn ToSql + Sync); 0] = [];
    let mut stream = std::pin::pin!(
        client
            .query_raw(sql, no_params)
            .await
            .map_err(|e| format!("query failed: {}", format_sql_error(&e)))?
    );

    // First batch doubles as the NUMERIC sampling population. Track EOF so
    // we don't re-poll the stream after ReadyForQuery — tokio-opengauss's
    // RowStream returns Err(closed) on poll after EOF rather than None.
    let mut first_batch: Vec<Row> = Vec::with_capacity(opts.batch_size);
    let mut stream_exhausted = false;
    while first_batch.len() < opts.batch_size {
        match stream.next().await {
            None => {
                stream_exhausted = true;
                break;
            }
            Some(Ok(r)) => first_batch.push(r),
            Some(Err(e)) => {
                return Err(format!("query failed: {}", format_sql_error(&e)));
            }
        }
    }

    if first_batch.is_empty() {
        return Ok(ExportStats { rows: 0, bytes: 0 });
    }

    let columns = first_batch[0].columns();
    let col_types: Vec<Type> = columns.iter().map(|c| c.type_().clone()).collect();
    let col_names: Vec<String> = columns.iter().map(|c| c.name().to_string()).collect();
    let schema = build_schema(&col_names, &col_types, &first_batch);

    let props = WriterProperties::builder()
        .set_compression(opts.compression)
        .build();
    let mut counting = CountingWriter {
        inner: writer,
        bytes: 0,
    };
    let mut arrow_writer =
        ArrowWriter::try_new(&mut counting, Arc::new(schema.clone()), Some(props))
            .map_err(|e| format!("parquet writer init: {e}"))?;

    let first = build_record_batch(&schema, &col_types, &first_batch)?;
    arrow_writer
        .write(&first)
        .map_err(|e| format!("parquet write: {e}"))?;
    let mut total_rows = first_batch.len();

    // Only continue draining if the first batch hit batch_size (didn't EOF).
    if !stream_exhausted {
        let mut buf: Vec<Row> = Vec::with_capacity(opts.batch_size);
        while let Some(next) = stream.next().await {
            let row = next.map_err(|e| format!("query failed: {}", format_sql_error(&e)))?;
            buf.push(row);
            if buf.len() >= opts.batch_size {
                let batch = build_record_batch(&schema, &col_types, &buf)?;
                arrow_writer
                    .write(&batch)
                    .map_err(|e| format!("parquet write: {e}"))?;
                total_rows += buf.len();
                buf.clear();
            }
        }
        if !buf.is_empty() {
            let batch = build_record_batch(&schema, &col_types, &buf)?;
            arrow_writer
                .write(&batch)
                .map_err(|e| format!("parquet write: {e}"))?;
            total_rows += buf.len();
        }
    }

    arrow_writer
        .close()
        .map_err(|e| format!("parquet close: {e}"))?;
    Ok(ExportStats {
        rows: total_rows,
        bytes: counting.bytes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal::Decimal;
    use std::str::FromStr;

    #[test]
    fn pg_type_to_arrow_primitive_mapping() {
        assert_eq!(pg_type_to_arrow(&Type::INT2), DataType::Int16);
        assert_eq!(pg_type_to_arrow(&Type::INT4), DataType::Int32);
        assert_eq!(pg_type_to_arrow(&Type::INT8), DataType::Int64);
        assert_eq!(pg_type_to_arrow(&Type::FLOAT4), DataType::Float32);
        assert_eq!(pg_type_to_arrow(&Type::FLOAT8), DataType::Float64);
        assert_eq!(pg_type_to_arrow(&Type::BOOL), DataType::Boolean);
        assert_eq!(pg_type_to_arrow(&Type::BYTEA), DataType::Binary);
    }

    #[test]
    fn pg_type_to_arrow_temporal_mapping() {
        assert_eq!(pg_type_to_arrow(&Type::DATE), DataType::Date32);
        assert_eq!(
            pg_type_to_arrow(&Type::TIMESTAMP),
            DataType::Timestamp(TimeUnit::Nanosecond, None)
        );
        // TIMESTAMPTZ → Utf8 fallback (gaussdb FromSql can't decode it)
        assert_eq!(pg_type_to_arrow(&Type::TIMESTAMPTZ), DataType::Utf8);
        assert_eq!(
            pg_type_to_arrow(&Type::TIME),
            DataType::Time64(TimeUnit::Nanosecond)
        );
    }

    #[test]
    fn pg_type_to_arrow_oid_family_widens_to_int64() {
        // PG stores these as u32; widening to i64 avoids wrap on the high bit.
        for ty in [
            Type::OID,
            Type::REGPROC,
            Type::REGCLASS,
            Type::REGTYPE,
            Type::XID,
            Type::CID,
        ] {
            assert_eq!(pg_type_to_arrow(&ty), DataType::Int64, "OID-family {ty:?}");
        }
    }

    #[test]
    fn pg_type_to_arrow_text_family_is_utf8() {
        for ty in [
            Type::VARCHAR,
            Type::TEXT,
            Type::BPCHAR,
            Type::NAME,
            Type::UUID,
            Type::JSON,
            Type::JSONB,
            Type::INET,
            Type::CIDR,
            Type::MACADDR,
            Type::MACADDR8,
            Type::INTERVAL,
        ] {
            assert_eq!(pg_type_to_arrow(&ty), DataType::Utf8, "text-ish {ty:?}");
        }
    }

    #[test]
    fn pg_type_to_arrow_unknown_falls_back_to_utf8() {
        // Point / hstore / custom types all land on Utf8 (mirrors output.rs).
        assert_eq!(pg_type_to_arrow(&Type::POINT), DataType::Utf8);
    }

    #[test]
    fn pg_type_to_arrow_numeric_placeholder_is_utf8() {
        // Sampling decides between Decimal128 and Utf8; pre-sampling placeholder.
        assert_eq!(pg_type_to_arrow(&Type::NUMERIC), DataType::Utf8);
    }

    #[test]
    fn integer_digits_zero_and_small() {
        assert_eq!(integer_digits(&Decimal::from(0)), 1);
        assert_eq!(integer_digits(&Decimal::from(5)), 1);
        assert_eq!(integer_digits(&Decimal::from(42)), 2);
        assert_eq!(integer_digits(&Decimal::from(1000)), 4);
    }

    #[test]
    fn integer_digits_negative_and_fractional() {
        assert_eq!(integer_digits(&Decimal::from(-7)), 1);
        assert_eq!(integer_digits(&Decimal::from(-123)), 3);
        assert_eq!(integer_digits(&Decimal::from_str("100.5").unwrap()), 3);
    }

    #[test]
    fn sample_numeric_integer_picks_minimal_decimal128() {
        let v = vec![Some(Decimal::from(42)), Some(Decimal::from(100))];
        let s = sample_numeric_from_decimals(&v);
        assert_eq!(
            s,
            NumericStrategy::Decimal128 {
                precision: 3,
                scale: 0
            }
        );
    }

    #[test]
    fn sample_numeric_fractional_uses_max_scale() {
        let v = vec![
            Some(Decimal::from_str("1.50").unwrap()),
            Some(Decimal::from_str("99.999").unwrap()),
        ];
        let s = sample_numeric_from_decimals(&v);
        // max int_digits = 2, max scale = 3 → precision = 5
        assert_eq!(
            s,
            NumericStrategy::Decimal128 {
                precision: 5,
                scale: 3
            }
        );
    }

    #[test]
    fn sample_numeric_all_null_defaults_minimal_decimal128() {
        let v: Vec<Option<Decimal>> = vec![None, None];
        let s = sample_numeric_from_decimals(&v);
        assert_eq!(
            s,
            NumericStrategy::Decimal128 {
                precision: 1,
                scale: 0
            }
        );
    }

    #[test]
    fn sample_numeric_empty_slice_is_default_decimal128() {
        let v: Vec<Option<Decimal>> = vec![];
        let s = sample_numeric_from_decimals(&v);
        assert_eq!(
            s,
            NumericStrategy::Decimal128 {
                precision: 1,
                scale: 0
            }
        );
    }

    #[test]
    fn parquet_compression_parse_round_trip() {
        assert_eq!(
            ParquetCompression::parse("snappy").unwrap(),
            ParquetCompression::Snappy
        );
        assert_eq!(
            ParquetCompression::parse("SNAPPY").unwrap(),
            ParquetCompression::Snappy
        );
        assert_eq!(
            ParquetCompression::parse("zstd").unwrap(),
            ParquetCompression::Zstd
        );
        assert_eq!(
            ParquetCompression::parse("none").unwrap(),
            ParquetCompression::None
        );
        assert_eq!(
            ParquetCompression::parse("uncompressed").unwrap(),
            ParquetCompression::None
        );
        assert!(ParquetCompression::parse("lz4").is_err());
    }

    #[test]
    fn parquet_compression_to_parquet_codec() {
        assert_eq!(
            ParquetCompression::None.to_parquet(),
            Compression::UNCOMPRESSED
        );
        assert_eq!(ParquetCompression::Snappy.to_parquet(), Compression::SNAPPY);
        assert!(matches!(
            ParquetCompression::Zstd.to_parquet(),
            Compression::ZSTD(_)
        ));
    }

    #[test]
    fn parquet_opts_default_matches_constants() {
        let o = ParquetOpts::default();
        assert_eq!(o.batch_size, DEFAULT_BATCH_SIZE);
        assert_eq!(o.compression, Compression::SNAPPY);
    }
}
