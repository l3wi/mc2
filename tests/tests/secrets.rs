//! Integration: cluster secrets + node injection material for sandboxes.
//!
//! CI does not boot microVMs. These tests exercise the same control-plane path
//! the node loop uses before `Sandbox::create_detached` (set → apply → desired set).

use mc2_server::build_desired_set;
use mc2_store::{SecretsKey, Store};
use mc2_tests::TestCluster;
use reqwest::StatusCode;

const SECRET_NAME: &str = "SMOKE_TOKEN";
const SECRET_VALUE: &str = "smoke-secret-value-never-echo";
const SECRET_ENV: &str = "API_TOKEN";
const ALLOW_HOST: &str = "api.example.com";

const STACK_WITH_SECRET: &str = r#"
name: smoke-secrets
services:
  keep:
    image: alpine:3.20
    scale: 1
    cpus: 1
    mem_limit: 128m
    network:
      profiles: [public]
    secrets:
      - name: SMOKE_TOKEN
        env: API_TOKEN
        allowHosts:
          - api.example.com
    restart: on-failure
    command: ["sleep", "infinity"]
"#;

fn load_key(cluster: &TestCluster) -> SecretsKey {
    SecretsKey::load_file(&cluster.data_dir.join("secrets.key")).unwrap()
}

#[tokio::test]
async fn set_list_delete_secret_no_value_echo() {
    let cluster = TestCluster::start().await.expect("start");

    let put = cluster
        .client()
        .put(format!("{}/v1/secrets/{SECRET_NAME}", cluster.base_url))
        .bearer_auth(&cluster.api_token)
        .json(&serde_json::json!({ "value": SECRET_VALUE }))
        .send()
        .await
        .unwrap();
    assert_eq!(put.status(), StatusCode::OK);
    let meta: serde_json::Value = put.json().await.unwrap();
    assert_eq!(meta["name"], SECRET_NAME);
    assert!(meta.get("value").is_none());
    let body = meta.to_string();
    assert!(!body.contains(SECRET_VALUE));

    let list = cluster
        .client()
        .get(format!("{}/v1/secrets", cluster.base_url))
        .bearer_auth(&cluster.api_token)
        .send()
        .await
        .unwrap();
    assert_eq!(list.status(), StatusCode::OK);
    let names: serde_json::Value = list.json().await.unwrap();
    assert_eq!(names.as_array().unwrap().len(), 1);
    assert_eq!(names[0]["name"], SECRET_NAME);
    assert!(!names.to_string().contains(SECRET_VALUE));

    let del = cluster
        .client()
        .delete(format!("{}/v1/secrets/{SECRET_NAME}", cluster.base_url))
        .bearer_auth(&cluster.api_token)
        .send()
        .await
        .unwrap();
    assert_eq!(del.status(), StatusCode::NO_CONTENT);
}

