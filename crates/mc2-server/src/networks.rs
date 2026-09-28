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
use std::collections::{BTreeMap, BTreeSet};

/// `GET /v1/networks` response.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NetworksView {
    pub networks: Vec<NetworkView>,
    /// Every claimed `expose` port and its owning stack/service. Claims are
    /// exclusive server-wide, so this is the authoritative owner map.
    pub exposed_ports: Vec<NetworkPortClaimView>,
}

/// One claimed east–west port (`expose`) and its owner.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NetworkPortClaimView {
    pub port: u16,
    pub stack: String,
    pub service: String,
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
/// Generates a default-allow mesh: an edge to the **owner** of every exposed
/// port on a network this instance shares, plus an edge to its own service's
/// exposed ports (replicas of one service reach each other). Exposed ports are
/// exclusive server-wide (enforced at apply), so a port has exactly one owner
/// and its guest `gateway:P` → host `127.0.0.1:P` splice cannot be shared with
/// another stack. Output is sorted (`BTreeMap`/`BTreeSet`) so the plan — and the
/// recreate hash derived from it — is independent of store iteration order.
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

    #[derive(Default)]
    struct Owner {
        ports: BTreeSet<u16>,
        networks: BTreeSet<String>,
    }

    // Index every exposed port by its owning `(stack, service)`.
    let mut owners: BTreeMap<(String, String), Owner> = BTreeMap::new();
    for p in peers {
        let Ok(pspec) = serde_json::from_str::<ServiceSpec>(&p.spec_json) else {
            continue;
        };
        if pspec.expose.is_empty() {
            continue;
        }
        let owner = owners
            .entry((p.stack.clone(), p.service.clone()))
            .or_default();
        for e in &pspec.expose {
            owner.ports.insert(e.port);
        }
        owner.networks.extend(instance_networks(&p.stack, &pspec));
    }

    let mut allows = Vec::new();
    for ((owner_stack, owner_service), owner) in &owners {
        let is_self = owner_stack == &inst.stack && owner_service == &inst.service;
        if !is_self && my_networks.is_disjoint(&owner.networks) {
            continue; // no shared network → not reachable
        }
        // Same stack keeps the default `<stack>` DNS name (back-compat DNS);
        // a cross-stack peer is reached through a shared named network.
        let net = if owner_stack == &inst.stack {
            inst.stack.clone()
        } else {
            owner
                .networks
                .intersection(&my_networks)
                .find(|n| **n != inst.stack)
                .cloned()
                .unwrap_or_else(|| inst.stack.clone())
        };
        for port in &owner.ports {
            allows.push(NetworkAllowDesired {
                to_service: owner_service.clone(),
                port: *port,
                protocol: "tcp".into(),
                fqdn: mc2_api::network_fqdn(&net, owner_service),
                short_name: owner_service.clone(),
            });
        }
    }

    mc2_runtime::DesiredNetwork { exposes, allows }
}

