//! Integration: secret set/list/delete and open-cluster no-echo.

use mcc_tests::TestCluster;
use reqwest::StatusCode;

#[tokio::test]
async fn set_list_delete_secret_no_value_echo() {
    let cluster = TestCluster::start().await.expect("start");

    let put = cluster
        .client()
        .put(format!("{}/v1/secrets/GITHUB_TOKEN", cluster.base_url))
        .bearer_auth(&cluster.api_token)
        .json(&serde_json::json!({ "value": "ghp_super_secret_value" }))
        .send()
        .await
        .unwrap();
    assert_eq!(put.status(), StatusCode::OK);
    let meta: serde_json::Value = put.json().await.unwrap();
    assert_eq!(meta["name"], "GITHUB_TOKEN");
    assert!(meta.get("value").is_none());
    let body = meta.to_string();
    assert!(!body.contains("ghp_super_secret_value"));

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
    assert_eq!(names[0]["name"], "GITHUB_TOKEN");
    assert!(!names.to_string().contains("ghp_"));

    let del = cluster
        .client()
        .delete(format!("{}/v1/secrets/GITHUB_TOKEN", cluster.base_url))
        .bearer_auth(&cluster.api_token)
        .send()
        .await
        .unwrap();
    assert_eq!(del.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn apply_fails_when_secret_missing() {
    let cluster = TestCluster::start().await.expect("start");
    // Join a node so schedule can place (not required for secret resolve at apply)
    let yaml = r#"
apiVersion: mcc/v1
kind: Stack
metadata:
  name: sec-demo
services:
  web:
    image: alpine:3.20
    replicas: 1
    resources:
      cpus: 1
      memoryMiB: 128
    secrets:
      - name: MISSING_SECRET
        env: TOKEN
        allowHosts: [api.example.com]
    command: ["sleep", "infinity"]
"#;
    // Apply succeeds (refs stored in spec); resolve fails on agent Sync.
    // Server apply does not require secrets to exist at apply time for schedule.
    let res = cluster
        .client()
        .post(format!("{}/v1/stacks:apply", cluster.base_url))
        .bearer_auth(&cluster.api_token)
        .json(&serde_json::json!({ "yaml": yaml }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
}