/// Smoke: secret set → stack apply → desired set carries injection material for the sandbox.
///
/// Mirrors lab path before msb `create_detached` (env + value + allowHosts).
#[tokio::test]
async fn secret_reaches_desired_set_for_sandbox() {
    let cluster = TestCluster::start().await.expect("start");

    let put = cluster
        .client()
        .put(format!("{}/v1/secrets/{SECRET_NAME}", cluster.base_url))
        .bearer_auth(&cluster.api_token)
        .json(&serde_json::json!({ "value": SECRET_VALUE }))
        .send()
        .await
        .unwrap();
    assert_eq!(put.status(), StatusCode::OK);

    let apply = cluster
        .client()
        .post(format!("{}/v1/stacks:apply", cluster.base_url))
        .bearer_auth(&cluster.api_token)
        .json(&serde_json::json!({ "yaml": STACK_WITH_SECRET }))
        .send()
        .await
        .unwrap();
    assert_eq!(apply.status(), StatusCode::OK);
    let body: serde_json::Value = apply.json().await.unwrap();
    assert_eq!(body["stack"], "smoke-secrets");
    assert_eq!(body["scheduled"], 1);
    assert_eq!(body["pending"], 0);

    let key = load_key(&cluster);
    let (desired, _) = build_desired_set(cluster.store.clone(), &key, &cluster.local_node_id)
        .await
        .expect("build desired set with resolved secrets");
    assert_eq!(desired.len(), 1);

    let d = &desired[0];
    assert_eq!(d.stack, "smoke-secrets");
    assert_eq!(d.service, "keep");
    assert_eq!(d.secrets.len(), 1, "node must receive secret injections");

    let inj = &d.secrets[0];
    assert_eq!(inj.env, SECRET_ENV);
    assert_eq!(inj.value, SECRET_VALUE);
    assert_eq!(inj.allow_hosts, vec![ALLOW_HOST]);
    assert!(
        d.runtime_id.contains("smoke") || d.runtime_id.contains("keep"),
        "runtime_id={:?}",
        d.runtime_id
    );

    // REST must still never expose plaintext after injection path runs.
    let list = cluster
        .client()
        .get(format!("{}/v1/secrets", cluster.base_url))
        .bearer_auth(&cluster.api_token)
        .send()
        .await
        .unwrap();
    assert!(!list.text().await.unwrap().contains(SECRET_VALUE));

    // Report Running as the node loop would after sandbox create.
    cluster
        .store
        .update_instance_status(&d.instance_id, "Running", Some(&d.runtime_id), None)
        .await
        .unwrap();

    let (st, instances) = cluster
        .get_json("/v1/instances", Some(&cluster.api_token))
        .await
        .unwrap();
    assert_eq!(st, StatusCode::OK);
    let arr = instances.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["phase"], "Running");
    assert_eq!(arr[0]["node_id"], cluster.local_node_id);
}

/// Missing secret is fine at apply (desired state stores refs) but the desired
/// set fails closed.
#[tokio::test]
async fn desired_set_fails_when_secret_missing() {
    let cluster = TestCluster::start().await.expect("start");

    let yaml = r#"
name: sec-missing
services:
  web:
    image: alpine:3.20
    scale: 1
    cpus: 1
    mem_limit: 128m
    secrets:
      - name: MISSING_SECRET
        env: TOKEN
        allowHosts: [api.example.com]
    command: ["sleep", "infinity"]
"#;
    let res = cluster
        .client()
        .post(format!("{}/v1/stacks:apply", cluster.base_url))
        .bearer_auth(&cluster.api_token)
        .json(&serde_json::json!({ "yaml": yaml }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["scheduled"], 1);

    let key = load_key(&cluster);
    let err = build_desired_set(cluster.store.clone(), &key, &cluster.local_node_id)
        .await
        .expect_err("desired set must refuse missing secret");
    let msg = format!("{err:#}").to_lowercase();
    assert!(
        msg.contains("missing_secret") || msg.contains("not found") || msg.contains("secret"),
        "unexpected message: {msg}"
    );
}

/// Empty allowHosts must not produce inject-able material (msb requires host allowlist).
#[tokio::test]
async fn desired_set_fails_when_allow_hosts_empty() {
    let cluster = TestCluster::start().await.expect("start");

    let put = cluster
        .client()
        .put(format!("{}/v1/secrets/{SECRET_NAME}", cluster.base_url))
        .bearer_auth(&cluster.api_token)
        .json(&serde_json::json!({ "value": SECRET_VALUE }))
        .send()
        .await
        .unwrap();
    assert_eq!(put.status(), StatusCode::OK);

    let yaml = r#"
name: sec-empty-hosts
services:
  web:
    image: alpine:3.20
    scale: 1
    cpus: 1
    mem_limit: 128m
    secrets:
      - name: SMOKE_TOKEN
        env: API_TOKEN
        allowHosts: []
    command: ["sleep", "infinity"]
"#;
    let res = cluster
        .client()
        .post(format!("{}/v1/stacks:apply", cluster.base_url))
        .bearer_auth(&cluster.api_token)
        .json(&serde_json::json!({ "yaml": yaml }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let key = load_key(&cluster);
    let err = build_desired_set(cluster.store.clone(), &key, &cluster.local_node_id)
        .await
        .expect_err("desired set must refuse empty allowHosts");
    let msg = format!("{err:#}").to_lowercase();
    assert!(msg.contains("allow"), "message={msg}");
}
