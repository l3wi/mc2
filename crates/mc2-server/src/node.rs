//! Local node reconcile loop: drive the embedded microsandbox runtime from
//! the store's desired set, and write observed phases back to the store.
//!
//! Replaces the former `mc2-agent` gRPC client/server pair with direct calls.

use crate::desired::build_desired_set;
use crate::fabric_serve::FabricTable;
use crate::ingress_files::{warn_ingress_dir_unset, IngressFileWriter, SelfIngressRoute};
use crate::ssh_serve::SshServeTable;
use anyhow::{Context, Result};
use mc2_api::HealthcheckSpec;
use mc2_runtime::{
    backoff_secs, desired_recreate_hash, FabricObserved, InstanceReport, NodeRuntime,
    RestartPolicy, SandboxPhase,
};
use mc2_store::{NodeHeartbeat, SecretsKey, Store};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::{info, warn};

/// Per-sandbox restart / health bookkeeping on the node.
#[derive(Debug, Default)]
struct InstanceRuntimeState {
    spec_hash: Option<String>,
    running_since: Option<Instant>,
    restart_count: u32,
    next_restart_ok: Option<Instant>,
    last_health: Option<Instant>,
    /// Healthcheck passed at least once since the last (re)create.
    health_ok: bool,
    /// Consecutive health-probe failures (reset on success).
    health_failures: u32,
}

/// Aggregate liveness of one service within a stack, used to gate `depends_on`.
#[derive(Debug, Clone, Default)]
struct ServiceLive {
    running: bool,
    healthy: bool,
}

/// Configuration for the local node loop.
#[derive(Debug, Clone)]
pub struct NodeConfig {
    pub name: String,
    pub labels_json: String,
    pub cpus: u32,
    pub memory_mib: u64,
    pub reconcile_interval: Duration,
    pub ingress_config_dir: Option<PathBuf>,
    /// Public hostname to publish the control plane through the ingress catalog.
    pub public_hostname: Option<String>,
    /// Traefik cert resolver for the control-plane TLS route.
    pub public_tls_cert_resolver: String,
    /// REST listener port (backend for the control-plane self-route).
    pub rest_port: u16,
}

/// Register/refresh the local node row; returns its stable node_id.
pub async fn ensure_local_node(store: Arc<dyn Store>, cfg: &NodeConfig) -> Result<String> {
    let rec = store
        .upsert_local_node(mc2_store::NodeJoin {
            name: cfg.name.clone(),
            labels_json: cfg.labels_json.clone(),
            arch: std::env::consts::ARCH.to_string(),
            cpus: cfg.cpus,
            memory_mib: cfg.memory_mib,
        })
        .await
        .context("upsert local node")?;
    Ok(rec.id)
}

/// The reconcile loop. Runs until `shutdown` fires.
pub async fn run(
    store: Arc<dyn Store>,
    secrets_key: Arc<SecretsKey>,
    runtime: Arc<dyn NodeRuntime>,
    cfg: NodeConfig,
    mut shutdown: tokio::sync::broadcast::Receiver<()>,
) -> Result<()> {
    let node_id = ensure_local_node(store.clone(), &cfg).await?;

    let self_route = cfg.public_hostname.as_ref().map(|host| SelfIngressRoute {
        host: host.clone(),
        port: cfg.rest_port,
        tls_cert_resolver: cfg.public_tls_cert_resolver.clone(),
    });

    info!(
        node = %cfg.name,
        node_id = %node_id,
        runtime = "microsandbox-sdk",
        ssh = "sdk",
        ingress_dir = ?cfg.ingress_config_dir,
        api = mc2_api::API_VERSION,
        "local node starting"
    );

    let interval = cfg.reconcile_interval.max(Duration::from_secs(1));
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    // Track runtime ids we created so we can GC on scale-down.
    let mut owned: HashSet<String> = HashSet::new();
    let mut rt_state: HashMap<String, InstanceRuntimeState> = HashMap::new();
    let mut ssh_table = SshServeTable::new();
    let mut fabric_table = FabricTable::new();
    let mut ingress_writer = cfg
        .ingress_config_dir
        .clone()
        .map(|d| IngressFileWriter::new(d, cfg.name.clone()));

    loop {
        tokio::select! {
            _ = shutdown.recv() => {
                info!("local node shutting down");
                break;
            }
            _ = ticker.tick() => {
                // Heartbeat: keep the node row Ready.
                let _ = store
                    .touch_node(
                        &node_id,
                        NodeHeartbeat {
                            cpus: cfg.cpus,
                            memory_mib: cfg.memory_mib,
                            status: "Ready".into(),
                        },
                    )
                    .await;

                match reconcile(
                    store.clone(),
                    secrets_key.as_ref(),
                    &node_id,
                    runtime.as_ref(),
                    &mut owned,
                    &mut rt_state,
                    &mut ssh_table,
                    &mut fabric_table,
                    ingress_writer.as_mut(),
                    self_route.as_ref(),
                )
                .await
                {
                    Ok(()) => mc2_metrics::record_reconcile(true),
                    Err(e) => {
                        mc2_metrics::record_reconcile(false);
                        warn!(error = %e, "reconcile failed");
                    }
                }
            }
        }
    }

    Ok(())
}

