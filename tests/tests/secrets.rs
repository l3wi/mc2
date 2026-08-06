//! Integration: cluster secrets + agent injection material for sandboxes.
//!
//! CI does not boot microVMs. These tests exercise the same control-plane path a
//! real agent uses before `Sandbox::create_detached` (set → apply → Sync secrets).

use mcc_api::agent::agent_service_client::AgentServiceClient;
use mcc_api::agent::{Capacity, InstanceStatus, JoinRequest, ReportStatusRequest, SyncRequest};
use mcc_runtime::desired_from_sync;
use mcc_tests::TestCluster;
use reqwest::StatusCode;
use std::collections::HashMap;

const SECRET_NAME: &str = "SMOKE_TOKEN";
const SECRET_VALUE: &str = "smoke-secret-value-never-echo";
const SECRET_ENV: &str = "API_TOKEN";
const ALLOW_HOST: &str = "api.example.com";

const STACK_WITH_SECRET: &str = r#"
apiVersion: mcc/v1
kind: Stack
metadata:
  name: smoke-secrets
  labels:
    purpose: secrets-smoke
services:
  keep:
    image: alpine:3.20
    replicas: 1
    resources:
      cpus: 1
      memoryMiB: 128
    network:
      profiles: [public]
    secrets:
      - name: SMOKE_TOKEN
        env: API_TOKEN
        allowHosts:
          - api.example.com
    restartPolicy: on-failure
    command: ["sleep", "infinity"]
"#;

async fn join_worker(
    cluster: &TestCluster,
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
            node_name: "smoke-worker".into(),
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

/// Smoke: secret set → stack apply → agent Sync delivers injection material for the sandbox.
///
/// Mirrors lab path before msb `create_detached` (env + value + allowHosts).
#[tokio::test]
async fn secret_reaches_agent_sync_for_sandbox() {
    let cluster = TestCluster::start().await.expect("start");
    let (mut agent, node_id, node_token) = join_worker(&cluster).await;

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

    let sync = agent
        .sync(SyncRequest {
            node_id: node_id.clone(),
            node_token: node_token.clone(),
        })
        .await
        .expect("sync with resolved secrets")
        .into_inner();
    assert_eq!(sync.instances.len(), 1);

    let desired = &sync.instances[0];
    assert_eq!(desired.stack, "smoke-secrets");
    assert_eq!(desired.service, "keep");
    assert_eq!(
        desired.secrets.len(),
        1,
        "agent must receive secret injections"
    );

    let inj = &desired.secrets[0];
    assert_eq!(inj.env, SECRET_ENV);
    assert_eq!(inj.value, SECRET_VALUE);
    assert_eq!(inj.allow_hosts, vec![ALLOW_HOST]);

    // Same mapping the real agent uses before SDK create.
    let work = desired_from_sync(&sync.instances).expect("desired_from_sync");
    assert_eq!(work.len(), 1);
    assert_eq!(work[0].secrets.len(), 1);
    assert_eq!(work[0].secrets[0].env, SECRET_ENV);
    assert_eq!(work[0].secrets[0].value, SECRET_VALUE);
    assert_eq!(work[0].secrets[0].allow_hosts, vec![ALLOW_HOST]);
    assert!(
        work[0].runtime_id.contains("smoke") || work[0].runtime_id.contains("keep"),
        "runtime_id={:?}",
        work[0].runtime_id
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

    // Report Running as a real agent would after sandbox create.
    agent
        .report_status(ReportStatusRequest {
            node_id: node_id.clone(),
            node_token: node_token.clone(),
            instances: vec![InstanceStatus {
                instance_id: desired.instance_id.clone(),
                phase: "Running".into(),
                message: "secrets smoke (injection material verified; no hypervisor)".into(),
                runtime_id: work[0].runtime_id.clone(),
            }],
        })
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
    assert_eq!(arr[0]["node_id"], node_id);
}

/// Missing secret is fine at apply (desired state stores refs) but Sync fails closed.
#[tokio::test]
async fn sync_fails_when_secret_missing() {
    let cluster = TestCluster::start().await.expect("start");
    let (mut agent, node_id, node_token) = join_worker(&cluster).await;

    let yaml = r#"
apiVersion: mcc/v1
kind: Stack
metadata:
  name: sec-missing
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

    let err = agent
        .sync(SyncRequest {
            node_id,
            node_token,
        })
        .await
        .expect_err("sync must refuse missing secret");
    let status = err.code();
    assert_eq!(
        status,
        tonic::Code::FailedPrecondition,
        "got {status:?}: {err}"
    );
    let msg = err.message().to_lowercase();
    assert!(
        msg.contains("missing_secret") || msg.contains("not found") || msg.contains("secret"),
        "unexpected message: {}",
        err.message()
    );
}

/// Empty allowHosts must not produce inject-able material (msb requires host allowlist).
#[tokio::test]
async fn sync_fails_when_allow_hosts_empty() {
    let cluster = TestCluster::start().await.expect("start");
    let (mut agent, node_id, node_token) = join_worker(&cluster).await;

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
apiVersion: mcc/v1
kind: Stack
metadata:
  name: sec-empty-hosts
services:
  web:
    image: alpine:3.20
    replicas: 1
    resources:
      cpus: 1
      memoryMiB: 128
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

    let err = agent
        .sync(SyncRequest {
            node_id,
            node_token,
        })
        .await
        .expect_err("sync must refuse empty allowHosts");
    assert_eq!(err.code(), tonic::Code::FailedPrecondition);
    assert!(
        err.message().to_lowercase().contains("allow"),
        "message={}",
        err.message()
    );
}
