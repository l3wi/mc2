//! Apply stack YAML: parse, upsert, reconcile replicas, schedule.

use crate::scheduler::{pick_node, residual_capacity, service_load_map};
use anyhow::{Context, Result};
use mc2_api::{parse_stack_yaml, ServiceSpec, StackDocument};
use mc2_store::{InstanceRecord, Store};
use serde::Serialize;
use std::sync::Arc;
use tracing::info;

/// Apply-time classification so the REST layer can map user errors to 400
/// (validation / port allocation) instead of substring-matching messages.
#[derive(Debug, thiserror::Error)]
pub enum ApplyError {
    /// Stack YAML parse/validation failure (user error).
    #[error("{0}")]
    Validation(String),
    /// Host port allocation conflict (fixed-block / auto range).
    #[error("{0}")]
    Allocation(String),
    /// Store, serialization, or scheduling failure.
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

#[derive(Debug, Clone, Serialize)]
pub struct ApplyResult {
    pub stack: String,
    pub services: u32,
    pub instances: u32,
    pub scheduled: u32,
    pub pending: u32,
    /// SSH front ends declared in the stack (printed by `mc2 up`).
    #[serde(default)]
    pub ssh: Vec<ApplySshEndpoint>,
}

/// One service's declared SSH front end (desired; auto ports resolve on first
/// reconcile — see `mc2 ssh ls`).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplySshEndpoint {
    pub service: String,
    pub bind: String,
    /// Fixed host port; `None` = auto-allocate on reconcile.
    pub port: Option<u16>,
    /// Traefik entrypoint from an `ingress.tcp` route, when configured.
    pub entrypoint: Option<String>,
    pub replicas: u32,
}

/// Resolve `ports` with `published: 0` (target-only / hostname sugar) and
/// `scale > 1` fixed ports to concrete, stable host ports, producing one
/// [`ServiceSpec`] per replica (index = ordinal). Persisted in the stored spec
/// so ingress routes and the desired set see fixed backend ports across restarts.
///
/// Allocation rules:
/// - Fixed `published: P`: replica `i` gets `P + i` (a contiguous block). Reuses
///   the previous allocation for the same (ordinal, target) when present.
/// - Auto (`published: 0`): replica `i` gets a distinct free port from the
///   10000+ pool, reusing the previous per-(ordinal, target) allocation.
async fn resolve_replica_ports(
    store: &dyn Store,
    stack: &str,
    service: &str,
    base: &ServiceSpec,
) -> Result<Vec<ServiceSpec>, ApplyError> {
    use std::collections::{BTreeMap, BTreeSet};

    let scale = base.scale.max(1);
    let mut used_by_others: BTreeSet<u16> = BTreeSet::new();
    let mut existing: BTreeMap<(u32, u16), u16> = BTreeMap::new(); // (ordinal, target) → published
    for inst in store.list_instances().await.map_err(anyhow::Error::from)? {
        let Ok(s) = serde_json::from_str::<ServiceSpec>(&inst.spec_json) else {
            continue;
        };
        for p in s.ports {
            if p.published == 0 {
                continue;
            }
            if inst.stack != stack || inst.service != service {
                used_by_others.insert(p.published);
            } else if p.target != 0 {
                existing.insert((inst.ordinal, p.target), p.published);
            }
        }
    }

    // Ports already allocated to this service (reusable), plus the ports we
    // assign during this pass.
    let mut local: BTreeSet<u16> = existing.values().copied().collect();

    let mut out = Vec::with_capacity(scale as usize);
    for ord in 0..scale {
        let mut spec = base.clone();
        for p in spec.ports.iter_mut() {
            if p.published != 0 {
                let cand = u16::try_from(u32::from(p.published) + ord).with_context(|| {
                    format!(
                        "published port {} + replica {ord} exceeds the host port range",
                        p.published
                    )
                })?;
                let reuse = existing.contains_key(&(ord, p.target));
                if used_by_others.contains(&cand) || (!reuse && local.contains(&cand)) {
                    return Err(ApplyError::Allocation(format!(
                        "published host port {cand} (from {} + replica {ord}) conflicts \
                         with another allocation in this stack",
                        p.published
                    )));
                }
                local.insert(cand);
                p.published = cand;
            } else {
                let port = match existing.get(&(ord, p.target)) {
                    Some(&port) => port,
                    None => (10000..=u16::MAX)
                        .find(|cand| !used_by_others.contains(cand) && !local.contains(cand))
                        .context("no free host port for auto-allocation")?,
                };
                local.insert(port);
                p.published = port;
            }
        }
        out.push(spec);
    }
    Ok(out)
}

