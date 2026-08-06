//! Integration: apply stack → schedule onto Ready node → agent mock Running.

use mcc_api::agent::agent_service_client::AgentServiceClient;
use mcc_api::agent::{
    Capacity, HeartbeatRequest, InstanceStatus, JoinRequest, ReportStatusRequest, SyncRequest,
};
use mcc_tests::TestCluster;
use reqwest::StatusCode;
use std::collections::HashMap;

const DEMO: &str = r#"
apiVersion: mcc/v1
kind: Stack
metadata:
  name: demo
services:
  web:
    image: python:3.12
    replicas: 2
    resources:
      cpus: 1
      memoryMiB: 512
"#;

#[tokio::test]
async fn apply_schedules_and_agent_marks_running() {
    let cluster = TestCluster::start().await.expect("start");

    // Join agent first
    let mut agent = AgentServiceClient::connect(cluster.grpc_url.clone())
        .await
        .expect("grpc");
    let join = agent
        .join(JoinRequest {
            join_token: cluster.join_token.clone(),
            node_name: "worker".into(),
            labels: HashMap::new(),
            arch: "aarch64".into(),
            capacity: Some(Capacity {
                cpus: 8,
                memory_mib: 16384,
            }),
        })
        .await
        .unwrap()
        .into_inner();

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

    // Simulate agent mock runtime: Sync + ReportStatus Running
    let sync = agent
        .sync(SyncRequest {
            node_id: join.node_id.clone(),
            node_token: join.node_token.clone(),
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(sync.instances.len(), 2);

    let reports: Vec<InstanceStatus> = sync
        .instances
        .iter()
        .map(|d| {
            let rid = mcc_runtime::sandbox_name(&d.stack, &d.service, d.ordinal);
            InstanceStatus {
                instance_id: d.instance_id.clone(),
                phase: "Running".into(),
                message: "mock".into(),
                runtime_id: format!("mock://{rid}"),
            }
        })
        .collect();
    agent
        .report_status(ReportStatusRequest {
            node_id: join.node_id.clone(),
            node_token: join.node_token.clone(),
            instances: reports,
        })
        .await
        .unwrap();

    let (st, instances) = cluster
        .get_json("/v1/instances", Some(&cluster.api_token))
        .await
        .unwrap();
    assert_eq!(st, StatusCode::OK);
    let arr = instances.as_array().unwrap();
    assert_eq!(arr.len(), 2);
    assert!(arr.iter().all(|i| i["phase"] == "Running"));
    assert!(arr.iter().all(|i| i["node_id"] == join.node_id));

    // heartbeat still works
    agent
        .heartbeat(HeartbeatRequest {
            node_id: join.node_id,
            node_token: join.node_token,
            capacity: Some(Capacity {
                cpus: 8,
                memory_mib: 16384,
            }),
            status: "Ready".into(),
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn apply_without_nodes_leaves_pending() {
    let cluster = TestCluster::start().await.expect("start");
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
