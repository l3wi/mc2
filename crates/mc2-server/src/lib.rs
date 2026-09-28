//! MicroCommandControl orchestrator: state, REST API, scheduler, and the local
//! node loop that drives the embedded microsandbox runtime.

mod api;
mod apply;
mod auth;
mod bootstrap;
pub mod desired;
mod host_metrics;
mod http_serve;
mod ingress;
mod ingress_files;
mod limits;
mod liveness;
mod network_serve;
mod networks;
mod node;
mod scheduler;
mod secrets;
mod ssh;
mod ssh_serve;

pub use api::router;
pub use apply::{
    apply_stack_yaml, probe_host_loopback_port, run_scheduler, ApplyResult, PortProbe,
};
pub use bootstrap::{
    deliver_bootstrap_token, expand_data_dir, generate_api_token, rotate_api_token, Bootstrap,
    BootstrapResult, FreshCredentials,
};
pub use desired::{build_desired_set, DesiredFailure, DesiredSet};
pub use limits::HttpLimits;
pub use liveness::Liveness;
pub use secrets::{decrypt_secret, set_secret};

use anyhow::{Context, Result};
use mc2_store::{SecretsKey, SqliteStore, Store};
use std::future::Future;
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

/// Arguments for `mc2 server` (the single-process orchestrator).
#[derive(Debug, Clone, clap::Args)]
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

    /// Local node name (defaults to hostname). Display only — the node's
    /// identity is persisted in the store, so renaming keeps placements.
    #[arg(long, env = "MC2_NODE_NAME")]
    pub node_name: Option<String>,

    /// Node labels as `key=value` (repeatable)
    #[arg(long = "label", value_name = "KEY=VALUE")]
    pub labels: Vec<String>,

    /// Node reconcile interval seconds
    #[arg(long, default_value_t = 10, env = "MC2_RECONCILE_INTERVAL_SECS")]
    pub reconcile_interval_secs: u64,

    /// Directory for Traefik Ingress catalog files. Same-node BYO proxy.
    #[arg(long, env = "MC2_INGRESS_CONFIG_DIR")]
    pub ingress_config_dir: Option<PathBuf>,

    /// Public hostname to publish this control plane through the ingress
    /// catalog (remote client access; TLS via Traefik).
    #[arg(long, env = "MC2_PUBLIC_HOSTNAME")]
    pub public_hostname: Option<String>,

    /// Traefik cert resolver for the control-plane TLS route (default `le`).
    #[arg(long, default_value = "le", env = "MC2_PUBLIC_TLS_CERT_RESOLVER")]
    pub public_tls_cert_resolver: String,

    /// Root for MC2-owned named-volume directories (default ~/.mc2/volumes).
    /// Each volume is a plain host directory mounted into the VM as a bind
    /// mount with a `size` quota; data persists across recreate and stack removal.
    #[arg(long, env = "MC2_VOLUME_DIR")]
    pub volume_dir: Option<PathBuf>,

    /// Bootstrap only: init data dir + tokens + exit (no listen)
    #[arg(long)]
    pub init_only: bool,

    /// Run this process without API auth (per-start switch; lab only).
    ///
    /// The API token still exists in the store — auth is just skipped for this
    /// run. Requires a loopback `--bind` unless
    /// `--allow-unauthenticated-remote` is also set, and is refused together
    /// with `--public-hostname`.
    #[arg(long, env = "MC2_NO_AUTH", default_value_t = false)]
    pub no_auth: bool,

    /// Explicit confirmation for `--no-auth` on a non-loopback bind (escape
    /// hatch; logs a warning on every start).
    #[arg(
        long,
        env = "MC2_ALLOW_UNAUTHENTICATED_REMOTE",
        default_value_t = false
    )]
    pub allow_unauthenticated_remote: bool,

    /// Allow stack services to request `network.profiles: [host]`.
    ///
    /// `host` gives the guest the whole host loopback — the control API and
    /// every published port — so it is refused at apply unless this is set.
    #[arg(long, env = "MC2_ALLOW_HOST_PROFILE", default_value_t = false)]
    pub allow_host_profile: bool,

    /// Max concurrent REST connections; beyond this the listener answers 503
    /// (minimum 1)
    #[arg(
        long,
        default_value_t = crate::limits::DEFAULT_MAX_CONNECTIONS,
        env = "MC2_MAX_CONNECTIONS"
    )]
    pub max_connections: usize,

    /// Whole-request deadline for ordinary REST routes, seconds
    /// (`exec` and `logs` are exempt; `0` disables the deadline)
    #[arg(
        long,
        default_value_t = crate::limits::DEFAULT_REQUEST_TIMEOUT_SECS,
        env = "MC2_REQUEST_TIMEOUT_SECS"
    )]
    pub request_timeout_secs: u64,

    /// Max concurrent SSH sessions across every instance listener (minimum 1)
    #[arg(
        long,
        default_value_t = crate::limits::DEFAULT_MAX_SSH_SESSIONS,
        env = "MC2_MAX_SSH_SESSIONS"
    )]
    pub max_ssh_sessions: usize,

    /// Max concurrent SSH sessions for one instance listener (minimum 1)
    #[arg(
        long,
        default_value_t = crate::limits::DEFAULT_MAX_SSH_SESSIONS_PER_LISTENER,
        env = "MC2_MAX_SSH_SESSIONS_PER_LISTENER"
    )]
    pub max_ssh_sessions_per_listener: usize,

    /// Max reserved CPUs across the cluster; `0` = unlimited (default)
    #[arg(long, default_value_t = 0, env = "MC2_LIMIT_CPUS")]
    pub limit_cpus: u32,

    /// Max reserved memory MiB across the cluster; `0` = unlimited (default)
    #[arg(long, default_value_t = 0, env = "MC2_LIMIT_MEMORY_MIB")]
    pub limit_memory_mib: u64,

    /// Max reserved disk MiB across all stacks — declared-or-default volume
    /// sizes (`volumes.<name>.size`) plus root disks × replicas
    /// (`services.<name>.storage_opt.size`); `0` = unlimited (default)
    #[arg(long, default_value_t = 0, env = "MC2_LIMIT_DISK_MIB")]
    pub limit_disk_mib: u64,

    /// Log bootstrap result and exit without listening (tests / CI)
    #[arg(long, hide = true)]
    pub dry_run: bool,
}

