use gaussdb::{Client, Row, tls::NoTls};
use std::path::PathBuf;

const CONNECTION_NAME: &str = "ogagila";

fn gaussdb_config_path() -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    let path = PathBuf::from(home).join(".gaussdb.toml");
    path.exists().then_some(path)
}

async fn connect() -> Client {
    if let Ok(url) = std::env::var("GAUSSDB_TEST_URL") {
        let mut config: gaussdb::Config = url.parse().expect("invalid GAUSSDB_TEST_URL");
        config.opengauss_compat(true);
        config.ssl_mode(gaussdb::driver::config::SslMode::Disable);
        let (client, connection) = config
            .connect(NoTls)
            .await
            .expect("DB connect failed via GAUSSDB_TEST_URL");
        tokio::spawn(async move {
            let _ = connection.await;
        });
        return client;
    }

    if let Some(config_path) = gaussdb_config_path() {
        match gaussdb::config::connect_async(None, Some(&config_path), Some(CONNECTION_NAME)).await
        {
            Ok(client) => return client,
            Err(e) => eprintln!("Config connect failed ({e}), falling back to defaults"),
        }
    }

    let url = "host=127.0.0.1 port=5432 user=gaussdb password=Gaussdb@123 dbname=postgres";
    let (client, connection) = gaussdb::connect(url, NoTls)
        .await
        .expect("DB connect failed; set GAUSSDB_TEST_URL or run docker");
    tokio::spawn(async move {
        let _ = connection.await;
    });
    client
}

async fn first_row(client: &Client, sql: &str) -> Row {
    let rows = client.query(sql, &[]).await.expect("query failed");
    assert!(!rows.is_empty(), "query returned no rows: {sql}");
    rows.into_iter().next().unwrap()
}

// ============================================================================
// Regression guard: OID columns MUST be readable as u32.
// Bug #39: `output.rs` used `i32` for OID/REGPROC/REGTYPE columns, but
// `FromSql<i32>` only accepts `Type::INT4` — not `Type::OID`. This caused
// the typed extraction to fail and fall through to `<unsupported type oid>`.
// ============================================================================

#[tokio::test]
async fn oid_column_readable_as_u32() {
    let client = connect().await;
    let row = first_row(&client, "SELECT 42::oid AS oid_val").await;
    let v: u32 = row.try_get(0).expect("OID should be readable as u32");
    assert_eq!(v, 42);
}

#[tokio::test]
async fn oid_column_nullable_as_option_u32() {
    let client = connect().await;
    let row = first_row(&client, "SELECT NULL::oid AS oid_val").await;
    let v: Option<u32> = row
        .try_get(0)
        .expect("NULL OID should be readable as Option<u32>");
    assert_eq!(v, None);
}

// ============================================================================
// Bug reproduction: `i32` fails for OID columns because FromSql<i32> only
// accepts Type::INT4, not Type::OID.
// ============================================================================

#[tokio::test]
#[should_panic(expected = "WrongType")]
async fn oid_column_rejected_by_i32() {
    let client = connect().await;
    let row = first_row(&client, "SELECT 42::oid AS oid_val").await;
    let _: i32 = row
        .try_get(0)
        .expect("BUG: i32 should NOT accept OID — this is the issue");
}

// ============================================================================
// System catalog columns of OID type (the original repro from issue #39).
// ============================================================================

#[tokio::test]
async fn pg_class_oid_columns_readable_as_u32() {
    let client = connect().await;
    let row = first_row(
        &client,
        "SELECT oid, relnamespace, relowner, reltype FROM pg_class WHERE relname = 'pg_type'",
    )
    .await;

    let oid: u32 = row.try_get(0).expect("pg_class.oid should be u32");
    let relnamespace: u32 = row.try_get(1).expect("pg_class.relnamespace should be u32");
    let relowner: u32 = row.try_get(2).expect("pg_class.relowner should be u32");
    let reltype: u32 = row.try_get(3).expect("pg_class.reltype should be u32");

    assert!(oid > 0);
    assert_eq!(relnamespace, 11); // pg_catalog
    assert_eq!(relowner, 10); // bootstrap superuser
    assert!(reltype > 0);
}

#[tokio::test]
async fn pg_proc_oid_columns_readable_as_u32() {
    let client = connect().await;
    let row = first_row(
        &client,
        "SELECT oid, pronamespace, proowner FROM pg_proc WHERE proname = 'to_number' LIMIT 1",
    )
    .await;

    let oid: u32 = row.try_get(0).expect("pg_proc.oid should be u32");
    let pronamespace: u32 = row.try_get(1).expect("pg_proc.pronamespace should be u32");
    let proowner: u32 = row.try_get(2).expect("pg_proc.proowner should be u32");

    assert_eq!(pronamespace, 11); // pg_catalog
    assert_eq!(proowner, 10); // bootstrap superuser
    assert!(oid > 0);
}

