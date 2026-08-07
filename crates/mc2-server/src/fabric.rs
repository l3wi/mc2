//! Build per-instance fabric plans for agent Sync (D13).

use mc2_api::agent::{FabricAllow, FabricDesired, FabricExpose};
use mc2_api::{fabric_fqdn, ServiceSpec};
use mc2_store::InstanceRecord;
use std::collections::HashMap;

/// Resolve fabric desired state for one instance given the full stack instance set.
pub fn build_fabric_desired(
    inst: &InstanceRecord,
    spec: &ServiceSpec,
    peers: &[InstanceRecord],
) -> FabricDesired {
    let exposes: Vec<FabricExpose> = spec
        .expose
        .iter()
        .map(|e| FabricExpose {
            guest_port: u32::from(e.port),
            protocol: e.protocol.clone(),
        })
        .collect();

    // Index peers by service → instances (prefer lowest ordinal ready).
    let mut by_service: HashMap<String, Vec<&InstanceRecord>> = HashMap::new();
    for p in peers {
        if p.stack != inst.stack {
            continue;
        }
        by_service.entry(p.service.clone()).or_default().push(p);
    }
    for list in by_service.values_mut() {
        list.sort_by_key(|i| i.ordinal);
    }

    let mut allows = Vec::new();
    for a in &spec.allow {
        let backend = by_service
            .get(&a.to)
            .and_then(|list| {
                // Prefer lowest ordinal that is bound and not Failed/Stopped.
                list.iter()
                    .copied()
                    .find(|i| {
                        i.node_id.is_some()
                            && i.phase != "Failed"
                            && i.phase != "Stopped"
                            && i.phase != "Pending"
                    })
                    .or_else(|| list.first().copied())
            });

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

        allows.push(FabricAllow {
            to_service: a.to.clone(),
            port: u32::from(a.port),
            protocol: a.protocol.clone(),
            fqdn: fabric_fqdn(&inst.stack, &a.to),
            short_name: a.to.clone(),
            backend_instance_id,
            backend_node_id,
            backend_local,
            backend_ordinal,
        });
    }

    FabricDesired { exposes, allows }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mc2_api::{AllowSpec, ExposeSpec};

    fn bare_spec() -> ServiceSpec {
        ServiceSpec {
            image: "alpine".into(),
            replicas: 1,
            resources: Default::default(),
            ports: vec![],
            network: Default::default(),
            env: Default::default(),
            secrets: vec![],
            volumes: vec![],
            restart_policy: "on-failure".into(),
            health: None,
            labels: Default::default(),
            command: None,
            node_name: None,
            node_selector: Default::default(),
            ssh: None,
            expose: vec![],
            allow: vec![],
            networks: vec![],
        }
    }

    #[test]
    fn fabric_plan_marks_local_backend() {
        let mut web = bare_spec();
        web.allow = vec![AllowSpec {
            to: "db".into(),
            port: 5432,
            protocol: "tcp".into(),
        }];
        let mut db_spec = bare_spec();
        db_spec.expose = vec![ExposeSpec {
            port: 5432,
            protocol: "tcp".into(),
            name: None,
        }];

        let web_inst = InstanceRecord {
            id: "i-web".into(),
            stack: "shop".into(),
            service: "web".into(),
            ordinal: 0,
            node_id: Some("n1".into()),
            phase: "Running".into(),
            runtime_id: Some("shop-web-0".into()),
            message: None,
            spec_json: serde_json::to_string(&web).unwrap(),
            updated_at: "".into(),
        };
        let db_inst = InstanceRecord {
            id: "i-db".into(),
            stack: "shop".into(),
            service: "db".into(),
            ordinal: 0,
            node_id: Some("n1".into()),
            phase: "Running".into(),
            runtime_id: Some("shop-db-0".into()),
            message: None,
            spec_json: serde_json::to_string(&db_spec).unwrap(),
            updated_at: "".into(),
        };

        let plan = build_fabric_desired(&web_inst, &web, &[web_inst.clone(), db_inst]);
        assert_eq!(plan.allows.len(), 1);
        assert!(plan.allows[0].backend_local);
        assert_eq!(plan.allows[0].fqdn, "db.shop.svc.mc2");
        assert_eq!(plan.allows[0].backend_instance_id, "i-db");
    }

    #[test]
    fn fabric_plan_cross_node_not_local() {
        let mut web = bare_spec();
        web.allow = vec![AllowSpec {
            to: "db".into(),
            port: 5432,
            protocol: "tcp".into(),
        }];
        let web_inst = InstanceRecord {
            id: "i-web".into(),
            stack: "shop".into(),
            service: "web".into(),
            ordinal: 0,
            node_id: Some("n1".into()),
            phase: "Scheduled".into(),
            runtime_id: None,
            message: None,
            spec_json: "{}".into(),
            updated_at: "".into(),
        };
        let db_inst = InstanceRecord {
            id: "i-db".into(),
            stack: "shop".into(),
            service: "db".into(),
            ordinal: 0,
            node_id: Some("n2".into()),
            phase: "Running".into(),
            runtime_id: Some("shop-db-0".into()),
            message: None,
            spec_json: "{}".into(),
            updated_at: "".into(),
        };
        let plan = build_fabric_desired(&web_inst, &web, &[db_inst]);
        assert!(!plan.allows[0].backend_local);
    }
}
