//! Build per-instance fabric plans for the node loop (D13).
//!
//! Server-wide named networks (compose-faithful): every service is on its
//! stack's implicit default network plus any named networks it joins. A
//! service reaches every peer (any stack) that shares a network, for each
//! exposed port — default-allow. DNS: same-stack peers use
//! `<svc>.<stack>.svc.mc2` (back-compat); cross-stack peers use the shared
//! named network `<svc>.<network>.svc.mc2`.

use mc2_api::{network_fqdn, ServiceSpec};
use mc2_runtime::{DesiredFabric, FabricAllowDesired, FabricExposeDesired};
use mc2_store::InstanceRecord;
use std::collections::{BTreeSet, HashMap};

/// A service's network set: its stack's implicit default network + named
/// memberships.
fn instance_networks(stack: &str, spec: &ServiceSpec) -> BTreeSet<String> {
    let mut nets: BTreeSet<String> = spec.networks.iter().cloned().collect();
    nets.insert(stack.to_string());
    nets
}

/// Resolve fabric desired state for one instance given the node's instance set.
///
/// Generates a default-allow mesh: an edge to every peer service (any stack)
/// that shares a network, for every guest port that peer exposes.
pub fn build_fabric_desired(
    inst: &InstanceRecord,
    spec: &ServiceSpec,
    peers: &[InstanceRecord],
) -> DesiredFabric {
    let exposes: Vec<FabricExposeDesired> = spec
        .expose
        .iter()
        .map(|e| FabricExposeDesired {
            guest_port: e.port,
            protocol: e.protocol.clone(),
        })
        .collect();

    let my_networks = instance_networks(&inst.stack, spec);

    // Index peers by service → instances (prefer lowest ordinal ready).
    let mut by_service: HashMap<String, Vec<&InstanceRecord>> = HashMap::new();
    for p in peers {
        by_service.entry(p.service.clone()).or_default().push(p);
    }
    for list in by_service.values_mut() {
        list.sort_by_key(|i| i.ordinal);
    }

    let mut allows = Vec::new();
    for (service, list) in by_service {
        if service == inst.service {
            continue; // no self-edges
        }
        // Shared networks across this peer service's instances.
        let shared: Vec<(String, String)> = list
            .iter()
            .filter_map(|p| {
                let Ok(pspec) = serde_json::from_str::<ServiceSpec>(&p.spec_json) else {
                    return None;
                };
                let theirs = instance_networks(&p.stack, &pspec);
                let common: Vec<&String> = my_networks.intersection(&theirs).collect();
                if common.is_empty() {
                    return None;
                }
                // Same stack → default network (back-compat DNS). Cross-stack →
                // a shared named network.
                let net = if p.stack == inst.stack {
                    inst.stack.clone()
                } else {
                    common
                        .iter()
                        .find(|n| ***n != inst.stack)
                        .map(|n| (*n).clone())
                        .unwrap_or_else(|| inst.stack.clone())
                };
                Some((p.id.clone(), net))
            })
            .collect();
        if shared.is_empty() {
            continue;
        }

        // Reachable ports: union of expose ports across the service's instances.
        let mut ports: BTreeSet<u16> = BTreeSet::new();
        for p in &list {
            if let Ok(pspec) = serde_json::from_str::<ServiceSpec>(&p.spec_json) {
                for e in &pspec.expose {
                    ports.insert(e.port);
                }
            }
        }
        if ports.is_empty() {
            continue;
        }

        // Backend: lowest ordinal that is bound and not Failed/Stopped.
        let backend = list
            .iter()
            .copied()
            .find(|i| {
                i.node_id.is_some()
                    && i.phase != "Failed"
                    && i.phase != "Stopped"
                    && i.phase != "Pending"
            })
            .or_else(|| list.first().copied());

        let (backend_instance_id, backend_node_id, backend_ordinal, backend_local) = match backend {
            Some(b) => {
                let nid = b.node_id.clone().unwrap_or_default();
                let local = match (&inst.node_id, &b.node_id) {
                    (Some(a), Some(b)) => a == b,
                    _ => false,
                };
                (b.id.clone(), nid, b.ordinal, local)
            }
            None => (String::new(), String::new(), 0, false),
        };

        let net = shared[0].1.clone();
        for port in ports {
            allows.push(FabricAllowDesired {
                to_service: service.clone(),
                port,
                protocol: "tcp".into(),
                fqdn: network_fqdn(&net, &service),
                short_name: service.clone(),
                backend_instance_id: backend_instance_id.clone(),
                backend_node_id: backend_node_id.clone(),
                backend_local,
                backend_ordinal,
            });
        }
    }

    DesiredFabric { exposes, allows }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mc2_api::ExposeSpec;
    use std::collections::BTreeMap;

    fn bare_spec() -> ServiceSpec {
        ServiceSpec {
            image: "alpine".into(),
            scale: 1,
            cpus: 1.0,
            mem_limit_mib: 512,
            ports: vec![],
            network: Default::default(),
            env: Default::default(),
            secrets: vec![],
            volumes: vec![],
            restart: "on-failure".into(),
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

    fn inst(
        id: &str,
        service: &str,
        node: Option<&str>,
        phase: &str,
        spec: &ServiceSpec,
    ) -> InstanceRecord {
        inst_in("shop", id, service, node, phase, spec)
    }

    fn inst_in(
        stack: &str,
        id: &str,
        service: &str,
        node: Option<&str>,
        phase: &str,
        spec: &ServiceSpec,
    ) -> InstanceRecord {
        InstanceRecord {
            id: id.into(),
            stack: stack.into(),
            service: service.into(),
            ordinal: 0,
            node_id: node.map(str::to_string),
            phase: phase.into(),
            runtime_id: None,
            message: None,
            spec_json: serde_json::to_string(spec).unwrap(),
            healthy: false,
            updated_at: "".into(),
        }
    }

    #[test]
    fn fabric_plan_full_mesh_no_allow_needed() {
        let web = bare_spec();
        let mut db = bare_spec();
        db.expose = vec![ExposeSpec {
            port: 5432,
            protocol: "tcp".into(),
            name: None,
        }];
        let mut redis = bare_spec();
        redis.expose = vec![ExposeSpec {
            port: 6379,
            protocol: "tcp".into(),
            name: None,
        }];

        let web_inst = inst("i-web", "web", Some("n1"), "Running", &web);
        let db_inst = inst("i-db", "db", Some("n1"), "Running", &db);
        let redis_inst = inst("i-redis", "redis", Some("n1"), "Running", &redis);

        let plan = build_fabric_desired(
            &web_inst,
            &web,
            &[web_inst.clone(), db_inst.clone(), redis_inst.clone()],
        );
        // web reaches db:5432 and redis:6379 — no allow declared.
        let mut edges: Vec<(String, u16)> = plan
            .allows
            .iter()
            .map(|a| (a.to_service.clone(), a.port))
            .collect();
        edges.sort();
        assert_eq!(
            edges,
            vec![("db".to_string(), 5432), ("redis".to_string(), 6379)]
        );
        assert!(plan.allows.iter().all(|a| a.backend_local));
        let db_edge = plan.allows.iter().find(|a| a.to_service == "db").unwrap();
        let redis_edge = plan
            .allows
            .iter()
            .find(|a| a.to_service == "redis")
            .unwrap();
        assert_eq!(db_edge.fqdn, "db.shop.svc.mc2");
        assert_eq!(redis_edge.fqdn, "redis.shop.svc.mc2");
    }

    #[test]
    fn fabric_plan_skips_self_and_portless_peers() {
        let web = bare_spec();
        let worker = bare_spec(); // no expose
        let web_inst = inst("i-web", "web", Some("n1"), "Running", &web);
        let worker_inst = inst("i-worker", "worker", Some("n1"), "Running", &worker);

        let plan = build_fabric_desired(&web_inst, &web, &[web_inst.clone(), worker_inst.clone()]);
        // No self edge, no edge to a service that exposes nothing.
        assert!(plan.allows.is_empty(), "{:?}", plan.allows);
    }

    #[test]
    fn fabric_plan_marks_cross_node_when_peer_remote() {
        let web = bare_spec();
        let mut db = bare_spec();
        db.expose = vec![ExposeSpec {
            port: 5432,
            protocol: "tcp".into(),
            name: None,
        }];
        let web_inst = inst("i-web", "web", Some("n1"), "Running", &web);
        let db_remote = inst("i-db", "db", Some("n2"), "Running", &db);

        let plan = build_fabric_desired(&web_inst, &web, &[db_remote]);
        assert_eq!(plan.allows.len(), 1);
        assert_eq!(plan.allows[0].backend_instance_id, "i-db");
        assert!(!plan.allows[0].backend_local, "backend on a different node");
    }

    #[test]
    fn fabric_plan_prefers_lowest_ordinal_backend() {
        let web = bare_spec();
        let mut db = bare_spec();
        db.expose = vec![ExposeSpec {
            port: 5432,
            protocol: "tcp".into(),
            name: None,
        }];
        let web_inst = inst("i-web", "web", Some("n1"), "Running", &web);
        let db0 = inst("i-db-0", "db", Some("n1"), "Scheduled", &db);
        let db1 = inst("i-db-1", "db", Some("n1"), "Running", &db);

        let plan = build_fabric_desired(&web_inst, &web, &[db0, db1]);
        assert_eq!(plan.allows.len(), 1);
        assert_eq!(plan.allows[0].backend_instance_id, "i-db-0");
        assert!(plan.allows[0].backend_local);
    }

    #[test]
    fn cross_stack_shared_network_edges() {
        // Stack A's `web` and stack B's `db` join the same named network.
        let mut web = bare_spec();
        web.networks = vec!["backend".into()];
        let mut db = bare_spec();
        db.networks = vec!["backend".into()];
        db.expose = vec![ExposeSpec {
            port: 5432,
            protocol: "tcp".into(),
            name: None,
        }];

        let web_inst = inst_in("a", "a-web", "web", Some("n1"), "Running", &web);
        let db_inst = inst_in("b", "b-db", "db", Some("n1"), "Running", &db);
        let plan = build_fabric_desired(&web_inst, &web, std::slice::from_ref(&db_inst));
        assert_eq!(plan.allows.len(), 1);
        assert_eq!(plan.allows[0].fqdn, "db.backend.svc.mc2");
        assert_eq!(plan.allows[0].backend_instance_id, "b-db");

        // A peer that shares no network is not reachable (cross-stack isolation).
        let mut other = bare_spec();
        other.expose = vec![ExposeSpec {
            port: 9000,
            protocol: "tcp".into(),
            name: None,
        }];
        let other_inst = inst_in("c", "c-other", "other", Some("n1"), "Running", &other);
        let plan2 = build_fabric_desired(&web_inst, &web, &[db_inst, other_inst]);
        assert_eq!(plan2.allows.len(), 1, "no edge to an unshared-network peer");
        assert_eq!(plan2.allows[0].to_service, "db");
    }

    #[test]
    fn same_stack_keeps_default_network_fqdn() {
        // Same-stack peers keep `<svc>.<stack>.svc.mc2` even when both also
        // join a named network (back-compat DNS).
        let mut web = bare_spec();
        web.networks = vec!["backend".into()];
        let mut db = bare_spec();
        db.networks = vec!["backend".into()];
        db.expose = vec![ExposeSpec {
            port: 5432,
            protocol: "tcp".into(),
            name: None,
        }];
        let web_inst = inst("a-web", "web", Some("n1"), "Running", &web);
        let db_inst = inst("a-db", "db", Some("n1"), "Running", &db);
        let plan = build_fabric_desired(&web_inst, &web, &[db_inst]);
        assert_eq!(plan.allows[0].fqdn, "db.shop.svc.mc2");
    }
}
