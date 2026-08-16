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
name: smoke-volumes
volumes:
  data:
    kind: dir
services:
  keep:
    image: alpine:3.20
    scale: 1
    cpus: 1
    mem_limit: 128m
    network:
      profiles: [public]
    volumes:
      - name: data
        target: /data
    restart: on-failure
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
    assert!(spec_json.contains("\"target\":\"/data\""), "{spec_json}");
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
name: vol-missing
volumes:
  data:
    kind: dir
services:
  web:
    image: alpine:3.20
    volumes:
      - name: other
        target: /data
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

/// `GET /v1/volumes` lists retained MC2 volumes from the volume root and
/// ignores non-MC2 entries.
#[tokio::test]
async fn list_volumes_reports_retained_volumes() {
    let dir = tempfile::tempdir().unwrap();
    let volumes_root = dir.path().join("volumes");
    std::fs::create_dir_all(volumes_root.join("mc2-shop--data")).unwrap();
    std::fs::create_dir_all(volumes_root.join("mc2-shop--cache")).unwrap();
    std::fs::write(volumes_root.join("mc2-shop--data").join("payload"), "x").unwrap();
    // Not an MC2 volume identity — must be ignored.
    std::fs::create_dir_all(volumes_root.join("scratch")).unwrap();

    let cluster = TestCluster::start_with_volume_dir(true, volumes_root.clone())
        .await
        .expect("start");

    let (status, body) = cluster
        .get_json("/v1/volumes", Some(&cluster.api_token))
        .await
        .unwrap();
    assert_eq!(status, StatusCode::OK);
    let arr = body.as_array().unwrap();
    assert_eq!(arr.len(), 2, "{body}");

    // Deterministic order: stack, then volume name (alphabetically cache < data).
    let first = &arr[0];
    assert_eq!(first["stack"], "shop");
    assert_eq!(first["volume"], "cache");
    assert_eq!(first["name"], "mc2-shop--cache");
    assert!(first["path"].as_str().unwrap().contains("mc2-shop--cache"));

    let second = &arr[1];
    assert_eq!(second["stack"], "shop");
    assert_eq!(second["volume"], "data");
    assert!(second["size_mib"].is_u64());

    for v in arr.iter() {
        assert_ne!(v["name"].as_str().unwrap(), "scratch");
    }
}