/// Apply a stack document from YAML text.
pub async fn apply_stack_yaml(
    store: Arc<dyn Store>,
    yaml: &str,
) -> Result<ApplyResult, ApplyError> {
    let doc = parse_stack_yaml(yaml).map_err(ApplyError::Validation)?;
    apply_stack(store, &doc, yaml).await
}

pub async fn apply_stack(
    store: Arc<dyn Store>,
    doc: &StackDocument,
    raw_yaml: &str,
) -> Result<ApplyResult, ApplyError> {
    store
        .upsert_stack(&doc.name, "{}", raw_yaml)
        .await
        .context("upsert stack")?;

    let mut total_instances = 0u32;
    for (svc_name, spec) in &doc.services {
        // Resolve per-replica host ports (fixed blocks + auto) and persist them,
        // so ingress routes and the desired set see stable backend ports.
        let specs = resolve_replica_ports(store.as_ref(), &doc.name, svc_name, spec).await?;
        let spec_jsons = specs
            .iter()
            .map(|s| serde_json::to_string(s).context("serialize service spec"))
            .collect::<Result<Vec<String>>>()?;
        let inst = store
            .reconcile_service_replicas_multi(&doc.name, svc_name, &spec_jsons)
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

    let ssh: Vec<ApplySshEndpoint> = doc
        .services
        .iter()
        .filter(|(_, s)| s.ssh.as_ref().is_some_and(|ssh| ssh.enabled))
        .map(|(name, s)| {
            let spec = s.ssh.as_ref().expect("filtered enabled");
            let entrypoint = doc
                .ingress
                .as_ref()
                .and_then(|ing| ing.tcp.iter().find(|r| r.service == *name))
                .map(|r| r.entry_point.clone());
            ApplySshEndpoint {
                service: name.clone(),
                bind: spec.bind.clone(),
                port: (spec.port != 0).then_some(spec.port),
                entrypoint,
                replicas: s.scale,
            }
        })
        .collect();

    info!(
        stack = %doc.name,
        services = doc.services.len(),
        instances = total_instances,
        scheduled,
        pending,
        ssh = ssh.len(),
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
        ssh,
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
    use std::collections::BTreeMap;

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
            depends_on: BTreeMap::new(),
        }
    }

    fn spec_with_fixed_port(published: u16, target: u16, scale: u32) -> ServiceSpec {
        ServiceSpec {
            image: "alpine".into(),
            scale,
            cpus: 1.0,
            mem_limit_mib: 512,
            ports: vec![PortSpec {
                published,
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
            depends_on: BTreeMap::new(),
        }
    }

    #[tokio::test]
    async fn resolve_auto_ports_allocates_and_reuses() {
        let store = MemoryStore::new();
        store.init_cluster("").await.unwrap();
        store.upsert_stack("demo", "{}", "yaml").await.unwrap();

        // First apply: target-only → concrete host port from the 10000+ range.
        let specs =
            resolve_replica_ports(store.as_ref(), "demo", "web", &spec_with_auto_port(3001))
                .await
                .unwrap();
        assert_eq!(specs.len(), 1);
        let first = specs[0].ports[0].published;
        assert!(
            first >= 10000,
            "auto host port should come from the free range"
        );
        assert_ne!(first, 0);

        // Persist the resolved spec (as apply would) so re-apply reuses it.
        store
            .reconcile_service_replicas(
                "demo",
                "web",
                1,
                &serde_json::to_string(&specs[0]).unwrap(),
            )
            .await
            .unwrap();

        // Re-apply with the same target-only port → same host port (stable).
        let again =
            resolve_replica_ports(store.as_ref(), "demo", "web", &spec_with_auto_port(3001))
                .await
                .unwrap();
        assert_eq!(
            again[0].ports[0].published, first,
            "re-apply must reuse the allocation"
        );

        // A different target gets a different free port, avoiding the used one.
        let other =
            resolve_replica_ports(store.as_ref(), "demo", "web", &spec_with_auto_port(3002))
                .await
                .unwrap();
        assert_ne!(other[0].ports[0].published, first);
    }

    #[tokio::test]
    async fn scaled_fixed_port_gets_a_distinct_block_per_replica() {
        let store = MemoryStore::new();
        store.init_cluster("").await.unwrap();
        store.upsert_stack("demo", "{}", "yaml").await.unwrap();

        let specs = resolve_replica_ports(
            store.as_ref(),
            "demo",
            "web",
            &spec_with_fixed_port(5000, 3000, 3),
        )
        .await
        .unwrap();
        assert_eq!(specs.len(), 3);
        let host_ports: Vec<u16> = specs.iter().map(|s| s.ports[0].published).collect();
        assert_eq!(host_ports, vec![5000, 5001, 5002]);
    }

    #[tokio::test]
    async fn scaled_auto_ports_are_distinct_and_reused_per_replica() {
        let store = MemoryStore::new();
        store.init_cluster("").await.unwrap();
        store.upsert_stack("demo", "{}", "yaml").await.unwrap();

        let mut base = spec_with_auto_port(3000);
        base.scale = 3;
        let specs = resolve_replica_ports(store.as_ref(), "demo", "web", &base)
            .await
            .unwrap();
        assert_eq!(specs.len(), 3);
        let ports: Vec<u16> = specs.iter().map(|s| s.ports[0].published).collect();
        let mut uniq = ports.clone();
        uniq.sort_unstable();
        uniq.dedup();
        assert_eq!(uniq.len(), 3, "each replica must get a distinct host port");

        // Persist and re-apply → the same per-replica ports are reused.
        let jsons: Vec<String> = specs
            .iter()
            .map(|s| serde_json::to_string(s).unwrap())
            .collect();
        store
            .reconcile_service_replicas_multi("demo", "web", &jsons)
            .await
            .unwrap();
        let again = resolve_replica_ports(store.as_ref(), "demo", "web", &base)
            .await
            .unwrap();
        assert_eq!(
            again
                .iter()
                .map(|s| s.ports[0].published)
                .collect::<Vec<_>>(),
            ports
        );
    }
}

#[tokio::test]
async fn apply_reports_declared_ssh_endpoints() {
    let store = mc2_store::MemoryStore::new();
    store.init_cluster("").await.unwrap();
    let yaml = r#"
name: ssh-demo
services:
  web:
    image: alpine
    ssh: true
  admin:
    image: alpine
    ssh:
      enabled: true
      port: 2222
      authorizedKeys: [dev]
  worker:
    image: alpine
ingress:
  tcp:
    - name: web-ssh
      entryPoint: ssh
      service: admin
"#;
    let result = apply_stack_yaml(store, yaml).await.unwrap();
    assert_eq!(result.ssh.len(), 2, "{:?}", result.ssh);

    let web = result.ssh.iter().find(|e| e.service == "web").unwrap();
    assert_eq!(web.bind, "127.0.0.1");
    assert!(web.port.is_none(), "short form → auto port");
    assert!(web.entrypoint.is_none(), "no tcp route for web");

    let admin = result.ssh.iter().find(|e| e.service == "admin").unwrap();
    assert_eq!(admin.port, Some(2222));
    assert_eq!(admin.entrypoint.as_deref(), Some("ssh"));
}
