//! Compose-like stack YAML schema (mc2/v1).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Top-level stack document.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StackDocument {
    pub api_version: String,
    pub kind: String,
    pub metadata: StackMetadata,
    pub services: BTreeMap<String, ServiceSpec>,
    #[serde(default)]
    pub volumes: BTreeMap<String, VolumeSpec>,
    /// Logical network membership only (not a free mesh). See D13.
    #[serde(default)]
    pub networks: BTreeMap<String, StackNetworkSpec>,
    /// North–south HTTP(S) routes (file catalog → Traefik). D7.
    #[serde(default)]
    pub ingress: Option<IngressSpec>,
}

/// Stack-level Ingress (BYO Traefik via agent file export).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IngressSpec {
    #[serde(default)]
    pub tls: IngressTlsSpec,
    #[serde(default)]
    pub rules: Vec<IngressRule>,
    /// Traefik TCP routes, currently intended for host-side SSH endpoints.
    #[serde(default)]
    pub tcp: Vec<IngressTcpRoute>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IngressTcpRoute {
    pub name: String,
    pub entry_point: String,
    pub service: String,
}

/// TLS intent only — certs/ACME live in the proxy.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IngressTlsSpec {
    #[serde(default)]
    pub enabled: bool,
    /// Traefik certificate resolver name (static config).
    #[serde(default)]
    pub cert_resolver: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IngressRule {
    pub host: String,
    #[serde(default)]
    pub paths: Vec<IngressPath>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IngressPath {
    #[serde(default = "default_ingress_path")]
    pub path: String,
    /// `Prefix` (default) or `Exact`.
    #[serde(default = "default_path_type")]
    pub path_type: String,
    pub service: String,
    /// Guest port; must match a `ports[].guest` on the service.
    pub port: u16,
}

fn default_ingress_path() -> String {
    "/".into()
}

fn default_path_type() -> String {
    "Prefix".into()
}

/// True if bind is loopback (catalog backends must stay on loopback in v1).
pub fn is_loopback_bind(bind: &str) -> bool {
    let b = bind.trim();
    b == "127.0.0.1" || b == "::1" || b.eq_ignore_ascii_case("localhost")
}

/// Stack-level network group (membership / docs only in v1).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StackNetworkSpec {
    /// Only `mediated` is valid (D13).
    #[serde(default = "default_network_mode")]
    pub mode: String,
}

fn default_network_mode() -> String {
    "mediated".into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StackMetadata {
    pub name: String,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceSpec {
    pub image: String,
    #[serde(default = "default_replicas")]
    pub replicas: u32,
    #[serde(default)]
    pub resources: ResourceSpec,
    #[serde(default)]
    pub ports: Vec<PortSpec>,
    #[serde(default)]
    pub network: NetworkSpec,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub secrets: Vec<SecretRef>,
    #[serde(default)]
    pub volumes: Vec<VolumeMount>,
    #[serde(default = "default_restart")]
    pub restart_policy: String,
    #[serde(default)]
    pub health: Option<HealthSpec>,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
    #[serde(default)]
    pub command: Option<Vec<String>>,
    /// Hard pin to a node name.
    #[serde(default)]
    pub node_name: Option<String>,
    /// Soft placement by node labels.
    #[serde(default)]
    pub node_selector: BTreeMap<String, String>,
    /// Host-side msb SSH serve (not guest sshd). Optional.
    #[serde(default)]
    pub ssh: Option<SshSpec>,
    /// Cluster-internal listeners (loopback publish; not LAN). D13 fabric.
    #[serde(default)]
    pub expose: Vec<ExposeSpec>,
    /// Explicit east–west allows (default deny). Same-stack only in v1.
    #[serde(default)]
    pub allow: Vec<AllowSpec>,
    /// Optional logical network membership names (not connectivity).
    #[serde(default)]
    pub networks: Vec<String>,
}

/// Internal service listener (fabric `expose`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExposeSpec {
    pub port: u16,
    #[serde(default = "default_proto")]
    pub protocol: String,
    #[serde(default)]
    pub name: Option<String>,
}

/// Client edge: this service may reach `to`:`port` (fabric `allow`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AllowSpec {
    pub to: String,
    pub port: u16,
    #[serde(default = "default_proto")]
    pub protocol: String,
}

/// Desired host-side SSH front end for a service (msb `ssh` feature).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SshSpec {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_ssh_bind")]
    pub bind: String,
    /// Host port on the agent; `0` = auto-allocate.
    #[serde(default)]
    pub port: u16,
    #[serde(default = "default_ssh_user")]
    pub user: String,
    #[serde(default = "default_true")]
    pub sftp: bool,
    /// Cluster key names (`mc2 ssh-key` / `/v1/ssh/keys`).
    #[serde(default)]
    pub authorized_keys: Vec<String>,
}

