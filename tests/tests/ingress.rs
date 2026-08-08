//! Integration / smoke: Ingress catalog plan (routes ↔ host ports) + lifecycle.
//!
//! CI does not boot microVMs or Traefik. These tests exercise the same
//! control-plane path: apply stack → desired set `ingress_routes` → REST
//! `/v1/ingress`, plus re-apply / store-report lifecycle. File ready-gate +
//! TCP probe coverage lives in `mc2-server` unit tests (`ingress_files`).

use mc2_runtime::render_traefik_dynamic;
use mc2_server::build_desired_set;
use mc2_store::{NodeJoin, SecretsKey, Store};
use mc2_tests::TestCluster;
use reqwest::StatusCode;
use std::sync::Arc;

const STACK_INGRESS: &str = r#"
name: smoke-ingress
services:
  web:
    image: alpine:3.20
    scale: 1
    cpus: 1
    mem_limit: 128m
    ports:
      - "18080:8000"
    network:
      profiles: [public]
    restart: on-failure
    command: ["sleep", "infinity"]
ingress:
  tls:
    enabled: true
    certResolver: le
  rules:
    - host: smoke-ingress.local
      paths:
        - path: /
          pathType: Prefix
          service: web
          port: 8000
"#;

const STACK_INGRESS_UPDATED: &str = r#"
name: smoke-ingress
services:
  web:
    image: alpine:3.20
    scale: 1
    ports:
      - "19090:8000"
    restart: on-failure
    command: ["sleep", "infinity"]
ingress:
  tls:
    enabled: false
  rules:
    - host: smoke-ingress.local
      paths:
        - path: /v2
          pathType: Prefix
          service: web
          port: 8000
"#;

const STACK_NO_INGRESS: &str = r#"
name: smoke-ingress
services:
  web:
    image: alpine:3.20
    scale: 1
    ports:
      - "18080:8000"
    restart: on-failure
    command: ["sleep", "infinity"]
"#;

const STACK_MULTI_PATH: &str = r#"
name: multi
services:
  web:
    image: alpine:3.20
    scale: 1
    ports:
      - "18080:8000"
      - "18081:8001"
    restart: on-failure
    command: ["sleep", "infinity"]
ingress:
  rules:
    - host: multi.local
      paths:
        - path: /api
          service: web
          port: 8001
        - path: /
          service: web
          port: 8000
"#;

fn load_key(cluster: &TestCluster) -> SecretsKey {
    SecretsKey::load_file(&cluster.data_dir.join("secrets.key")).unwrap()
}

async fn desired_routes(
    cluster: &TestCluster,
) -> (
    Vec<mc2_runtime::DesiredSandbox>,
    Vec<mc2_runtime::DesiredIngressRoute>,
) {
    let key = load_key(cluster);
    build_desired_set(cluster.store.clone(), &key, &cluster.local_node_id)
        .await
        .expect("build desired set")
}