/// Cluster reservation budget enforced at apply time. `0` = unlimited.
///
/// CPU/RAM are reserved from instance specs. Disk is a **reservation**: the
/// declared-or-default volume sizes plus each replica's root disk, summed over
/// every stack (checked before the store is touched).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ResourceLimits {
    pub cpus: u32,
    pub memory_mib: u64,
    pub disk_mib: u64,
}

/// Shared server state for HTTP handlers.
#[derive(Clone)]
pub struct AppState {
    pub store: Arc<dyn Store>,
    pub data_dir: PathBuf,
    pub version: &'static str,
    pub secrets_key: Arc<SecretsKey>,
    /// Named-volume root (for `rm --volumes`); None → msb default.
    pub volume_dir: Option<PathBuf>,
    /// Shared microsandbox runtime (exec/logs handlers + node loop).
    pub runtime: Arc<dyn mc2_runtime::NodeRuntime>,
    /// Cluster resource budget (`--limit-*`); all-zero = unlimited.
    pub limits: ResourceLimits,
    /// REST listener admission limits (`--max-connections`,
    /// `--request-timeout-secs`).
    pub http_limits: HttpLimits,
    /// Per-process `--no-auth`: skip the bearer check for this run only.
    pub no_auth: bool,
    /// Operator REST bind port; reserved against `expose` claims. `0` = unknown.
    pub rest_port: u16,
    /// `--allow-host-profile`: permit `network.profiles: [host]`.
    pub allow_host_profile: bool,
    /// Host-loopback probe for `expose` ports (real in production, stubbed in tests).
    pub port_probe: apply::PortProbe,
    /// Serializes applies (and stack deletes) so `expose` port claims cannot race.
    pub apply_lock: Arc<tokio::sync::Mutex<()>>,
    /// Node-loop readiness for the unauthenticated `/health` probe (D1).
    pub liveness: Arc<Liveness>,
}

