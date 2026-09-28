//! Build the node's desired sandbox set directly from the store.

use crate::ingress::build_ingress_routes_for_node;
use crate::networks::build_network_desired;
use crate::secrets::{filter_secret_refs, resolve_injections};
use crate::ssh::resolve_ssh_desired;
use anyhow::{Context, Result};
use mc2_api::ServiceSpec;
use mc2_runtime::{
    sandbox_name, DesiredIngressRoute, DesiredSandbox, InjectedSecret, InstanceReport,
};
use mc2_store::{InstancePhase, InstanceRecord, SecretsKey, Store};
use std::sync::Arc;
use tracing::warn;

/// An instance whose desired state could not be resolved (B4).
///
/// One unresolvable instance — a missing secret, an unreadable spec — must not
/// stop the node from reconciling everything else. The instance is reported
/// `Failed` with this message, and its runtime id stays in the GC keep-set so a
/// sandbox that is already running is never collected because its desired state
/// could not be built.
#[derive(Debug, Clone)]
pub struct DesiredFailure {
    pub instance_id: String,
    pub runtime_id: String,
    pub stack: String,
    pub service: String,
    pub message: String,
}

impl DesiredFailure {
    /// The observed-state report the node persists for this instance.
    pub fn failure_report(&self) -> InstanceReport {
        InstanceReport {
            instance_id: self.instance_id.clone(),
            phase: InstancePhase::Failed.as_str().into(),
            message: self.message.clone(),
            runtime_id: self.runtime_id.clone(),
            ssh: None,
            network: None,
        }
    }
}

/// Everything the node needs for one reconcile pass.
#[derive(Debug, Default)]
pub struct DesiredSet {
    /// Instances ready to reconcile, secret/ssh/network resolved.
    pub sandboxes: Vec<DesiredSandbox>,
    /// Instances whose desired state could not be resolved (B4).
    pub failures: Vec<DesiredFailure>,
    /// The node's ingress route plan.
    pub ingress_routes: Vec<DesiredIngressRoute>,
}

/// Desired set for the local node: every instance bound to `node_id` with
/// secrets / ssh / network resolved, plus the node's ingress route plan.
///
/// A store failure is still an error for the whole pass; a *per-instance*
/// resolution failure is returned as a [`DesiredFailure`] so one bad secret
/// cannot halt the node (B4).
pub async fn build_desired_set(
    store: Arc<dyn Store>,
    secrets_key: &SecretsKey,
    node_id: &str,
) -> Result<DesiredSet> {
    let rows = store
        .list_instances_for_node(node_id)
        .await
        .context("list instances for node")?;

    // Peers in the same stacks (any node) for network backend resolution.
    let all_instances = store.list_instances().await.context("list instances")?;

    let stacks = store.list_stacks().await.context("list stacks")?;
    let stacks_yaml: Vec<(String, String)> =
        stacks.into_iter().map(|s| (s.name, s.raw_yaml)).collect();
    let ingress_routes = build_ingress_routes_for_node(node_id, &stacks_yaml, &all_instances);

    // Include all phases bound to this node so restartPolicy can act on
    // Failed/Stopped (scale-down deletes rows; a deleted stack leaves the set).
    let mut set = DesiredSet {
        ingress_routes,
        ..Default::default()
    };
    for i in rows {
        let runtime_id = sandbox_name(&i.stack, &i.service, i.ordinal);
        match desired_for_instance(&store, secrets_key, &i, &all_instances).await {
            Ok(sandbox) => set.sandboxes.push(sandbox),
            Err(e) => set.failures.push(DesiredFailure {
                instance_id: i.id.clone(),
                runtime_id,
                stack: i.stack.clone(),
                service: i.service.clone(),
                message: format!("{e:#}"),
            }),
        }
    }

    Ok(set)
}

/// Resolve one instance's secrets / ssh / network into a desired sandbox.
async fn desired_for_instance(
    store: &Arc<dyn Store>,
    secrets_key: &SecretsKey,
    i: &InstanceRecord,
    all_instances: &[InstanceRecord],
) -> Result<DesiredSandbox> {
    let spec: ServiceSpec = serde_json::from_str(&i.spec_json)
        .map_err(|e| anyhow::anyhow!("parse service spec for {}: {e}", i.id))?;

    // Explicit `environment:` overrides `secrets[].env` on a name collision:
    // overridden secrets are not resolved (and thus not decrypted/required).
    let (secret_refs, dropped) = filter_secret_refs(&spec.secrets, &spec.env);
    if !dropped.is_empty() {
        warn!(
            service = %i.service,
            env_vars = ?dropped,
            "environment overrides secrets[].env for these vars"
        );
    }
    let resolved = resolve_injections(store.clone(), secrets_key, &secret_refs)
        .await
        .with_context(|| format!("resolve secrets for {}", i.id))?;
    let secrets = resolved
        .into_iter()
        .map(|s| InjectedSecret {
            env: s.env,
            value: s.value,
            allow_hosts: s.allow_hosts,
        })
        .collect();

    let ssh = resolve_ssh_desired(store.clone(), &i.id, spec.ssh.as_ref())
        .await
        .with_context(|| format!("resolve ssh for {}", i.id))?;

    // Always plan the network plane: a consumer with no `expose` of its own
    // still needs egress rules + DNS for its peers' ports (B7), and a
    // service's own exposed ports are reachable to its replicas.
    let network = build_network_desired(i, &spec, all_instances);

    Ok(DesiredSandbox {
        instance_id: i.id.clone(),
        stack: i.stack.clone(),
        service: i.service.clone(),
        ordinal: i.ordinal,
        runtime_id: sandbox_name(&i.stack, &i.service, i.ordinal),
        spec,
        secrets,
        ssh,
        network,
    })
}
