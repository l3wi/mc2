//! MicroCommandControl node agent.
//!
//! Join + heartbeat over gRPC; reconcile desired instances via the
//! embedded microsandbox Rust SDK (only runtime).

use anyhow::{bail, Context, Result};
use clap::Parser;
use mcc_api::agent::agent_service_client::AgentServiceClient;
use mcc_api::agent::{
    Capacity, HeartbeatRequest, InstanceStatus, JoinRequest, ReportStatusRequest, SyncRequest,
};
mod ssh_serve;

use mcc_runtime::{
    backoff_secs, default_runtime, desired_from_sync, NodeRuntime, RestartPolicy, SandboxPhase,
};
use ssh_serve::{SshBackend, SshServeConfig, SshServeTable};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tonic::transport::{Certificate, Channel, ClientTlsConfig, Endpoint};
use tracing::{info, warn};

/// Per-sandbox restart / health bookkeeping on the agent.
#[derive(Debug, Default)]
struct InstanceRuntimeState {
    restart_count: u32,
    next_restart_ok: Option<Instant>,
    last_health: Option<Instant>,
    running_since: Option<Instant>,
}

/// Arguments for `mcc agent`.
#[derive(Debug, Clone, Parser)]
pub struct AgentArgs {
    /// Control plane gRPC endpoint (`http://host:7444` or `https://host:7444`)
    #[arg(long, env = "MCC_SERVER")]
    pub server: Option<String>,

    /// Join token from `mcc server` bootstrap
    #[arg(long, env = "MCC_JOIN_TOKEN")]
    pub token: Option<String>,

    /// Node name (defaults to hostname)
    #[arg(long, env = "MCC_NODE_NAME")]
    pub name: Option<String>,

    /// Path to PEM CA cert (for https gRPC with lab self-signed CA)
    #[arg(long, env = "MCC_TLS_CA")]
    pub tls_ca: Option<PathBuf>,

    /// Skip TLS certificate verification (lab only)
    #[arg(long, env = "MCC_TLS_INSECURE")]
    pub insecure: bool,

    /// Heartbeat / reconcile interval seconds
    #[arg(long, default_value_t = 10, env = "MCC_HEARTBEAT_INTERVAL_SECS")]
    pub heartbeat_interval_secs: u64,

    /// Advertise CPU capacity (default: host logical CPUs)
    #[arg(long, env = "MCC_NODE_CPUS")]
    pub cpus: Option<u32>,

    /// Advertise memory capacity MiB (default: 8192 if unknown)
    #[arg(long, env = "MCC_NODE_MEMORY_MIB")]
    pub memory_mib: Option<u64>,

    /// Node labels as `key=value` (repeatable)
    #[arg(long = "label", value_name = "KEY=VALUE")]
    pub labels: Vec<String>,

    /// How this agent opens host SSH for instances: `auto` | `msb-cli` | `disabled` | `sdk`
    ///
    /// - `auto` (default): use msb CLI when `msb ssh` exists, else report Failed with a hint  
    /// - `msb-cli`: require `msb ssh serve` / `authorize`  
    /// - `disabled`: never open listeners (CP still tracks keys/desired)  
    /// - `sdk`: in-process microsandbox SSH (not available until upstream deps resolve)
    #[arg(
        long,
        default_value = "auto",
        env = "MCC_SSH_BACKEND",
        value_name = "MODE"
    )]
    pub ssh_backend: String,

    /// Path to `msb` binary for `msb-cli` / `auto` SSH backend (default: `msb` on PATH)
    #[arg(long, env = "MCC_MSB_BIN", default_value = "msb")]
    pub msb_bin: PathBuf,

    /// Log and exit without connecting
    #[arg(long, hide = true)]
    pub dry_run: bool,
}

