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
    timeout_config: Option<&crate::config::TimeoutConfig>,
) -> Result<
    (Arc<tokio_opengauss::Client>, tokio::task::JoinHandle<()>),
    Box<dyn std::error::Error + Send + Sync>,
> {
    let (client, handle) = if needs_tls(url) {
        let connector = native_tls::TlsConnector::builder()
            .danger_accept_invalid_certs(true)
            .danger_accept_invalid_hostnames(true)
            .build()?;
        let tls = opengauss_native_tls::MakeTlsConnector::new(connector);
        let (client, connection) = tokio_opengauss::connect(url, tls).await?;
        let client = Arc::new(client);

        let handle = tokio::spawn(async move {
            if let Err(e) = connection.await {
                tracing::error!("database connection lost: {}", e);
            }
        });

        if let Some(tc) = timeout_config {
            if let Some(st) = tc.statement_timeout {
                let ms = st.as_millis() as u64;
                let set_sql = format!("SET statement_timeout = {}", ms);
                if let Err(e) = client.batch_execute(&set_sql).await {
                    tracing::warn!(
                        "failed to apply statement_timeout={}ms for new connection: {}",
                        ms, e
                    );
                } else {
                    tracing::info!("applied statement_timeout={}ms to new connection", ms);
                }
            }
        }

        (client, handle)
    } else {
        let (client, connection) = tokio_opengauss::connect(url, tokio_opengauss::NoTls).await?;
        let client = Arc::new(client);

        let handle = tokio::spawn(async move {
            if let Err(e) = connection.await {
                tracing::error!("database connection lost: {}", e);
            }
        });

        if let Some(tc) = timeout_config {
            if let Some(st) = tc.statement_timeout {
                let ms = st.as_millis() as u64;
                let set_sql = format!("SET statement_timeout = {}", ms);
                if let Err(e) = client.batch_execute(&set_sql).await {
                    tracing::warn!(
                        "failed to apply statement_timeout={}ms for new connection: {}",
                        ms, e
                    );
                } else {
                    tracing::info!("applied statement_timeout={}ms to new connection", ms);
                }
            }
        }

        (client, handle)
    };

    Ok((client, handle))
}
