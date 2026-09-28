//! Build per-node Ingress route plans for the node loop (D7).

use mc2_api::{make_ingress_route_id, parse_stack_yaml, IngressSpec, ServiceSpec};
use mc2_runtime::DesiredIngressRoute;
use mc2_store::InstanceRecord;
use std::collections::{HashMap, HashSet};

/// Build ingress routes for stacks that have at least one instance on `node_id`.
///
/// Backend selection is shared by every route kind — see [`select_backend`].
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

/// The backend chosen for one route, together with the host port that route
/// should point at.
///
/// The port is resolved from the *selected* instance's stored spec, so a route
/// can never name one replica while pointing at another replica's port (B9).
struct SelectedBackend<'a> {
    instance: Option<&'a InstanceRecord>,
    host_port: u16,
}

impl SelectedBackend<'_> {
    fn instance_id(&self) -> String {
        self.instance.map(|i| i.id.clone()).unwrap_or_default()
    }

    fn ordinal(&self) -> u32 {
        self.instance.map(|i| i.ordinal).unwrap_or(0)
    }
}

/// The one backend selector shared by hostname-sugar, HTTP and TCP routes (B9).
///
/// Preference order: a Running replica whose healthcheck has passed (only
/// meaningful when the service declares one) → any Running replica → the
/// lowest ordinal. `host_port` is the `published` port the selected instance's
/// stored spec resolves `target` to, falling back to `fallback` (the raw YAML
/// value, which is `0` for target-only / hostname-sugar ports).
fn select_backend<'a>(
    by_service: &HashMap<String, Vec<&'a InstanceRecord>>,
    service: &str,
    healthcheck: bool,
    target: u16,
    fallback: u16,
) -> SelectedBackend<'a> {
    // `list` is sorted by ordinal, so "lowest ordinal" is its first element.
    let list: &[&InstanceRecord] = match by_service.get(service) {
        Some(list) => list,
        None => &[],
    };
    let chosen = list
        .iter()
        .copied()
        .find(|i| i.phase == "Running" && (!healthcheck || i.healthy))
        .or_else(|| list.iter().copied().find(|i| i.phase == "Running"))
        .or_else(|| list.first().copied());

    let host_port = chosen
        .and_then(|i| published_port(&i.spec_json, target))
        .filter(|p| *p != 0)
        .unwrap_or(fallback);

    SelectedBackend {
        instance: chosen,
        host_port,
    }
}

/// The `published` host port an instance's stored spec resolves `target` to.
fn published_port(spec_json: &str, target: u16) -> Option<u16> {
    let spec: ServiceSpec = serde_json::from_str(spec_json).ok()?;
    spec.ports
        .iter()
        .find(|p| p.target == target)
        .map(|p| p.published)
}

/// Instances of `stack` bound to `node_id`, grouped by service and sorted by
/// ordinal (the order [`select_backend`] relies on).
fn instances_by_service<'a>(
    stack: &str,
    node_id: &str,
    instances: &'a [InstanceRecord],
) -> HashMap<String, Vec<&'a InstanceRecord>> {
    let mut by_service: HashMap<String, Vec<&InstanceRecord>> = HashMap::new();
    for i in instances {
        if i.stack == stack && i.node_id.as_deref() == Some(node_id) {
            by_service.entry(i.service.clone()).or_default().push(i);
        }
    }
    for list in by_service.values_mut() {
        list.sort_by_key(|i| i.ordinal);
    }
    by_service
}

