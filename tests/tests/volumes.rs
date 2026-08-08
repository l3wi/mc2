//! Integration: stack volumes → validation, scheduling, desired-set mount material.
//!
//! CI does not boot microVMs. These tests exercise the same control-plane path the
//! node loop uses before `Sandbox::create_detached` (apply → desired set → mount plan).

use mc2_runtime::volume_mount_plan;
use mc2_server::build_desired_set;
use mc2_store::SecretsKey;
use mc2_tests::TestCluster;
use reqwest::StatusCode;

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

/// Apply → instance scheduled; the persisted spec keeps user-facing volume names.
#[tokio::test]
async fn apply_persists_volume_mounts_and_schedules_sticky() {
    let cluster = TestCluster::start().await.expect("start");

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
    assert_eq!(arr[0]["node_id"], cluster.local_node_id);

    // Persisted spec is the user-facing contract: YAML names, not resolved
    // msb names. Resolution happens node-side at create time.
    let spec_json = arr[0]["spec_json"].as_str().unwrap();
    assert!(spec_json.contains("\"name\":\"data\""), "{spec_json}");
    assert!(spec_json.contains("\"mount\":\"/data\""), "{spec_json}");
    assert!(!spec_json.contains("smoke-volumes--"), "{spec_json}");
}

/// The desired set delivers the exact material `create_detached` mounts from.
#[tokio::test]
async fn desired_set_delivers_volume_material() {
    let cluster = TestCluster::start().await.expect("start");

    let apply = cluster
        .client()
        .post(format!("{}/v1/stacks:apply", cluster.base_url))
        .bearer_auth(&cluster.api_token)
        .json(&serde_json::json!({ "yaml": STACK_WITH_VOLUME }))
        .send()
        .await
        .unwrap();
    assert_eq!(apply.status(), StatusCode::OK);

    let key = SecretsKey::load_file(&cluster.data_dir.join("secrets.key")).unwrap();
    let (desired, _) = build_desired_set(cluster.store.clone(), &key, &cluster.local_node_id)
        .await
        .expect("build desired set");
    assert_eq!(desired.len(), 1);

    // Same mapping the node loop uses before SDK create: guest path →
    // resolved msb named-volume identity.
    let plan = volume_mount_plan(&desired[0]);
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
