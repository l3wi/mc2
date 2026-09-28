//! Volume listing: `GET /v1/volumes`.

use crate::api::{ApiError, ApiResult};
use crate::AppState;
use axum::extract::State;
use mc2_api::VolumeView;
use mc2_runtime::parse_volume_name;
use std::collections::HashMap;

/// List MC2-owned volume directories retained on the node's volume root, with
/// their declared size limit and measured usage.
///
/// Volumes are filesystem-only in v1 — directories named
/// `mc2-<stack>--<volume>` under `--volume-dir` (default `<data-dir>/volumes`) —
/// so this lists the root and reports each MC2-named directory. Non-MC2
/// entries are skipped. Usage is measured best-effort; the limit comes from
/// the owning stack's stored `volumes.<name>.size`.
pub async fn list_volumes(State(state): State<AppState>) -> ApiResult<Vec<VolumeView>> {
    let root = state.volume_dir.as_path();

    // Declared limits per (stack, volume), from the stored stack documents.
    let mut declared: HashMap<(String, String), u64> = HashMap::new();
    for stack in state.store.list_stacks().await.map_err(ApiError::store)? {
        if let Ok(doc) = mc2_api::parse_stack_yaml(&stack.raw_yaml) {
            for (name, vol) in doc.volumes {
                declared.insert((stack.name.clone(), name), vol.size_mib);
            }
        }
    }

    let mut views = Vec::new();

    let Ok(entries) = std::fs::read_dir(root) else {
        // Missing volume root == no volumes.
        return Ok(axum::Json(views));
    };

    for entry in entries.flatten() {
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if !meta.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some((stack, volume)) = parse_volume_name(&name) else {
            continue;
        };
        let path = entry.path();
        let limit_mib = declared.get(&(stack.clone(), volume.clone())).copied();
        views.push(VolumeView {
            used_mib: crate::host_metrics::dir_size_mib(&path),
            path: path.display().to_string(),
            limit_mib,
            volume,
            stack,
            name,
        });
    }

    // Deterministic order: stack, then volume name.
    views.sort_by(|a, b| a.stack.cmp(&b.stack).then_with(|| a.volume.cmp(&b.volume)));

    Ok(axum::Json(views))
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use mc2_store::{MemoryStore, StackPlan, Store};
    use tower::ServiceExt;

    use crate::api::router;
    use crate::api::testing::test_state_open;

    #[tokio::test]
    async fn lists_mc2_volumes_from_volume_root() {
        let root = tempfile::tempdir().unwrap();
        let vol_dir = root.path().join("volumes");
        std::fs::create_dir_all(vol_dir.join("mc2-demo--data")).unwrap();
        std::fs::create_dir_all(vol_dir.join("mc2-demo--cache")).unwrap();
        std::fs::create_dir_all(vol_dir.join("mc2-other--data")).unwrap();
        std::fs::write(
            vol_dir.join("mc2-demo--data").join("payload"),
            vec![0u8; 2048],
        )
        .unwrap();
        // Non-MC2 directory must be ignored.
        std::fs::create_dir_all(vol_dir.join("scratch")).unwrap();
        // A stray file must be ignored.
        std::fs::write(vol_dir.join("notes.txt"), "hi").unwrap();

        let store = MemoryStore::new();
        store.init_cluster("").await.unwrap();
        // The owning stack declares the sizes; `other` is unknown here.
        store
            .commit_stack_plan(&StackPlan::replicas(
                "demo",
                "{}",
                "name: demo\nvolumes:\n  data:\n    kind: dir\n    size: 20GiB\n  cache:\n    kind: dir\n    size: 5GiB\nservices:\n  web:\n    image: alpine\n",
                [],
            ))
            .await
            .unwrap();
        let mut state = test_state_open(store);
        state.volume_dir = vol_dir;

        let app = router(state);
        let res = app
            .oneshot(
                Request::builder()
                    .uri("/v1/volumes")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), axum::http::StatusCode::OK);
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        let views: Vec<mc2_api::VolumeView> = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(views.len(), 3);

        // Deterministic order: demo/cache, demo/data, other/data.
        let first = &views[0];
        assert_eq!(first.stack, "demo");
        assert_eq!(first.volume, "cache");
        assert_eq!(first.name, "mc2-demo--cache");
        assert!(first.path.contains("mc2-demo--cache"));
        assert_eq!(first.limit_mib, Some(5 * 1024));

        let second = &views[1];
        assert_eq!(second.volume, "data");
        assert_eq!(second.limit_mib, Some(20 * 1024));
        assert_eq!(second.used_mib, 0, "2 KiB floors to 0 MiB");

        // A volume whose stack is not stored has no declared limit.
        assert_eq!(views[2].stack, "other");
        assert_eq!(views[2].limit_mib, None);
    }

    #[tokio::test]
    async fn empty_when_volume_root_missing() {
        let store = MemoryStore::new();
        store.init_cluster("").await.unwrap();
        let mut state = test_state_open(store);
        state.volume_dir = std::path::PathBuf::from("/tmp/definitely-missing-mc2-vol");

        let app = router(state);
        let res = app
            .oneshot(
                Request::builder()
                    .uri("/v1/volumes")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), axum::http::StatusCode::OK);
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        let views: Vec<mc2_api::VolumeView> = serde_json::from_slice(&bytes).unwrap();
        assert!(views.is_empty());
    }
}
