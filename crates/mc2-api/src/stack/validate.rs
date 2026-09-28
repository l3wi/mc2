//! Semantic validation for parsed stack documents.

use crate::stack::schema::{IngressSpec, StackDocument, MAX_DISK_SIZE_MIB};

/// Maximum stack-name length; keeps `{stack}--{service}--{ordinal}` ≤ 128 bytes.
pub(crate) const STACK_NAME_MAX: usize = 40;
/// Maximum service-name length (DNS label).
pub(crate) const SERVICE_NAME_MAX: usize = 63;
/// Maximum network-name length (DNS label).
pub(crate) const NETWORK_NAME_MAX: usize = 63;
/// Maximum volume-name length.
pub(crate) const VOLUME_NAME_MAX: usize = 63;

/// Maximum service `cpus`. The microsandbox builder takes the vCPU count as a
/// `u8` and `mc2-runtime` clamps to `[1, 255]`, so anything above 255 would be
/// silently distorted into a smaller reservation than the YAML asks for.
pub(crate) const MAX_CPUS: f64 = 255.0;

/// Network profile closed set (`none` must be the only entry; unknown values
/// are rejected — never treated as `public`).
const NETWORK_PROFILES: [&str; 4] = ["public", "private", "host", "none"];

pub(crate) fn validate_stack(doc: &StackDocument) -> Result<(), String> {
    if doc.name.trim().is_empty() {
        return Err("name is required".into());
    }
    if doc.services.is_empty() {
        return Err("services must not be empty".into());
    }
    validate_lower_name("stack name", &doc.name, STACK_NAME_MAX)?;
    for net_name in doc.networks.keys() {
        validate_lower_name("network name", net_name, NETWORK_NAME_MAX)?;
    }
    validate_volumes(doc)?;
    validate_depends_on(doc)?;
    if let Some(ing) = &doc.ingress {
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
        validate_lower_name("service name", name, SERVICE_NAME_MAX)?;
        for net in &svc.networks {
            validate_lower_name(
                &format!("service {name}: network name"),
                net,
                NETWORK_NAME_MAX,
            )?;
        }
        validate_profiles(name, &svc.network.profiles)?;
        if let Some(storage) = &svc.storage_opt {
            validate_disk_size(
                &format!("service {name}: storage_opt.size"),
                storage.size_mib,
            )?;
        }
        if svc.image.trim().is_empty() {
            return Err(format!("service {name}: image is required"));
        }
        if svc.scale == 0 {
            return Err(format!("service {name}: scale must be >= 1 for MVP"));
        }
        // The runtime clamps to [1, 255] vCPUs, so a non-positive/NaN cpus
        // would reserve nothing (or an unreadable `null`) while the VM takes a
        // full vCPU. Reject anything the clamp would distort.
        if !svc.cpus.is_finite() || svc.cpus <= 0.0 || svc.cpus > MAX_CPUS {
            return Err(format!(
                "service {name}: cpus must be a finite number in (0, {MAX_CPUS}] (got {})",
                svc.cpus
            ));
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
        if let Some(hc) = &svc.healthcheck {
            if !hc.disable {
                if let Some(test) = &hc.test {
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

/// Shared lowercase name grammar for stack/service/network names:
/// `[a-z0-9]([a-z0-9_-]*[a-z0-9])?` with no `--`.
fn valid_lower_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    let alnum = |c: u8| c.is_ascii_lowercase() || c.is_ascii_digit();
    if bytes.is_empty() || !alnum(bytes[0]) || !alnum(bytes[bytes.len() - 1]) {
        return false;
    }
    bytes.iter().all(|&c| alnum(c) || c == b'-' || c == b'_') && !name.contains("--")
}

/// Suggest a valid name for `raw`: lowercase it, map every other character
/// (including `.`) to `-`, collapse `--`, trim edge `-`/`_`, truncate to `max`.
fn suggest_lower_name(raw: &str, max: usize) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut prev_dash = false;
    for c in raw.chars() {
        let c = c.to_ascii_lowercase();
        if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' {
            out.push(c);
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    let mut s: String = out.trim_matches(['-', '_']).chars().take(max).collect();
    while s.ends_with('-') || s.ends_with('_') {
        s.pop();
    }
    s
}

/// Append `(suggested: …)` to `msg` when a valid alternative is derivable.
fn with_suggestion(msg: String, value: &str, max: usize) -> String {
    let suggested = suggest_lower_name(value, max);
    if suggested.is_empty() || suggested == value {
        msg
    } else {
        format!("{msg} (suggested: {suggested:?})")
    }
}

fn validate_lower_name(field: &str, value: &str, max: usize) -> Result<(), String> {
    if !valid_lower_name(value) {
        return Err(with_suggestion(
            format!(
                "invalid {field} {value:?}: must match [a-z0-9]([a-z0-9_-]*[a-z0-9])? \
                 and must not contain '--'"
            ),
            value,
            max,
        ));
    }
    if value.len() > max {
        return Err(with_suggestion(
            format!(
                "invalid {field} {value:?}: {} characters; the maximum is {max}",
                value.len()
            ),
            value,
            max,
        ));
    }
    Ok(())
}

/// Closed profile set: values from [`NETWORK_PROFILES`]; `none` alone; no
/// duplicates. An empty list keeps its default (`public`).
fn validate_profiles(service: &str, profiles: &[String]) -> Result<(), String> {
    let mut seen = std::collections::BTreeSet::new();
    for p in profiles {
        if !NETWORK_PROFILES.contains(&p.as_str()) {
            return Err(format!(
                "service {service}: network.profiles value {p:?} is not supported \
                 (expected one of public, private, host, none)"
            ));
        }
        if p == "none" && profiles.len() > 1 {
            return Err(format!(
                "service {service}: network.profiles [none] must be the only entry"
            ));
        }
        if !seen.insert(p.as_str()) {
            return Err(format!(
                "service {service}: duplicate network.profiles value {p:?}"
            ));
        }
    }
    Ok(())
}

/// Validate an RFC 1123 hostname: lowercase `[a-z0-9-]` labels of 1–63
/// characters that do not start or end with `-`, total ≤ 253 characters, with
/// no port, no wildcard and no trailing dot.
///
/// Used for `ingress.rules[].host`, the `ports[].hostname` sugar, and the
/// server's `--public-hostname`.
pub fn validate_hostname(host: &str) -> Result<(), String> {
    if host.is_empty() {
        return Err("hostname must not be empty".into());
    }
    if host.len() > 253 {
        return Err(format!(
            "invalid hostname {host:?}: {} characters; the maximum is 253",
            host.len()
        ));
    }
    if host.ends_with('.') {
        return Err(format!("invalid hostname {host:?}: must not end with '.'"));
    }
    if host.contains('*') {
        return Err(format!(
            "invalid hostname {host:?}: wildcards are not supported (use an exact hostname)"
        ));
    }
    if host.contains(':') {
        return Err(format!(
            "invalid hostname {host:?}: must not include a port"
        ));
    }
    for label in host.split('.') {
        if label.is_empty() {
            return Err(format!("invalid hostname {host:?}: empty label"));
        }
        if label.len() > 63 {
            return Err(format!(
                "invalid hostname {host:?}: label {label:?} is {} characters; the maximum is 63",
                label.len()
            ));
        }
        if label.starts_with('-') || label.ends_with('-') {
            return Err(format!(
                "invalid hostname {host:?}: label {label:?} must not start or end with '-'"
            ));
        }
        if !label
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        {
            return Err(format!(
                "invalid hostname {host:?}: labels must be lowercase [a-z0-9-]"
            ));
        }
    }
    Ok(())
}

/// Validate a Traefik identifier (`^[A-Za-z0-9_-]+$`). Used for
/// `ingress.tls.certResolver`, `ingress.tcp[].entryPoint`, `ingress.tcp[].name`
/// and the server's `--public-tls-cert-resolver`.
pub fn validate_traefik_ident(value: &str) -> Result<(), String> {
    if value.is_empty() {
        return Err("identifier must not be empty".into());
    }
    if !value
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err(format!(
            "invalid Traefik identifier {value:?}: must match [A-Za-z0-9_-]+"
        ));
    }
    Ok(())
}

/// Validate an ingress path: must start with `/` and contain no whitespace,
/// quotes, backticks, backslashes or control characters.
pub fn validate_ingress_path(path: &str) -> Result<(), String> {
    if !path.starts_with('/') {
        return Err(format!(
            "invalid ingress path {path:?}: must start with '/'"
        ));
    }
    if path
        .chars()
        .any(|c| c.is_control() || c.is_whitespace() || matches!(c, '\'' | '"' | '`' | '\\'))
    {
        return Err(format!(
            "invalid ingress path {path:?}: must not contain whitespace, quotes, \
             backticks, backslashes or control characters"
        ));
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
        if !valid_volume_name(name) || name.len() > VOLUME_NAME_MAX {
            return Err(with_suggestion(
                format!(
                    "invalid volume name {name:?}: must match [a-z0-9][a-z0-9._-]*, \
                     must not contain '--' or end with '-', and be at most \
                     {VOLUME_NAME_MAX} characters"
                ),
                name,
                VOLUME_NAME_MAX,
            ));
        }
        let kind = vol.kind.trim().to_ascii_lowercase();
        if kind != "dir" {
            return Err(format!(
                "volume {name}: unsupported kind {:?} (only dir is supported in v1)",
                vol.kind
            ));
        }
        validate_disk_size(&format!("volume {name}: size"), vol.size_mib)?;
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

/// A declared disk size must be positive and within [`MAX_DISK_SIZE_MIB`].
/// `field` names the YAML key path so the author can find it.
fn validate_disk_size(field: &str, mib: u64) -> Result<(), String> {
    if mib == 0 {
        return Err(format!("{field} must be greater than 0"));
    }
    if mib > MAX_DISK_SIZE_MIB {
        return Err(format!(
            "{field}: {mib} MiB is larger than the maximum of {} MiB (1 TiB)",
            MAX_DISK_SIZE_MIB
        ));
    }
    Ok(())
}

/// Volume name charset for the msb named-volume namespace; `--` is reserved
/// as the stack/volume separator, and trailing `-` is rejected so the
/// `mc2-{stack}--{volume}` identity stays unambiguous.
fn valid_volume_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() || c.is_ascii_digit() => {}
        _ => return false,
    }
    name.chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-'))
        && !name.contains("--")
        && !name.ends_with('-')
}

fn validate_ingress(doc: &StackDocument, ing: &IngressSpec) -> Result<(), String> {
    if ing.rules.is_empty() && ing.tcp.is_empty() {
        return Err("ingress.rules or ingress.tcp must not be empty when ingress is set".into());
    }
    if let Some(resolver) = &ing.tls.cert_resolver {
        validate_traefik_ident(resolver).map_err(|e| format!("ingress.tls.certResolver: {e}"))?;
    }

    // Traefik router/service keys are derived from route ids; two routes that
    // collide on a key would silently overwrite one another in the catalog.
    let mut keys: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();

    for (ti, route) in ing.tcp.iter().enumerate() {
        if route.name.trim().is_empty() || route.entry_point.trim().is_empty() {
            return Err(format!("ingress.tcp[{ti}] requires name and entryPoint"));
        }
        validate_traefik_ident(&route.name).map_err(|e| format!("ingress.tcp[{ti}].name: {e}"))?;
        validate_traefik_ident(&route.entry_point)
            .map_err(|e| format!("ingress.tcp[{ti}].entryPoint: {e}"))?;
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
        let id = format!("{}-tcp-{}-{}", doc.name, route.name, route.service);
        insert_route_key(&mut keys, &id, &format!("ingress.tcp[{ti}]"))?;
    }

    for (ri, rule) in ing.rules.iter().enumerate() {
        if rule.host.trim().is_empty() {
            return Err(format!("ingress.rules[{ri}].host is required"));
        }
        validate_hostname(&rule.host).map_err(|e| format!("ingress.rules[{ri}].host: {e}"))?;
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
            validate_ingress_path(&path.path)
                .map_err(|e| format!("ingress.rules[{ri}].paths[{pi}].path: {e}"))?;
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
            let id = crate::stack::make_ingress_route_id(
                &doc.name,
                &rule.host,
                &path.path,
                &path.service,
                path.port,
            );
            insert_route_key(&mut keys, &id, &format!("ingress.rules[{ri}].paths[{pi}]"))?;
        }
    }

    // `ports[].hostname` sugar synthesizes an additional TLS route per entry.
    for (svc_name, svc) in &doc.services {
        for p in &svc.ports {
            let Some(host) = &p.hostname else {
                continue;
            };
            let id = crate::stack::make_ingress_route_id(&doc.name, host, "/", svc_name, p.target);
            insert_route_key(
                &mut keys,
                &id,
                &format!("service {svc_name}: ports hostname {host:?}"),
            )?;
        }
    }
    Ok(())
}

/// Record the Traefik key for `id`, rejecting a collision with a prior route.
fn insert_route_key(
    keys: &mut std::collections::BTreeMap<String, String>,
    id: &str,
    origin: &str,
) -> Result<(), String> {
    let key = crate::stack::route_key(id);
    if let Some(prev) = keys.insert(key.clone(), origin.to_string()) {
        return Err(format!(
            "duplicate ingress router {key:?}: {prev} and {origin} produce the same Traefik key"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hostname_accepts_rfc1123_lowercase() {
        for good in ["demo.local", "a.b.c", "x1-y2.example.com"] {
            validate_hostname(good).unwrap_or_else(|e| panic!("{good}: {e}"));
        }
        validate_hostname(&"a".repeat(63)).unwrap();
        validate_hostname(&format!("{}.{}", "a".repeat(63), "b".repeat(63))).unwrap();
    }

    #[test]
    fn hostname_rejects_ports_wildcards_and_bad_labels() {
        for (bad, needle) in [
            ("", "empty"),
            ("Demo.local", "lowercase"),
            ("-a.local", "start or end"),
            ("a-.local", "start or end"),
            ("a..b", "empty label"),
            ("demo.local.", "end with"),
            ("*.demo.local", "wildcard"),
            ("demo.local:8080", "port"),
        ] {
            let err = validate_hostname(bad).unwrap_err();
            assert!(err.contains(needle), "{bad:?}: {err}");
        }
        let err = validate_hostname(&"a".repeat(64)).unwrap_err();
        assert!(err.contains("maximum is 63"), "{err}");
    }

    #[test]
    fn traefik_ident_rejects_non_identifier_chars() {
        for good in ["le", "web", "entry_point-1"] {
            validate_traefik_ident(good).unwrap_or_else(|e| panic!("{good}: {e}"));
        }
        for bad in ["", "a b", "a.b", "a\nb", "a/b"] {
            assert!(validate_traefik_ident(bad).is_err(), "{bad:?} must fail");
        }
    }

    #[test]
    fn ingress_path_rejects_control_and_quotes() {
        for good in ["/", "/api", "/a/b-c_d"] {
            validate_ingress_path(good).unwrap_or_else(|e| panic!("{good}: {e}"));
        }
        for bad in ["", "api", "/a b", "/a\tb", "/a`b", "/a'b", "/a\"b", "/a\\b"] {
            assert!(validate_ingress_path(bad).is_err(), "{bad:?} must fail");
        }
    }
}
