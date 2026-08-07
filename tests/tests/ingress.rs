//! Integration / smoke: Ingress catalog plan (routes ↔ host ports) + lifecycle.
//!
//! CI does not boot microVMs or Traefik. These tests exercise the same
//! control-plane path: apply stack → Sync `ingress_routes` → REST `/v1/ingress`,
//! plus re-apply / report-status lifecycle. File ready-gate + TCP probe coverage
//! lives in `mc2-agent` unit tests (`ingress_files`).

use mc2_api::agent::agent_service_client::AgentServiceClient;
use mc2_api::agent::{Capacity, InstanceStatus, JoinRequest, ReportStatusRequest, SyncRequest};
use mc2_runtime::{render_traefik_dynamic, DesiredIngressRoute};
use mc2_tests::TestCluster;
use reqwest::StatusCode;
use std::collections::HashMap;

const STACK_INGRESS: &str = r#"
apiVersion: mc2/v1
kind: Stack
metadata:
  name: smoke-ingress
  labels:
    purpose: ingress-smoke
services:
  web:
    image: alpine:3.20
    replicas: 1
    resources:
      cpus: 1
      memoryMiB: 128
    ports:
      - host: 18080
        guest: 8000
        protocol: tcp
    network:
      profiles: [public]
    restartPolicy: on-failure
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
apiVersion: mc2/v1
kind: Stack
metadata:
  name: smoke-ingress
services:
  web:
    image: alpine:3.20
    replicas: 1
    ports:
      - host: 19090
        guest: 8000
    restartPolicy: on-failure
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
apiVersion: mc2/v1
kind: Stack
metadata:
  name: smoke-ingress
services:
  web:
    image: alpine:3.20
    replicas: 1
    ports:
      - host: 18080
        guest: 8000
    restartPolicy: on-failure
    command: ["sleep", "infinity"]
"#;

const STACK_MULTI_PATH: &str = r#"
apiVersion: mc2/v1
kind: Stack
metadata:
  name: multi
services:
  web:
    image: alpine:3.20
    replicas: 1
    ports:
      - host: 18080
        guest: 8000
      - host: 18081
        guest: 8001
    restartPolicy: on-failure
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

async fn join_worker(
    cluster: &TestCluster,
    name: &str,
) -> (
    AgentServiceClient<tonic::transport::Channel>,
    String,
    String,
) {
    let mut agent = AgentServiceClient::connect(cluster.grpc_url.clone())
        .await
        .expect("grpc connect");
    let join = agent
        .join(JoinRequest {
            join_token: cluster.join_token.clone(),
            node_name: name.into(),
            labels: HashMap::new(),
            arch: "aarch64".into(),
            capacity: Some(Capacity {
                cpus: 4,
                memory_mib: 8192,
            }),
        })
        .await
        .expect("join")
        .into_inner();
    (agent, join.node_id, join.node_token)
}

fn report_running(instance_id: &str, runtime_id: &str) -> InstanceStatus {
    InstanceStatus {
        instance_id: instance_id.into(),
        phase: "Running".into(),
        message: "ingress smoke (no hypervisor)".into(),
        runtime_id: runtime_id.into(),
        ssh: None,
        fabric: None,
    }
}

