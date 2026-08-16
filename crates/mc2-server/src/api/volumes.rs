//! Volume listing: `GET /v1/volumes`.

use crate::api::ApiResult;
use crate::AppState;
use axum::extract::State;
use mc2_api::VolumeView;
use mc2_runtime::parse_volume_name;

/// List named volumes retained on the node's volume root.
///
/// Volumes are filesystem-only in v1 — directories named
/// `mc2-<stack>--<volume>` under `--volume-dir` (default
/// `~/.microsandbox/volumes`) — so this lists the root and reports each
/// MC2-named directory. Non-MC2 entries are skipped. Sizes are measured
/// best-effort.
pub async fn list_volumes(State(state): State<AppState>) -> ApiResult<Vec<VolumeView>> {
    let root = crate::api::stacks::volume_root(state.volume_dir.as_deref());
    let mut views = Vec::new();

    let Ok(entries) = std::fs::read_dir(&root) else {
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
        views.push(VolumeView {
            size_mib: crate::host_metrics::dir_size_mib(&path),
            path: path.display().to_string(),
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
    use mc2_store::{MemoryStore, Store};
    use tower::ServiceExt;

    use crate::api::router;
    use crate::api::testing::test_state;

    #[tokio::test]
    async fn lists_mc2_volumes_from_volume_root() {
        let root = tempfile::tempdir().unwrap();
        let vol_dir = root.path().join("volumes");
        std::fs::create_dir_all(vol_dir.join("mc2-demo--data")).unwrap();
        std::fs::create_dir_all(vol_dir.join("mc2-demo--cache")).unwrap();
        std::fs::create_dir_all(vol_dir.join("mc2-other--data")).unwrap();
        // Non-MC2 directory must be ignored.
        std::fs::create_dir_all(vol_dir.join("scratch")).unwrap();
        // A stray file must be ignored.
        std::fs::write(vol_dir.join("notes.txt"), "hi").unwrap();

        let store = MemoryStore::new();
        store.init_cluster("").await.unwrap();
        let mut state = test_state(store);
        state.volume_dir = Some(vol_dir);

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

        let first = &views[0];
        assert_eq!(first.stack, "demo");
        assert_eq!(first.volume, "cache");
        assert_eq!(first.name, "mc2-demo--cache");
        assert!(first.path.contains("mc2-demo--cache"));
        assert!(first.size_mib > 0 || views.iter().all(|v| v.size_mib == 0));
    }

    #[tokio::test]
    async fn empty_when_volume_root_missing() {
        let store = MemoryStore::new();
        store.init_cluster("").await.unwrap();
        let mut state = test_state(store);
        state.volume_dir = Some(std::path::PathBuf::from("/tmp/definitely-missing-mc2-vol"));

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