/// True when a bind spec (`host:port`, bare host, or `[ipv6]:port`) is loopback.
///
/// `localhost` and any `127.0.0.0/8` / `::1` address count. Unparseable hosts
/// are treated as non-loopback (fail closed).
pub fn bind_is_loopback(bind: &str) -> bool {
    let host = if let Some(rest) = bind.strip_prefix('[') {
        rest.split(']').next().unwrap_or("")
    } else if bind.matches(':').count() > 1 {
        // Bare IPv6 literal, no port.
        bind
    } else {
        bind.rsplit_once(':').map(|(h, _)| h).unwrap_or(bind)
    };
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

/// Startup guard for `--no-auth`.
///
/// * never allowed with `--public-hostname` (would publish an open control plane);
/// * a non-loopback bind needs the explicit `--allow-unauthenticated-remote`.
pub fn validate_auth_startup(
    no_auth: bool,
    bind: &str,
    public_hostname: Option<&str>,
    allow_unauthenticated_remote: bool,
) -> Result<()> {
    if !no_auth {
        return Ok(());
    }
    if public_hostname.is_some() {
        anyhow::bail!(
            "--no-auth cannot be combined with --public-hostname: that would publish the control \
             plane with no credential. Drop --no-auth (recommended), or drop --public-hostname."
        );
    }
    if !bind_is_loopback(bind) && !allow_unauthenticated_remote {
        anyhow::bail!(
            "--no-auth with a non-loopback bind ({bind}) would expose the orchestrator to the \
             network with no credential. Use a loopback --bind (e.g. 127.0.0.1:7443), or add \
             --allow-unauthenticated-remote to confirm unauthenticated remote access."
        );
    }
    Ok(())
}

/// Startup guard for the values rendered into the Traefik catalog for the
/// control-plane route (`--public-hostname`, `--public-tls-cert-resolver`).
pub fn validate_public_endpoint_flags(
    public_hostname: Option<&str>,
    cert_resolver: &str,
) -> Result<()> {
    if let Some(host) = public_hostname {
        mc2_api::validate_hostname(host)
            .map_err(|e| anyhow::anyhow!("invalid --public-hostname {host:?}: {e}"))?;
    }
    mc2_api::validate_traefik_ident(cert_resolver).map_err(|e| {
        anyhow::anyhow!("invalid --public-tls-cert-resolver {cert_resolver:?}: {e}")
    })?;
    Ok(())
}

/// How often the OTLP gauges are refreshed.
const METRICS_INTERVAL: Duration = Duration::from_secs(15);

/// Periodically export status gauges (when OTLP is enabled).
///
/// Returns when `cancel` fires. Non-critical: [`supervise_metrics`] restarts it
/// with backoff and a metrics failure never takes the orchestrator down (D1).
async fn metrics_loop(store: Arc<dyn Store>, interval: Duration, cancel: CancellationToken) {
    let mut tick = tokio::time::interval(interval);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => return,
            _ = tick.tick() => {}
        }
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

/// Keep the metrics loop running: if it ever stops or panics, restart it with
/// exponential backoff (1 s → 60 s). Non-critical — readiness and the node loop
/// are unaffected by a metrics outage.
async fn supervise_metrics(store: Arc<dyn Store>, cancel: CancellationToken) {
    let mut backoff = Duration::from_secs(1);
    loop {
        let mut task = tokio::spawn(metrics_loop(
            store.clone(),
            METRICS_INTERVAL,
            cancel.clone(),
        ));
        tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                task.abort();
                return;
            }
            res = &mut task => match res {
                Ok(()) => warn!("metrics loop stopped"),
                Err(e) => warn!(error = %e, "metrics loop failed"),
            },
        }
        tokio::select! {
            biased;
            _ = cancel.cancelled() => return,
            _ = tokio::time::sleep(backoff) => {}
        }
        backoff = (backoff * 2).min(Duration::from_secs(60));
    }
}

