//! Build the node's desired sandbox set directly from the store.

use crate::fabric::build_fabric_desired;
use crate::ingress::build_ingress_routes_for_node;
use crate::secrets::{filter_secret_refs, resolve_injections};
use crate::ssh::resolve_ssh_desired;
use anyhow::{Context, Result};
use mc2_api::ServiceSpec;
use mc2_runtime::{
    sandbox_name, DesiredFabric, DesiredIngressRoute, DesiredSandbox, InjectedSecret,
};
use mc2_store::{SecretsKey, Store};
use std::sync::Arc;
use tracing::warn;

/// Desired set for the local node: every instance bound to `node_id` with
/// secrets / ssh / fabric resolved, plus the node's ingress route plan.
pub async fn build_desired_set(
    store: Arc<dyn Store>,
    secrets_key: &SecretsKey,
    node_id: &str,
) -> Result<(Vec<DesiredSandbox>, Vec<DesiredIngressRoute>)> {
    let rows = store
        .list_instances_for_node(node_id)
        .await
        .context("list instances for node")?;

    // Peers in the same stacks (any node) for fabric backend resolution.
    let all_instances = store.list_instances().await.context("list instances")?;

    let stacks = store.list_stacks().await.context("list stacks")?;
    let stacks_yaml: Vec<(String, String)> =
        stacks.into_iter().map(|s| (s.name, s.raw_yaml)).collect();
    let ingress_routes = build_ingress_routes_for_node(node_id, &stacks_yaml, &all_instances);

    // Include all phases bound to this node so restartPolicy can act on
    // Failed/Stopped (scale-down deletes rows; unbound instances leave the set).
    let mut instances = Vec::new();
    for i in rows {
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

        let fabric = if spec.expose.is_empty() {
            DesiredFabric::default()
        } else {
            build_fabric_desired(&i, &spec, &all_instances)
        };

        let runtime_id = sandbox_name(&i.stack, &i.service, i.ordinal);
        instances.push(DesiredSandbox {
            instance_id: i.id,
            stack: i.stack,
            service: i.service,
            ordinal: i.ordinal,
            runtime_id,
            spec,
            secrets,
            ssh,
            fabric,
        });
    }

    Ok((instances, ingress_routes))
}