#[tokio::test]
async fn pg_type_oid_columns_readable_as_u32() {
    let client = connect().await;
    let row = first_row(
        &client,
        "SELECT oid, typowner, typnamespace, typrelid FROM pg_type WHERE typname = 'oid'",
    )
    .await;

    let oid: u32 = row.try_get(0).expect("pg_type.oid should be u32");
    let typowner: u32 = row.try_get(1).expect("pg_type.typowner should be u32");
    let typnamespace: u32 = row.try_get(2).expect("pg_type.typnamespace should be u32");
    let _typrelid: u32 = row.try_get(3).expect("pg_type.typrelid should be u32");

    assert_eq!(oid, 26);
    assert_eq!(typnamespace, 11); // pg_catalog
    assert!(typowner > 0);
}

// ============================================================================
// Non-regression: INT4, INT8, TEXT still work correctly.
// ============================================================================

#[tokio::test]
async fn int4_still_works() {
    let client = connect().await;
    let row = first_row(&client, "SELECT 42::int4 AS int4_col").await;
    let v: i32 = row.try_get(0).expect("INT4 should be readable as i32");
    assert_eq!(v, 42);
}

#[tokio::test]
async fn int8_still_works() {
    let client = connect().await;
    let row = first_row(&client, "SELECT 42::int8 AS int8_col").await;
    let v: i64 = row.try_get(0).expect("INT8 should be readable as i64");
    assert_eq!(v, 42);
}

#[tokio::test]
async fn text_still_works() {
    let client = connect().await;
    let row = first_row(&client, "SELECT 'hello'::text AS text_col").await;
    let v: &str = row.try_get(0).expect("TEXT should be readable as &str");
    assert_eq!(v, "hello");
}

// ============================================================================
// All OID-using system columns across major catalogs.
// ============================================================================

#[tokio::test]
async fn all_oid_columns_in_pg_class() {
    let client = connect().await;
    let row = first_row(
        &client,
        "SELECT oid, relnamespace, relowner, reltype, relam, reltablespace,
                reltoastrelid, reltoastidxid
         FROM pg_class WHERE relname = 'pg_type'",
    )
    .await;

    for col_idx in 0..8 {
        let v: u32 = row
            .try_get(col_idx)
            .unwrap_or_else(|_| panic!("pg_class column {col_idx} should be readable as u32"));
        // relam and reltablespace may be 0 (invalid OID)
        let _ = v;
    }
}

#[tokio::test]
async fn all_oid_columns_in_pg_type() {
    let client = connect().await;
    let row = first_row(
        &client,
        "SELECT oid, typowner, typnamespace, typrelid, typelem, typarray
         FROM pg_type WHERE typname = 'int4'",
    )
    .await;

    for col_idx in 0..6 {
        let v: u32 = row
            .try_get(col_idx)
            .unwrap_or_else(|_| panic!("pg_type OID column {col_idx} should be readable as u32"));
        let _ = v;
    }
}

#[tokio::test]
async fn regproc_columns_readable_as_u32() {
    let client = connect().await;
    let row = first_row(
        &client,
        "SELECT typinput, typoutput, typreceive, typsend
         FROM pg_type WHERE typname = 'int4'",
    )
    .await;

    for col_idx in 0..4 {
        let v: u32 = row.try_get(col_idx).unwrap_or_else(|_| {
            panic!("REGPROC column {col_idx} should now be readable as u32 after fix #39")
        });
        assert!(
            v > 0,
            "REGPROC column {col_idx} returned 0 — unexpected for int4 I/O functions"
        );
    }
}

#[tokio::test]
async fn all_oid_columns_in_pg_proc() {
    let client = connect().await;
    let row = first_row(
        &client,
        "SELECT oid, pronamespace, proowner, prolang, prorettype,
                proallargtypes[1] AS first_arg_type
         FROM pg_proc WHERE proname = 'to_number' LIMIT 1",
    )
    .await;

    let oid: u32 = row.try_get(0).expect("pg_proc.oid");
    let pronamespace: u32 = row.try_get(1).expect("pg_proc.pronamespace");
    let proowner: u32 = row.try_get(2).expect("pg_proc.proowner");
    let prolang: u32 = row.try_get(3).expect("pg_proc.prolang");
    let prorettype: u32 = row.try_get(4).expect("pg_proc.prorettype");
    let first_arg_type: Option<u32> = row.try_get(5).expect("pg_proc.proallargtypes[1]");

    assert_eq!(pronamespace, 11);
    assert_eq!(proowner, 10);
    assert!(oid > 0);
    assert!(prolang > 0);
    assert!(prorettype > 0);
    // proallargtypes[1] may be NULL if function has no arg types
    if let Some(t) = first_arg_type {
        assert!(t > 0);
    }
}

// ============================================================================
// Edge case: OID arrays (oidvector / oid[]) should also be accessible.
// ============================================================================

#[tokio::test]
async fn oid_array_column() {
    let client = connect().await;
    let rows = client
        .query(
            "SELECT proargtypes FROM pg_proc WHERE proargtypes IS NOT NULL AND proname = 'substr' LIMIT 1",
            &[],
        )
        .await
        .expect("query failed");
    if rows.is_empty() {
        return;
    }
    let row = &rows[0];
    let result: Result<Vec<u32>, _> = row.try_get(0);
    assert!(result.is_ok(), "OID array should be readable as Vec<u32>");
}
