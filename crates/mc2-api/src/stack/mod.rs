//! Compose-style stack YAML schema (mc2/v1): parse, validate, and naming
//! helpers. The schema types live in `schema`, compose-value deserializers in
//! `decode`, and semantic validation in `validate`.

mod decode;
mod schema;
mod validate;

pub use decode::split_command_string;
pub use schema::{
    DependsOnSpec, ExposeSpec, HealthcheckSpec, IngressPath, IngressRule, IngressSpec,
    IngressTcpRoute, IngressTlsSpec, NetworkSpec, PortSpec, SecretRef, ServiceSpec, SshSpec,
    StackDocument, StackNetworkSpec, VolumeMount, VolumeSpec,
};

/// Parse and validate a stack YAML document (canonical compose-style schema).
pub fn parse_stack_yaml(yaml: &str) -> Result<StackDocument, String> {
    let doc: StackDocument =
        serde_yaml::from_str(yaml).map_err(|e| format!("invalid stack YAML: {e}"))?;
    validate::validate_stack(&doc)?;
    Ok(doc)
}

/// True if bind is loopback (catalog backends must stay on loopback in v1).
pub fn is_loopback_bind(bind: &str) -> bool {
    let b = bind.trim();
    b == "127.0.0.1" || b == "::1" || b.eq_ignore_ascii_case("localhost")
}

/// Default-network FQDN on the stack's default network: `<service>.<stack>.svc.mc2`.
pub fn default_network_fqdn(stack: &str, service: &str) -> String {
    format!("{service}.{stack}.svc.mc2")
}

