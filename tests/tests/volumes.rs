//! Integration: stack volumes → validation, scheduling, desired-set mount material.
//!
//! CI does not boot microVMs. These tests exercise the same control-plane path the
//! node loop uses before `Sandbox::create_detached` (apply → desired set → mount plan).

use mc2_runtime::volume_bind_plan;
use mc2_server::build_desired_set;
use mc2_store::{SecretsKey, Store};
use mc2_tests::TestCluster;
use reqwest::StatusCode;
use std::collections::HashMap;

const STACK_WITH_VOLUME: &str = r#"
name: smoke-volumes
volumes:
  data:
    kind: dir
    size: 2GiB
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

/// The desired set delivers the exact material `create_detached` mounts from:
/// MC2-owned host directories under the volume root, each with a quota.
#[tokio::test]
async fn desired_set_delivers_bind_mount_material_with_quotas() {
    let dir = tempfile::tempdir().unwrap();
    let volumes_root = dir.path().join("volumes");
    let cluster = TestCluster::start_with_volume_dir(true, volumes_root.clone())
        .await
        .expect("start");

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
    let desired = build_desired_set(cluster.store.clone(), &key, &cluster.local_node_id)
        .await
        .expect("build desired set")
        .sandboxes;
    assert_eq!(desired.len(), 1);

    // Apply propagated the declared volume size into the persisted spec, so the
    // node-side quota plan needs no stack lookup.
    assert_eq!(desired[0].spec.volumes[0].size_mib, 2 * 1024);

    // `mc2-{stack}--{volume}` under the volume root, mounted with an explicit
    // quota (`size − usage`, measured fresh by the runtime before create).
    let plan = volume_bind_plan(&desired[0], &volumes_root, &HashMap::new());
    assert_eq!(plan.len(), 1);
    assert_eq!(plan[0].guest, "/data");
    assert_eq!(plan[0].host, volumes_root.join("mc2-smoke-volumes--data"));
    assert_eq!(plan[0].quota_mib, 2 * 1024);
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
    assert!(second["used_mib"].is_u64());
    // No stored stack declares these volumes → no declared limit.
    assert!(second["limit_mib"].is_null(), "{body}");

    for v in arr.iter() {
        assert_ne!(v["name"].as_str().unwrap(), "scratch");
    }
}

/// `GET /v1/volumes` reports the declared `size` from the owning stack.
#[tokio::test]
async fn list_volumes_reports_declared_size_and_usage() {
    let dir = tempfile::tempdir().unwrap();
    let volumes_root = dir.path().join("volumes");
    let vol_dir = volumes_root.join("mc2-shop--data");
    std::fs::create_dir_all(&vol_dir).unwrap();
    std::fs::write(vol_dir.join("payload"), vec![0u8; 3 * 1024 * 1024]).unwrap();

    let cluster = TestCluster::start_with_volume_dir(true, volumes_root)
        .await
        .expect("start");

    let yaml = r#"
name: shop
volumes:
  data:
    kind: dir
    size: 20GiB
services:
  web:
    image: alpine
    volumes:
      - name: data
        target: /data
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

    let (status, body) = cluster
        .get_json("/v1/volumes", Some(&cluster.api_token))
        .await
        .unwrap();
    assert_eq!(status, StatusCode::OK);
    let arr = body.as_array().unwrap();
    assert_eq!(arr.len(), 1, "{body}");
    assert_eq!(arr[0]["limit_mib"], 20 * 1024);
    assert_eq!(arr[0]["used_mib"], 3, "{body}");
}

/// `--limit-disk-mib` is a reservation: a stack whose root disks exceed the
/// budget is a 400 with a breakdown, before anything is stored.
#[tokio::test]
async fn apply_over_disk_reservation_is_400_with_a_breakdown() {
    let cluster = TestCluster::start_with_limits(
        true,
        None,
        mc2_server::ResourceLimits {
            cpus: 0,
            memory_mib: 0,
            disk_mib: 1024,
        },
    )
    .await
    .expect("start");

    // The default 4 GiB root disk alone exceeds the 1 GiB budget.
    let yaml = "name: big\nservices:\n  web:\n    image: alpine\n";
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
    assert!(err.contains("disk reservation"), "{err}");
    assert!(err.contains("volumes"), "{err}");
    assert!(err.contains("root disks"), "{err}");
    assert!(err.contains("limit 1024 MiB"), "{err}");
    assert!(err.contains("--limit-disk-mib"), "{err}");
    // Nothing was stored.
    assert!(cluster.store.get_stack("big").await.unwrap().is_none());
}

/// Shrinking a volume below its directory's current usage is a 400.
#[tokio::test]
async fn apply_shrinking_a_volume_below_usage_is_400() {
    let dir = tempfile::tempdir().unwrap();
    let volumes_root = dir.path().join("volumes");
    let vol_dir = volumes_root.join("mc2-shrink--data");
    std::fs::create_dir_all(&vol_dir).unwrap();
    std::fs::write(vol_dir.join("payload"), vec![0u8; 4 * 1024 * 1024]).unwrap();

    let cluster = TestCluster::start_with_volume_dir(true, volumes_root)
        .await
        .expect("start");

    let yaml = r#"
name: shrink
volumes:
  data:
    kind: dir
    size: 2MiB
services:
  web:
    image: alpine
    volumes:
      - name: data
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
    assert!(err.contains("already holds"), "{err}");
    assert!(err.contains("is smaller"), "{err}");
}
