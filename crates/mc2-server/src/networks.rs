//! Server-wide network membership summary (drives `mc2 network`).
//!
//! Every instance is on its stack's implicit **default** network (named
//! `<stack>`) plus any **named** networks it joins (`services.<svc>.networks`),
//! matching the network builder's reachability rule. This module aggregates the
//! desired set into per-network views: member stacks, services, instances
//! (phase/health), network listeners (`expose`) and north–south host publishes.

use anyhow::{Context, Result};
use mc2_api::ServiceSpec;
use mc2_store::{InstanceRecord, Store};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet, HashMap};

/// `GET /v1/networks` response.
#[derive(Debug, Clone, Default, Serialize)]
pub struct NetworksView {
    pub networks: Vec<NetworkView>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NetworkView {
    pub name: String,
    /// `default` (the stack's implicit network) or `named` (server-wide).
    pub kind: String,
    pub stacks: Vec<String>,
    pub instances: Vec<NetworkInstanceView>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NetworkInstanceView {
    pub instance_id: String,
    pub stack: String,
    pub service: String,
    pub ordinal: u32,
    pub runtime_id: Option<String>,
    pub phase: String,
    pub healthy: bool,
    pub node_id: Option<String>,
    /// Network listeners (`expose`) reachable on this network.
    pub expose_ports: Vec<u16>,
    /// North–south host publishes (loopback), per-replica resolved.
    pub ports: Vec<NetworkPortView>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NetworkPortView {
    pub published: u16,
    pub target: u16,
    pub protocol: String,
}

struct NetworkAcc {
    kind: &'static str,
    stacks: BTreeSet<String>,
    instances: Vec<NetworkInstanceView>,
}

/// A service's network set: its stack's implicit default network + named
/// memberships (same rule as the network builder).
fn instance_networks(stack: &str, spec: &ServiceSpec) -> BTreeSet<String> {
    let mut nets: BTreeSet<String> = spec.networks.iter().cloned().collect();
    nets.insert(stack.to_string());
    nets
}

/// Resolve network desired state for one instance given the node's instance set.
///
/// Generates a default-allow mesh: an edge to every peer service (any stack)
/// that shares a network, for every guest port that peer exposes.
pub fn build_network_desired(
    inst: &InstanceRecord,
    spec: &ServiceSpec,
    peers: &[InstanceRecord],
) -> mc2_runtime::DesiredNetwork {
    use mc2_runtime::{NetworkAllowDesired, NetworkExposeDesired};
    let exposes: Vec<NetworkExposeDesired> = spec
        .expose
        .iter()
        .map(|e| NetworkExposeDesired {
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
            allows.push(NetworkAllowDesired {
                to_service: service.clone(),
                port,
                protocol: "tcp".into(),
                fqdn: mc2_api::network_fqdn(&net, &service),
                short_name: service.clone(),
                backend_instance_id: backend_instance_id.clone(),
                backend_node_id: backend_node_id.clone(),
                backend_local,
                backend_ordinal,
            });
        }
    }

    mc2_runtime::DesiredNetwork { exposes, allows }
}

/// Aggregate the desired set into per-network views.
pub async fn build_networks_view(store: &dyn Store) -> Result<NetworksView> {
    let mut acc: BTreeMap<String, NetworkAcc> = BTreeMap::new();
    for inst in store.list_instances().await.context("list instances")? {
        let Ok(spec) = serde_json::from_str::<ServiceSpec>(&inst.spec_json) else {
            continue;
        };
        let expose_ports: Vec<u16> = spec.expose.iter().map(|e| e.port).collect();
        let ports: Vec<NetworkPortView> = spec
            .ports
            .iter()
            .filter(|p| p.published != 0)
            .map(|p| NetworkPortView {
                published: p.published,
                target: p.target,
                protocol: p.protocol.clone(),
            })
            .collect();
        for net in instance_networks(&inst.stack, &spec) {
            let entry = acc.entry(net.clone()).or_insert_with(|| NetworkAcc {
                kind: "default",
                stacks: BTreeSet::new(),
                instances: Vec::new(),
            });
            // Explicit membership via `networks:` marks it a named network.
            if spec.networks.contains(&net) {
                entry.kind = "named";
            }
            entry.stacks.insert(inst.stack.clone());
            entry
                .instances
                .push(network_instance_view(&inst, &expose_ports, &ports));
        }
    }
    Ok(NetworksView {
        networks: acc
            .into_iter()
            .map(|(name, a)| NetworkView {
                name,
                kind: a.kind.into(),
                stacks: a.stacks.into_iter().collect(),
                instances: a.instances,
            })
            .collect(),
    })
}

fn network_instance_view(
    inst: &InstanceRecord,
    expose_ports: &[u16],
    ports: &[NetworkPortView],
) -> NetworkInstanceView {
    NetworkInstanceView {
        instance_id: inst.id.clone(),
        stack: inst.stack.clone(),
        service: inst.service.clone(),
        ordinal: inst.ordinal,
        runtime_id: inst.runtime_id.clone(),
        phase: inst.phase.clone(),
        healthy: inst.healthy,
        node_id: inst.node_id.clone(),
        expose_ports: expose_ports.to_vec(),
        ports: ports.to_vec(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mc2_api::{ExposeSpec, NetworkSpec, PortSpec, ServiceSpec};
    use mc2_store::MemoryStore;

    fn spec_json(networks: &[&str], expose: &[u16], ports: &[(u16, u16)]) -> String {
        let s = ServiceSpec {
            image: "alpine".into(),
            scale: 1,
            cpus: 1.0,
            mem_limit_mib: 512,
            ports: ports
                .iter()
                .map(|&(p, t)| PortSpec {
                    published: p,
                    target: t,
                    protocol: "tcp".into(),
                    hostname: None,
                })
                .collect(),
            network: NetworkSpec::default(),
            env: BTreeMap::new(),
            secrets: vec![],
            volumes: vec![],
            restart: "no".into(),
            healthcheck: None,
            labels: BTreeMap::new(),
            command: None,
            node_name: None,
            node_selector: BTreeMap::new(),
            ssh: None,
            expose: expose
                .iter()
                .map(|&p| ExposeSpec {
                    port: p,
                    protocol: "tcp".into(),
                    name: None,
                })
                .collect(),
            networks: networks.iter().map(|n| n.to_string()).collect(),
            depends_on: BTreeMap::new(),
        };
        serde_json::to_string(&s).unwrap()
    }

    fn find<'a>(view: &'a NetworksView, name: &str) -> &'a NetworkView {
        view.networks.iter().find(|n| n.name == name).unwrap()
    }

    #[tokio::test]
    async fn aggregates_default_and_named_networks() {
        let store = MemoryStore::new();
        store.init_cluster("").await.unwrap();
        store.upsert_stack("shop", "{}", "yaml").await.unwrap();
        store.upsert_stack("billing", "{}", "yaml").await.unwrap();

        // shop/db on named `backend` (no publish); shop/web on `backend` + a port.
        store
            .reconcile_service_replicas_multi(
                "shop",
                "db",
                &[spec_json(&["backend"], &[5432], &[])],
            )
            .await
            .unwrap();
        store
            .reconcile_service_replicas_multi(
                "shop",
                "web",
                &[spec_json(&["backend"], &[8080], &[(18080, 8000)])],
            )
            .await
            .unwrap();
        // billing/worker: no named networks → default only.
        store
            .reconcile_service_replicas_multi("billing", "worker", &[spec_json(&[], &[], &[])])
            .await
            .unwrap();

        let view = build_networks_view(store.as_ref()).await.unwrap();

        // Named `backend` spans shop only here, is `named`, with db + web.
        let backend = find(&view, "backend");
        assert_eq!(backend.kind, "named");
        assert_eq!(backend.stacks, vec!["shop"]);
        assert_eq!(backend.instances.len(), 2);

        let db = backend
            .instances
            .iter()
            .find(|i| i.service == "db")
            .unwrap();
        assert_eq!(db.expose_ports, vec![5432]);
        assert!(db.ports.is_empty());

        let web = backend
            .instances
            .iter()
            .find(|i| i.service == "web")
            .unwrap();
        assert_eq!(web.expose_ports, vec![8080]);
        assert_eq!(web.ports[0].published, 18080);
        assert_eq!(web.ports[0].target, 8000);

        // Default networks: one per stack, marked `default`.
        let shop_default = find(&view, "shop");
        assert_eq!(shop_default.kind, "default");
        assert_eq!(shop_default.instances.len(), 2);
        let billing_default = find(&view, "billing");
        assert_eq!(billing_default.kind, "default");
        assert_eq!(billing_default.instances.len(), 1);
    }
}

#[cfg(test)]
mod desired_tests {
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
    fn plan_full_mesh_no_allow_needed() {
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

        let plan = build_network_desired(
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
    fn plan_skips_self_and_portless_peers() {
        let web = bare_spec();
        let worker = bare_spec(); // no expose
        let web_inst = inst("i-web", "web", Some("n1"), "Running", &web);
        let worker_inst = inst("i-worker", "worker", Some("n1"), "Running", &worker);

        let plan = build_network_desired(&web_inst, &web, &[web_inst.clone(), worker_inst.clone()]);
        // No self edge, no edge to a service that exposes nothing.
        assert!(plan.allows.is_empty(), "{:?}", plan.allows);
    }

    #[test]
    fn plan_marks_cross_node_when_peer_remote() {
        let web = bare_spec();
        let mut db = bare_spec();
        db.expose = vec![ExposeSpec {
            port: 5432,
            protocol: "tcp".into(),
            name: None,
        }];
        let web_inst = inst("i-web", "web", Some("n1"), "Running", &web);
        let db_remote = inst("i-db", "db", Some("n2"), "Running", &db);

        let plan = build_network_desired(&web_inst, &web, &[db_remote]);
        assert_eq!(plan.allows.len(), 1);
        assert_eq!(plan.allows[0].backend_instance_id, "i-db");
        assert!(!plan.allows[0].backend_local, "backend on a different node");
    }

    #[test]
    fn plan_prefers_lowest_ordinal_backend() {
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

        let plan = build_network_desired(&web_inst, &web, &[db0, db1]);
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
        let plan = build_network_desired(&web_inst, &web, std::slice::from_ref(&db_inst));
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
        let plan2 = build_network_desired(&web_inst, &web, &[db_inst, other_inst]);
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
        let plan = build_network_desired(&web_inst, &web, &[db_inst]);
        assert_eq!(plan.allows[0].fqdn, "db.shop.svc.mc2");
    }
}
