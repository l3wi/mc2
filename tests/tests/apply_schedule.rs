//! Integration: apply stack → schedule onto the local node → desired set
//! resolves → store writes mark Running.
//!
//! Does not start microVMs (no hypervisor in CI). The "node" is driven by
//! direct store writes, the same rows the in-process node loop would write.

use mc2_server::build_desired_set;
use mc2_store::{SecretsKey, Store};
use mc2_tests::TestCluster;
use reqwest::StatusCode;

const DEMO: &str = r#"
name: demo
services:
  web:
    image: python:3.12
    scale: 2
    cpus: 1
    mem_limit: 512m
"#;

#[tokio::test]
async fn apply_schedules_and_reports_running() {
    let cluster = TestCluster::start().await.expect("start");

    // Apply stack
    let url = format!("{}/v1/stacks:apply", cluster.base_url);
    let res = cluster
        .client()
        .post(&url)
        .bearer_auth(&cluster.api_token)
        .json(&serde_json::json!({ "yaml": DEMO }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["stack"], "demo");
    assert_eq!(body["instances"], 2);
    assert_eq!(body["scheduled"], 2);
    assert_eq!(body["pending"], 0);

    // Desired set resolves for the local node (what the node loop would run).
    let key = SecretsKey::load_file(&cluster.data_dir.join("secrets.key")).unwrap();
    let (desired, routes) = build_desired_set(cluster.store.clone(), &key, &cluster.local_node_id)
        .await
        .expect("build desired set");
    assert_eq!(desired.len(), 2);
    assert!(routes.is_empty());
    let mut names: Vec<String> = desired.iter().map(|d| d.runtime_id.clone()).collect();
    names.sort();
    assert_eq!(names, vec!["demo-web-0", "demo-web-1"]);

    // Simulate the node loop's store writes after SDK reconcile.
    for d in &desired {
        cluster
            .store
            .update_instance_status(&d.instance_id, "Running", Some(&d.runtime_id), None)
            .await
            .unwrap();
    }

    let (st, instances) = cluster
        .get_json("/v1/instances", Some(&cluster.api_token))
        .await
        .unwrap();
    assert_eq!(st, StatusCode::OK);
    let arr = instances.as_array().unwrap();
    assert_eq!(arr.len(), 2);
    assert!(arr.iter().all(|i| i["phase"] == "Running"));
    assert!(arr
        .iter()
        .all(|i| i["node_id"] == serde_json::Value::String(cluster.local_node_id.clone())));
}

#[tokio::test]
async fn apply_without_nodes_leaves_pending() {
    let cluster = TestCluster::start_with_node(false)
        .await
        .expect("start without node");
    let url = format!("{}/v1/stacks:apply", cluster.base_url);
    let res = cluster
        .client()
        .post(&url)
        .bearer_auth(&cluster.api_token)
        .json(&serde_json::json!({ "yaml": DEMO }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["scheduled"], 0);
    assert_eq!(body["pending"], 2);
}

#[tokio::test]
async fn scaled_ports_persist_per_replica_with_environment_key() {
    let cluster = TestCluster::start().await.expect("start");
    let yaml = r#"
name: scaled
services:
  web:
    image: python:3.12
    scale: 3
    cpus: 1
    mem_limit: 512m
    environment:
      MODE: lab
    ports:
      - "5000:3000"
      - "3001"
    depends_on:
      - db
  db:
    image: postgres:16
    expose: [5432]
    healthcheck:
      test: ["pg_isready"]
"#;
    let url = format!("{}/v1/stacks:apply", cluster.base_url);
    let res = cluster
        .client()
        .post(&url)
        .bearer_auth(&cluster.api_token)
        .json(&serde_json::json!({ "yaml": yaml }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["instances"], 4);

    // Per-replica specs carry distinct published ports; `environment` (not `env`).
    let instances = cluster.store.list_instances().await.unwrap();
    let web: Vec<_> = instances.iter().filter(|i| i.service == "web").collect();
    assert_eq!(web.len(), 3);
    let mut fixed_ports: Vec<u16> = Vec::new();
    let mut auto_ports: Vec<u16> = Vec::new();
    for inst in &web {
        let spec: mc2_api::ServiceSpec = serde_json::from_str(&inst.spec_json).unwrap();
        assert_eq!(spec.env.get("MODE").unwrap(), "lab");
        assert_eq!(spec.depends_on.len(), 1);
        fixed_ports.push(spec.ports[0].published);
        auto_ports.push(spec.ports[1].published);
    }
    assert_eq!(fixed_ports, vec![5000, 5001, 5002]);
    let mut uniq_auto = auto_ports.clone();
    uniq_auto.sort_unstable();
    uniq_auto.dedup();
    assert_eq!(
        uniq_auto.len(),
        3,
        "auto host ports must be distinct per replica"
    );
    assert!(auto_ports.iter().all(|p| *p >= 10000));
}
