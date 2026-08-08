//! Compose-style stack YAML schema (mc2/v1).

use serde::{Deserialize, Deserializer, Serialize};
use std::collections::BTreeMap;

/// Top-level stack document (Docker Compose-shaped).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StackDocument {
    /// Stack name (compose `name:`; derived from the file when absent).
    #[serde(default)]
    pub name: String,
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
#[serde(rename_all = "camelCase", deny_unknown_fields)]
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
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IngressTcpRoute {
    pub name: String,
    pub entry_point: String,
    pub service: String,
}

/// TLS intent only — certs/ACME live in the proxy.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IngressTlsSpec {
    #[serde(default)]
    pub enabled: bool,
    /// Traefik certificate resolver name (static config).
    #[serde(default)]
    pub cert_resolver: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IngressRule {
    pub host: String,
    #[serde(default)]
    pub paths: Vec<IngressPath>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
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
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StackNetworkSpec {
    /// Only `mediated` is valid (D13).
    #[serde(default = "default_network_mode")]
    pub mode: String,
}

fn default_network_mode() -> String {
    "mediated".into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ServiceSpec {
    pub image: String,
    /// Replica count (compose `scale`).
    #[serde(default = "default_scale")]
    pub scale: u32,
    /// vCPUs (float, as compose `cpus`).
    #[serde(default = "default_cpus_f64")]
    pub cpus: f64,
    /// Guest memory MiB (compose `mem_limit`, accepts `512m`/`1g`/bytes).
    #[serde(
        default = "default_memory_mib",
        rename = "mem_limit",
        deserialize_with = "de_mem_limit"
    )]
    pub mem_limit_mib: u64,
    #[serde(default, deserialize_with = "de_ports")]
    pub ports: Vec<PortSpec>,
    #[serde(default)]
    pub network: NetworkSpec,
    #[serde(default, deserialize_with = "de_env")]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub secrets: Vec<SecretRef>,
    #[serde(default)]
    pub volumes: Vec<VolumeMount>,
    /// Restart policy (compose `restart`: no|on-failure|always|unless-stopped).
    #[serde(default = "default_restart")]
    pub restart: String,
    #[serde(default)]
    pub healthcheck: Option<HealthcheckSpec>,
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
    #[serde(default, deserialize_with = "de_expose")]
    pub expose: Vec<ExposeSpec>,
    /// Server-wide network membership (default-allow). Absent → the stack's
    /// implicit default network. Named networks span stacks.
    #[serde(default)]
    pub networks: Vec<String>,
}

/// Internal service listener (fabric `expose`). Compose list form
/// (`[5432]` / `["5432"]`) and map form (`{port, protocol, name}`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExposeSpec {
    pub port: u16,
    #[serde(default = "default_proto")]
    pub protocol: String,
    #[serde(default)]
    pub name: Option<String>,
}

fn de_expose<'de, D>(d: D) -> Result<Vec<ExposeSpec>, D::Error>
where
    D: Deserializer<'de>,
{
    use serde::de::Error;
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Entry {
        Num(u16),
        Str(String),
        Map(ExposeSpec),
    }
    let entries = Vec::<Entry>::deserialize(d)?;
    let mut out = Vec::with_capacity(entries.len());
    for e in entries {
        match e {
            Entry::Num(p) => out.push(ExposeSpec {
                port: p,
                protocol: "tcp".into(),
                name: None,
            }),
            Entry::Str(s) => {
                let (port, proto) = match s.split_once('/') {
                    Some((p, pr)) => (p, pr.to_ascii_lowercase()),
                    None => (s.as_str(), "tcp".to_string()),
                };
                let port = port.parse::<u16>().map_err(Error::custom)?;
                out.push(ExposeSpec {
                    port,
                    protocol: proto,
                    name: None,
                });
            }
            Entry::Map(m) => out.push(m),
        }
    }
    Ok(out)
}

/// Desired host-side SSH front end for a service (msb `ssh` feature).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
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

fn default_scale() -> u32 {
    1
}

fn default_restart() -> String {
    "no".into()
}

fn default_cpus_f64() -> f64 {
    1.0
}

fn default_memory_mib() -> u64 {
    512
}

