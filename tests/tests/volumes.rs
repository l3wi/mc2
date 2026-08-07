//! Integration: stack volumes → validation, scheduling, agent Sync mount material.
//!
//! CI does not boot microVMs. These tests exercise the same control-plane path a
//! real agent uses before `Sandbox::create_detached` (apply → Sync → mount plan),
//! mirroring `secrets.rs`.

use mc2_api::agent::agent_service_client::AgentServiceClient;
use mc2_api::agent::{Capacity, JoinRequest, SyncRequest};
use mc2_runtime::{desired_from_sync, volume_mount_plan};
use mc2_tests::TestCluster;
use reqwest::StatusCode;
use std::collections::HashMap;

const STACK_WITH_VOLUME: &str = r#"
apiVersion: mc2/v1
kind: Stack
metadata:
  name: smoke-volumes
  labels:
    purpose: volumes-smoke
volumes:
  data:
    kind: dir
services:
  keep:
    image: alpine:3.20
    replicas: 1
    resources:
      cpus: 1
      memoryMiB: 128
    network:
      profiles: [public]
    volumes:
      - name: data
        mount: /data
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
            node_name: "volumes-worker".into(),
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

/// Apply → instance scheduled; the persisted spec keeps user-facing volume names.
#[tokio::test]
async fn apply_persists_volume_mounts_and_schedules_sticky() {
    let cluster = TestCluster::start().await.expect("start");
    let (_, node_id, _) = join_worker(&cluster).await;

    let apply = cluster
        .client()
        .post(format!("{}/v1/stacks:apply", cluster.base_url))
        .bearer_auth(&cluster.api_token)
        .json(&serde_json::json!({ "yaml": STACK_WITH_VOLUME }))
        .send()
        .await
        .unwrap();
    assert_eq!(apply.status(), StatusCode::OK);
    let body: serde_json::Value = apply.json().await.unwrap();
    assert_eq!(body["stack"], "smoke-volumes");
    assert_eq!(body["scheduled"], 1);
    assert_eq!(body["pending"], 0);

    let (st, instances) = cluster
        .get_json("/v1/instances", Some(&cluster.api_token))
        .await
        .unwrap();
    assert_eq!(st, StatusCode::OK);
    let arr = instances.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["node_id"], node_id);

    // Persisted spec is the user-facing contract: YAML names, not resolved
    // msb names. Resolution happens agent-side at create time.
    let spec_json = arr[0]["spec_json"].as_str().unwrap();
    assert!(spec_json.contains("\"name\":\"data\""), "{spec_json}");
    assert!(spec_json.contains("\"mount\":\"/data\""), "{spec_json}");
    assert!(!spec_json.contains("smoke-volumes--"), "{spec_json}");
}

/// Agent Sync delivers the exact material `create_detached` mounts from.
#[tokio::test]
async fn sync_delivers_volume_material_to_agent() {
    let cluster = TestCluster::start().await.expect("start");
    let (mut agent, node_id, node_token) = join_worker(&cluster).await;

    let apply = cluster
        .client()
        .post(format!("{}/v1/stacks:apply", cluster.base_url))
        .bearer_auth(&cluster.api_token)
        .json(&serde_json::json!({ "yaml": STACK_WITH_VOLUME }))
        .send()
        .await
        .unwrap();
    assert_eq!(apply.status(), StatusCode::OK);

    let sync = agent
        .sync(SyncRequest {
            node_id,
            node_token,
        })
        .await
        .expect("sync")
        .into_inner();
    assert_eq!(sync.instances.len(), 1);

    // Same mapping the real agent uses before SDK create: guest path →
    // resolved msb named-volume identity.
    let work = desired_from_sync(&sync.instances).expect("desired_from_sync");
    assert_eq!(work.len(), 1);
    let plan = volume_mount_plan(&work[0]);
    assert_eq!(
        plan,
        vec![("/data".to_string(), "mc2-smoke-volumes--data".to_string())]
    );
}

/// Undeclared volume mount is a client error (400), naming the volume.
#[tokio::test]
async fn apply_rejects_undeclared_volume() {
    let cluster = TestCluster::start().await.expect("start");

    let yaml = r#"
apiVersion: mc2/v1
kind: Stack
metadata:
  name: vol-missing
volumes:
  data:
    kind: dir
services:
  web:
    image: alpine:3.20
    volumes:
      - name: other
        mount: /data
"#;
    let res = cluster
        .client()
        .post(format!("{}/v1/stacks:apply", cluster.base_url))
        .bearer_auth(&cluster.api_token)
        .json(&serde_json::json!({ "yaml": yaml }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let v: serde_json::Value = res.json().await.unwrap();
    let err = v["error"].as_str().unwrap_or("");
    assert!(err.contains("other"), "{err}");
    assert!(err.contains("not defined"), "{err}");
}