/// Smoke: apply → Sync carries routes bound to `ports:` host/guest → REST catalogs them.
#[tokio::test]
async fn apply_sync_catalog_routes_map_to_host_ports() {
    let cluster = TestCluster::start().await.expect("start");
    let (mut agent, node_id, node_token) = join_worker(&cluster, "ingress-worker").await;

    let apply = cluster
        .client()
        .post(format!("{}/v1/stacks:apply", cluster.base_url))
        .bearer_auth(&cluster.api_token)
        .json(&serde_json::json!({ "yaml": STACK_INGRESS }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        apply.status(),
        StatusCode::OK,
        "{}",
        apply.text().await.unwrap()
    );
    let body: serde_json::Value = apply.json().await.unwrap();
    assert_eq!(body["stack"], "smoke-ingress");
    assert_eq!(body["scheduled"], 1);

    let sync = agent
        .sync(SyncRequest {
            node_id: node_id.clone(),
            node_token: node_token.clone(),
        })
        .await
        .expect("sync")
        .into_inner();

    assert_eq!(sync.instances.len(), 1);
    assert_eq!(
        sync.ingress_routes.len(),
        1,
        "expected one ingress route on Sync"
    );
    let r = &sync.ingress_routes[0];
    assert_eq!(r.stack, "smoke-ingress");
    assert_eq!(r.host, "smoke-ingress.local");
    assert_eq!(r.path, "/");
    assert_eq!(r.service, "web");
    assert_eq!(r.guest_port, 8000, "guest port from ingress path");
    assert_eq!(r.host_port, 18080, "host port from ports:");
    assert_eq!(r.bind, "127.0.0.1");
    assert!(r.tls_enabled);
    assert_eq!(r.cert_resolver, "le");
    assert_eq!(r.backend_instance_id, sync.instances[0].instance_id);
    assert!(!r.id.is_empty());

    // Pure render matches catalog → proxy upstream for that host port.
    let ready = DesiredIngressRoute {
        id: r.id.clone(),
        stack: r.stack.clone(),
        host: r.host.clone(),
        path: r.path.clone(),
        path_type: r.path_type.clone(),
        service: r.service.clone(),
        guest_port: r.guest_port as u16,
        host_port: r.host_port as u16,
        bind: r.bind.clone(),
        tls_enabled: r.tls_enabled,
        cert_resolver: r.cert_resolver.clone(),
        tcp: r.tcp,
        entry_point: r.entry_point.clone(),
        backend_instance_id: r.backend_instance_id.clone(),
        backend_ordinal: r.backend_ordinal,
    }
    .to_ready("127.0.0.1");
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
    assert_eq!(routes[0]["nodeId"], node_id);
}

/// Validation smoke: missing ports / non-loopback → 400 (not 500).
#[tokio::test]
async fn apply_rejects_bad_ingress() {
    let cluster = TestCluster::start().await.expect("start");

    let missing_ports = r#"
apiVersion: mc2/v1
kind: Stack
metadata:
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

    let non_loopback = r#"
apiVersion: mc2/v1
kind: Stack
metadata:
  name: bad2
services:
  web:
    image: alpine
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
    let res = cluster
        .client()
        .post(format!("{}/v1/stacks:apply", cluster.base_url))
        .bearer_auth(&cluster.api_token)
        .json(&serde_json::json!({ "yaml": non_loopback }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let err = res.text().await.unwrap();
    assert!(err.contains("loopback"), "{err}");
}

/// Lifecycle: host/path/port update on re-apply; remove ingress; multi-path; Running report.
#[tokio::test]
async fn ingress_lifecycle_update_remove_and_multi_path() {
    let cluster = TestCluster::start().await.expect("start");
    let (mut agent, node_id, node_token) = join_worker(&cluster, "ingress-life").await;

    // 1) Create
    let apply = cluster
        .client()
        .post(format!("{}/v1/stacks:apply", cluster.base_url))
        .bearer_auth(&cluster.api_token)
        .json(&serde_json::json!({ "yaml": STACK_INGRESS }))
        .send()
        .await
        .unwrap();
    assert_eq!(apply.status(), StatusCode::OK);

    let sync1 = agent
        .sync(SyncRequest {
            node_id: node_id.clone(),
            node_token: node_token.clone(),
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(sync1.ingress_routes.len(), 1);
    assert_eq!(sync1.ingress_routes[0].host_port, 18080);
    let instance_id = sync1.instances[0].instance_id.clone();
    let runtime_id = mc2_runtime::sandbox_name("smoke-ingress", "web", 0);

    agent
        .report_status(ReportStatusRequest {
            node_id: node_id.clone(),
            node_token: node_token.clone(),
            instances: vec![report_running(&instance_id, &runtime_id)],
        })
        .await
        .unwrap();

    let (st, instances) = cluster
        .get_json("/v1/instances", Some(&cluster.api_token))
        .await
        .unwrap();
    assert_eq!(st, StatusCode::OK);
    assert_eq!(instances.as_array().unwrap()[0]["phase"], "Running");

    // After Running, Sync still exposes same host→guest mapping.
    let sync_running = agent
        .sync(SyncRequest {
            node_id: node_id.clone(),
            node_token: node_token.clone(),
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(sync_running.ingress_routes[0].host_port, 18080);
    assert_eq!(
        sync_running.ingress_routes[0].backend_instance_id,
        instance_id
    );

    // 2) Update path + host port
    let apply2 = cluster
        .client()
        .post(format!("{}/v1/stacks:apply", cluster.base_url))
        .bearer_auth(&cluster.api_token)
        .json(&serde_json::json!({ "yaml": STACK_INGRESS_UPDATED }))
        .send()
        .await
        .unwrap();
    assert_eq!(apply2.status(), StatusCode::OK);

    let sync2 = agent
        .sync(SyncRequest {
            node_id: node_id.clone(),
            node_token: node_token.clone(),
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(sync2.ingress_routes.len(), 1);
    assert_eq!(sync2.ingress_routes[0].path, "/v2");
    assert_eq!(sync2.ingress_routes[0].host_port, 19090);
    assert!(!sync2.ingress_routes[0].tls_enabled);

    let (st, ingress) = cluster
        .get_json("/v1/ingress", Some(&cluster.api_token))
        .await
        .unwrap();
    assert_eq!(st, StatusCode::OK);
    assert_eq!(ingress["routes"][0]["path"], "/v2");
    assert_eq!(ingress["routes"][0]["hostPort"], 19090);

    // 3) Remove ingress section
    let apply3 = cluster
        .client()
        .post(format!("{}/v1/stacks:apply", cluster.base_url))
        .bearer_auth(&cluster.api_token)
        .json(&serde_json::json!({ "yaml": STACK_NO_INGRESS }))
        .send()
        .await
        .unwrap();
    assert_eq!(apply3.status(), StatusCode::OK);

    let sync3 = agent
        .sync(SyncRequest {
            node_id: node_id.clone(),
            node_token: node_token.clone(),
        })
        .await
        .unwrap()
        .into_inner();
    assert!(
        sync3.ingress_routes.is_empty(),
        "ingress removed: {:?}",
        sync3.ingress_routes
    );
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
    let apply4 = cluster
        .client()
        .post(format!("{}/v1/stacks:apply", cluster.base_url))
        .bearer_auth(&cluster.api_token)
        .json(&serde_json::json!({ "yaml": STACK_MULTI_PATH }))
        .send()
        .await
        .unwrap();
    assert_eq!(apply4.status(), StatusCode::OK);

    let sync4 = agent
        .sync(SyncRequest {
            node_id: node_id.clone(),
            node_token: node_token.clone(),
        })
        .await
        .unwrap()
        .into_inner();
    // smoke-ingress has no ingress; multi has two routes
    assert_eq!(sync4.ingress_routes.len(), 2);
    let ports: Vec<u32> = sync4.ingress_routes.iter().map(|r| r.host_port).collect();
    assert!(ports.contains(&18080), "{ports:?}");
    assert!(ports.contains(&18081), "{ports:?}");
    let guests: Vec<u32> = sync4.ingress_routes.iter().map(|r| r.guest_port).collect();
    assert!(
        guests.contains(&8000) && guests.contains(&8001),
        "{guests:?}"
    );

    // 5) Report Stopped — plan still lists backend, but agent file gate (unit tests)
    //    would drop ready; CP still returns desired routes.
    let multi_id = sync4
        .instances
        .iter()
        .find(|i| i.stack == "multi")
        .map(|i| i.instance_id.clone())
        .expect("multi instance");
    agent
        .report_status(ReportStatusRequest {
            node_id: node_id.clone(),
            node_token: node_token.clone(),
            instances: vec![InstanceStatus {
                instance_id: multi_id,
                phase: "Stopped".into(),
                message: "lifecycle stop".into(),
                runtime_id: mc2_runtime::sandbox_name("multi", "web", 0),
                ssh: None,
                fabric: None,
            }],
        })
        .await
        .unwrap();

    let sync5 = agent
        .sync(SyncRequest {
            node_id,
            node_token,
        })
        .await
        .unwrap()
        .into_inner();
    // Routes still planned from YAML (agent ready-gate is separate).
    assert_eq!(sync5.ingress_routes.len(), 2);
}

/// Sync before any node: no routes until instances are scheduled on a node.
#[tokio::test]
async fn no_ingress_routes_without_local_instances() {
    let cluster = TestCluster::start().await.expect("start");
    // Apply without a node → Pending, not on any node.
    let apply = cluster
        .client()
        .post(format!("{}/v1/stacks:apply", cluster.base_url))
        .bearer_auth(&cluster.api_token)
        .json(&serde_json::json!({ "yaml": STACK_INGRESS }))
        .send()
        .await
        .unwrap();
    assert_eq!(apply.status(), StatusCode::OK);
    let body: serde_json::Value = apply.json().await.unwrap();
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

    // Join then schedule: routes appear.
    let (mut agent, node_id, node_token) = join_worker(&cluster, "late-worker").await;
    // Trigger schedule by re-apply
    let apply2 = cluster
        .client()
        .post(format!("{}/v1/stacks:apply", cluster.base_url))
        .bearer_auth(&cluster.api_token)
        .json(&serde_json::json!({ "yaml": STACK_INGRESS }))
        .send()
        .await
        .unwrap();
    assert_eq!(apply2.status(), StatusCode::OK);

    let sync = agent
        .sync(SyncRequest {
            node_id,
            node_token,
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(sync.ingress_routes.len(), 1);
    assert_eq!(sync.ingress_routes[0].host_port, 18080);
}
