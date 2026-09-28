//! Spread scheduler: filter Ready nodes, pin/selector, minimize co-location.

use mc2_api::ServiceSpec;
use mc2_store::{InstancePhase, InstanceRecord, NodeRecord, NodeStatus};
use std::collections::HashMap;

/// Pure scheduling decision for one pending instance.
///
/// `service_load`: count of same-service instances per node_id.
/// `residual`: residual CPU/memory after existing load.
/// `all_instances`: full inventory for network co-location (allow-target affinity).
pub fn pick_node(
    _instance: &InstanceRecord,
    spec: &ServiceSpec,
    nodes: &[NodeRecord],
    service_load: &HashMap<String, u32>,
    residual: &HashMap<String, (u32, u64)>,
    all_instances: &[InstanceRecord],
) -> Option<String> {
    let ready: Vec<&NodeRecord> = nodes
        .iter()
        .filter(|n| n.status == NodeStatus::Ready.as_str())
        .collect();

    if ready.is_empty() {
        return None;
    }

    // Hard pin
    if let Some(ref pin) = spec.node_name {
        return ready.iter().find(|n| n.name == *pin).map(|n| n.id.clone());
    }

    let mut candidates: Vec<&NodeRecord> = ready
        .into_iter()
        .filter(|n| matches_selector(n, &spec.node_selector))
        .filter(|n| {
            let (cpu, mem) = residual
                .get(&n.id)
                .copied()
                .unwrap_or((n.cpus, n.memory_mib));
            (cpu >= effective_vcpus(spec.cpus)) && mem >= spec.mem_limit_mib
        })
        .collect();

    if candidates.is_empty() {
        return None;
    }

    let network_affinity = network_affinity_scores(all_instances);

    // Spread: fewest same-service instances; then higher network affinity; then name.
    candidates.sort_by(|a, b| {
        let la = service_load.get(&a.id).copied().unwrap_or(0);
        let lb = service_load.get(&b.id).copied().unwrap_or(0);
        let fa = network_affinity.get(&a.id).copied().unwrap_or(0);
        let fb = network_affinity.get(&b.id).copied().unwrap_or(0);
        la.cmp(&lb)
            .then_with(|| fb.cmp(&fa))
            .then_with(|| a.name.cmp(&b.name))
    });

    candidates.first().map(|n| n.id.clone())
}

/// Count network-peer instances per node (same-node network preference).
///
/// Full-mesh network: every service that exposes ports is a peer of every
/// other service, so co-locate with exposing instances (cross-node network
/// splices are unsupported).
fn network_affinity_scores(all_instances: &[InstanceRecord]) -> HashMap<String, u32> {
    let mut m = HashMap::new();
    for inst in all_instances {
        if matches!(
            InstancePhase::parse(&inst.phase),
            InstancePhase::Failed | InstancePhase::Stopped | InstancePhase::Pending
        ) {
            continue;
        }
        let exposes = serde_json::from_str::<ServiceSpec>(&inst.spec_json)
            .map(|s| !s.expose.is_empty())
            .unwrap_or(false);
        if !exposes {
            continue;
        }
        if let Some(ref nid) = inst.node_id {
            *m.entry(nid.clone()).or_insert(0) += 1;
        }
    }
    m
}

fn matches_selector(
    node: &NodeRecord,
    selector: &std::collections::BTreeMap<String, String>,
) -> bool {
    if selector.is_empty() {
        return true;
    }
    let labels: HashMap<String, String> =
        serde_json::from_str(&node.labels_json).unwrap_or_default();
    selector.iter().all(|(k, v)| labels.get(k) == Some(v))
}

/// Build residual capacity from nodes + currently bound instances.
pub fn residual_capacity(
    nodes: &[NodeRecord],
    instances: &[InstanceRecord],
) -> HashMap<String, (u32, u64)> {
    let mut res: HashMap<String, (u32, u64)> = nodes
        .iter()
        .map(|n| (n.id.clone(), (n.cpus, n.memory_mib)))
        .collect();

    for inst in instances {
        let Some(ref nid) = inst.node_id else {
            continue;
        };
        if matches!(
            InstancePhase::parse(&inst.phase),
            InstancePhase::Failed | InstancePhase::Stopped
        ) {
            continue;
        }
        let (need_cpu, need_mem) = resources_from_spec_json(&inst.spec_json);
        if let Some(entry) = res.get_mut(nid) {
            entry.0 = entry.0.saturating_sub(need_cpu);
            entry.1 = entry.1.saturating_sub(need_mem);
        }
    }
    res
}

