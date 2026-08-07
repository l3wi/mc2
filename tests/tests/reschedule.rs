//! Integration: node NotReady → non-sticky instance rescheduled to another Ready node.

use mc2_api::agent::agent_service_client::AgentServiceClient;
use mc2_api::agent::{
    Capacity, HeartbeatRequest, InstanceStatus, JoinRequest, ReportStatusRequest, SyncRequest,
};
use mc2_server::reschedule_not_ready;
use mc2_store::Store;
use mc2_tests::TestCluster;
use reqwest::StatusCode;
use std::collections::HashMap;
use std::sync::Arc;

struct Joined {
    client: AgentServiceClient<tonic::transport::Channel>,
    node_id: String,
    node_token: String,
    name: String,
}

async fn join(cluster: &TestCluster, name: &str) -> Joined {
    let mut client = AgentServiceClient::connect(cluster.grpc_url.clone())
        .await
        .expect("grpc");
    let join = client
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
        .unwrap()
        .into_inner();
    Joined {
        client,
        node_id: join.node_id,
        node_token: join.node_token,
        name: name.into(),
    }
}

#[tokio::test]
async fn not_ready_node_reschedules_to_peer() {
    let cluster = TestCluster::start().await.expect("start");
    let mut a = join(&cluster, "worker-a").await;
    let mut b = join(&cluster, "worker-b").await;

    let yaml = r#"
apiVersion: mc2/v1
kind: Stack
metadata:
  name: resched
services:
  web:
    image: alpine:3.20
    replicas: 1
    resources:
      cpus: 1
      memoryMiB: 128
    restartPolicy: on-failure
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

    let (st, instances) = cluster
        .get_json("/v1/instances", Some(&cluster.api_token))
        .await
        .unwrap();
    assert_eq!(st, StatusCode::OK);
    let arr = instances.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    let inst_id = arr[0]["id"].as_str().unwrap().to_string();
    let bound = arr[0]["node_id"].as_str().unwrap().to_string();

    let (dead, live) = if bound == a.node_id {
        (&mut a, &mut b)
    } else {
        (&mut b, &mut a)
    };

    // Mark bound node NotReady via gRPC heartbeat status.
    dead.client
        .heartbeat(HeartbeatRequest {
            node_id: dead.node_id.clone(),
            node_token: dead.node_token.clone(),
            capacity: Some(Capacity {
                cpus: 4,
                memory_mib: 8192,
            }),
            status: "NotReady".into(),
        })
        .await
        .unwrap();
    // Keep live Ready
    live.client
        .heartbeat(HeartbeatRequest {
            node_id: live.node_id.clone(),
            node_token: live.node_token.clone(),
            capacity: Some(Capacity {
                cpus: 4,
                memory_mib: 8192,
            }),
            status: "Ready".into(),
        })
        .await
        .unwrap();

    let (unbound, scheduled) = reschedule_not_ready(cluster.store.clone() as Arc<dyn Store>)
        .await
        .expect("reschedule");
    assert_eq!(unbound, 1, "should unbind from NotReady node");
    assert_eq!(scheduled, 1, "should bind to remaining Ready node");

    let after = cluster.store.get_instance(&inst_id).await.unwrap().unwrap();
    assert_eq!(after.phase, "Scheduled");
    assert_eq!(after.node_id.as_deref(), Some(live.node_id.as_str()));

    let sync = live
        .client
        .sync(SyncRequest {
            node_id: live.node_id.clone(),
            node_token: live.node_token.clone(),
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(sync.instances.len(), 1);
    assert_eq!(sync.instances[0].instance_id, inst_id);

    live.client
        .report_status(ReportStatusRequest {
            node_id: live.node_id.clone(),
            node_token: live.node_token.clone(),
            instances: vec![InstanceStatus {
                instance_id: inst_id,
                phase: "Running".into(),
                message: "reschedule harness".into(),
                runtime_id: "resched-web-0".into(),
                ssh: None,
                fabric: None,
            }],
        })
        .await
        .unwrap();

    let _ = (&dead.name, &live.name);
}

#[tokio::test]
async fn sticky_volume_stays_on_not_ready_node() {
    let cluster = TestCluster::start().await.expect("start");
    let mut a = join(&cluster, "sticky-a").await;
    let mut b = join(&cluster, "sticky-b").await;

    let yaml = r#"
apiVersion: mc2/v1
kind: Stack
metadata:
  name: sticky
volumes:
  data:
    kind: dir
services:
  db:
    image: alpine:3.20
    replicas: 1
    resources:
      cpus: 1
      memoryMiB: 128
    restartPolicy: on-failure
    volumes:
      - name: data
        mount: /data
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

    let (_, instances) = cluster
        .get_json("/v1/instances", Some(&cluster.api_token))
        .await
        .unwrap();
    let inst_id = instances[0]["id"].as_str().unwrap().to_string();
    let bound = instances[0]["node_id"].as_str().unwrap().to_string();

    let dead = if bound == a.node_id { &mut a } else { &mut b };
    dead.client
        .heartbeat(HeartbeatRequest {
            node_id: dead.node_id.clone(),
            node_token: dead.node_token.clone(),
            capacity: Some(Capacity {
                cpus: 4,
                memory_mib: 8192,
            }),
            status: "NotReady".into(),
        })
        .await
        .unwrap();

    let (unbound, _) = reschedule_not_ready(cluster.store.clone() as Arc<dyn Store>)
        .await
        .unwrap();
    assert_eq!(unbound, 0);

    let after = cluster.store.get_instance(&inst_id).await.unwrap().unwrap();
    assert_eq!(after.node_id.as_deref(), Some(bound.as_str()));
}