/// Build desired set, ensure sandboxes running, remove extras, persist phases.
#[allow(clippy::too_many_arguments)]
async fn reconcile(
    store: Arc<dyn Store>,
    secrets_key: &SecretsKey,
    node_id: &str,
    runtime: &dyn NodeRuntime,
    owned: &mut HashSet<String>,
    rt_state: &mut HashMap<String, InstanceRuntimeState>,
    ssh_table: &mut SshServeTable,
    fabric_table: &mut FabricTable,
    ingress_writer: Option<&mut IngressFileWriter>,
    self_route: Option<&SelfIngressRoute>,
) -> Result<()> {
    let (mut desired, ingress_routes) =
        build_desired_set(store.clone(), secrets_key, node_id).await?;

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

    let mut reports: Vec<InstanceReport> = Vec::new();
    let now = Instant::now();

    // Providers first so expose publish index is populated before client edges.
    desired.sort_by(|a, b| {
        let ae = a.fabric.exposes.is_empty();
        let be = b.fabric.exposes.is_empty();
        ae.cmp(&be) // false (has expose) sorts before true
            .then_with(|| a.service.cmp(&b.service))
    });

    // Seed per-(stack, service) liveness from the store; updated live below as
    // this cycle reports phases / health, so in-cycle dependencies start fast.
    let mut live: HashMap<(String, String), ServiceLive> = HashMap::new();
    for inst in store
        .list_instances()
        .await
        .context("list instances for deps")?
    {
        let key = (inst.stack, inst.service);
        let entry = live.entry(key).or_default();
        if inst.phase == "Running" {
            entry.running = true;
            entry.healthy = entry.healthy || inst.healthy;
        }
    }

    for d in &mut desired {
        let policy = RestartPolicy::parse(&d.spec.restart);
        let state = rt_state.entry(d.runtime_id.clone()).or_default();

        // Compose `depends_on` startup gate: skip ensure_running until every
        // dependency is Running (service_started) or Running+healthy
        // (service_healthy). Deps live in the same stack.
        if !d.spec.depends_on.is_empty() {
            if let Some(waiting) = waiting_deps(d, &live) {
                reports.push(InstanceReport {
                    instance_id: d.instance_id.clone(),
                    phase: "Pending".into(),
                    message: format!("depends_on: waiting for {waiting}"),
                    runtime_id: d.runtime_id.clone(),
                    ssh: None,
                    fabric: None,
                });
                continue;
            }
        }

        // Backoff gate before ensure_running when we recently recreated.
        if let Some(next) = state.next_restart_ok {
            if now < next {
                let ssh = ssh_table.reconcile(d, false).await;
                let fabric = fabric_table.reconcile_not_running(d).await;
                reports.push(InstanceReport {
                    instance_id: d.instance_id.clone(),
                    phase: "Creating".into(),
                    message: format!(
                        "restart backoff {}s",
                        next.saturating_duration_since(now).as_secs()
                    ),
                    runtime_id: d.runtime_id.clone(),
                    ssh: Some(ssh),
                    fabric: Some(fabric),
                });
                apply_live(&mut live, d, "Creating", false);
                continue;
            }
        }

        if let Err(e) = fabric_table.prepare_exposes(d).await {
            warn!(instance_id = %d.instance_id, error = %e, "fabric prepare_exposes failed");
            let ssh = ssh_table.reconcile(d, false).await;
            let fabric = fabric_table.reconcile_not_running(d).await;
            reports.push(InstanceReport {
                instance_id: d.instance_id.clone(),
                phase: "Failed".into(),
                message: e,
                runtime_id: d.runtime_id.clone(),
                ssh: Some(ssh),
                fabric: Some(fabric),
            });
            apply_live(&mut live, d, "Failed", false);
            continue;
        }

        // Recreate when create-time config changes (image, command, ports, fabric…).
        let want_hash = desired_recreate_hash(d);
        let mut force_recreate = false;
        if let Some(prev) = state.spec_hash.as_ref() {
            if prev != &want_hash {
                force_recreate = true;
                info!(
                    runtime_id = %d.runtime_id,
                    "desired spec changed; removing sandbox for recreate"
                );
            }
        }
        // Expose host ports are only bound at msb create. If we inherited a
        // Running sandbox without live publish (server restart / orphan), recreate.
        if !force_recreate {
            if let Some(ports) = fabric_table.expose_host_ports(&d.instance_id) {
                if !ports.is_empty() && !host_ports_accepting(&ports).await {
                    force_recreate = true;
                    info!(
                        runtime_id = %d.runtime_id,
                        ?ports,
                        "fabric expose host ports not live; recreating sandbox"
                    );
                }
            }
        }
        if force_recreate {
            fabric_table.drop_instance(&d.instance_id).await;
            // Re-prepare after drop so publish_index stays correct for this cycle.
            if let Err(e) = fabric_table.prepare_exposes(d).await {
                warn!(instance_id = %d.instance_id, error = %e, "fabric re-prepare after drop");
            }
            if let Err(e) = runtime.ensure_removed(&d.runtime_id).await {
                warn!(runtime_id = %d.runtime_id, error = %e, "remove before recreate");
            }
            owned.remove(&d.runtime_id);
            state.spec_hash = None;
            state.running_since = None;
            state.health_ok = false;
            state.health_failures = 0;
            state.last_health = None;
        }

        match runtime.ensure_running(d).await {
            Ok(mut st) => {
                owned.insert(d.runtime_id.clone());
                state.spec_hash = Some(want_hash);

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

                // Exec health when Running (compose `healthcheck` semantics:
                // interval / timeout / retries / start_period / disable).
                if st.phase == SandboxPhase::Running {
                    if let Some(ref health) = d.spec.healthcheck {
                        if health_active(health) {
                            let interval =
                                Duration::from_secs(u64::from(health.interval_seconds.max(1)));
                            let due = state
                                .last_health
                                .map(|t| now.duration_since(t) >= interval)
                                .unwrap_or(true);
                            if due {
                                state.last_health = Some(now);
                                let in_start_period = health.start_period_seconds > 0
                                    && state.running_since.is_some_and(|since| {
                                        now.duration_since(since)
                                            < Duration::from_secs(u64::from(
                                                health.start_period_seconds,
                                            ))
                                    });
                                match run_health_probe(runtime, &d.runtime_id, health).await {
                                    Ok(0) => {
                                        state.health_failures = 0;
                                        state.health_ok = true;
                                    }
                                    Ok(code) => {
                                        state.health_failures =
                                            state.health_failures.saturating_add(1);
                                        state.health_ok = false;
                                        if in_start_period {
                                            warn!(
                                                runtime_id = %d.runtime_id,
                                                code,
                                                "health probe failed (within start_period; ignored)"
                                            );
                                        } else if state.health_failures >= health.retries.max(1) {
                                            warn!(
                                                runtime_id = %d.runtime_id,
                                                code,
                                                failures = state.health_failures,
                                                "health exec failed past retries"
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
                                        } else {
                                            warn!(
                                                runtime_id = %d.runtime_id,
                                                code,
                                                failures = state.health_failures,
                                                "health exec failed"
                                            );
                                        }
                                    }
                                    Err(e) => {
                                        state.health_failures =
                                            state.health_failures.saturating_add(1);
                                        state.health_ok = false;
                                        if in_start_period {
                                            warn!(
                                                runtime_id = %d.runtime_id,
                                                error = %e,
                                                "health probe error (within start_period; ignored)"
                                            );
                                        } else if state.health_failures >= health.retries.max(1) {
                                            warn!(
                                                runtime_id = %d.runtime_id,
                                                error = %e,
                                                failures = state.health_failures,
                                                "health exec error past retries"
                                            );
                                            st = handle_health_failure(
                                                runtime,
                                                d,
                                                policy,
                                                state,
                                                now,
                                                format!("health: {e}"),
                                            )
                                            .await;
                                        } else {
                                            warn!(
                                                runtime_id = %d.runtime_id,
                                                error = %e,
                                                failures = state.health_failures,
                                                "health exec error"
                                            );
                                        }
                                    }
                                }
                            }
                        }
                    }
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
                let fabric = if running {
                    fabric_table.reconcile_running(d).await
                } else {
                    fabric_table.reconcile_not_running(d).await
                };
                let mut message = st.message.unwrap_or_default();
                if !fabric.message.is_empty() {
                    if !message.is_empty() {
                        message.push_str("; ");
                    }
                    message.push_str(&fabric.message);
                }
                for e in &fabric.edges {
                    if e.phase == "Failed" {
                        if !message.is_empty() {
                            message.push_str("; ");
                        }
                        message.push_str(&e.message);
                    }
                }
                info!(
                    instance_id = %d.instance_id,
                    runtime_id = %st.runtime_id,
                    phase,
                    "runtime reconciled"
                );
                reports.push(InstanceReport {
                    instance_id: d.instance_id.clone(),
                    phase: phase.into(),
                    message,
                    runtime_id: st.runtime_id,
                    ssh: Some(ssh),
                    fabric: Some(fabric),
                });
                apply_live(&mut live, d, phase, state.health_ok);
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
                let fabric = fabric_table.reconcile_not_running(d).await;
                reports.push(InstanceReport {
                    instance_id: d.instance_id.clone(),
                    phase: "Failed".into(),
                    message: format!("{e:#}"),
                    runtime_id: d.runtime_id.clone(),
                    ssh: Some(ssh),
                    fabric: Some(fabric),
                });
                apply_live(&mut live, d, "Failed", false);
            }
        }
    }

    let keep_ids: HashSet<String> = desired.iter().map(|d| d.instance_id.clone()).collect();
    ssh_table.close_missing(&keep_ids).await;
    // Rebuild shared fabric splices from the current desired set + observed phases.
    fabric_table
        .reconcile_splices(desired.as_slice(), &reports)
        .await;
    fabric_table.close_missing(&keep_ids).await;

    // Ingress file catalog (same-node BYO Traefik).
    let mut phases: HashMap<String, String> = HashMap::new();
    for r in &reports {
        phases.insert(r.instance_id.clone(), r.phase.clone());
    }
    if let Some(writer) = ingress_writer {
        match writer.reconcile(&ingress_routes, &phases, self_route).await {
            Ok(st) if st.wrote => {
                info!(
                    ready = st.ready,
                    pending = st.pending,
                    "ingress catalog updated"
                );
            }
            Ok(_) => {}
            Err(e) => warn!(error = %e, "ingress catalog render failed"),
        }
    } else {
        warn_ingress_dir_unset(ingress_routes.len());
    }

    // Persist observed phases directly to the store.
    for r in &reports {
        let _ = store
            .update_instance_status(
                &r.instance_id,
                &r.phase,
                if r.runtime_id.is_empty() {
                    None
                } else {
                    Some(r.runtime_id.as_str())
                },
                if r.message.is_empty() {
                    None
                } else {
                    Some(r.message.as_str())
                },
            )
            .await;

        // Healthcheck signal (drives depends_on: service_healthy).
        if let Some(hstate) = rt_state.get(&r.runtime_id) {
            let _ = store
                .update_instance_health(&r.instance_id, hstate.health_ok)
                .await;
        }

        if let Some(ref ssh) = r.ssh {
            let _ = store
                .update_instance_ssh_observed(
                    &r.instance_id,
                    &ssh.phase,
                    if ssh.bind.is_empty() {
                        None
                    } else {
                        Some(ssh.bind.as_str())
                    },
                    if ssh.port == 0 { None } else { Some(ssh.port) },
                    if ssh.message.is_empty() {
                        None
                    } else {
                        Some(ssh.message.as_str())
                    },
                )
                .await;
        }

        if let Some(ref fabric) = r.fabric {
            let phase = fabric_summary_phase(fabric);
            let json = fabric_observed_json(fabric);
            let msg = if fabric.message.is_empty() {
                None
            } else {
                Some(fabric.message.as_str())
            };
            let _ = store
                .update_instance_fabric_observed(&r.instance_id, &phase, &json, msg)
                .await;
        }
    }

    Ok(())
}

async fn handle_health_failure(
    runtime: &dyn NodeRuntime,
    d: &mc2_runtime::DesiredSandbox,
    policy: RestartPolicy,
    state: &mut InstanceRuntimeState,
    now: Instant,
    message: String,
) -> mc2_runtime::SandboxStatus {
    use mc2_runtime::SandboxStatus;
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

/// True when a healthcheck should actually run (not disabled, has a test).
fn health_active(h: &HealthcheckSpec) -> bool {
    !h.disable && h.test.as_ref().is_some_and(|t| !t.is_empty())
}

/// Run one health probe, honoring the compose `timeout` (0 = no timeout).
/// Returns the guest exit code, or an Err (probe error / timeout).
async fn run_health_probe(
    runtime: &dyn NodeRuntime,
    runtime_id: &str,
    health: &HealthcheckSpec,
) -> anyhow::Result<i32> {
    let command = health.test.as_ref().expect("active healthcheck has a test");
    let fut = runtime.exec_command(runtime_id, command);
    if health.timeout_seconds == 0 {
        return fut.await;
    }
    match tokio::time::timeout(Duration::from_secs(u64::from(health.timeout_seconds)), fut).await {
        Ok(res) => res,
        Err(_) => anyhow::bail!("health probe timed out"),
    }
}

/// Compose `depends_on` gate: `Some(...)` lists the unsatisfied dependencies
/// (and their conditions); `None` means all are satisfied and the instance may start.
fn waiting_deps(
    d: &mc2_runtime::DesiredSandbox,
    live: &HashMap<(String, String), ServiceLive>,
) -> Option<String> {
    let mut waiting: Vec<String> = Vec::new();
    for (dep, spec) in &d.spec.depends_on {
        let key = (d.stack.clone(), dep.clone());
        let state = live.get(&key);
        let cond = spec.condition.trim().to_ascii_lowercase();
        let satisfied = match cond.as_str() {
            "service_healthy" => state.is_some_and(|s| s.running && s.healthy),
            _ => state.is_some_and(|s| s.running),
        };
        if !satisfied {
            waiting.push(format!("{dep} ({cond})"));
        }
    }
    if waiting.is_empty() {
        None
    } else {
        Some(waiting.join(", "))
    }
}

/// Reflect an instance's observed phase into the service-liveness map.
fn apply_live(
    live: &mut HashMap<(String, String), ServiceLive>,
    d: &mc2_runtime::DesiredSandbox,
    phase: &str,
    healthy: bool,
) {
    let entry = live
        .entry((d.stack.clone(), d.service.clone()))
        .or_default();
    entry.running = phase == "Running";
    entry.healthy = healthy;
}

/// True if every host port accepts a TCP connect (msb publish live).
async fn host_ports_accepting(ports: &[u16]) -> bool {
    use tokio::net::TcpStream;
    use tokio::time::timeout;
    for &p in ports {
        let ok = timeout(
            Duration::from_millis(200),
            TcpStream::connect(std::net::SocketAddr::from(([127, 0, 0, 1], p))),
        )
        .await;
        match ok {
            Ok(Ok(_stream)) => {}
            _ => return false,
        }
    }
    true
}

fn fabric_observed_json(f: &FabricObserved) -> String {
    serde_json::to_string(f).unwrap_or_else(|_| "{}".into())
}

fn fabric_summary_phase(f: &FabricObserved) -> String {
    let mut has_ready = false;
    let mut has_failed = false;
    let mut has_pending = false;
    for e in &f.exposes {
        match e.phase.as_str() {
            "Ready" => has_ready = true,
            "Failed" => has_failed = true,
            _ => has_pending = true,
        }
    }
    for e in &f.edges {
        match e.phase.as_str() {
            "Ready" => has_ready = true,
            "Failed" => has_failed = true,
            _ => has_pending = true,
        }
    }
    if f.exposes.is_empty() && f.edges.is_empty() {
        return "Pending".into();
    }
    match (has_failed, has_pending, has_ready) {
        (true, _, true) => "Mixed".into(),
        (true, _, false) => "Failed".into(),
        (false, true, _) => "Pending".into(),
        (false, false, true) => "Ready".into(),
        _ => "Pending".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn sandbox(
        depends_on: BTreeMap<String, mc2_api::DependsOnSpec>,
    ) -> mc2_runtime::DesiredSandbox {
        mc2_runtime::DesiredSandbox {
            instance_id: "i1".into(),
            stack: "demo".into(),
            service: "web".into(),
            ordinal: 0,
            runtime_id: "demo-web-0".into(),
            spec: mc2_api::ServiceSpec {
                image: "alpine".into(),
                scale: 1,
                cpus: 1.0,
                mem_limit_mib: 512,
                ports: vec![],
                network: Default::default(),
                env: BTreeMap::new(),
                secrets: vec![],
                volumes: vec![],
                restart: "no".into(),
                healthcheck: None,
                labels: BTreeMap::new(),
                command: None,
                node_name: None,
                node_selector: BTreeMap::new(),
                ssh: None,
                expose: vec![],
                networks: vec![],
                depends_on,
            },
            secrets: vec![],
            ssh: Default::default(),
            fabric: Default::default(),
        }
    }

    fn dep(name: &str, condition: &str) -> BTreeMap<String, mc2_api::DependsOnSpec> {
        BTreeMap::from([(
            name.to_string(),
            mc2_api::DependsOnSpec {
                condition: condition.into(),
            },
        )])
    }

    fn live(
        stack: &str,
        service: &str,
        running: bool,
        healthy: bool,
    ) -> HashMap<(String, String), ServiceLive> {
        let mut m = HashMap::new();
        m.insert(
            (stack.to_string(), service.to_string()),
            ServiceLive { running, healthy },
        );
        m
    }

    #[test]
    fn service_started_gates_on_running() {
        let d = sandbox(dep("db", "service_started"));
        assert!(waiting_deps(&d, &live("demo", "db", false, false)).is_some());
        assert!(waiting_deps(&d, &live("demo", "db", true, false)).is_none());
    }

    #[test]
    fn service_healthy_requires_running_and_healthy() {
        let d = sandbox(dep("db", "service_healthy"));
        assert!(waiting_deps(&d, &live("demo", "db", true, false)).is_some());
        assert!(waiting_deps(&d, &live("demo", "db", false, true)).is_some());
        assert!(waiting_deps(&d, &live("demo", "db", true, true)).is_none());
    }

    #[test]
    fn waiting_message_lists_unsatisfied_deps() {
        let mut deps = dep("db", "service_healthy");
        deps.insert("cache".into(), mc2_api::DependsOnSpec::service_started());
        let d = sandbox(deps);
        let msg = waiting_deps(&d, &live("demo", "db", true, false)).unwrap();
        assert!(msg.contains("db (service_healthy)"), "{msg}");
        assert!(msg.contains("cache (service_started)"), "{msg}");
    }

    #[test]
    fn apply_live_tracks_running_and_healthy() {
        let mut m = HashMap::new();
        let d = sandbox(BTreeMap::new());
        apply_live(&mut m, &d, "Running", true);
        assert!(m[&("demo".into(), "web".into())].running);
        assert!(m[&("demo".into(), "web".into())].healthy);
        apply_live(&mut m, &d, "Failed", false);
        assert!(!m[&("demo".into(), "web".into())].running);
        assert!(!m[&("demo".into(), "web".into())].healthy);
    }
}
