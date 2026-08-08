//! Build per-node Ingress route plans for the node loop (D7).

use mc2_api::{make_ingress_route_id, parse_stack_yaml, IngressSpec, ServiceSpec};
use mc2_runtime::DesiredIngressRoute;
use mc2_store::InstanceRecord;
use std::collections::{HashMap, HashSet};

/// Build ingress routes for stacks that have at least one instance on `node_id`.
///
/// Backend selection: lowest ordinal among instances of the target service on
/// **this node** that are not Failed/Stopped (prefer Running).
pub fn build_ingress_routes_for_node(
    node_id: &str,
    stacks_yaml: &[(String, String)], // (stack_name, raw_yaml)
    instances: &[InstanceRecord],
) -> Vec<DesiredIngressRoute> {
    let mut out = Vec::new();

    let stacks_on_node: HashSet<String> = instances
        .iter()
        .filter(|i| i.node_id.as_deref() == Some(node_id))
        .map(|i| i.stack.clone())
        .collect();

    for (stack_name, raw_yaml) in stacks_yaml {
        if !stacks_on_node.contains(stack_name) {
            continue;
        }
        let Ok(doc) = parse_stack_yaml(raw_yaml) else {
            continue;
        };
        if let Some(ing) = doc.ingress.as_ref() {
            out.extend(routes_from_ingress(
                stack_name,
                ing,
                &doc.services,
                node_id,
                instances,
            ));
        }
        // `ports[].hostname` sugar ("mcp.example.com:3000") → routed hostname.
        out.extend(routes_from_port_hostnames(
            stack_name,
            &doc.services,
            node_id,
            instances,
        ));
    }

    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

/// `ports` entries with a `hostname` synthesize a TLS ingress route for that
/// hostname → the service's target port (backed by the resolved host port).
fn routes_from_port_hostnames(
    stack: &str,
    services: &std::collections::BTreeMap<String, ServiceSpec>,
    node_id: &str,
    instances: &[InstanceRecord],
) -> Vec<DesiredIngressRoute> {
    let mut by_service: HashMap<String, Vec<&InstanceRecord>> = HashMap::new();
    for i in instances {
        if i.stack != stack || i.node_id.as_deref() != Some(node_id) {
            continue;
        }
        by_service.entry(i.service.clone()).or_default().push(i);
    }
    for list in by_service.values_mut() {
        list.sort_by_key(|i| i.ordinal);
    }

    let mut routes = Vec::new();
    for (svc_name, svc) in services {
        for p in &svc.ports {
            let Some(ref host) = p.hostname else {
                continue;
            };
            let backend = by_service.get(svc_name).and_then(|list| {
                list.iter()
                    .copied()
                    .find(|i| i.phase == "Running")
                    .or_else(|| {
                        list.iter().copied().find(|i| {
                            i.phase != "Failed" && i.phase != "Stopped" && i.phase != "Pending"
                        })
                    })
                    .or_else(|| list.first().copied())
            });
            let (backend_instance_id, backend_ordinal) = match backend {
                Some(b) => (b.id.clone(), b.ordinal),
                None => (String::new(), 0),
            };
            routes.push(DesiredIngressRoute {
                id: make_ingress_route_id(stack, host, "/", svc_name, p.target),
                stack: stack.into(),
                host: host.clone(),
                path: "/".into(),
                path_type: "Prefix".into(),
                service: svc_name.clone(),
                guest_port: p.target,
                host_port: resolved_host_port(&by_service, svc_name, p.target, p.published),
                bind: "127.0.0.1".into(),
                tls_enabled: true,
                cert_resolver: "le".into(),
                tcp: false,
                entry_point: String::new(),
                backend_instance_id,
                backend_ordinal,
            });
        }
    }
    routes
}

/// The backend host port for a route. Prefers the resolved (auto-allocated)
/// `published` from the instance's stored spec over the raw YAML value (which
/// is `0` for target-only / hostname-sugar ports).
fn resolved_host_port(
    by_service: &HashMap<String, Vec<&InstanceRecord>>,
    service: &str,
    target: u16,
    fallback: u16,
) -> u16 {
    by_service
        .get(service)
        .and_then(|list| {
            list.iter().find_map(|i| {
                let spec: ServiceSpec = serde_json::from_str(&i.spec_json).ok()?;
                spec.ports
                    .iter()
                    .find(|p| p.target == target)
                    .map(|p| p.published)
            })
        })
        .filter(|p| *p != 0)
        .unwrap_or(fallback)
}

fn routes_from_ingress(
    stack: &str,
    ing: &IngressSpec,
    services: &std::collections::BTreeMap<String, ServiceSpec>,
    node_id: &str,
    instances: &[InstanceRecord],
) -> Vec<DesiredIngressRoute> {
    let tls_enabled = ing.tls.enabled;
    let cert_resolver = ing.tls.cert_resolver.clone().unwrap_or_default();

    let mut by_service: HashMap<String, Vec<&InstanceRecord>> = HashMap::new();
    for i in instances {
        if i.stack != stack || i.node_id.as_deref() != Some(node_id) {
            continue;
        }
        by_service.entry(i.service.clone()).or_default().push(i);
    }
    for list in by_service.values_mut() {
        list.sort_by_key(|i| i.ordinal);
    }

    let mut routes = Vec::new();
    for rule in &ing.rules {
        for path in &rule.paths {
            let Some(svc) = services.get(&path.service) else {
                continue;
            };
            let Some(ps) = svc.ports.iter().find(|p| p.target == path.port) else {
                continue;
            };

            let backend = by_service.get(&path.service).and_then(|list| {
                list.iter()
                    .copied()
                    .find(|i| i.phase == "Running")
                    .or_else(|| {
                        list.iter().copied().find(|i| {
                            i.phase != "Failed" && i.phase != "Stopped" && i.phase != "Pending"
                        })
                    })
                    .or_else(|| list.first().copied())
            });

            let (backend_instance_id, backend_ordinal) = match backend {
                Some(b) => (b.id.clone(), b.ordinal),
                None => (String::new(), 0),
            };

            let path_str = if path.path.trim().is_empty() {
                "/".to_string()
            } else {
                path.path.clone()
            };

            routes.push(DesiredIngressRoute {
                id: make_ingress_route_id(stack, &rule.host, &path_str, &path.service, path.port),
                stack: stack.into(),
                host: rule.host.clone(),
                path: path_str,
                path_type: path.path_type.clone(),
                service: path.service.clone(),
                guest_port: path.port,
                host_port: resolved_host_port(&by_service, &path.service, path.port, ps.published),
                bind: "127.0.0.1".into(),
                tls_enabled,
                cert_resolver: cert_resolver.clone(),
                tcp: false,
                entry_point: String::new(),
                backend_instance_id,
                backend_ordinal,
            });
        }
    }
    for tcp in &ing.tcp {
        let Some(svc) = services.get(&tcp.service) else {
            continue;
        };
        let Some(ssh) = svc.ssh.as_ref().filter(|ssh| ssh.enabled && ssh.port > 0) else {
            continue;
        };
        let backend = by_service.get(&tcp.service).and_then(|list| {
            list.iter()
                .copied()
                .find(|i| i.phase == "Running")
                .or_else(|| {
                    list.iter().copied().find(|i| {
                        i.phase != "Failed" && i.phase != "Stopped" && i.phase != "Pending"
                    })
                })
                .or_else(|| list.first().copied())
        });
        let (backend_instance_id, backend_ordinal) = backend
            .map(|b| (b.id.clone(), b.ordinal))
            .unwrap_or_default();
        routes.push(DesiredIngressRoute {
            id: format!("{stack}-tcp-{}-{}", tcp.name, tcp.service),
            stack: stack.into(),
            host: String::new(),
            path: String::new(),
            path_type: String::new(),
            service: tcp.service.clone(),
            guest_port: 0,
            host_port: ssh.port,
            bind: ssh.bind.clone(),
            tls_enabled: false,
            cert_resolver: String::new(),
            tcp: true,
            entry_point: tcp.entry_point.clone(),
            backend_instance_id,
            backend_ordinal,
        });
    }
    routes
}

#[cfg(test)]
mod tests {
    use super::*;
    use mc2_api::{IngressPath, IngressRule, IngressSpec, IngressTlsSpec, PortSpec};
    use std::collections::BTreeMap;

    fn bare_spec() -> ServiceSpec {
        ServiceSpec {
            image: "alpine".into(),
            scale: 1,
            cpus: 1.0,
            mem_limit_mib: 512,
            ports: vec![PortSpec {
                published: 8080,
                target: 8000,
                protocol: "tcp".into(),
                hostname: None,
            }],
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

    #[test]
    fn plan_picks_lowest_ordinal_on_node() {
        let mut services = std::collections::BTreeMap::new();
        services.insert("web".into(), bare_spec());
        let ing = IngressSpec {
            tls: IngressTlsSpec {
                enabled: true,
                cert_resolver: Some("le".into()),
            },
            rules: vec![IngressRule {
                host: "demo.local".into(),
                paths: vec![IngressPath {
                    path: "/".into(),
                    path_type: "Prefix".into(),
                    service: "web".into(),
                    port: 8000,
                }],
            }],
            tcp: vec![],
        };

        let instances = vec![
            InstanceRecord {
                id: "demo-web-1".into(),
                stack: "demo".into(),
                service: "web".into(),
                ordinal: 1,
                node_id: Some("n1".into()),
                phase: "Running".into(),
                runtime_id: None,
                message: None,
                spec_json: "{}".into(),
                healthy: false,
                updated_at: String::new(),
            },
            InstanceRecord {
                id: "demo-web-0".into(),
                stack: "demo".into(),
                service: "web".into(),
                ordinal: 0,
                node_id: Some("n1".into()),
                phase: "Running".into(),
                runtime_id: None,
                message: None,
                spec_json: "{}".into(),
                healthy: false,
                updated_at: String::new(),
            },
        ];

        let routes = routes_from_ingress("demo", &ing, &services, "n1", &instances);
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].backend_instance_id, "demo-web-0");
        assert_eq!(routes[0].host_port, 8080);
        assert_eq!(routes[0].host, "demo.local");
        assert_eq!(routes[0].guest_port, 8000);
    }

    fn stack_yaml_with_ingress(host: &str, guest: u16, host_port: u16) -> String {
        format!(
            r#"
name: demo
services:
  web:
    image: alpine
    ports:
      - "{host_port}:{guest}"
ingress:
  tls:
    enabled: false
  rules:
    - host: {host}
      paths:
        - path: /
          service: web
          port: {guest}
"#
        )
    }

    #[test]
    fn build_for_node_skips_other_nodes() {
        let yaml = stack_yaml_with_ingress("demo.local", 8000, 8080);
        let instances = vec![InstanceRecord {
            id: "demo-web-0".into(),
            stack: "demo".into(),
            service: "web".into(),
            ordinal: 0,
            node_id: Some("n-other".into()),
            phase: "Running".into(),
            runtime_id: None,
            message: None,
            spec_json: "{}".into(),
            healthy: false,
            updated_at: String::new(),
        }];
        let routes = build_ingress_routes_for_node("n1", &[("demo".into(), yaml)], &instances);
        assert!(routes.is_empty(), "no instances on n1");
    }

    #[test]
    fn build_maps_ports_guest_to_host() {
        let yaml = stack_yaml_with_ingress("app.local", 8000, 18080);
        let instances = vec![InstanceRecord {
            id: "demo-web-0".into(),
            stack: "demo".into(),
            service: "web".into(),
            ordinal: 0,
            node_id: Some("n1".into()),
            phase: "Running".into(),
            runtime_id: None,
            message: None,
            spec_json: "{}".into(),
            healthy: false,
            updated_at: String::new(),
        }];
        let routes = build_ingress_routes_for_node("n1", &[("demo".into(), yaml)], &instances);
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].guest_port, 8000);
        assert_eq!(routes[0].host_port, 18080);
        assert_eq!(routes[0].host, "app.local");
        assert_eq!(routes[0].bind, "127.0.0.1");
        assert_eq!(routes[0].backend_instance_id, "demo-web-0");
    }

    #[test]
    fn multi_path_routes() {
        let yaml = r#"
name: demo
services:
  web:
    image: alpine
    ports:
      - "8080:8000"
      - "8081:8001"
ingress:
  rules:
    - host: demo.local
      paths:
        - path: /api
          service: web
          port: 8001
        - path: /
          service: web
          port: 8000
"#;
        let instances = vec![InstanceRecord {
            id: "demo-web-0".into(),
            stack: "demo".into(),
            service: "web".into(),
            ordinal: 0,
            node_id: Some("n1".into()),
            phase: "Running".into(),
            runtime_id: None,
            message: None,
            spec_json: "{}".into(),
            healthy: false,
            updated_at: String::new(),
        }];
        let routes =
            build_ingress_routes_for_node("n1", &[("demo".into(), yaml.into())], &instances);
        assert_eq!(routes.len(), 2);
        let api = routes.iter().find(|r| r.path == "/api").unwrap();
        let root = routes.iter().find(|r| r.path == "/").unwrap();
        assert_eq!(api.host_port, 8081);
        assert_eq!(api.guest_port, 8001);
        assert_eq!(root.host_port, 8080);
        assert_eq!(root.guest_port, 8000);
    }

    #[test]
    fn no_ingress_in_yaml_yields_empty() {
        let yaml = r#"
name: demo
services:
  web:
    image: alpine
    ports:
      - "8080:8000"
"#;
        let instances = vec![InstanceRecord {
            id: "demo-web-0".into(),
            stack: "demo".into(),
            service: "web".into(),
            ordinal: 0,
            node_id: Some("n1".into()),
            phase: "Running".into(),
            runtime_id: None,
            message: None,
            spec_json: "{}".into(),
            healthy: false,
            updated_at: String::new(),
        }];
        let routes =
            build_ingress_routes_for_node("n1", &[("demo".into(), yaml.into())], &instances);
        assert!(routes.is_empty());
    }

    #[test]
    fn port_hostname_sugar_emits_tls_route() {
        let yaml = r#"
name: demo
services:
  web:
    image: alpine
    ports:
      - "mcp.example.com:3000"
"#;
        let instances = vec![InstanceRecord {
            id: "demo-web-0".into(),
            stack: "demo".into(),
            service: "web".into(),
            ordinal: 0,
            node_id: Some("n1".into()),
            phase: "Running".into(),
            runtime_id: None,
            message: None,
            spec_json: "{}".into(),
            healthy: false,
            updated_at: String::new(),
        }];
        let routes =
            build_ingress_routes_for_node("n1", &[("demo".into(), yaml.into())], &instances);
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].host, "mcp.example.com");
        assert_eq!(routes[0].guest_port, 3000);
        assert!(routes[0].tls_enabled);
        assert_eq!(routes[0].cert_resolver, "le");
        assert_eq!(routes[0].backend_instance_id, "demo-web-0");
    }

    #[test]
    fn resolved_host_port_prefers_instance_spec() {
        // Instance spec carries the auto-allocated `published`; the raw YAML
        // has `0` for target-only / hostname-sugar ports.
        let inst = InstanceRecord {
            id: "demo-web-0".into(),
            stack: "demo".into(),
            service: "web".into(),
            ordinal: 0,
            node_id: Some("n1".into()),
            phase: "Running".into(),
            runtime_id: None,
            message: None,
            spec_json: serde_json::json!({
                "image": "alpine",
                "scale": 1,
                "cpus": 1.0,
                "mem_limit": 512,
                "restart": "no",
                "ports": [{ "target": 3001, "published": 10023, "protocol": "tcp" }],
                "network": {}, "environment": {}, "secrets": [], "volumes": [],
                "healthcheck": null, "labels": {}, "command": null, "depends_on": {},
                "nodeName": null, "nodeSelector": {}, "ssh": null,
                "expose": [], "networks": []
            })
            .to_string(),
            healthy: false,
            updated_at: String::new(),
        };
        let by_service: HashMap<String, Vec<&InstanceRecord>> =
            HashMap::from([("web".into(), vec![&inst])]);

        assert_eq!(resolved_host_port(&by_service, "web", 3001, 0), 10023);
        // No instance port for this target → falls back to the raw value.
        assert_eq!(resolved_host_port(&by_service, "web", 3002, 0), 0);
        // Missing service → fallback.
        assert_eq!(resolved_host_port(&by_service, "other", 3001, 55), 55);
    }
}