/// Parse compose `mem_limit` (`512m`, `1g`, `1.5g`, or bytes) → MiB.
fn de_mem_limit<'de, D>(d: D) -> Result<u64, D::Error>
where
    D: Deserializer<'de>,
{
    use serde::de::Error;
    struct V;
    impl<'de> serde::de::Visitor<'de> for V {
        type Value = u64;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            write!(f, "a memory size (e.g. 512m, 1g, or bytes)")
        }
        fn visit_u64<E: Error>(self, v: u64) -> Result<u64, E> {
            Ok(v.div_ceil(1024 * 1024))
        }
        fn visit_str<E: Error>(self, s: &str) -> Result<u64, E> {
            let s = s.trim().to_ascii_lowercase();
            let (num, mult) = if let Some(n) = s.strip_suffix('g') {
                (n, 1024.0)
            } else if let Some(n) = s.strip_suffix('m') {
                (n, 1.0)
            } else if let Some(n) = s.strip_suffix('k') {
                (n, 1.0 / 1024.0)
            } else {
                (s.as_str(), 1.0 / (1024.0 * 1024.0))
            };
            let val: f64 = num.trim().parse().map_err(E::custom)?;
            Ok((val * mult).ceil() as u64)
        }
    }
    d.deserialize_any(V)
}

/// Accept compose `env` as a map or a `KEY=VALUE` list.
fn de_env<'de, D>(d: D) -> Result<BTreeMap<String, String>, D::Error>
where
    D: Deserializer<'de>,
{
    use serde::de::Error;
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Env {
        Map(BTreeMap<String, String>),
        List(Vec<String>),
    }
    match Env::deserialize(d)? {
        Env::Map(m) => Ok(m),
        Env::List(items) => {
            let mut out = BTreeMap::new();
            for item in items {
                let (k, v) = item.split_once('=').ok_or_else(|| {
                    Error::custom(format!("env entry {item:?} must be KEY=VALUE"))
                })?;
                out.insert(k.to_string(), v.to_string());
            }
            Ok(out)
        }
    }
}

/// Exec healthcheck (compose `healthcheck`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HealthcheckSpec {
    /// Probe command (CMD / CMD-SHELL prefix stripped). None → disabled.
    #[serde(default, deserialize_with = "de_healthcheck_test")]
    pub test: Option<Vec<String>>,
    /// Probe interval in seconds (compose `interval`, e.g. `30s`).
    #[serde(
        default = "default_health_interval",
        rename = "interval",
        deserialize_with = "de_interval"
    )]
    pub interval_seconds: u32,
}

fn de_healthcheck_test<'de, D>(d: D) -> Result<Option<Vec<String>>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Test {
        Str(String),
        Seq(Vec<String>),
    }
    let cmd: Vec<String> = match Test::deserialize(d)? {
        Test::Str(s) => vec!["/bin/sh".into(), "-c".into(), s],
        Test::Seq(seq) => seq,
    };
    let mut cmd = cmd;
    if let Some(first) = cmd.first().map(|s| s.to_ascii_lowercase()) {
        if first == "cmd" || first == "cmd-shell" {
            cmd.remove(0);
        }
    }
    if cmd.is_empty() {
        Ok(None)
    } else {
        Ok(Some(cmd))
    }
}

fn de_interval<'de, D>(d: D) -> Result<u32, D::Error>
where
    D: Deserializer<'de>,
{
    use serde::de::Error;
    struct V;
    impl<'de> serde::de::Visitor<'de> for V {
        type Value = u32;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            write!(f, "a duration (e.g. 30s)")
        }
        fn visit_u64<E: Error>(self, v: u64) -> Result<u32, E> {
            Ok(v.max(1) as u32)
        }
        fn visit_str<E: Error>(self, s: &str) -> Result<u32, E> {
            let s = s.trim().to_ascii_lowercase();
            let (num, mult) = if let Some(n) = s.strip_suffix("ms") {
                (n, 0.001)
            } else if let Some(n) = s.strip_suffix('s') {
                (n, 1.0)
            } else if let Some(n) = s.strip_suffix('m') {
                (n, 60.0)
            } else if let Some(n) = s.strip_suffix('h') {
                (n, 3600.0)
            } else {
                (s.as_str(), 1.0)
            };
            let val: f64 = num.trim().parse().map_err(E::custom)?;
            Ok((val * mult).max(1.0).ceil() as u32)
        }
    }
    d.deserialize_any(V)
}

