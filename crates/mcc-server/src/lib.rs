//! MicroCommandControl server process (control plane).
//!
//! Phase 0: CLI args + stub that exits after logging bootstrap intent.
//! Phase 1: SQLite store, REST, auth, real listen loop.

use anyhow::Result;
use clap::Parser;
use std::path::PathBuf;
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

    /// Log and exit without listening (used in early scaffolding / tests)
    #[arg(long, hide = true)]
    pub dry_run: bool,
}

/// Expand a leading `~` in a path string.
pub fn expand_data_dir(raw: &str) -> PathBuf {
    if let Some(rest) = raw.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    if raw == "~" {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home);
        }
    }
    PathBuf::from(raw)
}

/// Run the control plane.
pub async fn run(args: ServerArgs) -> Result<()> {
    let data_dir = expand_data_dir(&args.data_dir);
    info!(
        bind = %args.bind,
        data_dir = %data_dir.display(),
        api = mcc_api::API_VERSION,
        "MicroCommandControl server starting (Phase 0 stub)"
    );

    // Phase 1 will open SqliteStore, generate tokens, serve REST.
    let _store = mcc_store::MemoryStore::new();

    if args.dry_run {
        info!("dry_run: not listening");
        return Ok(());
    }

    info!("server listen not implemented yet — Phase 1. Use --dry-run to exit cleanly in CI.");
    anyhow::bail!("mcc server listen loop is Phase 1. For Phase 0 smoke: `mcc server --dry-run`");
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
    async fn dry_run_ok() {
        let args = ServerArgs {
            bind: "127.0.0.1:0".into(),
            data_dir: "/tmp/mcc-test".into(),
            dry_run: true,
        };
        run(args).await.expect("dry_run should succeed");
    }
}
