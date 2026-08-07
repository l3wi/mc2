//! Pure Ingress catalog → Traefik dynamic YAML + Caddyfile (D7).
//!
//! No I/O except callers writing the strings. Unit-tested without a hypervisor.

use serde::Serialize;
use std::collections::BTreeMap;
use std::fmt::Write as _;

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
    pub caddy_tls: String,
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
    pub caddy_tls: String,
}

/// Normalize path for proxy rules (`""` → `/`).
pub fn normalize_path(path: &str) -> String {
    mc2_api::normalize_ingress_path(path)
}

/// Stable router/service name safe for Traefik keys.
pub fn route_key(id: &str) -> String {
    id.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

/// Build Traefik file-provider dynamic YAML (`http.routers` + `http.services`).
pub fn render_traefik_dynamic(routes: &[ReadyIngressRoute]) -> String {
    if routes.is_empty() {
        return "# Managed by MC2 — no ready Ingress routes\nhttp: {}\n".into();
    }

    let mut out = String::from("# Managed by MC2 — do not edit; agent overwrites.\nhttp:\n  routers:\n");
    for r in routes {
        let key = route_key(&r.id);
        let path = normalize_path(&r.path);
        let rule = traefik_rule(&r.host, &path, &r.path_type);
        let _ = writeln!(out, "    {key}:");
        let _ = writeln!(out, "      rule: \"{rule}\"");
        if r.tls_enabled {
            let _ = writeln!(out, "      entryPoints:");
            let _ = writeln!(out, "        - websecure");
            let _ = writeln!(out, "      service: {key}");
            let _ = writeln!(out, "      tls:");
            if !r.cert_resolver.is_empty() {
                let _ = writeln!(out, "        certResolver: {}", r.cert_resolver);
            }
        } else {
            let _ = writeln!(out, "      entryPoints:");
            let _ = writeln!(out, "        - web");
            let _ = writeln!(out, "      service: {key}");
        }
    }
    out.push_str("  services:\n");
    for r in routes {
        let key = route_key(&r.id);
        let url = format!("http://{}:{}", r.backend_host, r.backend_port);
        let _ = writeln!(out, "    {key}:");
        let _ = writeln!(out, "      loadBalancer:");
        let _ = writeln!(out, "        servers:");
        let _ = writeln!(out, "          - url: \"{url}\"");
    }
    out
}

fn traefik_rule(host: &str, path: &str, path_type: &str) -> String {
    let host_esc = host.replace('`', "");
    let path_esc = path.replace('`', "");
    let exact = path_type.eq_ignore_ascii_case("exact");
    if path_esc == "/" && !exact {
        format!("Host(`{host_esc}`)")
    } else if exact {
        format!("Host(`{host_esc}`) && Path(`{path_esc}`)")
    } else {
        format!("Host(`{host_esc}`) && PathPrefix(`{path_esc}`)")
    }
}

/// Build a Caddyfile for ready routes (grouped by host).
pub fn render_caddyfile(routes: &[ReadyIngressRoute]) -> String {
    if routes.is_empty() {
        return "# Managed by MC2 — no ready Ingress routes\n".into();
    }

    let mut by_host: BTreeMap<String, Vec<&ReadyIngressRoute>> = BTreeMap::new();
    for r in routes {
        by_host.entry(r.host.clone()).or_default().push(r);
    }

    let mut out = String::from("# Managed by MC2 — do not edit; agent overwrites.\n");
    for (host, mut list) in by_host {
        // Longer paths first so more specific handle blocks win.
        list.sort_by(|a, b| {
            normalize_path(&b.path)
                .len()
                .cmp(&normalize_path(&a.path).len())
                .then_with(|| a.id.cmp(&b.id))
        });

        let tls_line = caddy_tls_line(list.first().copied());
        let _ = writeln!(out, "{host} {{");
        if let Some(line) = tls_line {
            let _ = writeln!(out, "\t{line}");
        }

        let multi = list.len() > 1
            || list
                .iter()
                .any(|r| normalize_path(&r.path) != "/" || r.path_type.eq_ignore_ascii_case("exact"));

        if !multi {
            let r = list[0];
            let _ = writeln!(
                out,
                "\treverse_proxy {}:{}",
                r.backend_host, r.backend_port
            );
        } else {
            for r in &list {
                let path = normalize_path(&r.path);
                if r.path_type.eq_ignore_ascii_case("exact") {
                    let _ = writeln!(out, "\thandle {path} {{");
                } else if path == "/" {
                    let _ = writeln!(out, "\thandle {{");
                } else {
                    // Prefix match
                    let prefix = if path.ends_with('/') {
                        path.clone()
                    } else {
                        format!("{path}*")
                    };
                    let _ = writeln!(out, "\thandle {prefix} {{");
                }
                let _ = writeln!(
                    out,
                    "\t\treverse_proxy {}:{}",
                    r.backend_host, r.backend_port
                );
                let _ = writeln!(out, "\t}}");
            }
        }
        out.push_str("}\n\n");
    }
    out
}

fn caddy_tls_line(r: Option<&ReadyIngressRoute>) -> Option<String> {
    let r = r?;
    if !r.tls_enabled {
        return Some("tls off".into());
    }
    let mode = r.caddy_tls.trim().to_ascii_lowercase();
    match mode.as_str() {
        "" | "auto" => None, // Caddy auto HTTPS
        "internal" => Some("tls internal".into()),
        "off" => Some("tls off".into()),
        other => Some(format!("tls {other}")),
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
            caddy_tls: r.caddy_tls.clone(),
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
    pub caddy_tls: String,
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
            caddy_tls: self.caddy_tls.clone(),
        }
    }
}

