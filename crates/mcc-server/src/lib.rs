//! MicroCommandControl server process (control plane).

mod apply;
mod auth;
mod bootstrap;
mod grpc;
mod http;
mod scheduler;
mod tls;
mod watcher;

pub use apply::{apply_stack_yaml, run_scheduler, ApplyResult};
pub use bootstrap::{expand_data_dir, Bootstrap, BootstrapResult, FreshCredentials};
pub use grpc::AgentSvc;
pub use http::router;
pub use tls::{ensure_dev_tls, TlsPaths};

use anyhow::{Context, Result};
use clap::Parser;
use mcc_api::agent::agent_service_server::AgentServiceServer;
use mcc_store::{SqliteStore, Store};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tonic::transport::{Identity, Server as GrpcServer, ServerTlsConfig};
use tracing::info;

/// Arguments for `mcc server`.
#[derive(Debug, Clone, Parser)]
pub struct ServerArgs {
    /// Address to bind the operator REST API
    #[arg(long, default_value = "127.0.0.1:7443", env = "MCC_BIND")]
    pub bind: String,

    /// Address to bind the agent gRPC API
    #[arg(long, default_value = "127.0.0.1:7444", env = "MCC_GRPC_BIND")]
    pub grpc_bind: String,

    /// Data directory (SQLite, tokens, TLS material)
    #[arg(long, default_value = "~/.mcc", env = "MCC_DATA_DIR")]
    pub data_dir: String,

    /// Path to secrets encryption key (32 bytes). Default: `<data_dir>/secrets.key`
    #[arg(long, env = "MCC_SECRETS_KEY_PATH")]
    pub secrets_key_path: Option<String>,

    /// Serve agent gRPC over plain h2c (no TLS). Default is TLS with lab certs in `<data_dir>/tls`.
    #[arg(long, env = "MCC_GRPC_PLAIN", default_value_t = false)]
    pub grpc_plain: bool,

    /// Seconds without heartbeat before a Ready node becomes NotReady
    #[arg(long, default_value_t = 45, env = "MCC_HEARTBEAT_GRACE_SECS")]
    pub heartbeat_grace_secs: u64,

    /// Bootstrap only: init data dir + tokens + exit (no listen)
    #[arg(long)]
    pub init_only: bool,

    /// Initialize without API/join tokens. REST and agent join work without
    /// credentials (lab only). Only applies on first bootstrap of a data dir.
    #[arg(long, env = "MCC_NO_AUTH", default_value_t = false)]
    pub no_auth: bool,

    /// Log bootstrap result and exit without listening (tests / CI)
    #[arg(long, hide = true)]
    pub dry_run: bool,
}

/// Shared server state for HTTP handlers.
#[derive(Clone)]
pub struct AppState {
    pub store: Arc<dyn Store>,
    pub data_dir: PathBuf,
    pub version: &'static str,
}