/// vCPUs the sandbox runtime actually allocates for a declared `cpus`.
///
/// Mirrors `create_detached` in `mc2-runtime` (`msb_sdk.rs`), which hands the
/// builder `(cpus.clamp(1.0, 255.0)) as u8`: values are clamped to `[1, 255]`
/// and truncated to a whole vCPU. Reservation accounting must charge exactly
/// this, or a stack could reserve less than its VMs take. A non-finite `cpus`
/// can never create a VM (validation rejects it), so charge the minimum
/// rather than letting the NaN → 0 cast reserve nothing.
pub fn effective_vcpus(cpus: f64) -> u32 {
    if !cpus.is_finite() {
        return 1;
    }
    u32::from(cpus.clamp(1.0, 255.0) as u8)
}

/// Cluster-wide reserved CPU/memory across bound instances (same accounting as
/// [`residual_capacity`]). Used for apply-time budget checks.
pub fn reserved_capacity(instances: &[InstanceRecord]) -> (u32, u64) {
    reserved_capacity_excluding(instances, None)
}

/// Reserved CPU/memory of every bound instance **not** belonging to `exclude`.
///
/// An apply replaces that stack's instances wholesale, so their current
/// reservation must not be counted alongside the incoming document; pass the
/// stack being applied. `None` counts every stack.
pub fn reserved_capacity_excluding(
    instances: &[InstanceRecord],
    exclude: Option<&str>,
) -> (u32, u64) {
    let mut cpu = 0u32;
    let mut mem = 0u64;
    for inst in instances {
        if exclude == Some(inst.stack.as_str()) {
            continue;
        }
        if inst.node_id.is_none() {
            continue;
        }
        if matches!(
            InstancePhase::parse(&inst.phase),
            InstancePhase::Failed | InstancePhase::Stopped
        ) {
            continue;
        }
        let (need_cpu, need_mem) = resources_from_spec_json(&inst.spec_json);
        cpu = cpu.saturating_add(need_cpu);
        mem = mem.saturating_add(need_mem);
    }
    (cpu, mem)
}

pub fn service_load_map(instances: &[InstanceRecord], service: &str) -> HashMap<String, u32> {
    let mut m = HashMap::new();
    for inst in instances {
        if inst.service != service {
            continue;
        }
        // Failed/Stopped do not consume placement load (align with residual_capacity).
        if matches!(
            InstancePhase::parse(&inst.phase),
            InstancePhase::Failed | InstancePhase::Stopped
        ) {
            continue;
        }
        if let Some(ref nid) = inst.node_id {
            *m.entry(nid.clone()).or_insert(0) += 1;
        }
    }
    m
}

