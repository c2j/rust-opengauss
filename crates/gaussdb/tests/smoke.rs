// 编译验证 + Send/Sync 断言(始终运行)
#[allow(dead_code)]
fn _assert_send<T: Send>() {}
#[allow(dead_code)]
fn _assert_sync<T: Sync>() {}

#[test]
fn reexports_compile() {
    _assert_send::<gaussdb::Client>();
    _assert_sync::<gaussdb::Row>();
    // gaussdb::connect is generic (T: MakeTlsConnect<Socket>), so check
    // it accepts NoTls by constructing a future (unused, never polled).
    let _fut = gaussdb::connect("host=localhost", gaussdb::NoTls);
    drop(_fut);
}

#[cfg(feature = "integration")]
#[tokio::test]
async fn smoke_connect() {
    let url = std::env::var("GAUSSDB_TEST_URL")
        .unwrap_or("host=127.0.0.1 user=gaussdb dbname=postgres".into());
    let (client, conn) = gaussdb::connect(&url, gaussdb::NoTls).await.unwrap();
    tokio::spawn(async move {
        let _ = conn.await;
    });
    let row = client.query_one("SELECT 1", &[]).await.unwrap();
    let _: i32 = row.get(0);
}
