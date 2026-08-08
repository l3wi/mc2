//! Integration: SSH key registry + instance open/close via REST API.
//!
//! Observed phase/bind/port come from the node loop's store writes; this
//! suite simulates those writes directly (no hypervisor).

use mc2_store::Store;
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

    // Apply so we have an instance (auto local node picks it up).
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

/// Node-loop store writes (observed Open/Closed) surface through the REST view.
#[tokio::test]
async fn node_observed_ssh_roundtrip_through_rest() {
    let cluster = TestCluster::start().await.expect("start");

    let put_key = cluster
        .client()
        .put(format!("{}/v1/ssh/keys/dev", cluster.base_url))
        .bearer_auth(&cluster.api_token)
        .json(&serde_json::json!({ "publicKey": FAKE_KEY }))
        .send()
        .await
        .unwrap();
    assert_eq!(put_key.status(), StatusCode::OK);

    let yaml = r#"
apiVersion: mc2/v1
kind: Stack
metadata:
  name: sshobs
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

    // Enable override, then simulate the node loop opening the serve port.
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

    cluster
        .store
        .update_instance_ssh_observed(id, "Open", Some("127.0.0.1"), Some(2222), Some("sdk"))
        .await
        .unwrap();

    let (st, view) = cluster
        .get_json(&format!("/v1/instances/{id}/ssh"), Some(&cluster.api_token))
        .await
        .unwrap();
    assert_eq!(st, StatusCode::OK);
    assert_eq!(view["phase"], "Open");
    assert_eq!(view["bind"], "127.0.0.1");
    assert_eq!(view["port"], 2222);
    assert_eq!(view["message"], "sdk");

    // Instance stops → node loop closes the serve; observed flips to Closed.
    cluster
        .store
        .update_instance_ssh_observed(id, "Closed", None, None, None)
        .await
        .unwrap();

    let (st, view) = cluster
        .get_json(&format!("/v1/instances/{id}/ssh"), Some(&cluster.api_token))
        .await
        .unwrap();
    assert_eq!(st, StatusCode::OK);
    assert_eq!(view["phase"], "Closed");
    assert!(view["bind"].is_null());
    assert!(view["port"].is_null());
}
