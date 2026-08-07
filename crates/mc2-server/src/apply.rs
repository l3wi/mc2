//! Apply stack YAML: parse, upsert, reconcile replicas, schedule.

use crate::scheduler::{pick_node, residual_capacity, service_load_map};
use anyhow::{Context, Result};
use mc2_api::{parse_stack_yaml, ServiceSpec, StackDocument};
use mc2_store::{InstanceRecord, Store};
use serde::Serialize;
use std::sync::Arc;
use tracing::info;

#[derive(Debug, Clone, Serialize)]
pub struct ApplyResult {
    pub stack: String,
    pub services: u32,
    pub instances: u32,
    pub scheduled: u32,
    pub pending: u32,
}

/// Apply a stack document from YAML text.
pub async fn apply_stack_yaml(store: Arc<dyn Store>, yaml: &str) -> Result<ApplyResult> {
    let doc = parse_stack_yaml(yaml).map_err(anyhow::Error::msg)?;
    apply_stack(store, &doc, yaml).await
}

pub async fn apply_stack(
    store: Arc<dyn Store>,
    doc: &StackDocument,
    raw_yaml: &str,
) -> Result<ApplyResult> {
    let labels_json = serde_json::to_string(&doc.metadata.labels).unwrap_or_else(|_| "{}".into());
    store
        .upsert_stack(&doc.metadata.name, &labels_json, raw_yaml)
        .await
        .context("upsert stack")?;

    let mut total_instances = 0u32;
    for (svc_name, spec) in &doc.services {
        let spec_json = serde_json::to_string(spec).context("serialize service spec")?;
        let inst = store
            .reconcile_service_replicas(&doc.metadata.name, svc_name, spec.replicas, &spec_json)
            .await
            .with_context(|| format!("reconcile {svc_name}"))?;
        total_instances += inst.len() as u32;
    }

    let scheduled = run_scheduler(store.clone()).await?;
    let pending = store
        .list_pending_instances()
        .await
        .context("list pending")?
        .len() as u32;

    info!(
        stack = %doc.metadata.name,
        services = doc.services.len(),
        instances = total_instances,
        scheduled,
        pending,
        "stack applied"
    );
    mc2_metrics::record_apply();
    mc2_metrics::record_schedule_binds(u64::from(scheduled));

    Ok(ApplyResult {
        stack: doc.metadata.name.clone(),
        services: doc.services.len() as u32,
        instances: total_instances,
        scheduled,
        pending,
    })
}

/// Schedule all Pending instances. Returns number newly bound.
pub async fn run_scheduler(store: Arc<dyn Store>) -> Result<u32> {
    let nodes = store.list_nodes().await.context("list nodes")?;
    let all = store.list_instances().await.context("list instances")?;
    let pending = store
        .list_pending_instances()
        .await
        .context("list pending")?;

    let residual = residual_capacity(&nodes, &all);
    let mut residual_mut = residual;
    let mut scheduled = 0u32;

    // Track load as we assign within this pass
    let mut instances_snapshot = all;

    for inst in pending {
        let spec: ServiceSpec = serde_json::from_str(&inst.spec_json)
            .with_context(|| format!("parse spec for {}", inst.id))?;
        let load = service_load_map(&instances_snapshot, &inst.service);
        let Some(node_id) =
            pick_node(&inst, &spec, &nodes, &load, &residual_mut, &instances_snapshot)
        else {
            continue;
        };

        let bound = store
            .bind_instance_to_node(&inst.id, &node_id)
            .await
            .with_context(|| format!("bind {}", inst.id))?;

        if let Some(entry) = residual_mut.get_mut(&node_id) {
            entry.0 = entry.0.saturating_sub(spec.resources.cpus);
            entry.1 = entry.1.saturating_sub(spec.resources.memory_mib);
        }
        // update snapshot
        if let Some(slot) = instances_snapshot.iter_mut().find(|i| i.id == inst.id) {
            *slot = bound.clone();
        } else {
            instances_snapshot.push(bound);
        }
        scheduled += 1;
    }

    Ok(scheduled)
}

/// List instances for operator REST/CLI.
pub async fn list_instance_views(store: Arc<dyn Store>) -> Result<Vec<InstanceRecord>> {
    Ok(store.list_instances().await?)
}
