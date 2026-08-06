//! Spread scheduler: filter Ready nodes, pin/selector, minimize co-location.

use mcc_api::ServiceSpec;
use mcc_store::{InstanceRecord, NodeRecord, NodeStatus};
use serde_json::Value;
use std::collections::HashMap;

/// Pure scheduling decision for one pending instance.
///
/// `service_load`: count of same-service instances per node_id.
/// `residual`: residual CPU/memory after existing load.
pub fn pick_node(
    _instance: &InstanceRecord,
    spec: &ServiceSpec,
    nodes: &[NodeRecord],
    service_load: &HashMap<String, u32>,
    residual: &HashMap<String, (u32, u64)>,
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
            cpu >= spec.resources.cpus && mem >= spec.resources.memory_mib
        })
        .collect();

    if candidates.is_empty() {
        return None;
    }

    // Spread: fewest instances of this service on the node; tie-break by name.
    candidates.sort_by(|a, b| {
        let la = service_load.get(&a.id).copied().unwrap_or(0);
        let lb = service_load.get(&b.id).copied().unwrap_or(0);
        la.cmp(&lb).then_with(|| a.name.cmp(&b.name))
    });

    candidates.first().map(|n| n.id.clone())
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
        if inst.phase == "Failed" || inst.phase == "Stopped" {
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

pub fn service_load_map(instances: &[InstanceRecord], service: &str) -> HashMap<String, u32> {
    let mut m = HashMap::new();
    for inst in instances {
        if inst.service != service {
            continue;
        }
        if let Some(ref nid) = inst.node_id {
            *m.entry(nid.clone()).or_insert(0) += 1;
        }
    }
    m
}

fn resources_from_spec_json(spec_json: &str) -> (u32, u64) {
    let v: Value = serde_json::from_str(spec_json).unwrap_or(Value::Null);
    let cpus = v
        .pointer("/resources/cpus")
        .and_then(|x| x.as_u64())
        .unwrap_or(1) as u32;
    let mem = v
        .pointer("/resources/memoryMiB")
        .or_else(|| v.pointer("/resources/memory_mib"))
        .and_then(|x| x.as_u64())
        .unwrap_or(512);
    (cpus, mem)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mcc_api::ResourceSpec;
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
            node_token_hash: String::new(),
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
            updated_at: String::new(),
        }
    }

    #[test]
    fn pin_to_node_name() {
        let nodes = vec![node("a", "mac", "{}", 4), node("b", "linux", "{}", 4)];
        let mut spec = ServiceSpec {
            image: "x".into(),
            replicas: 1,
            resources: ResourceSpec {
                cpus: 1,
                memory_mib: 512,
            },
            ports: vec![],
            network: Default::default(),
            env: BTreeMap::new(),
            secrets: vec![],
            volumes: vec![],
            restart_policy: "on-failure".into(),
            health: None,
            labels: BTreeMap::new(),
            command: None,
            node_name: Some("linux".into()),
            node_selector: BTreeMap::new(),
        };
        let id = pick_node(
            &pending("web"),
            &spec,
            &nodes,
            &HashMap::new(),
            &residual_capacity(&nodes, &[]),
        )
        .unwrap();
        assert_eq!(id, "b");
        spec.node_name = Some("missing".into());
        assert!(pick_node(
            &pending("web"),
            &spec,
            &nodes,
            &HashMap::new(),
            &residual_capacity(&nodes, &[])
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
            replicas: 1,
            resources: ResourceSpec {
                cpus: 1,
                memory_mib: 512,
            },
            ports: vec![],
            network: Default::default(),
            env: BTreeMap::new(),
            secrets: vec![],
            volumes: vec![],
            restart_policy: "on-failure".into(),
            health: None,
            labels: BTreeMap::new(),
            command: None,
            node_name: None,
            node_selector: BTreeMap::new(),
        };
        let id = pick_node(
            &pending("web"),
            &spec,
            &nodes,
            &load,
            &residual_capacity(&nodes, &[]),
        )
        .unwrap();
        assert_eq!(id, "b");
    }
}
