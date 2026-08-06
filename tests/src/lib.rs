//! Shared harness for MicroCommandControl **integration** tests.
//!
//! Unit tests live next to production code (`crates/*/src/**`).
//! Integration tests live in this package’s `tests/` directory and use this
//! harness to exercise real SQLite + HTTP (and later gRPC/agent) boundaries.
//!
//! See [docs/guides/testing.md](../docs/guides/testing.md).

use anyhow::{Context, Result};
use mcc_server::{router, AppState, Bootstrap};
use mcc_store::{SqliteStore, Store};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;
use tokio::task::JoinHandle;

/// Ephemeral control plane: temp data dir, real SQLite, real axum listener.
pub struct TestCluster {
    /// Kept alive so the temp directory is not deleted while the server runs.
    _dir: TempDir,
    pub data_dir: PathBuf,
    pub base_url: String,
    pub addr: SocketAddr,
    pub api_token: String,
    pub join_token: String,
    pub store: Arc<SqliteStore>,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    join: Option<JoinHandle<Result<(), std::io::Error>>>,
}

impl TestCluster {
    /// Bootstrap a new cluster and serve REST on `127.0.0.1:0`.
    pub async fn start() -> Result<Self> {
        let dir = tempfile::tempdir().context("tempdir")?;
        let data_dir = dir.path().to_path_buf();
        let secrets_key_path = data_dir.join("secrets.key");

        let boot = Bootstrap {
            data_dir: data_dir.clone(),
            secrets_key_path,
        }
        .ensure()
        .await
        .context("bootstrap")?;

        let creds = boot
            .fresh_credentials
            .context("expected fresh credentials on first bootstrap")?;

        let store = SqliteStore::open(&boot.db_path)
            .await
            .context("open store")?;

        let state = AppState {
            store: store.clone() as Arc<dyn Store>,
            data_dir: data_dir.clone(),
            version: env!("CARGO_PKG_VERSION"),
        };

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .context("bind")?;
        let addr = listener.local_addr().context("local_addr")?;
        let base_url = format!("http://{addr}");

        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let app = router(state);
        let join = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = rx.await;
                })
                .await
        });

        let cluster = Self {
            _dir: dir,
            data_dir,
            base_url: base_url.clone(),
            addr,
            api_token: creds.api_token,
            join_token: creds.join_token,
            store,
            shutdown: Some(tx),
            join: Some(join),
        };

        cluster.wait_healthy().await?;
        Ok(cluster)
    }

    /// Poll `GET /health` until the server responds or timeout.
    pub async fn wait_healthy(&self) -> Result<()> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(2))
            .build()?;
        let url = format!("{}/health", self.base_url);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            if tokio::time::Instant::now() > deadline {
                anyhow::bail!("server did not become healthy at {url}");
            }
            match client.get(&url).send().await {
                Ok(res) if res.status().is_success() => return Ok(()),
                _ => tokio::time::sleep(Duration::from_millis(20)).await,
            }
        }
    }

    pub fn client(&self) -> reqwest::Client {
        reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .expect("reqwest client")
    }

    pub async fn get_json(
        &self,
        path: &str,
        bearer: Option<&str>,
    ) -> Result<(reqwest::StatusCode, serde_json::Value)> {
        let url = format!("{}{path}", self.base_url);
        let mut req = self.client().get(url);
        if let Some(token) = bearer {
            req = req.bearer_auth(token);
        }
        let res = req.send().await.context("request")?;
        let status = res.status();
        let body = res.text().await.unwrap_or_default();
        let json = if body.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::from_str(&body).unwrap_or(serde_json::Value::String(body))
        };
        Ok((status, json))
    }
}

impl Drop for TestCluster {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        if let Some(join) = self.join.take() {
            // Best-effort: do not block drop on async runtime shutdown in all contexts.
            join.abort();
        }
    }
}
