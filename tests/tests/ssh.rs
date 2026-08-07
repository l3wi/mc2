//! Integration: SSH key registry + instance open/close via REST API.

use mc2_tests::TestCluster;
use reqwest::StatusCode;

const FAKE_KEY: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIJustAFakeKeyMaterialHere0000 test@mc2";

#[tokio::test]
async fn ssh_key_put_list_delete() {
    let cluster = TestCluster::start().await.expect("start");

    let put = cluster
        .client()
        .put(format!("{}/v1/ssh/keys/laptop", cluster.base_url))
        .bearer_auth(&cluster.api_token)
        .json(&serde_json::json!({ "publicKey": FAKE_KEY }))
        .send()
        .await
        .unwrap();
    assert_eq!(put.status(), StatusCode::OK);
    let meta: serde_json::Value = put.json().await.unwrap();
    assert_eq!(meta["name"], "laptop");
    assert!(meta["fingerprint"].as_str().unwrap().starts_with("SHA256:"));
    assert_eq!(meta["publicKey"], FAKE_KEY);

    let list = cluster
        .client()
        .get(format!("{}/v1/ssh/keys", cluster.base_url))
        .bearer_auth(&cluster.api_token)
        .send()
        .await
        .unwrap();
    assert_eq!(list.status(), StatusCode::OK);
    let keys: serde_json::Value = list.json().await.unwrap();
    assert_eq!(keys.as_array().unwrap().len(), 1);

    let del = cluster
        .client()
        .delete(format!("{}/v1/ssh/keys/laptop", cluster.base_url))
        .bearer_auth(&cluster.api_token)
        .send()
        .await
        .unwrap();
    assert_eq!(del.status(), StatusCode::NO_CONTENT);

    let gone = cluster
        .client()
        .get(format!("{}/v1/ssh/keys/laptop", cluster.base_url))
        .bearer_auth(&cluster.api_token)
        .send()
        .await
        .unwrap();
    assert_eq!(gone.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn instance_ssh_put_requires_keys_and_can_close() {
    use mc2_api::agent::agent_service_client::AgentServiceClient;
    use mc2_api::agent::{Capacity, JoinRequest};
    use std::collections::HashMap;

    let cluster = TestCluster::start().await.expect("start");

    // Register key
    let put_key = cluster
        .client()
        .put(format!("{}/v1/ssh/keys/dev", cluster.base_url))
        .bearer_auth(&cluster.api_token)
        .json(&serde_json::json!({ "publicKey": FAKE_KEY }))
        .send()
        .await
        .unwrap();
    assert_eq!(put_key.status(), StatusCode::OK);

    // Join + apply so we have an instance
    let mut agent = AgentServiceClient::connect(cluster.grpc_url.clone())
        .await
        .unwrap();
    agent
        .join(JoinRequest {
            join_token: cluster.join_token.clone(),
            node_name: "n1".into(),
            labels: HashMap::new(),
            arch: "aarch64".into(),
            capacity: Some(Capacity {
                cpus: 4,
                memory_mib: 8192,
            }),
        })
        .await
        .unwrap();

    let yaml = r#"
apiVersion: mc2/v1
kind: Stack
metadata:
  name: sshdemo
services:
  web:
    image: alpine:3.20
    replicas: 1
    resources: { cpus: 1, memoryMiB: 128 }
    command: ["sleep", "infinity"]
"#;
    let apply = cluster
        .client()
        .post(format!("{}/v1/stacks:apply", cluster.base_url))
        .bearer_auth(&cluster.api_token)
        .json(&serde_json::json!({ "yaml": yaml }))
        .send()
        .await
        .unwrap();
    assert_eq!(apply.status(), StatusCode::OK);

    let (_, instances) = cluster
        .get_json("/v1/instances", Some(&cluster.api_token))
        .await
        .unwrap();
    let id = instances[0]["id"].as_str().unwrap();

    // Reject open without keys
    let bad = cluster
        .client()
        .put(format!("{}/v1/instances/{id}/ssh", cluster.base_url))
        .bearer_auth(&cluster.api_token)
        .json(&serde_json::json!({ "enabled": true, "authorizedKeys": [] }))
        .send()
        .await
        .unwrap();
    assert_eq!(bad.status(), StatusCode::BAD_REQUEST);

    // Open desired
    let open = cluster
        .client()
        .put(format!("{}/v1/instances/{id}/ssh", cluster.base_url))
        .bearer_auth(&cluster.api_token)
        .json(&serde_json::json!({
            "enabled": true,
            "port": 0,
            "authorizedKeys": ["dev"]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(open.status(), StatusCode::OK);
    let body: serde_json::Value = open.json().await.unwrap();
    assert_eq!(body["desired"], true);
    assert_eq!(body["hasOverride"], true);

    // Close
    let close = cluster
        .client()
        .put(format!("{}/v1/instances/{id}/ssh", cluster.base_url))
        .bearer_auth(&cluster.api_token)
        .json(&serde_json::json!({ "enabled": false, "authorizedKeys": ["dev"] }))
        .send()
        .await
        .unwrap();
    assert_eq!(close.status(), StatusCode::OK);
    let body: serde_json::Value = close.json().await.unwrap();
    assert_eq!(body["desired"], false);

    // Delete key
    let del = cluster
        .client()
        .delete(format!("{}/v1/ssh/keys/dev", cluster.base_url))
        .bearer_auth(&cluster.api_token)
        .send()
        .await
        .unwrap();
    assert_eq!(del.status(), StatusCode::NO_CONTENT);
}
