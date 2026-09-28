//! Per-instance lifecycle inside one reconcile pass (F1 split).
//!
//! Everything that decides what happens to a single sandbox lives here:
//! `depends_on` gating, restart backoff, the fail-closed network check, the
//! recreate decision (B3), `ensure_running`, the health probe (B6b) and the
//! ssh/network observation. Ownership and the applied config hash are only
//! touched on confirmed success (B2/B3).

use super::health::run_healthcheck;
use super::state::{NodeRuntimeState, PassErrors, ServiceLive};
use anyhow::Result;
use mc2_runtime::{
    desired_recreate_hash, DesiredSandbox, InstanceReport, NodeRuntime, RestartPolicy, SandboxPhase,
};
use mc2_store::{InstancePhase, Store};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::{info, warn};

impl NodeRuntimeState {
    /// Reconcile one instance's sandbox and return the report for it.
    ///
    /// `Err` only for a failure that leaves the instance unreported this pass
    /// (a store read failure aborting it); the caller records it and keeps
    /// reconciling the other instances.
    pub(super) async fn reconcile_instance(
        &mut self,
        store: &Arc<dyn Store>,
        runtime: &Arc<dyn NodeRuntime>,
        d: &mut DesiredSandbox,
        now: Instant,
        live: &mut HashMap<(String, String), ServiceLive>,
        errors: &mut PassErrors,
    ) -> Result<InstanceReport> {
        let policy = RestartPolicy::parse(&d.spec.restart);
        let state = self.rt_state.entry(d.runtime_id.clone()).or_default();

        // Compose `depends_on` startup gate: skip ensure_running until every
        // dependency is Running (service_started) or Running+healthy
        // (service_healthy). Deps live in the same stack.
        if !d.spec.depends_on.is_empty() {
            if let Some(waiting) = waiting_deps(d, live) {
                return Ok(InstanceReport {
                    instance_id: d.instance_id.clone(),
                    phase: InstancePhase::Pending.as_str().into(),
                    message: format!("depends_on: waiting for {waiting}"),
                    runtime_id: d.runtime_id.clone(),
                    ssh: None,
                    network: None,
                });
            }
        }

        // Backoff gate before ensure_running when we recently recreated.
        if let Some(next) = state.next_restart_ok {
            if now < next {
                let ssh = self.ssh_table.reconcile(d, false).await;
                let network = self.network_table.reconcile_not_running(d).await;
                let report = InstanceReport {
                    instance_id: d.instance_id.clone(),
                    phase: InstancePhase::Creating.as_str().into(),
                    message: format!(
                        "restart backoff {}s",
                        next.saturating_duration_since(now).as_secs()
                    ),
                    runtime_id: d.runtime_id.clone(),
                    ssh: Some(ssh),
                    network: Some(network),
                };
                apply_live(
                    live,
                    &d.stack,
                    &d.service,
                    InstancePhase::Creating.as_str(),
                    false,
                );
                return Ok(report);
            }
        }

        // Fail closed: a create-time `Host:tcp:P` egress rule cannot be narrowed
        // after the fact, so a VM whose allowed port has no listener we bound
        // (bind failed, or the owning service isn't scheduled here) could reach
        // a foreign process on `P`. Never create it; name the port and cause.
        if let Some((port, cause)) = self
            .network_table
            .listener_gaps(&d.network.allows)
            .into_iter()
            .next()
        {
            let msg = format!(
                "network: no listener on 127.0.0.1:{port} ({cause}); not starting — \
                 the egress rule is create-time, so fix the port conflict and re-apply"
            );
            warn!(instance_id = %d.instance_id, %msg, "network fail-closed");
            let ssh = self.ssh_table.reconcile(d, false).await;
            let network = self.network_table.reconcile_not_running(d).await;
            let report = InstanceReport {
                instance_id: d.instance_id.clone(),
                phase: InstancePhase::Failed.as_str().into(),
                message: msg,
                runtime_id: d.runtime_id.clone(),
                ssh: Some(ssh),
                network: Some(network),
            };
            apply_live(
                live,
                &d.stack,
                &d.service,
                InstancePhase::Failed.as_str(),
                false,
            );
            return Ok(report);
        }

        if let Err(e) = self.network_table.prepare_exposes(d).await {
            warn!(instance_id = %d.instance_id, error = %e, "network prepare_exposes failed");
            let ssh = self.ssh_table.reconcile(d, false).await;
            let network = self.network_table.reconcile_not_running(d).await;
            let report = InstanceReport {
                instance_id: d.instance_id.clone(),
                phase: InstancePhase::Failed.as_str().into(),
                message: e,
                runtime_id: d.runtime_id.clone(),
                ssh: Some(ssh),
                network: Some(network),
            };
            apply_live(
                live,
                &d.stack,
                &d.service,
                InstancePhase::Failed.as_str(),
                false,
            );
            return Ok(report);
        }

        // Recreate when create-time config changes (image, command, ports, network…).
        let want_hash = desired_recreate_hash(d);
        let mut force_recreate = recreate_decision(
            store,
            d,
            state.spec_hash.as_deref(),
            self.owned.contains(&d.runtime_id),
            &want_hash,
        )
        .await?;
        if force_recreate {
            info!(
                runtime_id = %d.runtime_id,
                "sandbox does not match the applied config; removing it for recreate"
            );
        }
        // Expose host ports are only bound at msb create. If we inherited a
        // Running sandbox without live publish (server restart / orphan), recreate.
        if !force_recreate {
            if let Some(ports) = self.network_table.expose_host_ports(&d.instance_id) {
                if !ports.is_empty() && !host_ports_accepting(&ports).await {
                    force_recreate = true;
                    info!(
                        runtime_id = %d.runtime_id,
                        ?ports,
                        "network expose host ports not live; recreating sandbox"
                    );
                }
            }
        }
        if force_recreate {
            self.network_table.drop_instance(&d.instance_id).await;
            // Re-prepare after drop so publish_index stays correct for this cycle.
            if let Err(e) = self.network_table.prepare_exposes(d).await {
                warn!(instance_id = %d.instance_id, error = %e, "network re-prepare after drop");
            }
            if let Err(e) = runtime.ensure_removed(&d.runtime_id).await {
                // B3: the old VM is still there. Keep the old applied hash — it
                // describes the VM that is actually running — and abort this
                // instance's pass so the recreate is retried, rather than
                // recording the new config for the old sandbox.
                warn!(
                    runtime_id = %d.runtime_id,
                    error = %e,
                    "remove before recreate failed; keeping the old applied hash"
                );
                let ssh = self.ssh_table.reconcile(d, false).await;
                let network = self.network_table.reconcile_not_running(d).await;
                let report = InstanceReport {
                    instance_id: d.instance_id.clone(),
                    phase: InstancePhase::Failed.as_str().into(),
                    message: format!(
                        "recreate: removing the previous sandbox failed: {e:#} — retrying next pass"
                    ),
                    runtime_id: d.runtime_id.clone(),
                    ssh: Some(ssh),
                    network: Some(network),
                };
                apply_live(
                    live,
                    &d.stack,
                    &d.service,
                    InstancePhase::Failed.as_str(),
                    false,
                );
                return Ok(report);
            }
            self.owned.remove(&d.runtime_id);
            state.spec_hash = None;
            state.running_since = None;
            // New VM generation: health counters restart and any probe result
            // from the removed sandbox is dropped (B6b).
            state.reset_health();
        }

        match runtime.ensure_running(d).await {
            Ok(mut st) => {
                self.owned.insert(d.runtime_id.clone());
                state.spec_hash = Some(want_hash.clone());
                // B3: the sandbox now holds this config — record it only after
                // the successful ensure_running, and report a failure to persist
                // (the pass fails; the in-memory value keeps this process from
                // recreating, a restart would recreate — the safe direction).
                if let Err(e) = store
                    .set_instance_applied_hash(&d.instance_id, Some(&want_hash))
                    .await
                {
                    warn!(
                        instance_id = %d.instance_id,
                        error = %e,
                        "persist applied hash failed"
                    );
                    errors.record(anyhow::Error::new(e).context(format!(
                        "persist applied hash for instance {}",
                        d.instance_id
                    )));
                }

                // Reset restart counter after sustained Running.
                if st.phase == SandboxPhase::Running {
                    match state.running_since {
                        None => {
                            // First sighting of this VM generation: start the
                            // start_period clock and health counters fresh.
                            state.running_since = Some(now);
                            state.reset_health();
                        }
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
                // interval / timeout / retries / start_period / disable). The
                // probe runs in its own task (B6b): this pass only starts one
                // when due and reads the previous result, so a hung guest
                // command cannot stall the whole node.
                if st.phase == SandboxPhase::Running {
                    if let Some(new_st) = run_healthcheck(runtime, d, policy, state, now).await {
                        st = new_st;
                    }
                }

                let phase = match st.phase {
                    SandboxPhase::Running => InstancePhase::Running.as_str(),
                    SandboxPhase::Creating => InstancePhase::Creating.as_str(),
                    SandboxPhase::Failed => InstancePhase::Failed.as_str(),
                    SandboxPhase::Stopped => InstancePhase::Stopped.as_str(),
                    SandboxPhase::Pending | SandboxPhase::Unknown => {
                        InstancePhase::Creating.as_str()
                    }
                };
                let running = st.phase == SandboxPhase::Running;
                let ssh = self.ssh_table.reconcile(d, running).await;
                let network = if running {
                    self.network_table.reconcile_running(d).await
                } else {
                    self.network_table.reconcile_not_running(d).await
                };
                let mut message = st.message.unwrap_or_default();
                if !network.message.is_empty() {
                    if !message.is_empty() {
                        message.push_str("; ");
                    }
                    message.push_str(&network.message);
                }
                for e in &network.edges {
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
                let report = InstanceReport {
                    instance_id: d.instance_id.clone(),
                    phase: phase.into(),
                    message,
                    runtime_id: st.runtime_id,
                    ssh: Some(ssh),
                    network: Some(network),
                };
                apply_live(live, &d.stack, &d.service, phase, state.health_ok);
                Ok(report)
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
                    state.next_restart_ok = Some(
                        now + Duration::from_secs(mc2_runtime::backoff_secs(state.restart_count)),
                    );
                    state.running_since = None;
                }
                let ssh = self.ssh_table.reconcile(d, false).await;
                let network = self.network_table.reconcile_not_running(d).await;
                let report = InstanceReport {
                    instance_id: d.instance_id.clone(),
                    phase: InstancePhase::Failed.as_str().into(),
                    message: format!("{e:#}"),
                    runtime_id: d.runtime_id.clone(),
                    ssh: Some(ssh),
                    network: Some(network),
                };
                apply_live(
                    live,
                    &d.stack,
                    &d.service,
                    InstancePhase::Failed.as_str(),
                    false,
                );
                Ok(report)
            }
        }
    }
}

/// Recreate decision for one instance (B3).
///
/// `in_memory` is the config this process last confirmed running for the
/// instance; while it is known it decides on its own. On adoption (nothing
/// confirmed yet in this process) the **persisted** hash decides, and a sandbox
/// we own whose recorded hash differs — or is missing entirely — is recreated
/// rather than silently adopted as the new config.
async fn recreate_decision(
    store: &Arc<dyn Store>,
    d: &DesiredSandbox,
    in_memory: Option<&str>,
    sandbox_owned: bool,
    want_hash: &str,
) -> Result<bool> {
    if let Some(prev) = in_memory {
        return Ok(prev != want_hash);
    }
    if !sandbox_owned {
        // Nothing exists to adopt: ensure_running will create it.
        return Ok(false);
    }
    let persisted = store.get_instance_applied_hash(&d.instance_id).await?;
    Ok(persisted.as_deref() != Some(want_hash))
}

/// Compose `depends_on` gate: `Some(...)` lists the unsatisfied dependencies
/// (and their conditions); `None` means all are satisfied and the instance may start.
pub(super) fn waiting_deps(
    d: &DesiredSandbox,
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
pub(super) fn apply_live(
    live: &mut HashMap<(String, String), ServiceLive>,
    stack: &str,
    service: &str,
    phase: &str,
    healthy: bool,
) {
    let entry = live
        .entry((stack.to_string(), service.to_string()))
        .or_default();
    entry.running = phase == InstancePhase::Running.as_str();
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