fn default_ssh_bind() -> String {
    "127.0.0.1".into()
}

fn default_ssh_user() -> String {
    "root".into()
}

fn default_true() -> bool {
    true
}

fn default_replicas() -> u32 {
    1
}

fn default_restart() -> String {
    "on-failure".into()
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceSpec {
    #[serde(default = "default_cpus")]
    pub cpus: u32,
    #[serde(default = "default_memory", rename = "memoryMiB")]
    pub memory_mib: u64,
}

fn default_cpus() -> u32 {
    1
}

fn default_memory() -> u64 {
    512
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PortSpec {
    pub host: u16,
    pub guest: u16,
    #[serde(default = "default_proto")]
    pub protocol: String,
    #[serde(default = "default_bind")]
    pub bind: String,
}

fn default_proto() -> String {
    "tcp".into()
}

fn default_bind() -> String {
    "127.0.0.1".into()
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NetworkSpec {
    #[serde(default)]
    pub profiles: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SecretRef {
    pub name: String,
    pub env: String,
    #[serde(default)]
    pub allow_hosts: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VolumeMount {
    pub name: String,
    pub mount: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VolumeSpec {
    #[serde(default = "default_vol_kind")]
    pub kind: String,
}

fn default_vol_kind() -> String {
    "dir".into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthSpec {
    pub kind: String,
    #[serde(default)]
    pub command: Vec<String>,
    #[serde(default = "default_health_interval")]
    pub interval_seconds: u32,
}

fn default_health_interval() -> u32 {
    30
}

/// Parse and validate a stack YAML document.
pub fn parse_stack_yaml(yaml: &str) -> Result<StackDocument, String> {
    let doc: StackDocument =
        serde_yaml::from_str(yaml).map_err(|e| format!("invalid stack YAML: {e}"))?;
    validate_stack(&doc)?;
    Ok(doc)
}

fn validate_stack(doc: &StackDocument) -> Result<(), String> {
    if doc.api_version != crate::API_VERSION {
        return Err(format!(
            "unsupported apiVersion {:?} (want {})",
            doc.api_version,
            crate::API_VERSION
        ));
    }
    if doc.kind != "Stack" {
        return Err(format!("unsupported kind {:?} (want Stack)", doc.kind));
    }
    if doc.metadata.name.trim().is_empty() {
        return Err("metadata.name is required".into());
    }
    if doc.services.is_empty() {
        return Err("services must not be empty".into());
    }
    if let Some(ref ing) = doc.ingress {
        validate_ingress(doc, ing)?;
    }
    for (net_name, net) in &doc.networks {
        let mode = net.mode.trim().to_ascii_lowercase();
        if mode != "mediated" {
            return Err(format!(
                "network {net_name}: mode must be mediated (got {:?})",
                net.mode
            ));
        }
    }
    for (name, svc) in &doc.services {
        if svc.image.trim().is_empty() {
            return Err(format!("service {name}: image is required"));
        }
        if svc.replicas == 0 {
            return Err(format!("service {name}: replicas must be >= 1 for MVP"));
        }
        let rp = svc.restart_policy.trim().to_ascii_lowercase();
        if !matches!(rp.as_str(), "always" | "on-failure" | "never") {
            return Err(format!(
                "service {name}: restartPolicy must be always|on-failure|never (got {:?})",
                svc.restart_policy
            ));
        }
        if let Some(ref h) = svc.health {
            let kind = h.kind.trim().to_ascii_lowercase();
            if !matches!(kind.as_str(), "exec" | "none") {
                return Err(format!(
                    "service {name}: health.kind must be exec|none (got {:?})",
                    h.kind
                ));
            }
            if kind == "exec" && h.command.is_empty() {
                return Err(format!(
                    "service {name}: health.kind=exec requires a non-empty command"
                ));
            }
        }
        for net in &svc.networks {
            if !doc.networks.contains_key(net) {
                return Err(format!(
                    "service {name}: networks entry {net:?} is not defined under stack networks"
                ));
            }
        }
        let mut expose_ports = std::collections::BTreeSet::new();
        for ex in &svc.expose {
            if ex.port == 0 {
                return Err(format!("service {name}: expose.port must be non-zero"));
            }
            if !ex.protocol.eq_ignore_ascii_case("tcp") {
                return Err(format!(
                    "service {name}: expose protocol must be tcp in v1 (got {:?})",
                    ex.protocol
                ));
            }
            if !expose_ports.insert(ex.port) {
                return Err(format!("service {name}: duplicate expose.port {}", ex.port));
            }
        }
        for a in &svc.allow {
            if a.to.trim().is_empty() {
                return Err(format!("service {name}: allow.to is required"));
            }
            if a.to == *name {
                return Err(format!(
                    "service {name}: allow.to cannot target the same service"
                ));
            }
            if !doc.services.contains_key(&a.to) {
                return Err(format!(
                    "service {name}: allow.to {:?} is not a service in this stack",
                    a.to
                ));
            }
            if a.port == 0 {
                return Err(format!("service {name}: allow.port must be non-zero"));
            }
            if !a.protocol.eq_ignore_ascii_case("tcp") {
                return Err(format!(
                    "service {name}: allow protocol must be tcp in v1 (got {:?})",
                    a.protocol
                ));
            }
            let target = &doc.services[&a.to];
            let ok = target.expose.iter().any(|e| e.port == a.port);
            if !ok {
                return Err(format!(
                    "service {name}: allow to {}:{} requires that service to expose port {}",
                    a.to, a.port, a.port
                ));
            }
        }
    }
    Ok(())
}

fn validate_ingress(doc: &StackDocument, ing: &IngressSpec) -> Result<(), String> {
    if ing.rules.is_empty() {
        if ing.tcp.is_empty() {
            return Err(
                "ingress.rules or ingress.tcp must not be empty when ingress is set".into(),
            );
        }
    }
    for (ti, route) in ing.tcp.iter().enumerate() {
        if route.name.trim().is_empty() || route.entry_point.trim().is_empty() {
            return Err(format!("ingress.tcp[{ti}] requires name and entryPoint"));
        }
        let Some(service) = doc.services.get(&route.service) else {
            return Err(format!(
                "ingress.tcp[{ti}]: service {:?} is not in this stack",
                route.service
            ));
        };
        let Some(ssh) = service.ssh.as_ref().filter(|ssh| ssh.enabled) else {
            return Err(format!(
                "ingress.tcp[{ti}]: service {:?} must have SSH enabled",
                route.service
            ));
        };
        if ssh.port == 0 {
            return Err(format!(
                "ingress.tcp[{ti}]: service {:?} SSH port must be fixed (not 0)",
                route.service
            ));
        }
    }
    for (ri, rule) in ing.rules.iter().enumerate() {
        if rule.host.trim().is_empty() {
            return Err(format!("ingress.rules[{ri}].host is required"));
        }
        if rule.paths.is_empty() {
            return Err(format!(
                "ingress.rules[{ri}] ({}) must have at least one path",
                rule.host
            ));
        }
        for (pi, path) in rule.paths.iter().enumerate() {
            let pt = path.path_type.trim().to_ascii_lowercase();
            if !matches!(pt.as_str(), "prefix" | "exact") {
                return Err(format!(
                    "ingress.rules[{ri}].paths[{pi}].pathType must be Prefix|Exact (got {:?})",
                    path.path_type
                ));
            }
            if path.service.trim().is_empty() {
                return Err(format!(
                    "ingress.rules[{ri}].paths[{pi}].service is required"
                ));
            }
            if path.port == 0 {
                return Err(format!(
                    "ingress.rules[{ri}].paths[{pi}].port must be non-zero"
                ));
            }
            let Some(svc) = doc.services.get(&path.service) else {
                return Err(format!(
                    "ingress.rules[{ri}].paths[{pi}]: service {:?} is not in this stack",
                    path.service
                ));
            };
            let port_spec = svc.ports.iter().find(|p| p.guest == path.port);
            let Some(ps) = port_spec else {
                return Err(format!(
                    "ingress.rules[{ri}].paths[{pi}]: service {:?} has no ports entry with guest port {} (declare ports: for north–south)",
                    path.service, path.port
                ));
            };
            if !ps.protocol.eq_ignore_ascii_case("tcp") {
                return Err(format!(
                    "ingress.rules[{ri}].paths[{pi}]: backend port must be tcp (got {:?})",
                    ps.protocol
                ));
            }
            if !is_loopback_bind(&ps.bind) {
                return Err(format!(
                    "ingress.rules[{ri}].paths[{pi}]: ports.bind must be loopback (127.0.0.1) for Ingress v1 (got {:?})",
                    ps.bind
                ));
            }
            if ps.host == 0 {
                return Err(format!(
                    "ingress.rules[{ri}].paths[{pi}]: ports.host must be non-zero for Ingress v1 (got 0)"
                ));
            }
        }
    }
    Ok(())
}

/// Fabric FQDN: `<service>.<stack>.svc.mc2`.
pub fn fabric_fqdn(stack: &str, service: &str) -> String {
    format!("{service}.{stack}.svc.mc2")
}

/// Normalize Ingress path (`""` → `/`).
pub fn normalize_ingress_path(path: &str) -> String {
    let p = path.trim();
    if p.is_empty() {
        "/".into()
    } else if p.starts_with('/') {
        p.to_string()
    } else {
        format!("/{p}")
    }
}

/// Stable Ingress route id (stack + host + path + service + guest port).
pub fn make_ingress_route_id(
    stack: &str,
    host: &str,
    path: &str,
    service: &str,
    guest_port: u16,
) -> String {
    let path = normalize_ingress_path(path);
    format!("{stack}-{host}-{path}-{service}-{guest_port}")
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEMO: &str = r#"
apiVersion: mc2/v1
kind: Stack
metadata:
  name: demo
services:
  web:
    image: python:3.12
    replicas: 2
    resources:
      cpus: 1
      memoryMiB: 512
"#;

    #[test]
    fn parse_demo() {
        let doc = parse_stack_yaml(DEMO).unwrap();
        assert_eq!(doc.metadata.name, "demo");
        assert_eq!(doc.services["web"].replicas, 2);
        assert_eq!(doc.services["web"].resources.memory_mib, 512);
    }

    #[test]
    fn accepts_ingress_with_ports() {
        let yaml = r#"
apiVersion: mc2/v1
kind: Stack
metadata:
  name: demo
services:
  web:
    image: alpine
    ports:
      - host: 8080
        guest: 8000
ingress:
  tls:
    enabled: true
    certResolver: le
  rules:
    - host: demo.local
      paths:
        - path: /
          service: web
          port: 8000
"#;
        let doc = parse_stack_yaml(yaml).unwrap();
        let ing = doc.ingress.unwrap();
        assert!(ing.tls.enabled);
        assert_eq!(ing.rules[0].host, "demo.local");
        assert_eq!(ing.rules[0].paths[0].service, "web");
    }

    #[test]
    fn rejects_ingress_missing_ports() {
        let yaml = r#"
apiVersion: mc2/v1
kind: Stack
metadata:
  name: x
services:
  web:
    image: busybox
ingress:
  rules:
    - host: x.local
      paths:
        - service: web
          port: 8000
"#;
        let err = parse_stack_yaml(yaml).unwrap_err();
        assert!(err.contains("ports"), "{err}");
    }

    #[test]
    fn rejects_ingress_non_loopback_bind() {
        let yaml = r#"
apiVersion: mc2/v1
kind: Stack
metadata:
  name: x
services:
  web:
    image: busybox
    ports:
      - host: 8080
        guest: 8000
        bind: 0.0.0.0
ingress:
  rules:
    - host: x.local
      paths:
        - service: web
          port: 8000
"#;
        let err = parse_stack_yaml(yaml).unwrap_err();
        assert!(err.contains("loopback"), "{err}");
    }

    #[test]
    fn rejects_empty_ingress_rules() {
        let yaml = r#"
apiVersion: mc2/v1
kind: Stack
metadata:
  name: x
services:
  web:
    image: busybox
ingress:
  rules: []
"#;
        let err = parse_stack_yaml(yaml).unwrap_err();
        assert!(err.contains("rules"), "{err}");
    }

    #[test]
    fn fabric_allow_requires_expose() {
        let yaml = r#"
apiVersion: mc2/v1
kind: Stack
metadata:
  name: shop
services:
  db:
    image: postgres:16
  web:
    image: alpine
    allow:
      - to: db
        port: 5432
"#;
        let err = parse_stack_yaml(yaml).unwrap_err();
        assert!(err.contains("expose"), "{err}");
    }

    #[test]
    fn fabric_allow_ok() {
        let yaml = r#"
apiVersion: mc2/v1
kind: Stack
metadata:
  name: shop
networks:
  backend:
    mode: mediated
services:
  db:
    image: postgres:16
    networks: [backend]
    expose:
      - port: 5432
  web:
    image: alpine
    networks: [backend]
    allow:
      - to: db
        port: 5432
"#;
        let doc = parse_stack_yaml(yaml).unwrap();
        assert_eq!(doc.services["db"].expose[0].port, 5432);
        assert_eq!(doc.services["web"].allow[0].to, "db");
        assert_eq!(fabric_fqdn("shop", "db"), "db.shop.svc.mc2");
    }

    #[test]
    fn fabric_unknown_network_name() {
        let yaml = r#"
apiVersion: mc2/v1
kind: Stack
metadata:
  name: shop
services:
  web:
    image: alpine
    networks: [missing]
"#;
        let err = parse_stack_yaml(yaml).unwrap_err();
        assert!(err.contains("not defined"), "{err}");
    }
}
