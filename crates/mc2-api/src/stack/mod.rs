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
    StackDocument, StackNetworkSpec, StorageOptSpec, VolumeMount, VolumeSpec,
    DEFAULT_ROOT_DISK_MIB, DEFAULT_VOLUME_SIZE_MIB, MAX_DISK_SIZE_MIB,
};
pub use validate::{validate_hostname, validate_ingress_path, validate_traefik_ident};

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

/// Stable Traefik router/service key for a route id: ASCII alphanumerics, `-`
/// and `_` are kept, every other character becomes `-`. Two route ids that
/// normalize to the same key would collide in the Traefik catalog.
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
            cpus: 1,
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
            storage_opt: None,
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
    fn cpus_must_be_whole_vcpus_in_range() {
        for bad in ["0", "-1", ".nan", ".inf", "1000", "1.5", "0.5", "\"2.5\""] {
            let yaml =
                format!("name: demo\nservices:\n  web:\n    image: alpine\n    cpus: {bad}\n");
            let err = parse_stack_yaml(&yaml).unwrap_err();
            assert!(err.contains("cpus"), "{bad:?}: {err}");
        }
        // Whole values parse in every spelling Compose files use.
        for (ok, want) in [("1", 1), ("2.0", 2), ("\"4\"", 4), ("255", 255)] {
            let yaml =
                format!("name: demo\nservices:\n  web:\n    image: alpine\n    cpus: {ok}\n");
            let doc = parse_stack_yaml(&yaml).unwrap_or_else(|e| panic!("cpus {ok}: {e}"));
            assert_eq!(doc.services["web"].cpus, want, "{ok}");
        }
    }

    #[test]
    fn fractional_cpus_error_names_the_whole_choices() {
        let err = parse_stack_yaml("name: d\nservices:\n  w:\n    image: a\n    cpus: 1.5\n")
            .unwrap_err();
        assert!(err.contains("use 1 or 2"), "{err}");
        let err = parse_stack_yaml("name: d\nservices:\n  w:\n    image: a\n    cpus: 0.5\n")
            .unwrap_err();
        assert!(err.contains("use 1"), "{err}");
    }

    #[test]
    fn long_form_port_omitting_published_is_auto() {
        let yaml =
            "name: demo\nservices:\n  web:\n    image: alpine\n    ports:\n      - target: 80\n";
        let doc = parse_stack_yaml(yaml).unwrap();
        assert_eq!(doc.services["web"].ports[0].published, 0);
        assert_eq!(doc.services["web"].ports[0].target, 80);
    }

    #[test]
    fn rejects_unknown_long_form_port_keys() {
        let yaml = "name: demo\nservices:\n  web:\n    image: alpine\n    ports:\n      - target: 80\n        publised: 8080\n";
        let err = parse_stack_yaml(yaml).unwrap_err();
        assert!(
            err.contains("publised"),
            "must name the unknown field: {err}"
        );
    }

    #[test]
    fn rejects_explicit_zero_published_in_long_form() {
        let yaml = "name: demo\nservices:\n  web:\n    image: alpine\n    ports:\n      - target: 80\n        published: 0\n";
        let err = parse_stack_yaml(yaml).unwrap_err();
        assert!(err.contains("published"), "{err}");
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
        assert_eq!(doc.services["web"].cpus, 1);
        assert_eq!(doc.services["web"].mem_limit_mib, 512);
        let s = ServiceSpec {
            image: "x".into(),
            scale: 1,
            cpus: 1,
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
            storage_opt: None,
            expose: vec![],
            networks: vec![],
            depends_on: BTreeMap::new(),
        };
        assert_eq!(s.cpus, 1);
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
        // Compose default: an omitted timeout is 30s, never "no timeout".
        assert_eq!(h.timeout_seconds, 30);
        assert_eq!(h.start_period_seconds, 0);
    }

    /// B6a: every compose healthcheck form must survive
    /// YAML → spec → stored JSON → spec (the node reads the stored spec).
    #[test]
    fn healthcheck_forms_round_trip_through_json() {
        let yaml = r#"
name: x
services:
  shell_list:
    image: busybox
    healthcheck:
      test: ["CMD-SHELL", "curl -f http://localhost/"]
      timeout: 100ms
  shell_string:
    image: busybox
    healthcheck:
      test: "curl -f http://localhost/"
      start_period: 1500ms
  cmd:
    image: busybox
    healthcheck:
      test: ["CMD", "curl", "-f", "http://localhost/"]
  none:
    image: busybox
    healthcheck:
      test: ["NONE"]
  disabled:
    image: busybox
    healthcheck:
      disable: true
  empty:
    image: busybox
    healthcheck: {}
"#;
        let doc = parse_stack_yaml(yaml).unwrap();
        let sh = vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            "curl -f http://localhost/".to_string(),
        ];
        let shell_list = doc.services["shell_list"].healthcheck.clone().unwrap();
        // CMD-SHELL (and the bare string form) must actually reach a shell.
        assert_eq!(shell_list.test, Some(sh.clone()));
        assert_eq!(
            doc.services["shell_string"]
                .healthcheck
                .clone()
                .unwrap()
                .test,
            Some(sh)
        );
        // Sub-second durations round UP to the next second, never down to 0.
        assert_eq!(shell_list.timeout_seconds, 1);
        assert_eq!(
            doc.services["shell_string"]
                .healthcheck
                .clone()
                .unwrap()
                .start_period_seconds,
            2
        );
        // `CMD` is a prefix, not an argv element.
        assert_eq!(
            doc.services["cmd"].healthcheck.clone().unwrap().test,
            Some(vec![
                "curl".to_string(),
                "-f".to_string(),
                "http://localhost/".to_string()
            ])
        );
        // `NONE` disables the probe, exactly like `disable: true`.
        assert_eq!(doc.services["none"].healthcheck.clone().unwrap().test, None);
        assert!(
            doc.services["disabled"]
                .healthcheck
                .clone()
                .unwrap()
                .disable
        );

        for name in [
            "shell_list",
            "shell_string",
            "cmd",
            "none",
            "disabled",
            "empty",
        ] {
            let json = serde_json::to_string(&doc.services[name]).unwrap();
            let back: ServiceSpec = serde_json::from_str(&json)
                .unwrap_or_else(|e| panic!("{name}: stored spec unreadable: {e}"));
            assert_eq!(serde_json::to_string(&back).unwrap(), json, "{name}");
        }

        // Defaults for an empty map (compose's own defaults).
        let empty = doc.services["empty"].healthcheck.clone().unwrap();
        assert_eq!(empty.test, None);
        assert!(!empty.disable);
        assert_eq!(empty.interval_seconds, 30);
        assert_eq!(empty.timeout_seconds, 30);
        assert_eq!(empty.retries, 3);
        assert_eq!(empty.start_period_seconds, 0);
    }

    /// B6a: a probe must always have a finite deadline, so an explicit `0`
    /// timeout is rejected instead of meaning "wait forever".
    #[test]
    fn healthcheck_zero_timeout_is_rejected() {
        for value in ["0", "0s", "0ms"] {
            let yaml = format!(
                r#"
name: x
services:
  web:
    image: busybox
    healthcheck:
      test: ["true"]
      timeout: {value}
"#
            );
            let err = parse_stack_yaml(&yaml).unwrap_err();
            assert!(err.contains("timeout"), "{value}: {err}");
        }
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

    // ---- A2: name grammar -------------------------------------------------

    #[test]
    fn rejects_invalid_stack_names() {
        for bad in ["My Stack", "Shop", "api.v2", "a--b", "-a", "a-", "a b"] {
            let yaml = format!("name: \"{bad}\"\nservices:\n  web:\n    image: alpine\n");
            let err = parse_stack_yaml(&yaml).unwrap_err();
            assert!(err.contains("invalid stack name"), "{bad}: {err}");
        }
    }

    #[test]
    fn rejects_invalid_service_names() {
        for bad in ["Shop", "api.v2", "a--b", "-a", "a-"] {
            let yaml = format!("name: demo\nservices:\n  \"{bad}\":\n    image: alpine\n");
            let err = parse_stack_yaml(&yaml).unwrap_err();
            assert!(err.contains("invalid service name"), "{bad}: {err}");
        }
    }

    #[test]
    fn rejects_invalid_network_names() {
        let yaml = "name: demo\nnetworks:\n  \"Bad.Net\":\n    mode: mediated\nservices:\n  web:\n    image: alpine\n";
        let err = parse_stack_yaml(yaml).unwrap_err();
        assert!(err.contains("invalid network name"), "{err}");

        // Membership entries use the same grammar as declarations.
        let yaml = "name: demo\nservices:\n  web:\n    image: alpine\n    networks: [\"Bad\"]\n";
        let err = parse_stack_yaml(yaml).unwrap_err();
        assert!(err.contains("service web: network name"), "{err}");
    }

    #[test]
    fn rejects_overlong_names() {
        let stack = "a".repeat(41);
        let yaml = format!("name: {stack}\nservices:\n  web:\n    image: alpine\n");
        assert!(
            parse_stack_yaml(&yaml)
                .unwrap_err()
                .contains("maximum is 40"),
            "stack cap"
        );

        let svc = "b".repeat(64);
        let yaml = format!("name: demo\nservices:\n  {svc}:\n    image: alpine\n");
        assert!(
            parse_stack_yaml(&yaml)
                .unwrap_err()
                .contains("maximum is 63"),
            "service cap"
        );

        let net = "c".repeat(64);
        let yaml = format!("name: demo\nnetworks:\n  {net}:\n    mode: mediated\nservices:\n  web:\n    image: alpine\n");
        assert!(
            parse_stack_yaml(&yaml)
                .unwrap_err()
                .contains("maximum is 63"),
            "network cap"
        );
    }

    #[test]
    fn accepts_max_length_names() {
        let stack = "a".repeat(40);
        let svc = "b".repeat(63);
        let yaml = format!("name: {stack}\nservices:\n  {svc}:\n    image: alpine\n");
        let doc = parse_stack_yaml(&yaml).unwrap();
        assert_eq!(doc.name.len(), 40);
        assert_eq!(doc.services.keys().next().unwrap().len(), 63);
    }

    #[test]
    fn invalid_names_suggest_a_valid_alternative() {
        let err =
            parse_stack_yaml("name: Shop\nservices:\n  web:\n    image: alpine\n").unwrap_err();
        assert!(err.contains("suggested: \"shop\""), "{err}");

        let err =
            parse_stack_yaml("name: api.v2\nservices:\n  web:\n    image: alpine\n").unwrap_err();
        assert!(err.contains("suggested: \"api-v2\""), "{err}");
    }

    #[test]
    fn underscores_are_allowed_in_lowercase_names() {
        let yaml = "name: my_stack\nservices:\n  my_service:\n    image: alpine\n    networks: [team_net]\n";
        assert!(parse_stack_yaml(yaml).is_ok());
    }

    // ---- A4: ingress hardening -------------------------------------------

    #[test]
    fn rejects_ingress_host_injection_payloads() {
        for bad in [
            "evil.local\\nevil",
            "*.demo.local",
            "demo.local.",
            "Evil.Local",
        ] {
            let yaml = format!(
                "name: demo\nservices:\n  web:\n    image: alpine\n    ports: [\"8080:8000\"]\n\
                 ingress:\n  rules:\n    - host: \"{bad}\"\n      paths:\n\
                 \x20       - path: /\n          service: web\n          port: 8000\n"
            );
            let err = parse_stack_yaml(&yaml).unwrap_err();
            assert!(err.contains("hostname"), "{bad:?}: {err}");
        }
    }

    #[test]
    fn rejects_ingress_traefik_ident_injection_payloads() {
        let resolver = "name: demo\nservices:\n  web:\n    image: alpine\n    ports: [\"8080:8000\"]\n\
             ingress:\n  tls:\n    enabled: true\n    certResolver: \"le\\nevil: x\"\n  rules:\n\
             \x20   - host: demo.local\n      paths:\n        - path: /\n          service: web\n          port: 8000\n";
        let err = parse_stack_yaml(resolver).unwrap_err();
        assert!(err.contains("certResolver"), "{err}");

        let tcp = "name: demo\nservices:\n  web:\n    image: alpine\n    ssh:\n      enabled: true\n      port: 2222\n\
             ingress:\n  tcp:\n    - name: \"web\\nrouter\"\n      entryPoint: ssh\n      service: web\n";
        let err = parse_stack_yaml(tcp).unwrap_err();
        assert!(err.contains("ingress.tcp[0].name"), "{err}");
    }

    #[test]
    fn rejects_invalid_ingress_paths() {
        for bad in ["api", "/a b", "/a`b", "/a\"b", "/a\\b", ""] {
            let yaml = format!(
                "name: demo\nservices:\n  web:\n    image: alpine\n    ports: [\"8080:8000\"]\n\
                 ingress:\n  rules:\n    - host: demo.local\n      paths:\n\
                 \x20       - path: '{bad}'\n          service: web\n          port: 8000\n"
            );
            let err = parse_stack_yaml(&yaml).unwrap_err();
            assert!(err.contains("path"), "{bad:?}: {err}");
        }
    }

    #[test]
    fn rejects_invalid_ports_hostname_sugar() {
        let yaml = "name: demo\nservices:\n  web:\n    image: alpine\n    ports:\n      - \"Bad.Example.com:3000\"\n";
        let err = parse_stack_yaml(yaml).unwrap_err();
        assert!(err.contains("hostname"), "{err}");
    }

    #[test]
    fn rejects_duplicate_ingress_routers() {
        let yaml = "name: demo\nservices:\n  web:\n    image: alpine\n    ports: [\"8080:8000\"]\n\
             ingress:\n  rules:\n    - host: demo.local\n      paths:\n        - path: /\n          service: web\n          port: 8000\n\
             \x20   - host: demo.local\n      paths:\n        - path: /\n          service: web\n          port: 8000\n";
        let err = parse_stack_yaml(yaml).unwrap_err();
        assert!(err.contains("duplicate ingress router"), "{err}");
    }

    // ---- A7: closed profile set ------------------------------------------

    #[test]
    fn rejects_unknown_network_profiles() {
        for bad in ["privte", "local", "any", "PUBLIC", ""] {
            let yaml = format!(
                "name: demo\nservices:\n  web:\n    image: alpine\n    network:\n      profiles: [\"{bad}\"]\n"
            );
            let err = parse_stack_yaml(&yaml).unwrap_err();
            assert!(err.contains("network.profiles"), "{bad:?}: {err}");
        }
    }

    #[test]
    fn rejects_none_with_other_profiles_and_duplicates() {
        let yaml = "name: demo\nservices:\n  web:\n    image: alpine\n    network:\n      profiles: [none, public]\n";
        let err = parse_stack_yaml(yaml).unwrap_err();
        assert!(err.contains("only entry"), "{err}");

        let yaml = "name: demo\nservices:\n  web:\n    image: alpine\n    network:\n      profiles: [public, public]\n";
        let err = parse_stack_yaml(yaml).unwrap_err();
        assert!(err.contains("duplicate network.profiles"), "{err}");
    }

    #[test]
    fn accepts_the_closed_profile_set() {
        for p in ["public", "private", "host", "none"] {
            let yaml = format!(
                "name: demo\nservices:\n  web:\n    image: alpine\n    network:\n      profiles: [{p}]\n"
            );
            parse_stack_yaml(&yaml).unwrap_or_else(|e| panic!("{p} must be accepted: {e}"));
        }
        // Empty list keeps its default (public at runtime).
        let doc = parse_stack_yaml("name: demo\nservices:\n  web:\n    image: alpine\n").unwrap();
        assert!(doc.services["web"].network.profiles.is_empty());
    }

    // ---- A9: disk sizes --------------------------------------------------

    #[test]
    fn volume_size_parses_and_json_roundtrips() {
        let doc = parse_stack_yaml(&volume_stack(
            "  data:\n    kind: dir\n    size: 10GiB",
            "      - name: data\n        target: /data",
        ))
        .unwrap();
        assert_eq!(doc.volumes["data"].size_mib, 10 * 1024);

        // Stored-document JSON round-trip is lossless (bytes form on the wire).
        let json = serde_json::to_string(&doc).unwrap();
        assert!(json.contains("\"size\":10737418240"), "{json}");
        let back: StackDocument = serde_json::from_str(&json).unwrap();
        assert_eq!(back.volumes["data"].size_mib, 10 * 1024);
        assert_eq!(back.name, doc.name);
    }

    #[test]
    fn omitted_sizes_get_the_documented_defaults() {
        let doc = parse_stack_yaml(&volume_stack(
            "  data:\n    kind: dir",
            "      - name: data\n        target: /data",
        ))
        .unwrap();
        assert_eq!(doc.volumes["data"].size_mib, 10 * 1024, "volume default");
        let svc = &doc.services["web"];
        assert!(svc.storage_opt.is_none());
        assert_eq!(svc.root_disk_mib(), 4 * 1024, "root-disk default");
        // Mounts default to the volume default until apply copies the volume's size.
        assert_eq!(svc.volumes[0].size_mib, 10 * 1024);
    }

    #[test]
    fn storage_opt_size_parses_serializes_and_has_a_default() {
        let yaml = r#"
name: demo
services:
  web:
    image: alpine:3.20
    storage_opt:
      size: 8GiB
"#;
        let doc = parse_stack_yaml(yaml).unwrap();
        let svc = &doc.services["web"];
        assert_eq!(svc.storage_opt.as_ref().unwrap().size_mib, 8 * 1024);
        assert_eq!(svc.root_disk_mib(), 8 * 1024);

        let json = serde_json::to_string(svc).unwrap();
        assert!(json.contains("\"size\":8589934592"), "{json}");
        let back: ServiceSpec = serde_json::from_str(&json).unwrap();
        assert_eq!(back.root_disk_mib(), 8 * 1024);

        // A `storage_opt` block without `size` uses the 4 GiB default.
        let doc = parse_stack_yaml(
            "name: demo\nservices:\n  web:\n    image: alpine\n    storage_opt: {}\n",
        )
        .unwrap();
        assert_eq!(doc.services["web"].root_disk_mib(), 4 * 1024);
    }

    #[test]
    fn size_units_are_parsed_like_mem_limit() {
        for (raw, mib) in [
            ("512m", 512),
            ("1g", 1024),
            ("1.5g", 1536),
            ("2gb", 2048),
            ("10GiB", 10 * 1024),
            ("512MiB", 512),
            ("1t", 1024 * 1024),
            ("1048576", 1),
        ] {
            let yaml = format!(
                "name: demo\nservices:\n  web:\n    image: alpine\n    storage_opt:\n      size: \"{raw}\"\n"
            );
            let doc = parse_stack_yaml(&yaml).unwrap_or_else(|e| panic!("{raw}: {e}"));
            assert_eq!(doc.services["web"].root_disk_mib(), mib, "{raw}");
        }
    }

    #[test]
    fn zero_and_oversized_sizes_are_rejected() {
        let err = parse_stack_yaml(&volume_stack(
            "  data:\n    kind: dir\n    size: 0",
            "      - name: data\n        target: /data",
        ))
        .unwrap_err();
        assert!(err.contains("size must be greater than 0"), "{err}");

        let err = parse_stack_yaml(&volume_stack(
            "  data:\n    kind: dir\n    size: 2TiB",
            "      - name: data\n        target: /data",
        ))
        .unwrap_err();
        assert!(err.contains("maximum of"), "{err}");

        let err = parse_stack_yaml(
            "name: demo\nservices:\n  web:\n    image: alpine\n    storage_opt:\n      size: 0\n",
        )
        .unwrap_err();
        assert!(err.contains("storage_opt.size"), "{err}");
        assert!(err.contains("greater than 0"), "{err}");

        // A malformed unit is a parse error, not a silent default.
        let err = parse_stack_yaml(
            "name: demo\nservices:\n  web:\n    image: alpine\n    storage_opt:\n      size: 10qb\n",
        )
        .unwrap_err();
        assert!(err.contains("unsupported size unit"), "{err}");
    }

    #[test]
    fn mount_size_is_not_a_user_facing_yaml_key() {
        // The propagated mount size is an internal field; the declared size
        // lives on the volume. `deny_unknown_fields` keeps the mount strict.
        let err = parse_stack_yaml(&volume_stack(
            "  data:\n    kind: dir",
            "      - name: data\n        target: /data\n        size: 1GiB",
        ))
        .unwrap_err();
        assert!(err.contains("unknown field"), "{err}");
    }

    #[test]
    fn with_volume_sizes_propagates_the_declared_size() {
        let doc = parse_stack_yaml(&volume_stack(
            "  data:\n    kind: dir\n    size: 20GiB",
            "      - name: data\n        target: /data",
        ))
        .unwrap();
        let mut spec = doc.services["web"].clone();
        spec.with_volume_sizes(&doc.volumes);
        assert_eq!(spec.volumes[0].size_mib, 20 * 1024);
        assert_eq!(spec.volumes[0].name, "data");
        assert_eq!(spec.volumes[0].mount, "/data");
    }

    #[test]
    fn stored_spec_with_resolved_ports_round_trips() {
        // The server stores specs as JSON after resolving host ports; every
        // optional key serializes as `null` and must read back.
        let doc = parse_stack_yaml(
            "name: s\nservices:\n  web:\n    image: alpine\n    ports:\n      - \"3001\"\n      - \"app.example.com:3002\"\n",
        )
        .unwrap();
        let mut spec = doc.services["web"].clone();
        spec.ports[0].published = 10000;
        spec.ports[1].published = 10001;
        let json = serde_json::to_string(&spec).unwrap();
        let back: ServiceSpec = serde_json::from_str(&json).unwrap();
        assert_eq!(back.ports[0].published, 10000);
        assert_eq!(back.ports[0].hostname, None);
        assert_eq!(back.ports[1].hostname.as_deref(), Some("app.example.com"));
    }
}
