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

/// Resolve `ports` entries with `published: 0` (target-only / hostname sugar)
/// to concrete, stable host ports, persisted in the stored spec so ingress
/// routes and the desired set see a fixed backend port across restarts.
/// Reuses an existing allocation for the same (stack, service, target) so
/// re-applies keep the same port.
async fn resolve_auto_ports(
    store: &dyn Store,
    stack: &str,
    service: &str,
    spec: &mut ServiceSpec,
) -> Result<()> {
    use std::collections::{BTreeMap, BTreeSet};

    let mut used: BTreeSet<u16> = BTreeSet::new();
    let mut existing: BTreeMap<u16, u16> = BTreeMap::new(); // target → published
    for inst in store.list_instances().await? {
        if inst.stack != stack || inst.service != service {
            continue;
        }
        let Ok(s) = serde_json::from_str::<ServiceSpec>(&inst.spec_json) else {
            continue;
        };
        for p in s.ports {
            if p.published != 0 {
                used.insert(p.published);
                if p.target != 0 {
                    existing.insert(p.target, p.published);
                }
            }
        }
    }

    for p in spec.ports.iter_mut() {
        if p.published != 0 {
            continue;
        }
        let port = match existing.get(&p.target) {
            Some(&port) => port,
            None => {
                let port = (10000..=u16::MAX)
                    .find(|cand| !used.contains(cand))
                    .context("no free host port for auto-allocation")?;
                used.insert(port);
                port
            }
        };
        p.published = port;
    }
    Ok(())
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
    store
        .upsert_stack(&doc.name, "{}", raw_yaml)
        .await
        .context("upsert stack")?;

    let mut total_instances = 0u32;
    for (svc_name, spec) in &doc.services {
        let mut spec = spec.clone();
        // Resolve `ports` with published=0 (target-only / hostname sugar) to
        // concrete host ports and persist them, so ingress routes and the
        // desired set see a stable backend port across reconciles/restarts.
        resolve_auto_ports(store.as_ref(), &doc.name, svc_name, &mut spec).await?;
        let spec_json = serde_json::to_string(&spec).context("serialize service spec")?;
        let inst = store
            .reconcile_service_replicas(&doc.name, svc_name, spec.scale, &spec_json)
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
        stack = %doc.name,
        services = doc.services.len(),
        instances = total_instances,
        scheduled,
        pending,
        "stack applied"
    );
    mc2_metrics::record_apply();
    mc2_metrics::record_schedule_binds(u64::from(scheduled));

    Ok(ApplyResult {
        stack: doc.name.clone(),
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
        let Some(node_id) = pick_node(
            &inst,
            &spec,
            &nodes,
            &load,
            &residual_mut,
            &instances_snapshot,
        ) else {
            continue;
        };

        let bound = store
            .bind_instance_to_node(&inst.id, &node_id)
            .await
            .with_context(|| format!("bind {}", inst.id))?;

        if let Some(entry) = residual_mut.get_mut(&node_id) {
            entry.0 = entry.0.saturating_sub(spec.cpus.ceil() as u32);
            entry.1 = entry.1.saturating_sub(spec.mem_limit_mib);
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

#[cfg(test)]
mod tests {
    use super::*;
    use mc2_api::PortSpec;
    use mc2_store::MemoryStore;

    fn spec_with_auto_port(target: u16) -> ServiceSpec {
        ServiceSpec {
            image: "alpine".into(),
            scale: 1,
            cpus: 1.0,
            mem_limit_mib: 512,
            ports: vec![PortSpec {
                published: 0,
                target,
                protocol: "tcp".into(),
                hostname: None,
            }],
            network: Default::default(),
            env: Default::default(),
            secrets: vec![],
            volumes: vec![],
            restart: "no".into(),
            healthcheck: None,
            labels: Default::default(),
            command: None,
            node_name: None,
            node_selector: Default::default(),
            ssh: None,
            expose: vec![],
            networks: vec![],
        }
    }

    #[tokio::test]
    async fn resolve_auto_ports_allocates_and_reuses() {
        let store = MemoryStore::new();
        store.init_cluster("").await.unwrap();
        store.upsert_stack("demo", "{}", "yaml").await.unwrap();

        // First apply: target-only → concrete host port from the 10000+ range.
        let mut spec = spec_with_auto_port(3001);
        resolve_auto_ports(store.as_ref(), "demo", "web", &mut spec)
            .await
            .unwrap();
        let first = spec.ports[0].published;
        assert!(
            first >= 10000,
            "auto host port should come from the free range"
        );
        assert_ne!(first, 0);

        // Persist the resolved spec (as apply would) so re-apply reuses it.
        store
            .reconcile_service_replicas("demo", "web", 1, &serde_json::to_string(&spec).unwrap())
            .await
            .unwrap();

        // Re-apply with the same target-only port → same host port (stable).
        let mut again = spec_with_auto_port(3001);
        resolve_auto_ports(store.as_ref(), "demo", "web", &mut again)
            .await
            .unwrap();
        assert_eq!(
            again.ports[0].published, first,
            "re-apply must reuse the allocation"
        );

        // A different target gets a different free port, avoiding the used one.
        let mut other = spec_with_auto_port(3002);
        resolve_auto_ports(store.as_ref(), "demo", "web", &mut other)
            .await
            .unwrap();
        assert_ne!(other.ports[0].published, first);
    }
}
