use std::sync::Arc;

pub(crate) fn needs_tls(url: &str) -> bool {
    url.split_whitespace().any(|part| {
        if let Some(val) = part.strip_prefix("sslmode=") {
            matches!(val, "require" | "verify-ca" | "verify-full")
        } else {
            false
        }
    })
}

pub(crate) async fn do_connect(
    url: &str,
) -> Result<
    (Arc<tokio_opengauss::Client>, tokio::task::JoinHandle<()>),
    Box<dyn std::error::Error + Send + Sync>,
> {
    if needs_tls(url) {
        let connector = native_tls::TlsConnector::builder()
            .danger_accept_invalid_certs(true)
            .danger_accept_invalid_hostnames(true)
            .build()?;
        let tls = opengauss_native_tls::MakeTlsConnector::new(connector);
        let (client, connection) = tokio_opengauss::connect(url, tls).await?;
        let handle = tokio::spawn(async move {
            if let Err(e) = connection.await {
                tracing::error!("database connection lost: {}", e);
            }
        });
        Ok((Arc::new(client), handle))
    } else {
        let (client, connection) = tokio_opengauss::connect(url, tokio_opengauss::NoTls).await?;
        let handle = tokio::spawn(async move {
            if let Err(e) = connection.await {
                tracing::error!("database connection lost: {}", e);
            }
        });
        Ok((Arc::new(client), handle))
    }
}