async fn apply_yaml(cluster: &TestCluster, yaml: &str) -> serde_json::Value {
    let res = cluster
        .client()
        .post(format!("{}/v1/stacks:apply", cluster.base_url))
        .bearer_auth(&cluster.api_token)
        .json(&serde_json::json!({ "yaml": yaml }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK, "{res:?}");
    res.json().await.unwrap()
}

/// Smoke: apply → desired set carries routes bound to `ports:` host/guest →
/// REST catalogs them.
#[tokio::test]
async fn apply_desired_routes_map_to_host_ports() {
    let cluster = TestCluster::start().await.expect("start");

    let body = apply_yaml(&cluster, STACK_INGRESS).await;
    assert_eq!(body["stack"], "smoke-ingress");
    assert_eq!(body["scheduled"], 1);

    let (instances, routes) = desired_routes(&cluster).await;
    assert_eq!(instances.len(), 1);
    assert_eq!(routes.len(), 1, "expected one ingress route in desired set");
    let r = &routes[0];
    assert_eq!(r.stack, "smoke-ingress");
    assert_eq!(r.host, "smoke-ingress.local");
    assert_eq!(r.path, "/");
    assert_eq!(r.service, "web");
    assert_eq!(r.guest_port, 8000, "guest port from ingress path");
    assert_eq!(r.host_port, 18080, "host port from ports:");
    assert_eq!(r.bind, "127.0.0.1");
    assert!(r.tls_enabled);
    assert_eq!(r.cert_resolver, "le");
    assert_eq!(r.backend_instance_id, instances[0].instance_id);
    assert!(!r.id.is_empty());

    // Pure render matches catalog → proxy upstream for that host port.
    let ready = r.to_ready("127.0.0.1");
    let traefik = render_traefik_dynamic(std::slice::from_ref(&ready));
    assert!(
        traefik.contains("http://127.0.0.1:18080"),
        "traefik must target host port: {traefik}"
    );
    assert!(traefik.contains("Host(`smoke-ingress.local`)"), "{traefik}");

    let (st, ingress) = cluster
        .get_json("/v1/ingress", Some(&cluster.api_token))
        .await
        .unwrap();
    assert_eq!(st, StatusCode::OK);
    let routes = ingress["routes"].as_array().expect("routes array");
    assert_eq!(routes.len(), 1);
    assert_eq!(routes[0]["host"], "smoke-ingress.local");
    assert_eq!(routes[0]["hostPort"], 18080);
    assert_eq!(routes[0]["guestPort"], 8000);
    assert_eq!(routes[0]["service"], "web");
    assert_eq!(routes[0]["nodeId"], cluster.local_node_id);
}

/// Validation smoke: ingress referencing a missing backend port → 400 (not 500).
#[tokio::test]
async fn apply_rejects_bad_ingress() {
    let cluster = TestCluster::start().await.expect("start");

    let missing_ports = r#"
name: bad
services:
  web:
    image: alpine
ingress:
  rules:
    - host: x.local
      paths:
        - service: web
          port: 8000
"#;
    let res = cluster
        .client()
        .post(format!("{}/v1/stacks:apply", cluster.base_url))
        .bearer_auth(&cluster.api_token)
        .json(&serde_json::json!({ "yaml": missing_ports }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let err = res.text().await.unwrap();
    assert!(err.contains("ports") || err.contains("guest"), "{err}");

    // Target-only ports (auto host allocation) are now valid.
    let auto_port = r#"
name: bad2
services:
  web:
    image: alpine
    ports:
      - "8000"
"#;
    let res = cluster
        .client()
        .post(format!("{}/v1/stacks:apply", cluster.base_url))
        .bearer_auth(&cluster.api_token)
        .json(&serde_json::json!({ "yaml": auto_port }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        res.status(),
        StatusCode::OK,
        "target-only ports auto-allocate"
    );
}

/// Lifecycle: host/path/port update on re-apply; remove ingress; multi-path; Running report.
#[tokio::test]
async fn ingress_lifecycle_update_remove_and_multi_path() {
    let cluster = TestCluster::start().await.expect("start");

    // 1) Create
    apply_yaml(&cluster, STACK_INGRESS).await;

    let (instances, routes) = desired_routes(&cluster).await;
    assert_eq!(routes.len(), 1);
    assert_eq!(routes[0].host_port, 18080);
    let instance_id = instances[0].instance_id.clone();
    let runtime_id = mc2_runtime::sandbox_name("smoke-ingress", "web", 0);

    cluster
        .store
        .update_instance_status(&instance_id, "Running", Some(&runtime_id), None)
        .await
        .unwrap();

    let (st, insts) = cluster
        .get_json("/v1/instances", Some(&cluster.api_token))
        .await
        .unwrap();
    assert_eq!(st, StatusCode::OK);
    assert_eq!(insts.as_array().unwrap()[0]["phase"], "Running");

    // After Running, the desired set still exposes the same host→guest mapping.
    let (_, routes_running) = desired_routes(&cluster).await;
    assert_eq!(routes_running[0].host_port, 18080);
    assert_eq!(routes_running[0].backend_instance_id, instance_id);

    // 2) Update path + host port
    apply_yaml(&cluster, STACK_INGRESS_UPDATED).await;

    let (_, routes2) = desired_routes(&cluster).await;
    assert_eq!(routes2.len(), 1);
    assert_eq!(routes2[0].path, "/v2");
    assert_eq!(routes2[0].host_port, 19090);
    assert!(!routes2[0].tls_enabled);

    let (st, ingress) = cluster
        .get_json("/v1/ingress", Some(&cluster.api_token))
        .await
        .unwrap();
    assert_eq!(st, StatusCode::OK);
    assert_eq!(ingress["routes"][0]["path"], "/v2");
    assert_eq!(ingress["routes"][0]["hostPort"], 19090);

    // 3) Remove ingress section
    apply_yaml(&cluster, STACK_NO_INGRESS).await;

    let (_, routes3) = desired_routes(&cluster).await;
    assert!(routes3.is_empty(), "ingress removed: {:?}", routes3);
    let (st, ingress) = cluster
        .get_json("/v1/ingress", Some(&cluster.api_token))
        .await
        .unwrap();
    assert_eq!(st, StatusCode::OK);
    assert_eq!(
        ingress["routes"].as_array().map(|a| a.len()).unwrap_or(0),
        0
    );

    // 4) Multi-path stack (fresh apply on same cluster, new stack name)
    apply_yaml(&cluster, STACK_MULTI_PATH).await;

    let (instances4, routes4) = desired_routes(&cluster).await;
    // smoke-ingress has no ingress; multi has two routes
    assert_eq!(routes4.len(), 2);
    let ports: Vec<u16> = routes4.iter().map(|r| r.host_port).collect();
    assert!(ports.contains(&18080), "{ports:?}");
    assert!(ports.contains(&18081), "{ports:?}");
    let guests: Vec<u16> = routes4.iter().map(|r| r.guest_port).collect();
    assert!(
        guests.contains(&8000) && guests.contains(&8001),
        "{guests:?}"
    );

    // 5) Report Stopped — plan still lists backend, but the node file gate
    //    (unit tests) would drop ready; CP still returns desired routes.
    let multi_id = instances4
        .iter()
        .find(|i| i.stack == "multi")
        .map(|i| i.instance_id.clone())
        .expect("multi instance");
    cluster
        .store
        .update_instance_status(
            &multi_id,
            "Stopped",
            Some(&mc2_runtime::sandbox_name("multi", "web", 0)),
            Some("lifecycle stop"),
        )
        .await
        .unwrap();

    let (_, routes5) = desired_routes(&cluster).await;
    // Routes still planned from YAML (node ready-gate is separate).
    assert_eq!(routes5.len(), 2);
}

/// No routes until instances are scheduled on a node.
#[tokio::test]
async fn no_ingress_routes_without_local_instances() {
    let cluster = TestCluster::start_with_node(false)
        .await
        .expect("start without node");

    // Apply without a node → Pending, not on any node.
    let body = apply_yaml(&cluster, STACK_INGRESS).await;
    assert_eq!(body["pending"], 1);

    let (st, ingress) = cluster
        .get_json("/v1/ingress", Some(&cluster.api_token))
        .await
        .unwrap();
    assert_eq!(st, StatusCode::OK);
    assert_eq!(
        ingress["routes"].as_array().map(|a| a.len()).unwrap_or(0),
        0,
        "no node-bound instances → empty ingress catalog"
    );

    // Node appears, then schedule: routes appear.
    let node = cluster
        .store
        .upsert_local_node(NodeJoin {
            name: "late-node".into(),
            labels_json: "{}".into(),
            arch: "aarch64".into(),
            cpus: 4,
            memory_mib: 8192,
        })
        .await
        .unwrap();
    let store: Arc<dyn Store> = cluster.store.clone();
    let scheduled = mc2_server::run_scheduler(store).await.expect("schedule");
    assert_eq!(scheduled, 1);

    let key = load_key(&cluster);
    let (_, routes) = build_desired_set(cluster.store.clone(), &key, &node.id)
        .await
        .expect("build desired set");
    assert_eq!(routes.len(), 1);
    assert_eq!(routes[0].host_port, 18080);
}