/// Network FQDN on a named network: `<service>.<network>.svc.mc2`.
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
    use std::collections::BTreeMap;

    #[test]
    fn mem_limit_json_roundtrip_is_lossless() {
        let s = ServiceSpec {
            image: "alpine".into(),
            scale: 1,
            cpus: 1.0,
            mem_limit_mib: 512,
            ports: vec![],
            network: Default::default(),
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
            expose: vec![],
            networks: vec![],
            depends_on: BTreeMap::new(),
        };
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains("\"mem_limit\":536870912"), "{json}");
        let back: ServiceSpec = serde_json::from_str(&json).unwrap();
        assert_eq!(back.mem_limit_mib, 512);
        // compose `mem_limit` in bytes still parses to MiB.
        let bytes: ServiceSpec =
            serde_json::from_str(r#"{"image":"alpine","mem_limit":1073741824}"#).unwrap();
        assert_eq!(bytes.mem_limit_mib, 1024);
    }

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
    fn network_expose_without_allow_parses() {
        // Full-mesh network: no allow field needed; expose is the reachability gate.
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
        assert_eq!(default_network_fqdn("shop", "db"), "db.shop.svc.mc2");
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
    fn network_join_needs_no_declaration() {
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
            depends_on: BTreeMap::new(),
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

    #[test]
    fn environment_accepts_map_and_list() {
        let yaml = r#"
name: x
services:
  web:
    image: busybox
    environment:
      DEBUG: "1"
      PATH: /usr/bin
    command: ["sleep", "infinity"]
"#;
        let doc = parse_stack_yaml(yaml).unwrap();
        let env = &doc.services["web"].env;
        assert_eq!(env.get("DEBUG").unwrap(), "1");
        assert_eq!(env.get("PATH").unwrap(), "/usr/bin");

        let yaml = r#"
name: x
services:
  web:
    image: busybox
    environment:
      - A=1
      - B=two
"#;
        let doc = parse_stack_yaml(yaml).unwrap();
        assert_eq!(doc.services["web"].env.get("A").unwrap(), "1");
        assert_eq!(doc.services["web"].env.get("B").unwrap(), "two");
    }

    #[test]
    fn old_env_key_rejected() {
        let yaml = r#"
name: x
services:
  web:
    image: busybox
    env:
      A: "1"
"#;
        let err = parse_stack_yaml(yaml).unwrap_err();
        assert!(err.contains("env") || err.contains("unknown"), "{err}");
    }

    #[test]
    fn command_string_form_is_split_shell_like() {
        let yaml = r#"
name: x
services:
  web:
    image: busybox
    command: 'echo "hi there" --flag value'
"#;
        let doc = parse_stack_yaml(yaml).unwrap();
        assert_eq!(
            doc.services["web"].command,
            Some(vec![
                "echo".to_string(),
                "hi there".to_string(),
                "--flag".to_string(),
                "value".to_string(),
            ])
        );
    }

    #[test]
    fn split_command_string_handles_quotes_and_escapes() {
        assert_eq!(split_command_string(""), Vec::<String>::new());
        assert_eq!(split_command_string("   "), Vec::<String>::new());
        assert_eq!(split_command_string("a b c"), vec!["a", "b", "c"]);
        assert_eq!(
            split_command_string(r#"x 'single word' y"#),
            vec!["x", "single word", "y"]
        );
        assert_eq!(
            split_command_string(r#"a "sp ace" b"#),
            vec!["a", "sp ace", "b"]
        );
        assert_eq!(split_command_string(r#"eve\n"#), vec!["even"]);
        assert_eq!(split_command_string(r#"quote\"d"#), vec![r#"quote"d"#]);
    }

    #[test]
    fn healthcheck_full_field_set() {
        let yaml = r#"
name: x
services:
  web:
    image: busybox
    healthcheck:
      test: ["CMD", "curl", "-f", "http://localhost/"]
      interval: 10s
      timeout: 5s
      retries: 2
      start_period: 15s
  admin:
    image: busybox
    healthcheck:
      disable: true
"#;
        let doc = parse_stack_yaml(yaml).unwrap();
        let h = doc.services["web"].healthcheck.clone().unwrap();
        assert_eq!(h.interval_seconds, 10);
        assert_eq!(h.timeout_seconds, 5);
        assert_eq!(h.retries, 2);
        assert_eq!(h.start_period_seconds, 15);
        assert!(!h.disable);
        assert!(doc.services["admin"].healthcheck.clone().unwrap().disable);
        // Defaults when omitted.
        let yaml = r#"
name: x
services:
  web:
    image: busybox
    healthcheck:
      test: ["true"]
"#;
        let h = parse_stack_yaml(yaml).unwrap().services["web"]
            .healthcheck
            .clone()
            .unwrap();
        assert_eq!(h.retries, 3);
        assert_eq!(h.timeout_seconds, 0);
        assert_eq!(h.start_period_seconds, 0);
    }

    #[test]
    fn depends_on_list_and_map_forms() {
        let yaml = r#"
name: x
services:
  db:
    image: busybox
    healthcheck:
      test: ["pg_isready"]
  web:
    image: busybox
    depends_on:
      - db
  admin:
    image: busybox
    depends_on:
      db:
        condition: service_healthy
"#;
        let doc = parse_stack_yaml(yaml).unwrap();
        assert_eq!(
            doc.services["web"].depends_on["db"].condition,
            "service_started"
        );
        assert_eq!(
            doc.services["admin"].depends_on["db"].condition,
            "service_healthy"
        );
    }

    #[test]
    fn depends_on_unknown_service_rejected() {
        let yaml = r#"
name: x
services:
  web:
    image: busybox
    depends_on:
      - missing
"#;
        let err = parse_stack_yaml(yaml).unwrap_err();
        assert!(err.contains("not a service"), "{err}");
    }

    #[test]
    fn depends_on_bad_condition_rejected() {
        let yaml = r#"
name: x
services:
  web:
    image: busybox
  api:
    image: busybox
    depends_on:
      web:
        condition: service_completed_successfully
"#;
        let err = parse_stack_yaml(yaml).unwrap_err();
        assert!(err.contains("service_started|service_healthy"), "{err}");
    }

    #[test]
    fn depends_on_cycle_rejected() {
        let yaml = r#"
name: x
services:
  a:
    image: busybox
    depends_on: [b]
  b:
    image: busybox
    depends_on: [a]
"#;
        let err = parse_stack_yaml(yaml).unwrap_err();
        assert!(err.contains("cycle"), "{err}");
    }

    #[test]
    fn ssh_accepts_boolean_and_map() {
        // Short flag: enabled with defaults.
        let yaml = r#"
name: x
services:
  a:
    image: busybox
    ssh: true
  b:
    image: busybox
    ssh: false
  c:
    image: busybox
    ssh:
      enabled: true
      port: 2222
"#;
        let doc = parse_stack_yaml(yaml).unwrap();
        let a = doc.services["a"].ssh.as_ref().unwrap();
        assert!(a.enabled);
        assert_eq!(a.bind, "127.0.0.1");
        assert_eq!(a.port, 0);
        assert_eq!(a.user, "root");
        assert!(a.sftp);
        assert!(
            a.authorized_keys.is_empty(),
            "flag form → all registered keys"
        );
        assert!(!doc.services["b"].ssh.as_ref().unwrap().enabled);
        let c = doc.services["c"].ssh.as_ref().unwrap();
        assert_eq!(c.port, 2222);
    }
}
