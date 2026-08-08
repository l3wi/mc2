//! Integration: node NotReady → non-sticky instance unbound; rebinds when the
//! node returns to Ready. Sticky volumes stay bound.
//!
//! Single local node (v1): the flow is store-driven, no transport.

use mc2_server::reschedule_not_ready;
use mc2_store::{NodeHeartbeat, Store};
use mc2_tests::TestCluster;
use reqwest::StatusCode;
use std::sync::Arc;

const WEB: &str = r#"
name: resched
services:
  web:
    image: alpine:3.20
    scale: 1
    cpus: 1
    mem_limit: 128m
    restart: on-failure
    command: ["sleep", "infinity"]
"#;

#[tokio::test]
async fn not_ready_node_unbinds_and_rebinds_on_recovery() {
    let cluster = TestCluster::start().await.expect("start");
    let node_id = cluster.local_node_id.clone();

    let res = cluster
        .client()
        .post(format!("{}/v1/stacks:apply", cluster.base_url))
        .bearer_auth(&cluster.api_token)
        .json(&serde_json::json!({ "yaml": WEB }))
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
    assert_eq!(arr[0]["node_id"], node_id);

    // Node goes NotReady.
    cluster
        .store
        .touch_node(
            &node_id,
            NodeHeartbeat {
                cpus: 8,
                memory_mib: 16384,
                status: "NotReady".into(),
            },
        )
        .await
        .unwrap();

    let (unbound, scheduled) = reschedule_not_ready(cluster.store.clone() as Arc<dyn Store>)
        .await
        .expect("reschedule");
    assert_eq!(unbound, 1, "should unbind from NotReady node");
    assert_eq!(scheduled, 0, "no Ready node to schedule onto yet");

    let mid = cluster.store.get_instance(&inst_id).await.unwrap().unwrap();
    assert_eq!(mid.phase, "Pending");
    assert!(mid.node_id.is_none());

    // Node recovers → scheduler rebinds.
    cluster
        .store
        .touch_node(
            &node_id,
            NodeHeartbeat {
                cpus: 8,
                memory_mib: 16384,
                status: "Ready".into(),
            },
        )
        .await
        .unwrap();

    let (_, rescheduled) = reschedule_not_ready(cluster.store.clone() as Arc<dyn Store>)
        .await
        .expect("reschedule after recovery");
    assert_eq!(rescheduled, 1, "should rebind to the recovered node");

    let after = cluster.store.get_instance(&inst_id).await.unwrap().unwrap();
    assert_eq!(after.phase, "Scheduled");
    assert_eq!(after.node_id.as_deref(), Some(node_id.as_str()));
}

#[tokio::test]
async fn sticky_volume_stays_on_not_ready_node() {
    let cluster = TestCluster::start().await.expect("start");
    let node_id = cluster.local_node_id.clone();

    let yaml = r#"
name: sticky-resched
volumes:
  data:
    kind: dir
services:
  db:
    image: alpine:3.20
    scale: 1
    cpus: 1
    mem_limit: 128m
    volumes:
      - name: data
        target: /data
    restart: on-failure
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
    assert_eq!(bound, node_id);

    cluster
        .store
        .touch_node(
            &bound,
            NodeHeartbeat {
                cpus: 8,
                memory_mib: 16384,
                status: "NotReady".into(),
            },
        )
        .await
        .unwrap();

    let (unbound, _) = reschedule_not_ready(cluster.store.clone() as Arc<dyn Store>)
        .await
        .unwrap();
    assert_eq!(unbound, 0);

    let after = cluster.store.get_instance(&inst_id).await.unwrap().unwrap();
    assert_eq!(after.node_id.as_deref(), Some(bound.as_str()));
}