/// Stable id for a route (stack + host + path + service + guest port).
pub fn make_route_id(stack: &str, host: &str, path: &str, service: &str, guest_port: u16) -> String {
    mc2_api::make_ingress_route_id(stack, host, path, service, guest_port)
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
            instance_id: "demo-web-0".into(),
            ordinal: 0,
            tls_enabled: true,
            cert_resolver: "le".into(),
            caddy_tls: "internal".into(),
        }
    }

    #[test]
    fn traefik_renders_router_and_service() {
        let y = render_traefik_dynamic(&[sample_route()]);
        assert!(y.contains("Host(`demo.local`)"), "{y}");
        assert!(y.contains("http://127.0.0.1:8080"), "{y}");
        assert!(y.contains("certResolver: le"), "{y}");
        assert!(y.contains("websecure"), "{y}");
    }

    #[test]
    fn traefik_empty() {
        let y = render_traefik_dynamic(&[]);
        assert!(y.contains("http: {}"));
    }

    #[test]
    fn caddy_renders_site() {
        let c = render_caddyfile(&[sample_route()]);
        assert!(c.contains("demo.local {"), "{c}");
        assert!(c.contains("tls internal"), "{c}");
        assert!(c.contains("reverse_proxy 127.0.0.1:8080"), "{c}");
    }

    #[test]
    fn caddy_path_prefix() {
        let mut api = sample_route();
        api.id = "demo-demo.local-/api-web-8000".into();
        api.path = "/api".into();
        api.backend_port = 8081;
        let root = sample_route();
        let c = render_caddyfile(&[api, root]);
        assert!(c.contains("handle /api*"), "{c}");
        assert!(c.contains("handle {"), "{c}");
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
        let c = render_caddyfile(&[]);
        assert!(y.contains("http: {}"), "{y}");
        assert!(!c.contains("reverse_proxy"), "{c}");
        let j = render_catalog_json("n", "t", &[], &[pending]);
        assert!(j.contains("\"ready\": false"));
        assert!(j.contains("8080")); // backend port still recorded as pending
    }

    #[test]
    fn host_port_change_updates_upstream_url() {
        let mut r = sample_route();
        r.backend_port = 18080;
        let y1 = render_traefik_dynamic(&[r.clone()]);
        assert!(y1.contains("http://127.0.0.1:18080"), "{y1}");
        r.backend_port = 19090;
        let y2 = render_traefik_dynamic(&[r]);
        assert!(y2.contains("http://127.0.0.1:19090"), "{y2}");
        assert!(!y2.contains(":18080"), "{y2}");
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
            caddy_tls: "off".into(),
            backend_instance_id: "s-web-0".into(),
            backend_ordinal: 0,
        };
        let r = d.to_ready("127.0.0.1");
        assert_eq!(r.guest_port, 8000);
        assert_eq!(r.backend_port, 18080);
        assert_eq!(r.backend_host, "127.0.0.1");
        let y = render_traefik_dynamic(&[r.clone()]);
        assert!(y.contains("entryPoints:"), "{y}");
        assert!(y.contains("- web"), "{y}");
        assert!(!y.contains("websecure"), "{y}");
        let c = render_caddyfile(&[r]);
        assert!(c.contains("tls off"), "{c}");
        assert!(c.contains("reverse_proxy 127.0.0.1:18080"), "{c}");
    }

    #[test]
    fn multi_host_routes_separate_caddy_sites() {
        let mut a = sample_route();
        a.host = "a.local".into();
        a.id = "a".into();
        let mut b = sample_route();
        b.host = "b.local".into();
        b.id = "b".into();
        b.backend_port = 9090;
        let c = render_caddyfile(&[a, b]);
        assert!(c.contains("a.local {"), "{c}");
        assert!(c.contains("b.local {"), "{c}");
        assert!(c.contains(":8080"), "{c}");
        assert!(c.contains(":9090"), "{c}");
    }

    #[test]
    fn route_id_stable_for_catalog_key() {
        let id = make_route_id("smoke-ingress", "smoke-ingress.local", "/", "web", 8000);
        assert!(id.contains("smoke-ingress"));
        assert!(id.contains("web"));
        assert!(id.contains("8000"));
        assert_eq!(
            id,
            make_route_id("smoke-ingress", "smoke-ingress.local", "", "web", 8000)
        );
    }
}