fn default_health_interval() -> u32 {
    30
}

/// Port mapping (compose `ports`), north-south. `published: 0` = auto host
/// port (allocated server-side at apply). `hostname` = sugar: route that
/// hostname to `target` via ingress.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PortSpec {
    /// Host port; `0` = auto-allocate.
    #[serde(default)]
    pub published: u16,
    /// In-guest port.
    #[serde(default)]
    pub target: u16,
    #[serde(default = "default_proto")]
    pub protocol: String,
    /// Hostname sugar (`"mcp.example.com:3000"`) → ingress route to `target`.
    #[serde(default)]
    pub hostname: Option<String>,
}

fn de_ports<'de, D>(d: D) -> Result<Vec<PortSpec>, D::Error>
where
    D: Deserializer<'de>,
{
    use serde::de::Error;
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Entry {
        Short(String),
        Long {
            target: u16,
            published: Option<u16>,
            protocol: Option<String>,
            hostname: Option<String>,
        },
    }
    let entries = Vec::<Entry>::deserialize(d)?;
    let mut out = Vec::with_capacity(entries.len());
    for e in entries {
        match e {
            Entry::Long {
                target,
                published,
                protocol,
                hostname,
            } => {
                if published == Some(0) {
                    return Err(Error::custom(
                        "ports entry: published must be a non-zero host port",
                    ));
                }
                out.push(PortSpec {
                    published: published.unwrap_or(0),
                    target,
                    protocol: protocol.unwrap_or_else(|| "tcp".into()),
                    hostname,
                });
            }
            Entry::Short(s) => {
                let (rest, proto) = match s.split_once('/') {
                    Some((r, pr)) => (r, pr.to_ascii_lowercase()),
                    None => (s.as_str(), "tcp".to_string()),
                };
                let parts: Vec<&str> = rest.split(':').collect();
                let (published, target, hostname) = match parts.as_slice() {
                    // target-only → auto host port
                    [target] => (0, target.parse::<u16>().map_err(Error::custom)?, None),
                    // "published:target" or "hostname:target"
                    [a, target] => {
                        let target = target.parse::<u16>().map_err(Error::custom)?;
                        match a.parse::<u16>() {
                            Ok(published) => (published, target, None),
                            Err(_) => (0, target, Some((*a).to_string())),
                        }
                    }
                    // "ip:published:target" — ip advisory; always loopback
                    [_ip, published, target] => (
                        published.parse::<u16>().map_err(Error::custom)?,
                        target.parse::<u16>().map_err(Error::custom)?,
                        None,
                    ),
                    _ => {
                        return Err(Error::custom(format!(
                            "port {s:?}: expected [ip:]published:target[/protocol]"
                        )))
                    }
                };
                out.push(PortSpec {
                    published,
                    target,
                    protocol: proto,
                    hostname,
                });
            }
        }
    }
    Ok(out)
}