/// Run the agent.
pub async fn run(args: AgentArgs) -> Result<()> {
    let name = args
        .name
        .clone()
        .or_else(hostname)
        .unwrap_or_else(|| "unknown".into());

    let runtime: Arc<dyn NodeRuntime> = Arc::new(default_runtime());

    let ssh_backend = SshBackend::parse(&args.ssh_backend)
        .map_err(|e| anyhow::anyhow!("--ssh-backend / MCC_SSH_BACKEND: {e}"))?;
    let ssh_cfg = SshServeConfig {
        backend: ssh_backend,
        msb_bin: args.msb_bin.clone(),
    };

    info!(
        node = %name,
        server = ?args.server,
        has_token = args.token.is_some(),
        runtime = "microsandbox-sdk",
        ssh_backend = ssh_backend.as_str(),
        msb_bin = %args.msb_bin.display(),
        api = mcc_api::API_VERSION,
        "MicroCommandControl agent starting"
    );

    let _otlp = mcc_metrics::init("mcc-agent").context("init OTLP metrics")?;

    if args.dry_run {
        info!("dry_run: not connecting");
        return Ok(());
    }

    let server = args
        .server
        .clone()
        .context("missing --server / MCC_SERVER (e.g. https://127.0.0.1:7444)")?;
    // Join token optional when server was bootstrapped with --no-auth.
    let join_token = args.token.clone().unwrap_or_default();

    let mut client = connect(&server, args.tls_ca.as_ref(), args.insecure)
        .await
        .with_context(|| format!("connect to {server}"))?;

    let labels = parse_labels(&args.labels)?;
    let cpus = args.cpus.unwrap_or_else(|| num_cpus::get() as u32);
    let memory_mib = args.memory_mib.unwrap_or(8192);
    let arch = std::env::consts::ARCH.to_string();

    let join_resp = client
        .join(JoinRequest {
            join_token,
            node_name: name.clone(),
            labels,
            arch: arch.clone(),
            capacity: Some(Capacity { cpus, memory_mib }),
        })
        .await
        .context("Join RPC")?
        .into_inner();

    info!(
        node_id = %join_resp.node_id,
        name = %name,
        cpus,
        memory_mib,
        arch = %arch,
        "joined control plane"
    );

    let interval = Duration::from_secs(args.heartbeat_interval_secs.max(1));
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    // Track runtime ids we created so we can GC on scale-down.
    let mut owned: HashSet<String> = HashSet::new();
    let mut rt_state: HashMap<String, InstanceRuntimeState> = HashMap::new();
    let mut ssh_table = SshServeTable::new(ssh_cfg);

    if let Err(e) = reconcile(
        &mut client,
        &join_resp.node_id,
        &join_resp.node_token,
        runtime.as_ref(),
        &mut owned,
        &mut rt_state,
        &mut ssh_table,
    )
    .await
    {
        warn!(error = %e, "initial reconcile failed");
    }

    loop {
        tokio::select! {
            _ = shutdown_signal() => {
                info!("agent shutting down");
                break;
            }
            _ = ticker.tick() => {
                match client
                    .heartbeat(HeartbeatRequest {
                        node_id: join_resp.node_id.clone(),
                        node_token: join_resp.node_token.clone(),
                        capacity: Some(Capacity { cpus, memory_mib }),
                        status: "Ready".into(),
                    })
                    .await
                {
                    Ok(resp) => {
                        mcc_metrics::record_heartbeat();
                        if !resp.into_inner().ok {
                            warn!("heartbeat returned ok=false");
                        }
                    }
                    Err(e) => warn!(error = %e, "heartbeat failed"),
                }

                match reconcile(
                    &mut client,
                    &join_resp.node_id,
                    &join_resp.node_token,
                    runtime.as_ref(),
                    &mut owned,
                    &mut rt_state,
                    &mut ssh_table,
                )
                .await
                {
                    Ok(()) => mcc_metrics::record_reconcile(true),
                    Err(e) => {
                        mcc_metrics::record_reconcile(false);
                        warn!(error = %e, "reconcile failed");
                    }
                }
            }
        }
    }

    Ok(())
}

