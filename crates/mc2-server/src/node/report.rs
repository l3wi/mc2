//! Persist one instance's observed state to the store (D4).
//!
//! Every write is checked: a `NotFound` means the instance was deleted
//! concurrently and is benign, anything else is logged with the instance and
//! the operation and makes the reconcile pass count as failed.

use super::state::{InstanceRuntimeState, PassErrors};
use mc2_runtime::{InstanceReport, NetworkObserved, NetworkPhase};
use mc2_store::{Store, StoreError};
use std::collections::HashMap;
use std::sync::Arc;
use tracing::{debug, warn};

/// Write status / health / ssh / network for one instance's report.
pub(super) async fn persist_instance_report(
    store: &Arc<dyn Store>,
    rt_state: &HashMap<String, InstanceRuntimeState>,
    r: &InstanceReport,
    errors: &mut PassErrors,
) {
    let runtime_id = if r.runtime_id.is_empty() {
        None
    } else {
        Some(r.runtime_id.as_str())
    };
    let message = if r.message.is_empty() {
        None
    } else {
        Some(r.message.as_str())
    };
    if let Err(e) = store
        .update_instance_status(&r.instance_id, &r.phase, runtime_id, message)
        .await
    {
        record(errors, &r.instance_id, "update_instance_status", e);
    }

    // Healthcheck signal (drives depends_on: service_healthy).
    if let Some(hstate) = rt_state.get(&r.runtime_id) {
        if let Err(e) = store
            .update_instance_health(&r.instance_id, hstate.health_ok)
            .await
        {
            record(errors, &r.instance_id, "update_instance_health", e);
        }
    }

    if let Some(ssh) = &r.ssh {
        let res = store
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
        if let Err(e) = res {
            record(errors, &r.instance_id, "update_instance_ssh_observed", e);
        }
    }

    if let Some(network) = &r.network {
        let phase = network_summary_phase(network);
        let json = network_observed_json(network);
        let msg = if network.message.is_empty() {
            None
        } else {
            Some(network.message.as_str())
        };
        if let Err(e) = store
            .update_instance_network_observed(&r.instance_id, &phase, &json, msg)
            .await
        {
            record(
                errors,
                &r.instance_id,
                "update_instance_network_observed",
                e,
            );
        }
    }
}

/// Log a failed observed-state write, and fail the pass for anything but a
/// concurrently deleted instance.
fn record(errors: &mut PassErrors, instance_id: &str, operation: &str, e: StoreError) {
    if matches!(e, StoreError::NotFound(_)) {
        debug!(
            instance_id,
            operation, "instance deleted concurrently; observed-state write skipped"
        );
        return;
    }
    warn!(instance_id, operation, error = %e, "observed-state write failed");
    errors.record(anyhow::Error::new(e).context(format!("{operation} for instance {instance_id}")));
}

pub(super) fn network_observed_json(f: &NetworkObserved) -> String {
    serde_json::to_string(f).unwrap_or_else(|_| "{}".into())
}

pub(super) fn network_summary_phase(f: &NetworkObserved) -> String {
    let mut has_ready = false;
    let mut has_failed = false;
    let mut has_pending = false;
    for e in &f.exposes {
        match NetworkPhase::parse(&e.phase) {
            NetworkPhase::Ready => has_ready = true,
            NetworkPhase::Failed => has_failed = true,
            _ => has_pending = true,
        }
    }
    for e in &f.edges {
        match NetworkPhase::parse(&e.phase) {
            NetworkPhase::Ready => has_ready = true,
            NetworkPhase::Failed => has_failed = true,
            _ => has_pending = true,
        }
    }
    if f.exposes.is_empty() && f.edges.is_empty() {
        return NetworkPhase::Pending.as_str().into();
    }
    match (has_failed, has_pending, has_ready) {
        (true, _, true) => NetworkPhase::Mixed.as_str().into(),
        (true, _, false) => NetworkPhase::Failed.as_str().into(),
        (false, true, _) => NetworkPhase::Pending.as_str().into(),
        (false, false, true) => NetworkPhase::Ready.as_str().into(),
        _ => NetworkPhase::Pending.as_str().into(),
    }
}