fn resources_from_spec_json(spec_json: &str) -> (u32, u64) {
    let spec: ServiceSpec = match serde_json::from_str(spec_json) {
        Ok(s) => s,
        Err(_) => return (1, 512),
    };
    (effective_vcpus(spec.cpus), spec.mem_limit_mib)
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::BTreeMap;

    fn node(id: &str, name: &str, labels: &str, cpus: u32) -> NodeRecord {
        NodeRecord {
            id: id.into(),
            name: name.into(),
            labels_json: labels.into(),
            arch: "aarch64".into(),
            cpus,
            memory_mib: 8192,
            status: "Ready".into(),
            last_heartbeat: None,
            created_at: String::new(),
        }
    }

    fn pending(service: &str) -> InstanceRecord {
        InstanceRecord {
            id: "i1".into(),
            stack: "demo".into(),
            service: service.into(),
            ordinal: 0,
            node_id: None,
            phase: "Pending".into(),
            runtime_id: None,
            message: None,
            spec_json: r#"{"resources":{"cpus":1,"memoryMiB":512}}"#.into(),
            healthy: false,
            applied_hash: None,
            updated_at: String::new(),
        }
    }

    #[test]
    fn pin_to_node_name() {
        let nodes = vec![node("a", "mac", "{}", 4), node("b", "linux", "{}", 4)];
        let mut spec = ServiceSpec {
            image: "x".into(),
            scale: 1,
            cpus: 1.0,
            mem_limit_mib: 512,
            ports: vec![],
            network: Default::default(),
            env: BTreeMap::new(),
            secrets: vec![],
            volumes: vec![],
            restart: "on-failure".into(),
            healthcheck: None,
            labels: BTreeMap::new(),
            command: None,
            node_name: Some("linux".into()),
            node_selector: BTreeMap::new(),
            ssh: None,
            storage_opt: None,
            expose: vec![],
            networks: vec![],
            depends_on: BTreeMap::new(),
        };
        let id = pick_node(
            &pending("web"),
            &spec,
            &nodes,
            &HashMap::new(),
            &residual_capacity(&nodes, &[]),
            &[],
        )
        .unwrap();
        assert_eq!(id, "b");
        spec.node_name = Some("missing".into());
        assert!(pick_node(
            &pending("web"),
            &spec,
            &nodes,
            &HashMap::new(),
            &residual_capacity(&nodes, &[]),
            &[],
        )
        .is_none());
    }

    #[test]
    fn spread_prefers_empty_node() {
        let nodes = vec![node("a", "n1", "{}", 4), node("b", "n2", "{}", 4)];
        let mut load = HashMap::new();
        load.insert("a".into(), 2u32);
        let spec = ServiceSpec {
            image: "x".into(),
            scale: 1,
            cpus: 1.0,
            mem_limit_mib: 512,
            ports: vec![],
            network: Default::default(),
            env: BTreeMap::new(),
            secrets: vec![],
            volumes: vec![],
            restart: "on-failure".into(),
            healthcheck: None,
            labels: BTreeMap::new(),
            command: None,
            node_name: None,
            node_selector: BTreeMap::new(),
            ssh: None,
            storage_opt: None,
            expose: vec![],
            networks: vec![],
            depends_on: BTreeMap::new(),
        };
        let id = pick_node(
            &pending("web"),
            &spec,
            &nodes,
            &load,
            &residual_capacity(&nodes, &[]),
            &[],
        )
        .unwrap();
        assert_eq!(id, "b");
    }

    #[test]
    fn network_affinity_prefers_node_with_exposing_peer() {
        let nodes = vec![node("a", "n1", "{}", 4), node("b", "n2", "{}", 4)];
        let spec = ServiceSpec {
            image: "x".into(),
            scale: 1,
            cpus: 1.0,
            mem_limit_mib: 512,
            ports: vec![],
            network: Default::default(),
            env: BTreeMap::new(),
            secrets: vec![],
            volumes: vec![],
            restart: "on-failure".into(),
            healthcheck: None,
            labels: BTreeMap::new(),
            command: None,
            node_name: None,
            node_selector: BTreeMap::new(),
            ssh: None,
            storage_opt: None,
            expose: vec![],
            networks: vec![],
            depends_on: BTreeMap::new(),
        };
        // db exposes a network port and lives on node "a" → co-locate with it.
        let mut db_spec = spec.clone();
        db_spec.expose = vec![mc2_api::ExposeSpec {
            port: 5432,
            protocol: "tcp".into(),
            name: None,
        }];
        let peers = vec![InstanceRecord {
            id: "db0".into(),
            stack: "demo".into(),
            service: "db".into(),
            ordinal: 0,
            node_id: Some("a".into()),
            phase: "Running".into(),
            runtime_id: Some("demo-db-0".into()),
            message: None,
            spec_json: serde_json::to_string(&db_spec).unwrap(),
            healthy: false,
            applied_hash: None,
            updated_at: String::new(),
        }];
        let id = pick_node(
            &pending("web"),
            &spec,
            &nodes,
            &HashMap::new(),
            &residual_capacity(&nodes, &peers),
            &peers,
        )
        .unwrap();
        assert_eq!(id, "a");
    }

    #[test]
    fn effective_vcpus_matches_what_the_runtime_allocates() {
        // `create_detached` (mc2-runtime) clamps to [1, 255] then truncates.
        assert_eq!(effective_vcpus(0.0), 1, "sub-1 vCPU is raised to one");
        assert_eq!(effective_vcpus(0.5), 1);
        assert_eq!(effective_vcpus(1.0), 1);
        assert_eq!(effective_vcpus(1.9), 1, "the fraction is dropped");
        assert_eq!(effective_vcpus(2.5), 2);
        assert_eq!(effective_vcpus(255.0), 255);
        assert_eq!(
            effective_vcpus(300.0),
            255,
            "capped at the u8 the runtime takes"
        );
        assert_eq!(
            effective_vcpus(f64::NAN),
            1,
            "a NaN cpus never creates a VM"
        );
    }

    #[test]
    fn reserved_capacity_excludes_the_stack_being_applied() {
        fn bound(stack: &str, service: &str, cpus: f64) -> InstanceRecord {
            InstanceRecord {
                id: format!("{stack}-{service}"),
                stack: stack.into(),
                service: service.into(),
                ordinal: 0,
                node_id: Some("n1".into()),
                phase: "Running".into(),
                runtime_id: None,
                message: None,
                spec_json: format!(r#"{{"image":"x","cpus":{cpus}}}"#),
                healthy: false,
                applied_hash: None,
                updated_at: String::new(),
            }
        }
        let instances = vec![
            bound("app", "web", 3.0),
            bound("other", "db", 2.0),
            InstanceRecord {
                node_id: None,
                ..bound("pending", "job", 4.0)
            },
        ];
        assert_eq!(
            reserved_capacity(&instances),
            (5, 1024),
            "every bound stack (unbound instances reserve nothing)"
        );
        assert_eq!(
            reserved_capacity_excluding(&instances, Some("app")),
            (2, 512),
            "the stack being applied must not count against itself"
        );
        assert_eq!(
            reserved_capacity_excluding(&instances, Some("nope")),
            (5, 1024)
        );
    }
}