/// Pull desired set, ensure sandboxes running, remove extras, report phases.
async fn reconcile(
    client: &mut AgentServiceClient<Channel>,
    node_id: &str,
    node_token: &str,
    runtime: &dyn NodeRuntime,
    owned: &mut HashSet<String>,
    rt_state: &mut HashMap<String, InstanceRuntimeState>,
    ssh_table: &mut SshServeTable,
) -> Result<()> {
    let sync = client
        .sync(SyncRequest {
            node_id: node_id.into(),
            node_token: node_token.into(),
        })
        .await
        .context("Sync RPC")?
        .into_inner();

    let desired = desired_from_sync(&sync.instances)?;
    let desired_ids: HashSet<String> = desired.iter().map(|d| d.runtime_id.clone()).collect();

    // Scale down / GC
    let stale: Vec<String> = owned.difference(&desired_ids).cloned().collect();
    for rid in stale {
        if let Err(e) = runtime.ensure_removed(&rid).await {
            warn!(runtime_id = %rid, error = %e, "ensure_removed failed");
        }
        owned.remove(&rid);
        rt_state.remove(&rid);
    }

    let mut reports: Vec<InstanceStatus> = Vec::new();
    let now = Instant::now();

    for d in &desired {
        let policy = RestartPolicy::parse(&d.spec.restart_policy);
        let state = rt_state.entry(d.runtime_id.clone()).or_default();

        // Backoff gate before ensure_running when we recently recreated.
        if let Some(next) = state.next_restart_ok {
            if now < next {
                let ssh = ssh_table.reconcile(d, false).await;
                reports.push(InstanceStatus {
                    instance_id: d.instance_id.clone(),
                    phase: "Creating".into(),
                    message: format!(
                        "restart backoff {}s",
                        next.saturating_duration_since(now).as_secs()
                    ),
                    runtime_id: d.runtime_id.clone(),
                    ssh: Some(ssh),
                });
                continue;
            }
        }

        match runtime.ensure_running(d).await {
            Ok(mut st) => {
                owned.insert(d.runtime_id.clone());

                // Reset restart counter after sustained Running.
                if st.phase == SandboxPhase::Running {
                    match state.running_since {
                        None => state.running_since = Some(now),
                        Some(since) if now.duration_since(since) >= Duration::from_secs(60) => {
                            state.restart_count = 0;
                            state.next_restart_ok = None;
                        }
                        Some(_) => {}
                    }
                } else {
                    state.running_since = None;
                }

                // Exec health when Running.
                if st.phase == SandboxPhase::Running {
                    if let Some(ref health) = d.spec.health {
                        if health.kind.eq_ignore_ascii_case("exec") && !health.command.is_empty() {
                            let interval =
                                Duration::from_secs(u64::from(health.interval_seconds.max(1)));
                            let due = state
                                .last_health
                                .map(|t| now.duration_since(t) >= interval)
                                .unwrap_or(true);
                            if due {
                                state.last_health = Some(now);
                                match runtime.exec_command(&d.runtime_id, &health.command).await {
                                    Ok(0) => {
                                        // healthy
                                    }
                                    Ok(code) => {
                                        warn!(
                                            runtime_id = %d.runtime_id,
                                            code,
                                            "health exec failed"
                                        );
                                        st = handle_health_failure(
                                            runtime,
                                            d,
                                            policy,
                                            state,
                                            now,
                                            format!("health: exit {code}"),
                                        )
                                        .await;
                                    }
                                    Err(e) => {
                                        warn!(
                                            runtime_id = %d.runtime_id,
                                            error = %e,
                                            "health exec error"
                                        );
                                        st = handle_health_failure(
                                            runtime,
                                            d,
                                            policy,
                                            state,
                                            now,
                                            format!("health: {e:#}"),
                                        )
                                        .await;
                                    }
                                }
                            }
                        }
                    }
                }

                // Track recreates from Failed path (ensure_running did work).
                if matches!(st.phase, SandboxPhase::Creating)
                    && state.restart_count > 0
                    && state.next_restart_ok.is_none()
                {
                    // noop — counts set in handle_health_failure
                }

                let phase = match st.phase {
                    SandboxPhase::Running => "Running",
                    SandboxPhase::Creating => "Creating",
                    SandboxPhase::Failed => "Failed",
                    SandboxPhase::Stopped => "Stopped",
                    SandboxPhase::Pending | SandboxPhase::Unknown => "Creating",
                };
                let running = st.phase == SandboxPhase::Running;
                let ssh = ssh_table.reconcile(d, running).await;
                info!(
                    instance_id = %d.instance_id,
                    runtime_id = %st.runtime_id,
                    phase,
                    "runtime reconciled"
                );
                reports.push(InstanceStatus {
                    instance_id: d.instance_id.clone(),
                    phase: phase.into(),
                    message: st.message.unwrap_or_default(),
                    runtime_id: st.runtime_id,
                    ssh: Some(ssh),
                });
            }
            Err(e) => {
                warn!(
                    instance_id = %d.instance_id,
                    error = %e,
                    error_full = format!("{e:#}"),
                    "ensure_running failed"
                );
                // Schedule backoff for next attempt if policy allows restart.
                if policy != RestartPolicy::Never {
                    state.restart_count = state.restart_count.saturating_add(1);
                    state.next_restart_ok =
                        Some(now + Duration::from_secs(backoff_secs(state.restart_count)));
                    state.running_since = None;
                }
                let ssh = ssh_table.reconcile(d, false).await;
                reports.push(InstanceStatus {
                    instance_id: d.instance_id.clone(),
                    phase: "Failed".into(),
                    message: format!("{e:#}"),
                    runtime_id: d.runtime_id.clone(),
                    ssh: Some(ssh),
                });
            }
        }
    }

    let keep_ids: HashSet<String> = desired.iter().map(|d| d.instance_id.clone()).collect();
    ssh_table.close_missing(&keep_ids);

    if reports.is_empty() {
        return Ok(());
    }

    let ok = client
        .report_status(ReportStatusRequest {
            node_id: node_id.into(),
            node_token: node_token.into(),
            instances: reports,
        })
        .await
        .context("ReportStatus RPC")?
        .into_inner();
    if !ok.ok {
        warn!("ReportStatus returned ok=false");
    }
    Ok(())
}

