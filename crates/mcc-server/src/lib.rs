//! MicroCommandControl server process (control plane).

mod auth;
mod bootstrap;
mod http;

pub use bootstrap::{expand_data_dir, Bootstrap, BootstrapResult, FreshCredentials};
pub use http::router;

use anyhow::{Context, Result};
use clap::Parser;
use mcc_store::{SqliteStore, Store};
use std::path::PathBuf;
use std::sync::Arc;
use tracing::info;

/// Arguments for `mcc server`.
#[derive(Debug, Clone, Parser)]
pub struct ServerArgs {
    /// Address to bind the operator REST API
    #[arg(long, default_value = "127.0.0.1:7443", env = "MCC_BIND")]
    pub bind: String,

    /// Data directory (SQLite, tokens, TLS material)
    #[arg(long, default_value = "~/.mcc", env = "MCC_DATA_DIR")]
    pub data_dir: String,

    /// Path to secrets encryption key (32 bytes). Default: `<data_dir>/secrets.key`
    #[arg(long, env = "MCC_SECRETS_KEY_PATH")]
    pub secrets_key_path: Option<String>,

    /// Bootstrap only: init data dir + tokens + exit (no listen)
    #[arg(long)]
    pub init_only: bool,

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

    info!(
        bind = %args.bind,
        data_dir = %data_dir.display(),
        secrets_key = %secrets_key_path.display(),
        api = mcc_api::API_VERSION,
        "MicroCommandControl server starting"
    );

    let boot = Bootstrap {
        data_dir: data_dir.clone(),
        secrets_key_path,
    }
    .ensure()
    .await
    .context("bootstrap data directory")?;

    let store = SqliteStore::open(&boot.db_path)
        .await
        .context("open store")?;

    if let Some(ref plain) = boot.fresh_credentials {
        // Print once on first init — hashes only are stored in SQLite.
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
    } else {
        info!(db = %boot.db_path.display(), "cluster already initialized");
    }

    if args.init_only || args.dry_run {
        info!(
            init_only = args.init_only,
            dry_run = args.dry_run,
            "exiting without listen"
        );
        return Ok(());
    }

    let state = AppState {
        store,
        data_dir,
        version: env!("CARGO_PKG_VERSION"),
    };

    let app = http::router(state);

    let listener = tokio::net::TcpListener::bind(&args.bind)
        .await
        .with_context(|| format!("bind {}", args.bind))?;

    let local = listener.local_addr().context("local_addr")?;
    info!(%local, "REST listening (TLS later); health: GET /health, status: GET /v1/status");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("serve")?;

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
            data_dir: dir.path().to_string_lossy().into(),
            secrets_key_path: None,
            init_only: false,
            dry_run: true,
        };
        run(args).await.expect("dry_run should succeed");
        assert!(dir.path().join("mcc.db").exists());
        assert!(dir.path().join("secrets.key").exists());
    }
}
