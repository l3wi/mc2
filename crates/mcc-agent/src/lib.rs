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
use mcc_runtime::{default_runtime, desired_from_sync, NodeRuntime, SandboxPhase};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tonic::transport::{Certificate, Channel, ClientTlsConfig, Endpoint};
use tracing::{info, warn};

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

    info!(
        node = %name,
        server = ?args.server,
        has_token = args.token.is_some(),
        runtime = "microsandbox-sdk",
        api = mcc_api::API_VERSION,
        "MicroCommandControl agent starting"
    );

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

    if let Err(e) = reconcile(
        &mut client,
        &join_resp.node_id,
        &join_resp.node_token,
        runtime.as_ref(),
        &mut owned,
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
                        if !resp.into_inner().ok {
                            warn!("heartbeat returned ok=false");
                        }
                    }
                    Err(e) => warn!(error = %e, "heartbeat failed"),
                }

                if let Err(e) = reconcile(
                    &mut client,
                    &join_resp.node_id,
                    &join_resp.node_token,
                    runtime.as_ref(),
                    &mut owned,
                )
                .await
                {
                    warn!(error = %e, "reconcile failed");
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
    }

    let mut reports: Vec<InstanceStatus> = Vec::new();

    for d in &desired {
        match runtime.ensure_running(d).await {
            Ok(st) => {
                owned.insert(d.runtime_id.clone());
                let phase = match st.phase {
                    SandboxPhase::Running => "Running",
                    SandboxPhase::Creating => "Creating",
                    SandboxPhase::Failed => "Failed",
                    SandboxPhase::Stopped => "Stopped",
                    SandboxPhase::Pending | SandboxPhase::Unknown => "Creating",
                };
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
                });
            }
            Err(e) => {
                warn!(
                    instance_id = %d.instance_id,
                    error = %e,
                    "ensure_running failed"
                );
                reports.push(InstanceStatus {
                    instance_id: d.instance_id.clone(),
                    phase: "Failed".into(),
                    message: e.to_string(),
                    runtime_id: d.runtime_id.clone(),
                });
            }
        }
    }

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
