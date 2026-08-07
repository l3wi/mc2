//! Integration: agent gRPC Join/Heartbeat + REST `/v1/nodes`.

use mc2_api::agent::agent_service_client::AgentServiceClient;
use mc2_api::agent::{Capacity, HeartbeatRequest, JoinRequest};
use mc2_tests::TestCluster;
use reqwest::StatusCode;
use std::collections::HashMap;

#[tokio::test]
async fn join_then_list_nodes_ready() {
    let cluster = TestCluster::start().await.expect("start cluster");

    let mut client = AgentServiceClient::connect(cluster.grpc_url.clone())
        .await
        .expect("grpc connect");

    let join = client
        .join(JoinRequest {
            join_token: cluster.join_token.clone(),
            node_name: "lab-worker".into(),
            labels: HashMap::from([("role".into(), "worker".into())]),
            arch: "aarch64".into(),
            capacity: Some(Capacity {
                cpus: 4,
                memory_mib: 8192,
            }),
        })
        .await
        .expect("join")
        .into_inner();

    assert!(!join.node_id.is_empty());
    assert!(join.node_token.starts_with("mc2nt_"));

    let hb = client
        .heartbeat(HeartbeatRequest {
            node_id: join.node_id.clone(),
            node_token: join.node_token.clone(),
            capacity: Some(Capacity {
                cpus: 4,
                memory_mib: 8192,
            }),
            status: "Ready".into(),
        })
        .await
        .expect("heartbeat")
        .into_inner();
    assert!(hb.ok);

    let (status, body) = cluster
        .get_json("/v1/nodes", Some(&cluster.api_token))
        .await
        .unwrap();
    assert_eq!(status, StatusCode::OK);
    let nodes = body.as_array().expect("array");
    assert_eq!(nodes.len(), 1);
    assert_eq!(nodes[0]["name"], "lab-worker");
    assert_eq!(nodes[0]["status"], "Ready");
    assert_eq!(nodes[0]["cpus"], 4);

    let (st, st_body) = cluster
        .get_json("/v1/status", Some(&cluster.api_token))
        .await
        .unwrap();
    assert_eq!(st, StatusCode::OK);
    assert_eq!(st_body["nodes_total"], 1);
    assert_eq!(st_body["nodes_ready"], 1);
}

#[tokio::test]
async fn join_rejects_bad_token() {
    let cluster = TestCluster::start().await.expect("start");
    let mut client = AgentServiceClient::connect(cluster.grpc_url.clone())
        .await
        .expect("connect");

    let err = client
        .join(JoinRequest {
            join_token: "wrong".into(),
            node_name: "x".into(),
            labels: HashMap::new(),
            arch: "x86_64".into(),
            capacity: None,
        })
        .await
        .expect_err("should fail");
    assert_eq!(err.code(), tonic::Code::Unauthenticated);
}

#[tokio::test]
async fn rejoin_same_name_rotates_token() {
    let cluster = TestCluster::start().await.expect("start");
    let mut client = AgentServiceClient::connect(cluster.grpc_url.clone())
        .await
        .expect("connect");

    let first = client
        .join(JoinRequest {
            join_token: cluster.join_token.clone(),
            node_name: "same".into(),
            labels: HashMap::new(),
            arch: "aarch64".into(),
            capacity: Some(Capacity {
                cpus: 1,
                memory_mib: 1024,
            }),
        })
        .await
        .unwrap()
        .into_inner();

    let second = client
        .join(JoinRequest {
            join_token: cluster.join_token.clone(),
            node_name: "same".into(),
            labels: HashMap::new(),
            arch: "aarch64".into(),
            capacity: Some(Capacity {
                cpus: 2,
                memory_mib: 2048,
            }),
        })
        .await
        .unwrap()
        .into_inner();

    assert_eq!(first.node_id, second.node_id);
    assert_ne!(first.node_token, second.node_token);

    // Old token no longer works
    let err = client
        .heartbeat(HeartbeatRequest {
            node_id: first.node_id.clone(),
            node_token: first.node_token,
            capacity: None,
            status: "Ready".into(),
        })
        .await
        .expect_err("old token");
    assert_eq!(err.code(), tonic::Code::Unauthenticated);

    let ok = client
        .heartbeat(HeartbeatRequest {
            node_id: second.node_id,
            node_token: second.node_token,
            capacity: Some(Capacity {
                cpus: 2,
                memory_mib: 2048,
            }),
            status: "Ready".into(),
        })
        .await
        .unwrap()
        .into_inner();
    assert!(ok.ok);
}
