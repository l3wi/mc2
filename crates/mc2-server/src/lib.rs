//! MicroCommandControl server process (control plane + local node).

mod apply;
mod auth;
mod bootstrap;
pub mod desired;
mod fabric;
mod fabric_serve;
mod http;
mod ingress;
mod ingress_files;
mod node;
mod reschedule;
mod scheduler;
mod secrets;
mod ssh;
mod ssh_serve;
mod watcher;

pub use apply::{apply_stack_yaml, run_scheduler, ApplyResult};
pub use bootstrap::{expand_data_dir, Bootstrap, BootstrapResult, FreshCredentials};
pub use desired::build_desired_set;
pub use http::router;
pub use reschedule::reschedule_not_ready;
pub use secrets::{decrypt_secret, set_secret};

use anyhow::{Context, Result};
use clap::Parser;
use mc2_store::{SecretsKey, SqliteStore, Store};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tracing::info;

/// Arguments for `mc2 server` (control plane + local node in one process).
#[derive(Debug, Clone, Parser)]
pub struct ServerArgs {
    /// Address to bind the operator REST API
    #[arg(long, default_value = "127.0.0.1:7443", env = "MC2_BIND")]
    pub bind: String,

    /// Data directory (SQLite, tokens)
    #[arg(long, default_value = "~/.mc2", env = "MC2_DATA_DIR")]
    pub data_dir: String,

    /// Path to secrets encryption key (32 bytes). Default: `<data_dir>/secrets.key`
    #[arg(long, env = "MC2_SECRETS_KEY_PATH")]
    pub secrets_key_path: Option<String>,

    /// Seconds without heartbeat before a Ready node becomes NotReady
    #[arg(long, default_value_t = 45, env = "MC2_HEARTBEAT_GRACE_SECS")]
    pub heartbeat_grace_secs: u64,

    /// Interval for reschedule + Pending schedule loops (seconds)
    #[arg(long, default_value_t = 5, env = "MC2_RESCHEDULE_INTERVAL_SECS")]
    pub reschedule_interval_secs: u64,

    /// Local node name (defaults to hostname)
    #[arg(long, env = "MC2_NODE_NAME")]
    pub node_name: Option<String>,

    /// Node labels as `key=value` (repeatable)
    #[arg(long = "label", value_name = "KEY=VALUE")]
    pub labels: Vec<String>,

    /// Advertise CPU capacity (default: host logical CPUs)
    #[arg(long, env = "MC2_NODE_CPUS")]
    pub cpus: Option<u32>,

    /// Advertise memory capacity MiB (default: 8192 if unknown)
    #[arg(long, env = "MC2_NODE_MEMORY_MIB")]
    pub memory_mib: Option<u64>,

    /// Node reconcile interval seconds
    #[arg(long, default_value_t = 10, env = "MC2_RECONCILE_INTERVAL_SECS")]
    pub reconcile_interval_secs: u64,

    /// Directory for Traefik Ingress catalog files. Same-node BYO proxy.
    #[arg(long, env = "MC2_INGRESS_CONFIG_DIR")]
    pub ingress_config_dir: Option<PathBuf>,

    /// Named-volume root on durable node storage (default ~/.microsandbox/volumes).
    /// Volumes persist across sandbox recreation and are retained on stack removal.
    #[arg(long, env = "MC2_VOLUME_DIR")]
    pub volume_dir: Option<PathBuf>,

    /// Bootstrap only: init data dir + tokens + exit (no listen)
    #[arg(long)]
    pub init_only: bool,

    /// Initialize without an API token (lab only). Only applies on first
    /// bootstrap of a data dir.
    #[arg(long, env = "MC2_NO_AUTH", default_value_t = false)]
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
    pub secrets_key: Arc<SecretsKey>,
}

/// Periodically export cluster gauges (when OTLP is enabled).
async fn metrics_loop(store: Arc<dyn Store>, interval: Duration) {
    let mut tick = tokio::time::interval(interval);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        let Ok(counts) = store.cluster_counts().await else {
            continue;
        };
        let Ok(instances) = store.list_instances().await else {
            continue;
        };
        let mut by_phase: std::collections::BTreeMap<String, u64> =
            std::collections::BTreeMap::new();
        for i in instances {
            *by_phase.entry(i.phase).or_default() += 1;
        }
        let phase_counts: Vec<(String, u64)> = by_phase.into_iter().collect();
        mc2_metrics::set_cluster_gauges(
            counts.nodes_total as u64,
            counts.nodes_ready as u64,
            &phase_counts,
        );
    }
}

