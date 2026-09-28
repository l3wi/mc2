//! Pure Ingress catalog → Traefik dynamic YAML (D7).
//!
//! No I/O except callers writing the strings. Unit-tested without a hypervisor.
//! The dynamic config is built as typed structs and serialized with
//! `serde_yaml`, so no untrusted value can inject extra YAML keys.

use serde::Serialize;
use std::collections::BTreeMap;

/// One route ready for proxy config (backend already selected + probed).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadyIngressRoute {
    pub id: String,
    pub stack: String,
    pub host: String,
    pub path: String,
    pub path_type: String,
    pub service: String,
    pub guest_port: u16,
    pub backend_host: String,
    pub backend_port: u16,
    pub instance_id: String,
    pub ordinal: u32,
    pub tls_enabled: bool,
    pub cert_resolver: String,
    pub tcp: bool,
    pub entry_point: String,
}

/// Debug catalog written next to proxy files.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IngressCatalog {
    pub generated_at: String,
    pub node_name: String,
    pub routes: Vec<CatalogRoute>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogRoute {
    pub id: String,
    pub stack: String,
    pub host: String,
    pub path: String,
    pub path_type: String,
    pub service: String,
    pub guest_port: u16,
    pub backend: CatalogBackend,
    pub ready: bool,
    pub tls: CatalogTls,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogBackend {
    pub host: String,
    pub port: u16,
    pub instance_id: String,
    pub ordinal: u32,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogTls {
    pub enabled: bool,
    pub cert_resolver: String,
}

// ---------------------------------------------------------------------------
// Typed Traefik file-provider dynamic config
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct TraefikDynamic {
    #[serde(skip_serializing_if = "HttpDynamic::is_empty")]
    http: HttpDynamic,
    #[serde(skip_serializing_if = "TcpDynamic::is_empty")]
    tcp: TcpDynamic,
}

#[derive(Debug, Default, Serialize)]
struct HttpDynamic {
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    routers: BTreeMap<String, HttpRouter>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    services: BTreeMap<String, HttpService>,
}

impl HttpDynamic {
    fn is_empty(&self) -> bool {
        self.routers.is_empty() && self.services.is_empty()
    }
}

#[derive(Debug, Serialize)]
struct HttpRouter {
    rule: String,
    #[serde(rename = "entryPoints")]
    entry_points: Vec<String>,
    service: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    tls: Option<HttpTls>,
}

#[derive(Debug, Serialize)]
struct HttpTls {
    #[serde(rename = "certResolver", skip_serializing_if = "Option::is_none")]
    cert_resolver: Option<String>,
}

#[derive(Debug, Serialize)]
struct HttpService {
    #[serde(rename = "loadBalancer")]
    load_balancer: HttpLoadBalancer,
}

#[derive(Debug, Serialize)]
struct HttpLoadBalancer {
    servers: Vec<HttpServer>,
}

#[derive(Debug, Serialize)]
struct HttpServer {
    url: String,
}

#[derive(Debug, Default, Serialize)]
struct TcpDynamic {
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    routers: BTreeMap<String, TcpRouter>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    services: BTreeMap<String, TcpService>,
}

impl TcpDynamic {
    fn is_empty(&self) -> bool {
        self.routers.is_empty() && self.services.is_empty()
    }
}

#[derive(Debug, Serialize)]
struct TcpRouter {
    #[serde(rename = "entryPoints")]
    entry_points: Vec<String>,
    rule: String,
    service: String,
}

#[derive(Debug, Serialize)]
struct TcpService {
    #[serde(rename = "loadBalancer")]
    load_balancer: TcpLoadBalancer,
}

#[derive(Debug, Serialize)]
struct TcpLoadBalancer {
    servers: Vec<TcpServer>,
}

#[derive(Debug, Serialize)]
struct TcpServer {
    address: String,
}

/// Normalize path for proxy rules (`""` → `/`).
fn normalize_path(path: &str) -> String {
    mc2_api::normalize_ingress_path(path)
}

/// Stable router/service name safe for Traefik keys.
pub fn route_key(id: &str) -> String {
    mc2_api::route_key(id)
}

/// First line of the dynamic file when no route is ready.
const EMPTY_DYNAMIC_MARKER: &str = "# Managed by MC2 — no ready Ingress routes";

/// Build Traefik file-provider dynamic YAML (`http.routers` + `http.services`).
pub fn render_traefik_dynamic(routes: &[ReadyIngressRoute]) -> String {
    if routes.is_empty() {
        // Comment only: Traefik rejects an empty `http: {}` ("http cannot be a
        // standalone element") and would fail the whole file provider.
        return format!("{EMPTY_DYNAMIC_MARKER}\n");
    }

    let mut http = HttpDynamic::default();
    let mut tcp = TcpDynamic::default();
    for r in routes {
        let key = route_key(&r.id);
        if r.tcp {
            tcp.routers.insert(
                key.clone(),
                TcpRouter {
                    entry_points: vec![r.entry_point.clone()],
                    rule: "HostSNI(`*`)".into(),
                    service: key.clone(),
                },
            );
            tcp.services.insert(
                key,
                TcpService {
                    load_balancer: TcpLoadBalancer {
                        servers: vec![TcpServer {
                            address: format!("{}:{}", r.backend_host, r.backend_port),
                        }],
                    },
                },
            );
        } else {
            let path = normalize_path(&r.path);
            let entry_points = if r.tls_enabled {
                vec!["websecure".to_string()]
            } else {
                vec!["web".to_string()]
            };
            let tls = if r.tls_enabled {
                Some(HttpTls {
                    cert_resolver: if r.cert_resolver.is_empty() {
                        None
                    } else {
                        Some(r.cert_resolver.clone())
                    },
                })
            } else {
                None
            };
            http.routers.insert(
                key.clone(),
                HttpRouter {
                    rule: traefik_rule(&r.host, &path, &r.path_type),
                    entry_points,
                    service: key.clone(),
                    tls,
                },
            );
            http.services.insert(
                key,
                HttpService {
                    load_balancer: HttpLoadBalancer {
                        servers: vec![HttpServer {
                            url: format!("http://{}:{}", r.backend_host, r.backend_port),
                        }],
                    },
                },
            );
        }
    }

    let doc = TraefikDynamic { http, tcp };
    let body = serde_yaml::to_string(&doc).unwrap_or_default();
    format!("# Managed by MC2 — do not edit; agent overwrites.\n{body}")
}

fn traefik_rule(host: &str, path: &str, path_type: &str) -> String {
    let exact = path_type.eq_ignore_ascii_case("exact");
    if path == "/" && !exact {
        format!("Host(`{host}`)")
    } else if exact {
        format!("Host(`{host}`) && Path(`{path}`)")
    } else {
        format!("Host(`{host}`) && PathPrefix(`{path}`)")
    }
}

/// Build `catalog.json` body (pretty JSON).
pub fn render_catalog_json(
    node_name: &str,
    generated_at: &str,
    ready: &[ReadyIngressRoute],
    pending: &[ReadyIngressRoute],
) -> String {
    let mut routes = Vec::new();
    for r in ready {
        routes.push(catalog_route(r, true));
    }
    for r in pending {
        routes.push(catalog_route(r, false));
    }
    routes.sort_by(|a, b| a.id.cmp(&b.id));
    let cat = IngressCatalog {
        generated_at: generated_at.into(),
        node_name: node_name.into(),
        routes,
    };
    serde_json::to_string_pretty(&cat).unwrap_or_else(|_| "{}".into())
}

fn catalog_route(r: &ReadyIngressRoute, ready: bool) -> CatalogRoute {
    CatalogRoute {
        id: r.id.clone(),
        stack: r.stack.clone(),
        host: r.host.clone(),
        path: normalize_path(&r.path),
        path_type: r.path_type.clone(),
        service: r.service.clone(),
        guest_port: r.guest_port,
        backend: CatalogBackend {
            host: r.backend_host.clone(),
            port: r.backend_port,
            instance_id: r.instance_id.clone(),
            ordinal: r.ordinal,
        },
        ready,
        tls: CatalogTls {
            enabled: r.tls_enabled,
            cert_resolver: r.cert_resolver.clone(),
        },
    }
}

/// Desired route from control plane (before local Ready gate).
#[derive(Debug, Clone)]
pub struct DesiredIngressRoute {
    pub id: String,
    pub stack: String,
    pub host: String,
    pub path: String,
    pub path_type: String,
    pub service: String,
    pub guest_port: u16,
    pub host_port: u16,
    pub bind: String,
    pub tls_enabled: bool,
    pub cert_resolver: String,
    pub tcp: bool,
    pub entry_point: String,
    pub backend_instance_id: String,
    pub backend_ordinal: u32,
}

impl DesiredIngressRoute {
    pub fn to_ready(&self, backend_host: &str) -> ReadyIngressRoute {
        ReadyIngressRoute {
            id: self.id.clone(),
            stack: self.stack.clone(),
            host: self.host.clone(),
            path: self.path.clone(),
            path_type: self.path_type.clone(),
            service: self.service.clone(),
            guest_port: self.guest_port,
            backend_host: backend_host.into(),
            backend_port: self.host_port,
            instance_id: self.backend_instance_id.clone(),
            ordinal: self.backend_ordinal,
            tls_enabled: self.tls_enabled,
            cert_resolver: self.cert_resolver.clone(),
            tcp: self.tcp,
            entry_point: self.entry_point.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_route() -> ReadyIngressRoute {
        ReadyIngressRoute {
            id: "demo-demo.local-/-web-8000".into(),
            stack: "demo".into(),
            host: "demo.local".into(),
            path: "/".into(),
            path_type: "Prefix".into(),
            service: "web".into(),
            guest_port: 8000,
            backend_host: "127.0.0.1".into(),
            backend_port: 8080,
            instance_id: "demo--web--0".into(),
            ordinal: 0,
            tls_enabled: true,
            cert_resolver: "le".into(),
            tcp: false,
            entry_point: String::new(),
        }
    }

    fn parse(yaml: &str) -> serde_yaml::Value {
        serde_yaml::from_str(yaml)
            .unwrap_or_else(|e| panic!("rendered YAML must parse: {e}\n{yaml}"))
    }

    /// Fetch a nested mapping key, failing loudly when absent.
    fn get<'a>(v: &'a serde_yaml::Value, path: &[&str]) -> &'a serde_yaml::Value {
        let mut cur = v;
        for p in path {
            cur = cur
                .as_mapping()
                .unwrap_or_else(|| panic!("{p}: parent is not a mapping: {cur:?}"))
                .get(serde_yaml::Value::String((*p).into()))
                .unwrap_or_else(|| panic!("missing key {p} in {cur:?}"));
        }
        cur
    }

    #[test]
    fn traefik_renders_router_and_service() {
        let y = render_traefik_dynamic(&[sample_route()]);
        let v = parse(&y);
        let key = route_key("demo-demo.local-/-web-8000");
        let routers = get(&v, &["http", "routers"]);
        assert_eq!(routers.as_mapping().unwrap().len(), 1, "{y}");
        assert_eq!(
            get(&v, &["http", "routers", &key, "rule"]).as_str(),
            Some("Host(`demo.local`)")
        );
        assert_eq!(
            get(&v, &["http", "routers", &key, "entryPoints"])[0].as_str(),
            Some("websecure")
        );
        assert_eq!(
            get(&v, &["http", "routers", &key, "service"]).as_str(),
            Some(key.as_str())
        );
        assert_eq!(
            get(&v, &["http", "routers", &key, "tls", "certResolver"]).as_str(),
            Some("le")
        );
        assert_eq!(
            get(&v, &["http", "services", &key, "loadBalancer", "servers"])[0]
                .get("url")
                .and_then(|u| u.as_str()),
            Some("http://127.0.0.1:8080")
        );
    }

    #[test]
    fn traefik_empty() {
        let y = render_traefik_dynamic(&[]);
        assert_eq!(y, format!("{EMPTY_DYNAMIC_MARKER}\n"));
    }

    #[test]
    fn traefik_tcp_route_renders_address() {
        let mut r = sample_route();
        r.id = "ssh-ingress-tcp-web-ssh-web".into();
        r.tcp = true;
        r.entry_point = "ssh".into();
        r.backend_port = 2222;
        r.tls_enabled = false;
        let y = render_traefik_dynamic(std::slice::from_ref(&r));
        let v = parse(&y);
        let key = route_key(&r.id);
        assert!(v.get("http").is_none(), "{y}");
        assert_eq!(
            get(&v, &["tcp", "routers", &key, "entryPoints"])[0].as_str(),
            Some("ssh")
        );
        assert_eq!(
            get(&v, &["tcp", "routers", &key, "rule"]).as_str(),
            Some("HostSNI(`*`)")
        );
        assert_eq!(
            get(&v, &["tcp", "services", &key, "loadBalancer", "servers"])[0]
                .get("address")
                .and_then(|a| a.as_str()),
            Some("127.0.0.1:2222")
        );
    }

    #[test]
    fn injection_payload_stays_a_yaml_scalar() {
        // Validation rejects such hosts at parse time; if one ever reaches the
        // renderer it must remain an inert scalar, not extra YAML structure.
        let mut r = sample_route();
        r.host = "evil.local\n        evil: {rule: \"boo\"}".into();
        let y = render_traefik_dynamic(&[r]);
        let v = parse(&y);
        let routers = v.get("http").unwrap().get("routers").unwrap();
        assert_eq!(routers.as_mapping().unwrap().len(), 1, "{y}");
    }

    #[test]
    fn catalog_json_ready_flag() {
        let ready = vec![sample_route()];
        let mut pending = sample_route();
        pending.id = "other".into();
        pending.host = "other.local".into();
        let j = render_catalog_json("node1", "2026-01-01T00:00:00Z", &ready, &[pending]);
        assert!(j.contains("\"ready\": true"));
        assert!(j.contains("\"ready\": false"));
        assert!(j.contains("demo.local"));
    }

    #[test]
    fn pending_only_not_in_proxy_configs() {
        // Ready gate: only ready routes appear as upstreams.
        let pending = sample_route();
        let y = render_traefik_dynamic(&[]);
        assert_eq!(y, format!("{EMPTY_DYNAMIC_MARKER}\n"), "{y}");
        let j = render_catalog_json("n", "t", &[], &[pending]);
        assert!(j.contains("\"ready\": false"));
        assert!(j.contains("8080")); // backend port still recorded as pending
    }

    #[test]
    fn host_port_change_updates_upstream_url() {
        let mut r = sample_route();
        r.backend_port = 18080;
        let y1 = render_traefik_dynamic(&[r.clone()]);
        let v1 = parse(&y1);
        let key = route_key(&r.id);
        assert_eq!(
            get(&v1, &["http", "services", &key, "loadBalancer", "servers"])[0]
                .get("url")
                .and_then(|u| u.as_str()),
            Some("http://127.0.0.1:18080")
        );
        r.backend_port = 19090;
        let y2 = render_traefik_dynamic(&[r]);
        assert!(y2.contains("19090"), "{y2}");
        assert!(!y2.contains("18080"), "{y2}");
    }

    #[test]
    fn desired_to_ready_maps_guest_and_host_ports() {
        let d = DesiredIngressRoute {
            id: "id".into(),
            stack: "s".into(),
            host: "h.local".into(),
            path: "/".into(),
            path_type: "Prefix".into(),
            service: "web".into(),
            guest_port: 8000,
            host_port: 18080,
            bind: "127.0.0.1".into(),
            tls_enabled: false,
            cert_resolver: String::new(),
            tcp: false,
            entry_point: String::new(),
            backend_instance_id: "s--web--0".into(),
            backend_ordinal: 0,
        };
        let r = d.to_ready("127.0.0.1");
        assert_eq!(r.guest_port, 8000);
        assert_eq!(r.backend_port, 18080);
        assert_eq!(r.backend_host, "127.0.0.1");
        let y = render_traefik_dynamic(std::slice::from_ref(&r));
        let v = parse(&y);
        let key = route_key("id");
        let entry = get(&v, &["http", "routers", &key, "entryPoints"]);
        assert_eq!(entry[0].as_str(), Some("web"));
        assert!(
            get(&v, &["http", "routers", &key])
                .as_mapping()
                .unwrap()
                .get(serde_yaml::Value::String("tls".into()))
                .is_none(),
            "plain router must not carry tls: {y}"
        );
    }

    #[test]
    fn route_id_stable_for_catalog_key() {
        let id = mc2_api::make_ingress_route_id(
            "smoke-ingress",
            "smoke-ingress.local",
            "/",
            "web",
            8000,
        );
        assert!(id.contains("smoke-ingress"));
        assert!(id.contains("web"));
        assert!(id.contains("8000"));
        assert_eq!(
            id,
            mc2_api::make_ingress_route_id("smoke-ingress", "smoke-ingress.local", "", "web", 8000)
        );
    }
}
