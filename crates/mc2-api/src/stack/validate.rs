//! Semantic validation for parsed stack documents.

use crate::stack::schema::{IngressSpec, StackDocument};

pub(crate) fn validate_stack(doc: &StackDocument) -> Result<(), String> {
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
    validate_depends_on(doc)?;
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
            if !hc.disable {
                if let Some(ref test) = hc.test {
                    if test.is_empty() {
                        return Err(format!(
                            "service {name}: healthcheck.test command must not be empty"
                        ));
                    }
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

    // Shared network splices bind one host loopback port per exposed guest port,
    // so exposed ports must be unique across the whole stack.
    {
        let mut stack_expose_ports = std::collections::BTreeSet::new();
        for svc in doc.services.values() {
            for ex in &svc.expose {
                if !stack_expose_ports.insert(ex.port) {
                    return Err(format!(
                        "expose port {} is used by multiple services in this stack \
                         (network ports must be unique across the stack)",
                        ex.port
                    ));
                }
            }
        }
    }
    Ok(())
}

/// Compose `depends_on`: references must exist in-stack, conditions must be
/// supported (`service_started` | `service_healthy`), and the graph must be acyclic.
fn validate_depends_on(doc: &StackDocument) -> Result<(), String> {
    use std::collections::BTreeMap;

    for (svc_name, svc) in &doc.services {
        for (dep, spec) in &svc.depends_on {
            if !doc.services.contains_key(dep) {
                return Err(format!(
                    "service {svc_name}: depends_on {:?} is not a service in this stack",
                    dep
                ));
            }
            let cond = spec.condition.trim().to_ascii_lowercase();
            if !matches!(cond.as_str(), "service_started" | "service_healthy") {
                return Err(format!(
                    "service {svc_name}: depends_on {dep} condition must be service_started|service_healthy (got {:?})",
                    spec.condition
                ));
            }
            if cond == "service_healthy" {
                let has_hc = doc.services[dep]
                    .healthcheck
                    .as_ref()
                    .is_some_and(|h| !h.disable && h.test.is_some());
                if !has_hc {
                    return Err(format!(
                        "service {svc_name}: depends_on {dep} condition service_healthy \
                         requires {dep} to declare a healthcheck"
                    ));
                }
            }
        }
    }

    // DFS cycle detection (1 = on stack, 2 = done).
    let mut color: BTreeMap<&str, u8> = BTreeMap::new();
    fn visit<'a>(
        name: &'a str,
        doc: &'a StackDocument,
        color: &mut BTreeMap<&'a str, u8>,
        stack: &mut Vec<&'a str>,
    ) -> Result<(), String> {
        match color.get(name) {
            Some(&1) => {
                let mut chain: Vec<&str> = stack.clone();
                chain.push(name);
                return Err(format!("depends_on cycle detected: {}", chain.join(" -> ")));
            }
            Some(&2) => return Ok(()),
            _ => {}
        }
        color.insert(name, 1);
        stack.push(name);
        for dep in doc.services[name].depends_on.keys() {
            visit(dep, doc, color, stack)?;
        }
        stack.pop();
        color.insert(name, 2);
        Ok(())
    }
    for name in doc.services.keys() {
        visit(name, doc, &mut color, &mut Vec::new())?;
    }
    Ok(())
}

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