/// `ports` entries with a `hostname` synthesize a TLS ingress route for that
/// hostname → the service's target port (backed by the resolved host port).
fn routes_from_port_hostnames(
    stack: &str,
    services: &std::collections::BTreeMap<String, ServiceSpec>,
    node_id: &str,
    instances: &[InstanceRecord],
) -> Vec<DesiredIngressRoute> {
    let by_service = instances_by_service(stack, node_id, instances);

    let mut routes = Vec::new();
    for (svc_name, svc) in services {
        for p in &svc.ports {
            let Some(host) = &p.hostname else {
                continue;
            };
            let backend = select_backend(
                &by_service,
                svc_name,
                svc.healthcheck.is_some(),
                p.target,
                p.published,
            );
            routes.push(DesiredIngressRoute {
                id: make_ingress_route_id(stack, host, "/", svc_name, p.target),
                stack: stack.into(),
                host: host.clone(),
                path: "/".into(),
                path_type: "Prefix".into(),
                service: svc_name.clone(),
                guest_port: p.target,
                host_port: backend.host_port,
                bind: "127.0.0.1".into(),
                tls_enabled: true,
                cert_resolver: "le".into(),
                tcp: false,
                entry_point: String::new(),
                backend_instance_id: backend.instance_id(),
                backend_ordinal: backend.ordinal(),
            });
        }
    }
    routes
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

    let by_service = instances_by_service(stack, node_id, instances);

    let mut routes = Vec::new();
    for rule in &ing.rules {
        for path in &rule.paths {
            let Some(svc) = services.get(&path.service) else {
                continue;
            };
            let Some(ps) = svc.ports.iter().find(|p| p.target == path.port) else {
                continue;
            };

            let backend = select_backend(
                &by_service,
                &path.service,
                svc.healthcheck.is_some(),
                path.port,
                ps.published,
            );

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
                host_port: backend.host_port,
                bind: "127.0.0.1".into(),
                tls_enabled,
                cert_resolver: cert_resolver.clone(),
                tcp: false,
                entry_point: String::new(),
                backend_instance_id: backend.instance_id(),
                backend_ordinal: backend.ordinal(),
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
        // TCP routes target the host-side SSH endpoint, so the port is the
        // service's SSH port; only the instance choice comes from the selector.
        let backend = select_backend(
            &by_service,
            &tcp.service,
            svc.healthcheck.is_some(),
            0,
            ssh.port,
        );
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
            backend_instance_id: backend.instance_id(),
            backend_ordinal: backend.ordinal(),
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
            storage_opt: None,
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
                applied_hash: None,
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
                applied_hash: None,
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
            applied_hash: None,
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
            applied_hash: None,
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
            applied_hash: None,
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
            applied_hash: None,
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
            applied_hash: None,
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
    fn select_backend_uses_the_selected_instances_port() {
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
            applied_hash: None,
            updated_at: String::new(),
        };
        let by_service: HashMap<String, Vec<&InstanceRecord>> =
            HashMap::from([("web".into(), vec![&inst])]);

        let b = select_backend(&by_service, "web", false, 3001, 0);
        assert_eq!(b.instance_id(), "demo-web-0");
        assert_eq!(b.host_port, 10023);
        // No instance port for this target → falls back to the raw value.
        assert_eq!(
            select_backend(&by_service, "web", false, 3002, 0).host_port,
            0
        );
        // Missing service → fallback and no backend.
        let b = select_backend(&by_service, "other", false, 3001, 55);
        assert!(b.instance.is_none());
        assert_eq!(b.host_port, 55);
    }

    /// Instance with a stored spec resolving each `(target, published)` pair.
    fn web_instance(id: &str, ordinal: u32, phase: &str, ports: &[(u16, u16)]) -> InstanceRecord {
        let spec = ServiceSpec {
            ports: ports
                .iter()
                .map(|(target, published)| PortSpec {
                    published: *published,
                    target: *target,
                    protocol: "tcp".into(),
                    hostname: None,
                })
                .collect(),
            ..bare_spec()
        };
        InstanceRecord {
            id: id.into(),
            stack: "demo".into(),
            service: "web".into(),
            ordinal,
            node_id: Some("n1".into()),
            phase: phase.into(),
            runtime_id: None,
            message: None,
            spec_json: serde_json::to_string(&spec).unwrap(),
            healthy: false,
            applied_hash: None,
            updated_at: String::new(),
        }
    }

    /// B9: replica 0 Stopped on its own port, replica 1 Running on another.
    /// Hostname-sugar, HTTP and TCP routes must all name replica 1 *and* point
    /// at replica 1's port.
    #[test]
    fn failover_routes_all_kinds_to_the_running_replica() {
        let mut svc = bare_spec();
        svc.ports = vec![
            PortSpec {
                published: 8080,
                target: 8000,
                protocol: "tcp".into(),
                hostname: None,
            },
            PortSpec {
                published: 0,
                target: 9000,
                protocol: "tcp".into(),
                hostname: Some("app.local".into()),
            },
        ];
        svc.ssh = Some(mc2_api::SshSpec {
            enabled: true,
            bind: "127.0.0.1".into(),
            port: 2222,
            user: "root".into(),
            sftp: true,
            authorized_keys: vec![],
        });
        let services = BTreeMap::from([("web".to_string(), svc)]);

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
            tcp: vec![mc2_api::stack::IngressTcpRoute {
                name: "ssh".into(),
                entry_point: "ssh".into(),
                service: "web".into(),
            }],
        };

        // Each replica's stored spec carries its own resolved ports.
        let instances = vec![
            web_instance("demo-web-0", 0, "Stopped", &[(8000, 8080), (9000, 18080)]),
            web_instance("demo-web-1", 1, "Running", &[(8000, 8081), (9000, 18081)]),
        ];

        let http = routes_from_ingress("demo", &ing, &services, "n1", &instances);
        let http_rule = http.iter().find(|r| !r.tcp).unwrap();
        assert_eq!(http_rule.backend_instance_id, "demo-web-1");
        assert_eq!(http_rule.host_port, 8081, "must use replica 1's port");

        let tcp = http.iter().find(|r| r.tcp).unwrap();
        assert_eq!(tcp.backend_instance_id, "demo-web-1");
        assert_eq!(tcp.host_port, 2222, "TCP routes publish the SSH port");

        let sugar = routes_from_port_hostnames("demo", &services, "n1", &instances);
        assert_eq!(sugar.len(), 1);
        assert_eq!(sugar[0].host, "app.local");
        assert_eq!(sugar[0].backend_instance_id, "demo-web-1");
        assert_eq!(sugar[0].host_port, 18081, "must use replica 1's sugar port");
    }

    /// B9: with a healthcheck declared, a healthy Running replica wins over an
    /// unhealthy Running one even at a higher ordinal.
    #[test]
    fn selector_prefers_healthy_replica_when_service_has_a_healthcheck() {
        let mut healthy = web_instance("demo-web-1", 1, "Running", &[(8000, 8081)]);
        healthy.healthy = true;
        let instances = vec![
            web_instance("demo-web-0", 0, "Running", &[(8000, 8080)]),
            healthy,
        ];
        let by_service = instances_by_service("demo", "n1", &instances);

        assert_eq!(
            select_backend(&by_service, "web", true, 8000, 0).instance_id(),
            "demo-web-1"
        );
        // Without a healthcheck the field is meaningless → lowest ordinal.
        assert_eq!(
            select_backend(&by_service, "web", false, 8000, 0).instance_id(),
            "demo-web-0"
        );
    }
}