/// Run the control plane.
pub async fn run(args: ServerArgs) -> Result<()> {
    let data_dir = expand_data_dir(&args.data_dir);
    let secrets_key_path = args
        .secrets_key_path
        .as_ref()
        .map(|p| expand_data_dir(p))
        .unwrap_or_else(|| data_dir.join("secrets.key"));

    let use_tls = !args.grpc_plain;

    info!(
        bind = %args.bind,
        grpc_bind = %args.grpc_bind,
        grpc_tls = use_tls,
        data_dir = %data_dir.display(),
        secrets_key = %secrets_key_path.display(),
        api = mcc_api::API_VERSION,
        "MicroCommandControl server starting"
    );

    let boot = Bootstrap {
        data_dir: data_dir.clone(),
        secrets_key_path,
        no_auth: args.no_auth,
    }
    .ensure()
    .await
    .context("bootstrap data directory")?;

    let store = SqliteStore::open(&boot.db_path)
        .await
        .context("open store")?;

    if let Some(ref plain) = boot.fresh_credentials {
        eprintln!("=== MicroCommandControl bootstrap credentials (save these; shown once) ===");
        eprintln!(
            "API token  (REST Authorization: Bearer …): {}",
            plain.api_token
        );
        eprintln!(
            "Join token (agent --token):                {}",
            plain.join_token
        );
        eprintln!("Data dir: {}", data_dir.display());
        eprintln!("=========================================================================");
    } else if args.no_auth {
        if store.api_auth_required().await.unwrap_or(true) {
            info!("MCC_NO_AUTH/--no-auth ignored: cluster already has API auth configured");
        } else {
            info!(db = %boot.db_path.display(), "open cluster (no API/join tokens)");
        }
    } else {
        info!(db = %boot.db_path.display(), "cluster already initialized");
    }

    let tls_paths = if use_tls {
        Some(ensure_dev_tls(&data_dir).context("ensure dev TLS")?)
    } else {
        // Still generate material for operators who flip TLS on later.
        let _ = ensure_dev_tls(&data_dir);
        None
    };

    if args.init_only || args.dry_run {
        info!(
            init_only = args.init_only,
            dry_run = args.dry_run,
            "exiting without listen"
        );
        return Ok(());
    }

    let state = AppState {
        store: store.clone() as Arc<dyn Store>,
        data_dir: data_dir.clone(),
        version: env!("CARGO_PKG_VERSION"),
    };

    let app = router(state);
    let rest_listener = tokio::net::TcpListener::bind(&args.bind)
        .await
        .with_context(|| format!("bind REST {}", args.bind))?;
    let rest_addr = rest_listener.local_addr().context("rest local_addr")?;

    let grpc_addr: SocketAddr = args
        .grpc_bind
        .parse()
        .with_context(|| format!("parse grpc bind {}", args.grpc_bind))?;

    let grace = Duration::from_secs(args.heartbeat_grace_secs);
    let store_watch = store.clone() as Arc<dyn Store>;
    tokio::spawn(async move {
        watcher::not_ready_loop(store_watch, grace, Duration::from_secs(5)).await;
    });

    let agent = AgentSvc {
        store: store as Arc<dyn Store>,
    };
    let svc = AgentServiceServer::new(agent);

    info!(%rest_addr, "REST listening; health: GET /health, status: GET /v1/status, nodes: GET /v1/nodes");
    if let Some(ref paths) = tls_paths {
        info!(
            %grpc_addr,
            ca = %paths.ca_cert.display(),
            "gRPC listening with TLS; agents: mcc agent --server https://HOST:PORT --tls-ca <ca.pem>"
        );
    } else {
        info!(%grpc_addr, "gRPC listening plain (h2c); agents: mcc agent --server http://HOST:PORT");
    }

    let rest = axum::serve(rest_listener, app).with_graceful_shutdown(shutdown_signal());

    let grpc = async move {
        let mut builder = GrpcServer::builder();
        if let Some(paths) = tls_paths {
            let cert = std::fs::read(&paths.server_cert).context("read server cert")?;
            let key = std::fs::read(&paths.server_key).context("read server key")?;
            let identity = Identity::from_pem(cert, key);
            let tls = ServerTlsConfig::new().identity(identity);
            builder = builder.tls_config(tls).context("grpc tls_config")?;
        }
        builder
            .add_service(svc)
            .serve_with_shutdown(grpc_addr, shutdown_signal())
            .await
            .context("grpc serve")?;
        Ok::<(), anyhow::Error>(())
    };

    tokio::try_join!(
        async {
            rest.await.context("rest serve")?;
            Ok::<(), anyhow::Error>(())
        },
        grpc
    )?;

    info!("server stopped");
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("install ctrl+c handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("install signal handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
    info!("shutdown signal received");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expand_tilde() {
        std::env::set_var("HOME", "/tmp/home");
        let p = expand_data_dir("~/.mcc");
        assert_eq!(p, PathBuf::from("/tmp/home/.mcc"));
    }

    #[tokio::test]
    async fn dry_run_inits() {
        let dir = tempfile::tempdir().unwrap();
        let args = ServerArgs {
            bind: "127.0.0.1:0".into(),
            grpc_bind: "127.0.0.1:0".into(),
            data_dir: dir.path().to_string_lossy().into(),
            secrets_key_path: None,
            grpc_plain: true,
            heartbeat_grace_secs: 45,
            init_only: false,
            no_auth: false,
            dry_run: true,
        };
        run(args).await.expect("dry_run should succeed");
        assert!(dir.path().join("mcc.db").exists());
        assert!(dir.path().join("secrets.key").exists());
    }
}