/// Aggregate the desired set into per-network views.
pub async fn build_networks_view(store: &dyn Store) -> Result<NetworksView> {
    let mut acc: BTreeMap<String, NetworkAcc> = BTreeMap::new();
    // port → owning (stack, service); BTreeMap keeps the claims sorted.
    let mut claims: BTreeMap<u16, (String, String)> = BTreeMap::new();
    for inst in store.list_instances().await.context("list instances")? {
        let Ok(spec) = serde_json::from_str::<ServiceSpec>(&inst.spec_json) else {
            continue;
        };
        for e in &spec.expose {
            claims
                .entry(e.port)
                .or_insert_with(|| (inst.stack.clone(), inst.service.clone()));
        }
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
        exposed_ports: claims
            .into_iter()
            .map(|(port, (stack, service))| NetworkPortClaimView {
                port,
                stack,
                service,
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
    use mc2_store::{MemoryStore, StackPlan};

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
            storage_opt: None,
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
        // shop/db on named `backend` (no publish); shop/web on `backend` + a port.
        store
            .commit_stack_plan(&StackPlan::replicas(
                "shop",
                "{}",
                "yaml",
                vec![
                    ("db", vec![spec_json(&["backend"], &[5432], &[])]),
                    (
                        "web",
                        vec![spec_json(&["backend"], &[8080], &[(18080, 8000)])],
                    ),
                ],
            ))
            .await
            .unwrap();
        // billing/worker: no named networks → default only.
        store
            .commit_stack_plan(&StackPlan::replicas(
                "billing",
                "{}",
                "yaml",
                vec![("worker", vec![spec_json(&[], &[], &[])])],
            ))
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

        // Every claimed port names its owner, sorted.
        let claims: Vec<(u16, &str, &str)> = view
            .exposed_ports
            .iter()
            .map(|c| (c.port, c.stack.as_str(), c.service.as_str()))
            .collect();
        assert_eq!(claims, vec![(5432, "shop", "db"), (8080, "shop", "web")]);
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
            storage_opt: None,
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
            applied_hash: None,
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
        let db_edge = plan.allows.iter().find(|a| a.to_service == "db").unwrap();
        let redis_edge = plan
            .allows
            .iter()
            .find(|a| a.to_service == "redis")
            .unwrap();
        assert_eq!(db_edge.fqdn, "db.shop.svc.mc2");
        assert_eq!(redis_edge.fqdn, "redis.shop.svc.mc2");
    }

    /// B7: a service *without* `expose` still gets an egress edge (and DNS) for
    /// every peer port on its networks — being a pure consumer is not a reason
    /// to be isolated.
    #[test]
    fn plan_gives_a_portless_consumer_its_peer_edges() {
        let worker = bare_spec(); // no expose
        let mut db = bare_spec();
        db.expose = vec![ExposeSpec {
            port: 5432,
            protocol: "tcp".into(),
            name: None,
        }];
        let worker_inst = inst("i-worker", "worker", Some("n1"), "Running", &worker);
        let db_inst = inst("i-db", "db", Some("n1"), "Running", &db);

        let plan = build_network_desired(
            &worker_inst,
            &worker,
            &[worker_inst.clone(), db_inst.clone()],
        );
        assert!(plan.exposes.is_empty());
        assert_eq!(plan.allows.len(), 1, "{:?}", plan.allows);
        assert_eq!(plan.allows[0].to_service, "db");
        assert_eq!(plan.allows[0].port, 5432);
        assert_eq!(plan.allows[0].fqdn, "db.shop.svc.mc2");
    }

    /// B7: replicas of one service can reach their own service's ports (the
    /// shared splice round-robins across them).
    #[test]
    fn plan_includes_same_service_replicas() {
        let mut db = bare_spec();
        db.scale = 2;
        db.expose = vec![ExposeSpec {
            port: 5432,
            protocol: "tcp".into(),
            name: None,
        }];
        let db0 = inst("i-db-0", "db", Some("n1"), "Running", &db);
        let db1 = inst("i-db-1", "db", Some("n1"), "Running", &db);

        let plan = build_network_desired(&db0, &db, &[db0.clone(), db1]);
        assert_eq!(plan.allows.len(), 1, "{:?}", plan.allows);
        assert_eq!(plan.allows[0].to_service, "db");
        assert_eq!(plan.allows[0].port, 5432);
        assert_eq!(plan.allows[0].fqdn, "db.shop.svc.mc2");
    }

    /// B1: the plan (and therefore the recreate hash) must not depend on the
    /// order peers happen to arrive in.
    #[test]
    fn plan_is_order_invariant() {
        let mut web = bare_spec();
        web.scale = 2;
        web.expose = vec![ExposeSpec {
            port: 8080,
            protocol: "tcp".into(),
            name: None,
        }];
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

        let peers = vec![
            inst("i-web-1", "web", Some("n1"), "Running", &web),
            inst("i-db", "db", Some("n1"), "Running", &db),
            inst("i-redis", "redis", Some("n1"), "Running", &redis),
            inst("i-web-0", "web", Some("n1"), "Running", &web),
        ];
        let reversed: Vec<_> = peers.iter().rev().cloned().collect();

        let web_inst = peers[0].clone();
        let a = build_network_desired(&web_inst, &web, &peers);
        let b = build_network_desired(&web_inst, &web, &reversed);
        let edges = |p: &mc2_runtime::DesiredNetwork| -> Vec<(String, u16)> {
            p.allows
                .iter()
                .map(|x| (x.to_service.clone(), x.port))
                .collect()
        };
        assert_eq!(edges(&a), edges(&b));
        // Own service + both peers, sorted by owner.
        assert_eq!(
            edges(&a),
            vec![
                ("db".to_string(), 5432),
                ("redis".to_string(), 6379),
                ("web".to_string(), 8080),
            ]
        );
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
        assert_eq!(plan.allows[0].to_service, "db");

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