/// Run the control plane + local node.
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
        api = mc2_api::API_VERSION,
        "MicroCommandControl server starting"
    );

    let _otlp = mc2_metrics::init("mc2-server").context("init OTLP metrics")?;

    let boot = Bootstrap {
        data_dir: data_dir.clone(),
        secrets_key_path: secrets_key_path.clone(),
        no_auth: args.no_auth,
    }
    .ensure()
    .await
    .context("bootstrap data directory")?;

    let secrets_key = Arc::new(
        SecretsKey::load_file(&boot.secrets_key_path)
            .with_context(|| format!("load secrets key {}", boot.secrets_key_path.display()))?,
    );

    let store = SqliteStore::open(&boot.db_path)
        .await
        .context("open store")?;

    if let Some(ref plain) = boot.fresh_credentials {
        eprintln!("=== MicroCommandControl bootstrap credentials (save these; shown once) ===");
        eprintln!(
            "API token  (REST Authorization: Bearer …): {}",
            plain.api_token
        );
        eprintln!("Data dir: {}", data_dir.display());
        eprintln!("=========================================================================");
    } else if args.no_auth {
        if store.api_auth_required().await.unwrap_or(true) {
            info!("MC2_NO_AUTH/--no-auth ignored: cluster already has API auth configured");
        } else {
            info!(db = %boot.db_path.display(), "open cluster (no API token)");
        }
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
        store: store.clone() as Arc<dyn Store>,
        data_dir: data_dir.clone(),
        version: env!("CARGO_PKG_VERSION"),
        secrets_key: secrets_key.clone(),
    };

    let app = router(state);
    let rest_listener = tokio::net::TcpListener::bind(&args.bind)
        .await
        .with_context(|| format!("bind REST {}", args.bind))?;
    let rest_addr = rest_listener.local_addr().context("rest local_addr")?;

    let grace = Duration::from_secs(args.heartbeat_grace_secs);
    let store_watch = store.clone() as Arc<dyn Store>;
    tokio::spawn(async move {
        watcher::not_ready_loop(store_watch, grace, Duration::from_secs(5)).await;
    });

    let reschedule_every = Duration::from_secs(args.reschedule_interval_secs.max(1));
    let store_resched = store.clone() as Arc<dyn Store>;
    tokio::spawn(async move {
        reschedule::reschedule_loop(store_resched, reschedule_every).await;
    });
    let store_sched = store.clone() as Arc<dyn Store>;
    tokio::spawn(async move {
        reschedule::schedule_loop(store_sched, reschedule_every).await;
    });
    let store_metrics = store.clone() as Arc<dyn Store>;
    tokio::spawn(async move {
        metrics_loop(store_metrics, Duration::from_secs(15)).await;
    });

    // Local node loop (embedded microsandbox runtime).
    let (shutdown_tx, shutdown_rx) = tokio::sync::broadcast::channel::<()>(1);
    let node_cfg = node::NodeConfig {
        name: args
            .node_name
            .clone()
            .or_else(hostname)
            .unwrap_or_else(|| "local".into()),
        labels_json: serde_json::to_string(&parse_labels(&args.labels)?)
            .unwrap_or_else(|_| "{}".into()),
        cpus: args.cpus.unwrap_or_else(|| num_cpus::get() as u32),
        memory_mib: args.memory_mib.unwrap_or(8192),
        reconcile_interval: Duration::from_secs(args.reconcile_interval_secs.max(1)),
        volume_dir: args.volume_dir.clone(),
        ingress_config_dir: args.ingress_config_dir.clone(),
    };
    let store_node = store.clone() as Arc<dyn Store>;
    let node_task = tokio::spawn(async move {
        if let Err(e) = node::run(store_node, secrets_key, node_cfg, shutdown_rx).await {
            tracing::error!(error = %e, "local node loop failed");
        }
    });

    let (rest_tx, rest_rx) = tokio::sync::oneshot::channel::<()>();
    let rest_task = tokio::spawn(async move {
        let _ = axum::serve(rest_listener, app)
            .with_graceful_shutdown(async {
                let _ = rest_rx.await;
            })
            .await;
    });

    info!(%rest_addr, "REST listening; health: GET /health, status: GET /v1/status, nodes: GET /v1/nodes");

    shutdown_signal().await;
    let _ = shutdown_tx.send(());
    let _ = node_task.await;
    let _ = rest_tx.send(());
    let _ = rest_task.await;
    info!("server stopped");
    Ok(())
}

fn parse_labels(items: &[String]) -> Result<std::collections::BTreeMap<String, String>> {
    let mut map = std::collections::BTreeMap::new();
    for item in items {
        let (k, v) = item
            .split_once('=')
            .with_context(|| anyhow::anyhow!("label must be KEY=VALUE, got {item}"))?;
        map.insert(k.to_string(), v.to_string());
    }
    Ok(map)
}

fn hostname() -> Option<String> {
    std::env::var("HOSTNAME")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| {
            std::fs::read_to_string("/etc/hostname")
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        })
        .or_else(|| {
            std::process::Command::new("hostname")
                .output()
                .ok()
                .and_then(|o| {
                    let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
                    if s.is_empty() {
                        None
                    } else {
                        Some(s)
                    }
                })
        })
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
        let p = expand_data_dir("~/.mc2");
        assert_eq!(p, PathBuf::from("/tmp/home/.mc2"));
    }

    #[tokio::test]
    async fn dry_run_inits() {
        let dir = tempfile::tempdir().unwrap();
        let args = ServerArgs {
            bind: "127.0.0.1:0".into(),
            data_dir: dir.path().to_string_lossy().into(),
            secrets_key_path: None,
            heartbeat_grace_secs: 45,
            reschedule_interval_secs: 5,
            node_name: None,
            labels: vec![],
            cpus: None,
            memory_mib: None,
            reconcile_interval_secs: 10,
            ingress_config_dir: None,
            volume_dir: None,
            init_only: false,
            no_auth: false,
            dry_run: true,
        };
        run(args).await.expect("dry_run should succeed");
        assert!(dir.path().join("mc2.db").exists());
        assert!(dir.path().join("secrets.key").exists());
    }
}
