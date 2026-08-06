//! Shared harness for MicroCommandControl **integration** tests.
//!
//! Unit tests live next to production code (`crates/*/src/**`).
//! Integration tests live in this package’s `tests/` directory.
//!
//! See [docs/guides/testing.md](../docs/guides/testing.md).

use anyhow::{Context, Result};
use mcc_api::agent::agent_service_server::AgentServiceServer;
use mcc_server::{router, AgentSvc, AppState, Bootstrap};
use mcc_store::{SecretsKey, SqliteStore, Store};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;
use tokio::task::JoinHandle;
use tonic::transport::Server as GrpcServer;

/// Ephemeral control plane: temp data dir, real SQLite, REST + plain gRPC.
pub struct TestCluster {
    _dir: TempDir,
    pub data_dir: PathBuf,
    pub base_url: String,
    pub grpc_url: String,
    pub addr: SocketAddr,
    pub grpc_addr: SocketAddr,
    pub api_token: String,
    pub join_token: String,
    pub store: Arc<SqliteStore>,
    shutdown: Option<tokio::sync::broadcast::Sender<()>>,
    joins: Vec<JoinHandle<()>>,
}

impl TestCluster {
    /// Bootstrap a new cluster; serve REST and plain gRPC on ephemeral ports.
    pub async fn start() -> Result<Self> {
        let dir = tempfile::tempdir().context("tempdir")?;
        let data_dir = dir.path().to_path_buf();
        let secrets_key_path = data_dir.join("secrets.key");

        let boot = Bootstrap {
            data_dir: data_dir.clone(),
            secrets_key_path,
            no_auth: false,
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

        let secrets_key =
            Arc::new(SecretsKey::load_file(&boot.secrets_key_path).context("load secrets key")?);

        let state = AppState {
            store: store.clone() as Arc<dyn Store>,
            data_dir: data_dir.clone(),
            version: env!("CARGO_PKG_VERSION"),
            secrets_key: secrets_key.clone(),
        };

        let rest_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .context("bind rest")?;
        let addr = rest_listener.local_addr().context("rest local_addr")?;
        let base_url = format!("http://{addr}");

        let grpc_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .context("bind grpc")?;
        let grpc_addr = grpc_listener.local_addr().context("grpc local_addr")?;
        let grpc_url = format!("http://{grpc_addr}");

        let (tx, _) = tokio::sync::broadcast::channel::<()>(1);

        let app = router(state);
        let mut rx_rest = tx.subscribe();
        let rest_join = tokio::spawn(async move {
            let _ = axum::serve(rest_listener, app)
                .with_graceful_shutdown(async move {
                    let _ = rx_rest.recv().await;
                })
                .await;
        });

        let agent = AgentSvc {
            store: store.clone() as Arc<dyn Store>,
            secrets_key,
        };
        let svc = AgentServiceServer::new(agent);
        let mut rx_grpc = tx.subscribe();
        let grpc_join = tokio::spawn(async move {
            let incoming = tokio_stream::wrappers::TcpListenerStream::new(grpc_listener);
            let _ = GrpcServer::builder()
                .add_service(svc)
                .serve_with_incoming_shutdown(incoming, async move {
                    let _ = rx_grpc.recv().await;
                })
                .await;
        });

        let cluster = Self {
            _dir: dir,
            data_dir,
            base_url: base_url.clone(),
            grpc_url,
            addr,
            grpc_addr,
            api_token: creds.api_token,
            join_token: creds.join_token,
            store,
            shutdown: Some(tx),
            joins: vec![rest_join, grpc_join],
        };

        cluster.wait_healthy().await?;
        Ok(cluster)
    }

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
        for join in self.joins.drain(..) {
            join.abort();
        }
    }
}