fn default_proto() -> String {
    "tcp".into()
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkSpec {
    #[serde(default)]
    pub profiles: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SecretRef {
    pub name: String,
    pub env: String,
    #[serde(default)]
    pub allow_hosts: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VolumeMount {
    pub name: String,
    /// Guest mount path (compose `target`).
    #[serde(rename = "target")]
    pub mount: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VolumeSpec {
    #[serde(default = "default_vol_kind")]
    pub kind: String,
}

fn default_vol_kind() -> String {
    "dir".into()
}

/// Parse and validate a stack YAML document (canonical compose-style schema).
pub fn parse_stack_yaml(yaml: &str) -> Result<StackDocument, String> {
    let doc: StackDocument =
        serde_yaml::from_str(yaml).map_err(|e| format!("invalid stack YAML: {e}"))?;
    validate_stack(&doc)?;
    Ok(doc)
}

fn validate_stack(doc: &StackDocument) -> Result<(), String> {
    if doc.name.trim().is_empty() {
        return Err("name is required".into());
    }
    if doc.services.is_empty() {
        return Err("services must not be empty".into());
    }
    if doc.name.contains("--") {
        return Err(format!(
            "invalid stack name {:?}: must not contain '--' (volume namespace separator)",
            doc.name
        ));
    }
    validate_volumes(doc)?;
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
        if svc.scale == 0 {
            return Err(format!("service {name}: scale must be >= 1 for MVP"));
        }
        let rp = svc.restart.trim().to_ascii_lowercase();
        if !matches!(
            rp.as_str(),
            "no" | "always" | "on-failure" | "unless-stopped"
        ) {
            return Err(format!(
                "service {name}: restart must be no|on-failure|always|unless-stopped (got {:?})",
                svc.restart
            ));
        }
        if let Some(ref hc) = svc.healthcheck {
            if let Some(ref test) = hc.test {
                if test.is_empty() {
                    return Err(format!(
                        "service {name}: healthcheck.test command must not be empty"
                    ));
                }
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
    }

    // Shared fabric splices bind one host loopback port per exposed guest port,
    // so exposed ports must be unique across the whole stack.
    {
        let mut stack_expose_ports = std::collections::BTreeSet::new();
        for svc in doc.services.values() {
            for ex in &svc.expose {
                if !stack_expose_ports.insert(ex.port) {
                    return Err(format!(
                        "expose port {} is used by multiple services in this stack \
                         (fabric ports must be unique across the stack)",
                        ex.port
                    ));
                }
            }
        }
    }
    Ok(())
}

/// Node-local persistent volumes (v1): declared `dir` volumes, absolute unique
/// mounts, names safe for the msb volume namespace `mc2-{stack}--{volume}`.
fn validate_volumes(doc: &StackDocument) -> Result<(), String> {
    for (name, vol) in &doc.volumes {
        if !valid_volume_name(name) {
            return Err(format!(
                "invalid volume name {name:?}: must match [a-z0-9][a-z0-9._-]* and must not contain '--'"
            ));
        }
        let kind = vol.kind.trim().to_ascii_lowercase();
        if kind != "dir" {
            return Err(format!(
                "volume {name}: unsupported kind {:?} (only dir is supported in v1)",
                vol.kind
            ));
        }
    }
    for (name, svc) in &doc.services {
        let mut mount_paths = std::collections::BTreeSet::new();
        for m in &svc.volumes {
            if m.name.trim().is_empty() {
                return Err(format!("service {name}: volume mount name is required"));
            }
            if !doc.volumes.contains_key(&m.name) {
                return Err(format!(
                    "service {name}: volume {:?} is not defined under stack volumes",
                    m.name
                ));
            }
            if !m.mount.starts_with('/') {
                return Err(format!(
                    "service {name}: volume mount path must be absolute (got {:?})",
                    m.mount
                ));
            }
            if !mount_paths.insert(m.mount.clone()) {
                return Err(format!(
                    "service {name}: duplicate volume mount path {:?}",
                    m.mount
                ));
            }
        }
    }
    Ok(())
}

/// Volume name charset for the msb named-volume namespace; `--` is reserved
/// as the stack/volume separator.
fn valid_volume_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() || c.is_ascii_digit() => {}
        _ => return false,
    }
    name.chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-'))
        && !name.contains("--")
}

fn validate_ingress(doc: &StackDocument, ing: &IngressSpec) -> Result<(), String> {
    if ing.rules.is_empty() && ing.tcp.is_empty() {
        return Err("ingress.rules or ingress.tcp must not be empty when ingress is set".into());
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
            let port_spec = svc.ports.iter().find(|p| p.target == path.port);
            let Some(ps) = port_spec else {
                return Err(format!(
                    "ingress.rules[{ri}].paths[{pi}]: service {:?} has no ports entry with target port {} (declare ports: for north–south)",
                    path.service, path.port
                ));
            };
            if !ps.protocol.eq_ignore_ascii_case("tcp") {
                return Err(format!(
                    "ingress.rules[{ri}].paths[{pi}]: backend port must be tcp (got {:?})",
                    ps.protocol
                ));
            }
        }
    }
    Ok(())
}

/// Fabric FQDN on the stack's default network: `<service>.<stack>.svc.mc2`.
pub fn fabric_fqdn(stack: &str, service: &str) -> String {
    format!("{service}.{stack}.svc.mc2")
}

/// Fabric FQDN on a named network: `<service>.<network>.svc.mc2`.
pub fn network_fqdn(network: &str, service: &str) -> String {
    format!("{service}.{network}.svc.mc2")
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
name: demo
services:
  web:
    image: python:3.12
    scale: 2
    cpus: 1
    mem_limit: 512m
"#;

    #[test]
    fn parse_demo() {
        let doc = parse_stack_yaml(DEMO).unwrap();
        assert_eq!(doc.name, "demo");
        assert_eq!(doc.services["web"].scale, 2);
        assert_eq!(doc.services["web"].mem_limit_mib, 512);
    }

    #[test]
    fn accepts_ingress_with_ports() {
        let yaml = r#"
name: demo
services:
  web:
    image: alpine
    ports:
      - "8080:8000"
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
    fn ports_target_only_and_hostname_sugar() {
        let yaml = r#"
name: x
services:
  web:
    image: busybox
    ports:
      - "3001"
      - "mcp.example.com:3000"
      - "5000:3002/tcp"
"#;
        let doc = parse_stack_yaml(yaml).unwrap();
        let ports = &doc.services["web"].ports;
        assert_eq!(ports[0].target, 3001);
        assert_eq!(ports[0].published, 0, "target-only → auto host port");
        assert_eq!(ports[0].hostname, None);
        assert_eq!(ports[1].hostname.as_deref(), Some("mcp.example.com"));
        assert_eq!(ports[1].target, 3000);
        assert_eq!(ports[2].published, 5000);
        assert_eq!(ports[2].protocol, "tcp");
    }

    #[test]
    fn expose_accepts_compose_list_form() {
        let yaml = r#"
name: x
services:
  web:
    image: busybox
    expose: [5432, "6379"]
"#;
        let doc = parse_stack_yaml(yaml).unwrap();
        let exposes = &doc.services["web"].expose;
        assert_eq!(exposes.len(), 2);
        assert_eq!(exposes[0].port, 5432);
        assert_eq!(exposes[1].port, 6379);
    }

    #[test]
    fn rejects_old_ports_form() {
        let yaml = r#"
name: x
services:
  web:
    image: busybox
    ports:
      - host: 8080
        guest: 8000
"#;
        let err = parse_stack_yaml(yaml).unwrap_err();
        assert!(
            err.contains("ports") && err.contains("invalid stack"),
            "{err}"
        );
    }

    #[test]
    fn rejects_empty_ingress_rules() {
        let yaml = r#"
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
    fn compose_restart_and_healthcheck_map() {
        let yaml = r#"
name: demo
services:
  web:
    image: alpine
    restart: unless-stopped
    healthcheck:
      test: ["CMD", "curl", "-f", "http://localhost/"]
      interval: 10s
"#;
        let doc = parse_stack_yaml(yaml).unwrap();
        assert_eq!(doc.services["web"].restart, "unless-stopped");
        let h = doc.services["web"].healthcheck.clone().unwrap();
        assert_eq!(
            h.test,
            Some(vec![
                "curl".to_string(),
                "-f".to_string(),
                "http://localhost/".to_string()
            ])
        );
        assert_eq!(h.interval_seconds, 10);
    }

    #[test]
    fn fabric_expose_without_allow_parses() {
        // Full-mesh fabric: no allow field needed; expose is the reachability gate.
        let yaml = r#"
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
"#;
        let doc = parse_stack_yaml(yaml).unwrap();
        assert_eq!(doc.services["db"].expose[0].port, 5432);
        assert_eq!(fabric_fqdn("shop", "db"), "db.shop.svc.mc2");
    }

    #[test]
    fn expose_ports_must_be_unique_across_stack() {
        let yaml = r#"
name: shop
services:
  web:
    image: alpine
    expose:
      - port: 8080
  admin:
    image: alpine
    expose:
      - port: 8080
"#;
        let err = parse_stack_yaml(yaml).unwrap_err();
        assert!(err.contains("must be unique"), "{err}");
    }

    #[test]
    fn fabric_named_network_needs_no_declaration() {
        // Server-wide networks: joining an undeclared named network is valid.
        let yaml = r#"
name: shop
services:
  web:
    image: alpine
    networks: [backend]
"#;
        parse_stack_yaml(yaml).unwrap();
    }

    /// Volume stack helper: injects `volumes_yaml` at top level and
    /// `mounts_yaml` under the single service.
    fn volume_stack(volumes_yaml: &str, mounts_yaml: &str) -> String {
        format!(
            r#"
name: demo
volumes:
{volumes_yaml}
services:
  web:
    image: alpine:3.20
    volumes:
{mounts_yaml}
"#
        )
    }

    #[test]
    fn valid_volume_stack_accepted() {
        let doc = parse_stack_yaml(&volume_stack(
            "  data:\n    kind: dir",
            "      - name: data\n        target: /data",
        ))
        .unwrap();
        assert_eq!(doc.services["web"].volumes[0].mount, "/data");
        assert_eq!(doc.volumes["data"].kind, "dir");
    }

    #[test]
    fn omitted_resources_get_serde_defaults() {
        // An omitted `cpus` / `mem_limit` block uses serde defaults; a zero-memory
        // spec is rejected by the sandbox runtime, so the two must agree.
        let yaml = r#"
name: demo
services:
  web:
    image: alpine:3.20
"#;
        let doc = parse_stack_yaml(yaml).unwrap();
        assert_eq!(doc.services["web"].cpus, 1.0);
        assert_eq!(doc.services["web"].mem_limit_mib, 512);
        let s = ServiceSpec {
            image: "x".into(),
            scale: 1,
            cpus: 1.0,
            mem_limit_mib: 512,
            ports: vec![],
            network: Default::default(),
            env: Default::default(),
            secrets: vec![],
            volumes: vec![],
            restart: "no".into(),
            healthcheck: None,
            labels: Default::default(),
            command: None,
            node_name: None,
            node_selector: Default::default(),
            ssh: None,
            expose: vec![],
            networks: vec![],
        };
        assert_eq!(s.cpus, 1.0);
        assert_eq!(s.mem_limit_mib, 512);
    }

    #[test]
    fn volume_mount_without_declaration_rejected() {
        let err = parse_stack_yaml(&volume_stack(
            "  data:\n    kind: dir",
            "      - name: missing\n        target: /data",
        ))
        .unwrap_err();
        assert!(err.contains("not defined under stack volumes"), "{err}");
    }

    #[test]
    fn volume_kind_other_than_dir_rejected() {
        let err = parse_stack_yaml(&volume_stack(
            "  data:\n    kind: disk",
            "      - name: data\n        target: /data",
        ))
        .unwrap_err();
        assert!(err.contains("unsupported kind"), "{err}");
    }

    #[test]
    fn volume_name_empty_or_invalid_rejected() {
        for bad in [
            "  \"\":\n    kind: dir",
            "  Data!:\n    kind: dir",
            "  -data:\n    kind: dir",
        ] {
            let err = parse_stack_yaml(&volume_stack(bad, "      []")).unwrap_err();
            assert!(err.contains("invalid volume name"), "{err}");
        }
    }

    #[test]
    fn volume_name_double_dash_rejected() {
        let err =
            parse_stack_yaml(&volume_stack("  da--ta:\n    kind: dir", "      []")).unwrap_err();
        assert!(err.contains("invalid volume name"), "{err}");

        // Stack names must not collide with the `--` separator either.
        let yaml = r#"
name: my--stack
services:
  web:
    image: alpine:3.20
"#;
        let err = parse_stack_yaml(yaml).unwrap_err();
        assert!(err.contains("must not contain '--'"), "{err}");
    }

    #[test]
    fn relative_mount_path_rejected() {
        let err = parse_stack_yaml(&volume_stack(
            "  data:\n    kind: dir",
            "      - name: data\n        target: data/files",
        ))
        .unwrap_err();
        assert!(err.contains("must be absolute"), "{err}");
    }

    #[test]
    fn duplicate_mount_path_per_service_rejected() {
        let err = parse_stack_yaml(&volume_stack(
            "  a:\n    kind: dir\n  b:\n    kind: dir",
            "      - name: a\n        target: /data\n      - name: b\n        target: /data",
        ))
        .unwrap_err();
        assert!(err.contains("duplicate volume mount path"), "{err}");
    }
}