async fn handle_health_failure(
    runtime: &dyn NodeRuntime,
    d: &mcc_runtime::DesiredSandbox,
    policy: RestartPolicy,
    state: &mut InstanceRuntimeState,
    now: Instant,
    message: String,
) -> mcc_runtime::SandboxStatus {
    use mcc_runtime::SandboxStatus;
    if policy == RestartPolicy::Never {
        return SandboxStatus {
            runtime_id: d.runtime_id.clone(),
            phase: SandboxPhase::Failed,
            message: Some(message),
        };
    }
    state.restart_count = state.restart_count.saturating_add(1);
    state.next_restart_ok = Some(now + Duration::from_secs(backoff_secs(state.restart_count)));
    state.running_since = None;
    if let Err(e) = runtime.ensure_removed(&d.runtime_id).await {
        warn!(runtime_id = %d.runtime_id, error = %e, "remove after health fail");
    }
    match runtime.ensure_running(d).await {
        Ok(st) => st,
        Err(e) => SandboxStatus {
            runtime_id: d.runtime_id.clone(),
            phase: SandboxPhase::Failed,
            message: Some(format!("{message}; recreate: {e:#}")),
        },
    }
}

async fn connect(
    server: &str,
    tls_ca: Option<&PathBuf>,
    insecure: bool,
) -> Result<AgentServiceClient<Channel>> {
    let endpoint = Endpoint::from_shared(server.to_string()).context("parse server endpoint")?;

    let endpoint = if server.starts_with("https://") {
        let mut tls = ClientTlsConfig::new().domain_name(tls_domain(server));
        if let Some(ca_path) = tls_ca {
            let pem = std::fs::read_to_string(ca_path)
                .with_context(|| format!("read TLS CA {}", ca_path.display()))?;
            tls = tls.ca_certificate(Certificate::from_pem(pem));
        } else if insecure {
            bail!(
                "https requires --tls-ca <ca.pem> for MCC lab certs (see server data dir tls/ca.pem). \
                 Or use --server http://HOST:PORT with server --grpc-plain"
            );
        } else {
            bail!("https gRPC requires --tls-ca pointing at the server's tls/ca.pem");
        }
        endpoint.tls_config(tls).context("tls_config")?
    } else {
        endpoint
    };

    let channel = endpoint.connect().await.context("channel connect")?;
    Ok(AgentServiceClient::new(channel))
}

fn tls_domain(server: &str) -> String {
    let rest = server
        .strip_prefix("https://")
        .or_else(|| server.strip_prefix("http://"))
        .unwrap_or(server);
    rest.split(':')
        .next()
        .unwrap_or("localhost")
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_string()
}

fn parse_labels(items: &[String]) -> Result<HashMap<String, String>> {
    let mut map = HashMap::new();
    for item in items {
        let (k, v) = item
            .split_once('=')
            .with_context(|| format!("label must be KEY=VALUE, got {item}"))?;
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
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    {
        let mut sig = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("sigterm");
        tokio::select! {
            _ = ctrl_c => {}
            _ = sig.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        ctrl_c.await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn dry_run_ok() {
        let args = AgentArgs {
            server: Some("https://127.0.0.1:7444".into()),
            token: Some("test".into()),
            name: Some("test-node".into()),
            tls_ca: None,
            insecure: false,
            heartbeat_interval_secs: 10,
            cpus: None,
            memory_mib: None,
            labels: vec![],
            ssh_backend: "auto".into(),
            msb_bin: std::path::PathBuf::from("msb"),
            dry_run: true,
        };
        run(args).await.expect("dry_run should succeed");
    }

    #[test]
    fn parse_labels_ok() {
        let m = parse_labels(&["role=worker".into(), "zone=a".into()]).unwrap();
        assert_eq!(m.get("role").unwrap(), "worker");
        assert_eq!(m.get("zone").unwrap(), "a");
    }

    #[test]
    fn tls_domain_extract() {
        assert_eq!(tls_domain("https://127.0.0.1:7444"), "127.0.0.1");
        assert_eq!(tls_domain("http://localhost:7444"), "localhost");
    }
}