/// A supervised background task: its label and its result.
type CriticalOutput = (&'static str, Result<()>);

/// Wait for the shutdown signal or the first critical task to end.
///
/// * shutdown signal first → `Ok(())`, the caller runs the bounded drain;
/// * a critical task ending first → `Err`, carrying the task's own error or the
///   panic/cancel cause, so `run()` exits **non-zero** and a service manager can
///   restart a process whose node loop or REST listener is gone. Silently
///   continuing with a dead reconcile loop is exactly what D1 fixes.
async fn supervise<F>(critical: &mut JoinSet<CriticalOutput>, shutdown: F) -> Result<()>
where
    F: Future<Output = ()>,
{
    tokio::select! {
        _ = shutdown => Ok(()),
        joined = critical.join_next() => match joined {
            Some(Ok((name, Ok(())))) => anyhow::bail!("critical task `{name}` exited unexpectedly"),
            Some(Ok((name, Err(e)))) => Err(e.context(format!("critical task `{name}` failed"))),
            Some(Err(e)) if e.is_panic() => anyhow::bail!("critical task panicked: {e}"),
            Some(Err(e)) => anyhow::bail!("critical task cancelled: {e}"),
            None => anyhow::bail!("all critical tasks exited unexpectedly"),
        },
    }
}

/// Slice of the shutdown deadline reserved for aborting and reaping tasks that
/// did not stop on cancellation, so [`drain_or_abort`] stays inside its
/// deadline as a whole.
const ABORT_REAP_SLICE: Duration = Duration::from_millis(250);

/// Give the critical tasks `deadline` to stop, then abort whatever is left.
///
/// The whole call — graceful join **and** the abort/reap of anything that did
/// not stop — is bounded by `deadline`: even when a runtime call never returns
/// or a `logs --follow` stream is still open, the process returns to `main` and
/// exits within `SHUTDOWN_DEADLINE`.
async fn drain_or_abort(critical: &mut JoinSet<CriticalOutput>, deadline: Duration) {
    let graceful = deadline.saturating_sub(ABORT_REAP_SLICE);
    let drained = tokio::time::timeout(graceful, async {
        while critical.join_next().await.is_some() {}
    })
    .await;
    if drained.is_ok() {
        return;
    }
    warn!(
        deadline_secs = deadline.as_secs(),
        "shutdown deadline reached; aborting remaining tasks"
    );
    critical.abort_all();
    // An aborted task completes at its next poll, so this returns promptly; the
    // slice only guards against a task that cannot be interrupted at all.
    let _ = tokio::time::timeout(ABORT_REAP_SLICE, async {
        while critical.join_next().await.is_some() {}
    })
    .await;
}

/// Run the orchestrator (REST + scheduler + local node loop).
pub async fn run(args: ServerArgs) -> Result<()> {
    validate_auth_startup(
        args.no_auth,
        &args.bind,
        args.public_hostname.as_deref(),
        args.allow_unauthenticated_remote,
    )?;
    validate_public_endpoint_flags(
        args.public_hostname.as_deref(),
        &args.public_tls_cert_resolver,
    )?;

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

    if args.no_auth {
        tracing::warn!(
            bind = %args.bind,
            loopback = bind_is_loopback(&args.bind),
            "API auth is DISABLED for this run (--no-auth); the control plane is unauthenticated"
        );
    }

    let _otlp = mc2_metrics::init("mc2-server").context("init OTLP metrics")?;

    let boot = Bootstrap {
        data_dir: data_dir.clone(),
        secrets_key_path: secrets_key_path.clone(),
    }
    .ensure()
    .await
    .context("bootstrap data directory")?;

    let secrets_key = Arc::new(boot.secrets_key);

    let store = SqliteStore::open(&boot.db_path)
        .await
        .context("open store")?;

    if let Some(host) = &args.public_hostname {
        store
            .set_setting("public_hostname", host)
            .await
            .context("persist public hostname")?;
        info!(hostname = %host, "advertising control plane through ingress catalog");
    }
    if let Some(fresh) = &boot.fresh_credentials {
        crate::bootstrap::deliver_bootstrap_token(&data_dir, &fresh.api_token)?;
    } else {
        info!(db = %boot.db_path.display(), "data directory already initialized");
    }

    if args.init_only || args.dry_run {
        info!(
            init_only = args.init_only,
            dry_run = args.dry_run,
            "exiting without listen"
        );
        return Ok(());
    }

    let runtime: Arc<dyn mc2_runtime::NodeRuntime> = Arc::new(
        mc2_runtime::MicrosandboxRuntime::new(args.volume_dir.clone())
            .with_install_id(boot.install_id.clone()),
    );

    // SSH admission limits are process-wide (one process = one node).
    crate::ssh_serve::configure_limits(crate::limits::SshLimits {
        handshake_timeout: crate::limits::SSH_HANDSHAKE_TIMEOUT,
        max_sessions: args.max_ssh_sessions,
        max_sessions_per_listener: args.max_ssh_sessions_per_listener,
    });
    let http_limits = HttpLimits {
        max_connections: args.max_connections,
        request_timeout: Duration::from_secs(args.request_timeout_secs),
        header_read_timeout: crate::limits::HEADER_READ_TIMEOUT,
    };

    // Bind the REST listener before building state so the apply-time port
    // checks know the reserved REST port.
    let rest_listener = tokio::net::TcpListener::bind(&args.bind)
        .await
        .with_context(|| format!("bind REST {}", args.bind))?;
    let rest_addr = rest_listener.local_addr().context("rest local_addr")?;

    // Embedded node configuration (single node: this process).
    let node_cfg = node::NodeConfig {
        name: args
            .node_name
            .clone()
            .or_else(hostname)
            .unwrap_or_else(|| "local".into()),
        install_id: boot.install_id.clone(),
        labels_json: serde_json::to_string(&parse_labels(&args.labels)?)
            .unwrap_or_else(|_| "{}".into()),
        // Node capacity is auto-derived from the host; `--limit-*` is the
        // operator's resource budget. Fall back to 8192 MiB if detection fails.
        cpus: num_cpus::get() as u32,
        memory_mib: crate::host_metrics::host_memory_mib().max(8192),
        reconcile_interval: Duration::from_secs(args.reconcile_interval_secs.max(1)),
        ingress_config_dir: args.ingress_config_dir.clone(),
        public_hostname: args.public_hostname.clone(),
        public_tls_cert_resolver: args.public_tls_cert_resolver.clone(),
        rest_port: rest_addr.port(),
    };

    // Register the node **before** the REST listener accepts: an apply that
    // arrives with the node missing would be rejected for lack of capacity.
    // The id is stable (persisted), so a restart under a different
    // `--node-name` keeps every placement.
    let store_dyn = store.clone() as Arc<dyn Store>;
    let node_id = node::ensure_local_node(store_dyn, &node_cfg).await?;
    info!(node_id = %node_id, name = %node_cfg.name, "local node registered");

    let cancel = CancellationToken::new();
    let liveness = Arc::new(Liveness::for_interval(node_cfg.reconcile_interval));

    let state = AppState {
        store: store.clone() as Arc<dyn Store>,
        data_dir: data_dir.clone(),
        version: env!("CARGO_PKG_VERSION"),
        secrets_key: secrets_key.clone(),
        volume_dir: args.volume_dir.clone(),
        runtime: runtime.clone(),
        limits: ResourceLimits {
            cpus: args.limit_cpus,
            memory_mib: args.limit_memory_mib,
            disk_mib: args.limit_disk_mib,
        },
        http_limits,
        no_auth: args.no_auth,
        rest_port: rest_addr.port(),
        allow_host_profile: args.allow_host_profile,
        port_probe: apply::probe_host_loopback_port,
        apply_lock: Arc::new(tokio::sync::Mutex::new(())),
        liveness: liveness.clone(),
    };

    let app = router(state);

    // Non-critical background work: the metrics exporter restarts with backoff
    // and never takes the process down.
    let store_metrics = store.clone() as Arc<dyn Store>;
    let metrics_cancel = cancel.clone();
    let metrics_task = tokio::spawn(supervise_metrics(store_metrics, metrics_cancel));

    // Critical tasks. If either ends before shutdown, the orchestrator can no
    // longer do its job: `supervise` returns the cause and `run()` exits
    // non-zero so a service manager restarts the process (D1). The node loop
    // also stamps readiness so `/health` stops reporting ok while it is wedged.
    let mut critical: JoinSet<CriticalOutput> = JoinSet::new();
    {
        let store_node = store.clone() as Arc<dyn Store>;
        let runtime_node = runtime.clone();
        let node_cancel = cancel.clone();
        let node_liveness = liveness.clone();
        critical.spawn(async move {
            let res = node::run(
                store_node,
                secrets_key,
                runtime_node,
                node_id,
                node_cfg,
                node_cancel,
                node_liveness.clone(),
            )
            .await;
            node_liveness.mark_dead(match &res {
                Ok(()) => "node loop stopped".to_string(),
                Err(e) => format!("node loop failed: {e:#}"),
            });
            ("node", res)
        });
    }
    {
        let rest_cancel = cancel.clone();
        critical.spawn(async move {
            (
                "rest",
                crate::http_serve::serve(rest_listener, app, http_limits, rest_cancel).await,
            )
        });
    }

    let outcome = supervise(&mut critical, shutdown_signal()).await;

    // Shutdown (D2): cancel everything at once — the node loop stops at the
    // next await point and the listeners close — drain the REST connections
    // inside `SHUTDOWN_DEADLINE`, then abort whatever is still running. VMs are
    // deliberately left alone: they are detached and adoption on restart does
    // the bookkeeping.
    cancel.cancel();
    if let Err(e) = &outcome {
        tracing::error!(
            error = format!("{e:#}"),
            "critical task ended; exiting non-zero"
        );
    }
    drain_or_abort(&mut critical, crate::limits::SHUTDOWN_DEADLINE).await;
    metrics_task.abort();
    let _ = metrics_task.await;

    outcome?;
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

    #[test]
    fn parse_labels_ok_and_rejects_malformed() {
        let m = parse_labels(&["role=worker".into(), "zone=a".into()]).unwrap();
        assert_eq!(m.get("role").unwrap(), "worker");
        assert_eq!(m.get("zone").unwrap(), "a");
        assert!(parse_labels(&["missing-equals".into()]).is_err());
    }

    #[tokio::test]
    async fn dry_run_inits() {
        let dir = tempfile::tempdir().unwrap();
        let args = ServerArgs {
            bind: "127.0.0.1:0".into(),
            data_dir: dir.path().to_string_lossy().into(),
            secrets_key_path: None,
            node_name: None,
            labels: vec![],
            reconcile_interval_secs: 10,
            ingress_config_dir: None,
            public_hostname: None,
            public_tls_cert_resolver: "le".into(),
            volume_dir: None,
            init_only: false,
            no_auth: false,
            allow_unauthenticated_remote: false,
            allow_host_profile: false,
            max_connections: crate::limits::DEFAULT_MAX_CONNECTIONS,
            request_timeout_secs: crate::limits::DEFAULT_REQUEST_TIMEOUT_SECS,
            max_ssh_sessions: crate::limits::DEFAULT_MAX_SSH_SESSIONS,
            max_ssh_sessions_per_listener: crate::limits::DEFAULT_MAX_SSH_SESSIONS_PER_LISTENER,
            limit_cpus: 0,
            limit_memory_mib: 0,
            limit_disk_mib: 0,
            dry_run: true,
        };
        run(args).await.expect("dry_run should succeed");
        assert!(dir.path().join("mc2.db").exists());
        assert!(dir.path().join("secrets.key").exists());
    }

    #[test]
    fn bind_loopback_detection() {
        for b in [
            "127.0.0.1:7443",
            "127.0.0.5:7443",
            "localhost:7443",
            "LOCALHOST:7443",
            "[::1]:7443",
            "::1",
            "127.0.0.1",
        ] {
            assert!(bind_is_loopback(b), "{b} should be loopback");
        }
        for b in [
            "0.0.0.0:7443",
            "192.168.1.10:7443",
            "[::]:7443",
            "mc2.example.com:7443",
            "not-an-ip:7443",
        ] {
            assert!(!bind_is_loopback(b), "{b} should not be loopback");
        }
    }

    #[test]
    fn no_auth_loopback_is_allowed() {
        validate_auth_startup(true, "127.0.0.1:7443", None, false).unwrap();
        validate_auth_startup(true, "localhost:7443", None, false).unwrap();
    }

    #[test]
    fn no_auth_non_loopback_requires_hatch() {
        let err = validate_auth_startup(true, "0.0.0.0:7443", None, false).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("--allow-unauthenticated-remote"), "{msg}");
        assert!(msg.contains("loopback"), "{msg}");
        // With the hatch it passes.
        validate_auth_startup(true, "0.0.0.0:7443", None, true).unwrap();
    }

    #[test]
    fn no_auth_with_public_hostname_always_refused() {
        // No hatch on this path.
        for hatch in [false, true] {
            let err = validate_auth_startup(true, "127.0.0.1:7443", Some("mc2.example.com"), hatch)
                .unwrap_err();
            assert!(err.to_string().contains("--public-hostname"), "{err}");
        }
    }

    #[test]
    fn auth_on_never_trips_the_guard() {
        validate_auth_startup(false, "0.0.0.0:7443", Some("mc2.example.com"), false).unwrap();
    }

    #[test]
    fn public_endpoint_flags_reject_injection() {
        validate_public_endpoint_flags(Some("mc2.example.com"), "le").unwrap();
        validate_public_endpoint_flags(None, "le").unwrap();
        assert!(validate_public_endpoint_flags(Some("a.com\"\n  x: y"), "le").is_err());
        assert!(validate_public_endpoint_flags(Some("mc2.example.com"), "le\nfoo: bar").is_err());
    }

    #[tokio::test]
    async fn dry_run_no_auth_non_loopback_refused() {
        let dir = tempfile::tempdir().unwrap();
        let args = ServerArgs {
            bind: "0.0.0.0:0".into(),
            data_dir: dir.path().to_string_lossy().into(),
            secrets_key_path: None,
            node_name: None,
            labels: vec![],
            reconcile_interval_secs: 10,
            ingress_config_dir: None,
            public_hostname: None,
            public_tls_cert_resolver: "le".into(),
            volume_dir: None,
            init_only: false,
            no_auth: true,
            allow_unauthenticated_remote: false,
            allow_host_profile: false,
            max_connections: crate::limits::DEFAULT_MAX_CONNECTIONS,
            request_timeout_secs: crate::limits::DEFAULT_REQUEST_TIMEOUT_SECS,
            max_ssh_sessions: crate::limits::DEFAULT_MAX_SSH_SESSIONS,
            max_ssh_sessions_per_listener: crate::limits::DEFAULT_MAX_SSH_SESSIONS_PER_LISTENER,
            limit_cpus: 0,
            limit_memory_mib: 0,
            limit_disk_mib: 0,
            dry_run: true,
        };
        let err = run(args).await.unwrap_err();
        assert!(
            err.to_string().contains("--allow-unauthenticated-remote"),
            "{err}"
        );
    }

    /// D1: shutdown (not a task ending) is the normal path.
    #[tokio::test]
    async fn supervisor_returns_ok_when_shutdown_fires_first() {
        let mut critical: JoinSet<CriticalOutput> = JoinSet::new();
        critical.spawn(async { std::future::pending::<CriticalOutput>().await });
        supervise(&mut critical, std::future::ready(()))
            .await
            .unwrap();
    }

    /// D1: a critical task ending before shutdown is fatal, and its own error
    /// is the cause — `run()` then exits non-zero.
    #[tokio::test]
    async fn supervisor_exits_with_the_failing_task_error() {
        let mut critical: JoinSet<CriticalOutput> = JoinSet::new();
        critical.spawn(async { ("node", Err(anyhow::anyhow!("reconcile loop died"))) });
        let err = supervise(&mut critical, std::future::pending::<()>())
            .await
            .unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("node"), "{msg}");
        assert!(msg.contains("reconcile loop died"), "{msg}");
    }

    /// D1: a panicking critical task is fatal too, even though it returns no
    /// error value of its own.
    #[tokio::test]
    async fn supervisor_treats_a_panic_as_fatal() {
        let mut critical: JoinSet<CriticalOutput> = JoinSet::new();
        critical.spawn(async { panic!("reconcile exploded") });
        let err = supervise(&mut critical, std::future::pending::<()>())
            .await
            .unwrap_err();
        assert!(format!("{err:#}").contains("panicked"), "{err:#}");
    }

    /// D2: a critical task wedged in a call that never returns cannot hold
    /// shutdown past the deadline — it is aborted instead.
    #[tokio::test]
    async fn drain_aborts_a_wedged_task_within_the_deadline() {
        let mut critical: JoinSet<CriticalOutput> = JoinSet::new();
        critical.spawn(async {
            std::future::pending::<()>().await;
            ("node", Ok(()))
        });
        let started = std::time::Instant::now();
        drain_or_abort(&mut critical, Duration::from_millis(50)).await;
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "drain must be bounded: {:?}",
            started.elapsed()
        );
        assert!(critical.is_empty(), "aborted tasks must be reaped");
    }
}
