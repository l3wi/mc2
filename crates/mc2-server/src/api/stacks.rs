//! Stack endpoints: apply (up) and delete (down).

use crate::api::{ApiError, ApiResult};
use crate::apply::{apply_stack_yaml, ApplyError};
use crate::AppState;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct ApplyBody {
    pub yaml: String,
}

#[derive(Debug, Deserialize)]
pub struct StackDeleteQuery {
    #[serde(default)]
    pub volumes: bool,
}

pub async fn apply_stack(
    State(state): State<AppState>,
    _auth: crate::auth::AuthUser,
    Json(body): Json<ApplyBody>,
) -> ApiResult<crate::ApplyResult> {
    match apply_stack_yaml(state.store.clone(), &body.yaml).await {
        Ok(r) => Ok(Json(r)),
        Err(ApplyError::Validation(msg) | ApplyError::Allocation(msg)) => {
            Err(ApiError::bad_request(msg))
        }
        Err(ApplyError::Other(e)) => Err(ApiError::internal(e.to_string())),
    }
}

/// Tear down a stack: delete instances + definition. `?volumes=true` also
/// removes the stack's named volumes (best-effort).
pub async fn delete_stack(
    State(state): State<AppState>,
    _auth: crate::auth::AuthUser,
    Path(name): Path<String>,
    Query(params): Query<StackDeleteQuery>,
) -> Result<StatusCode, ApiError> {
    // Snapshot the YAML before deleting so `--volumes` knows the volume names.
    let stack = state
        .store
        .get_stack(&name)
        .await
        .map_err(ApiError::store)?;
    let Some(stack) = stack else {
        return Err(ApiError::not_found(format!("stack not found: {name}")));
    };

    if !state
        .store
        .delete_stack(&name)
        .await
        .map_err(ApiError::store)?
    {
        return Err(ApiError::not_found(format!("stack not found: {name}")));
    }

    if params.volumes {
        if let Ok(doc) = mc2_api::parse_stack_yaml(&stack.raw_yaml) {
            let root = volume_root(state.volume_dir.as_deref());
            for vol in doc.volumes.keys() {
                let vpath = root.join(mc2_runtime::volume_name(&name, vol));
                if vpath.exists() {
                    match std::fs::remove_dir_all(&vpath) {
                        Ok(()) => tracing::info!(path = %vpath.display(), "removed named volume"),
                        Err(e) => tracing::warn!(
                            path = %vpath.display(),
                            error = %e,
                            "remove named volume failed (best-effort)"
                        ),
                    }
                }
            }
        }
    }

    Ok(StatusCode::NO_CONTENT)
}

/// Resolve the named-volume root: `--volume-dir` if set, else the msb default.
fn volume_root(volume_dir: Option<&std::path::Path>) -> std::path::PathBuf {
    volume_dir
        .map(std::path::Path::to_path_buf)
        .unwrap_or_else(|| crate::expand_data_dir("~/.microsandbox/volumes"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use mc2_store::{MemoryStore, Store};
    use tower::ServiceExt;

    use crate::api::router;
    use crate::api::testing::test_state;

    #[tokio::test]
    async fn parse_errors_classify_as_validation() {
        let store: std::sync::Arc<dyn Store> = mc2_store::MemoryStore::new();
        let err = apply_stack_yaml(store, "name: x\nservices: {")
            .await
            .unwrap_err();
        assert!(matches!(err, ApplyError::Validation(_)), "{err:?}");
    }

    #[tokio::test]
    async fn validation_errors_classify_as_validation() {
        let store: std::sync::Arc<dyn Store> = mc2_store::MemoryStore::new();
        // Cross-service duplicate expose port fails stack validation.
        let yaml = r#"
name: shop
services:
  web:
    image: alpine
    expose:
      - port: 8080
  admin:
    image: alpine
    expose:
      - port: 8080
"#;
        let err = apply_stack_yaml(store, yaml).await.unwrap_err();
        assert!(matches!(err, ApplyError::Validation(_)), "{err:?}");
    }

    #[tokio::test]
    async fn delete_stack_tears_down_and_reports_missing() {
        let store = MemoryStore::new();
        store.init_cluster("").await.unwrap();
        store.upsert_stack("demo", "{}", "yaml").await.unwrap();
        store
            .reconcile_service_replicas("demo", "web", 1, r#"{"image":"x"}"#)
            .await
            .unwrap();
        let app = router(test_state(store.clone()));

        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/v1/stacks/demo")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NO_CONTENT);
        assert!(store.get_stack("demo").await.unwrap().is_none());
        assert!(store.list_instances().await.unwrap().is_empty());

        let res2 = app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/v1/stacks/demo")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res2.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn delete_stack_volumes_removes_named_volume_dirs() {
        let store = MemoryStore::new();
        store.init_cluster("").await.unwrap();
        let yaml = r#"
name: demo
volumes:
  data:
    kind: dir
services:
  web:
    image: alpine
    volumes:
      - name: data
        target: /data
"#;
        store.upsert_stack("demo", "{}", yaml).await.unwrap();

        let root = tempfile::tempdir().unwrap();
        let volume_path = root.path().join("mc2-demo--data");
        std::fs::create_dir_all(volume_path.join("sub")).unwrap();
        std::fs::write(volume_path.join("sub/keep.txt"), "x").unwrap();

        let mut state = test_state(store);
        state.volume_dir = Some(root.path().to_path_buf());
        let app = router(state);

        let res = app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/v1/stacks/demo?volumes=true")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NO_CONTENT);
        assert!(
            !root.path().join("mc2-demo--data").exists(),
            "volume dir removed"
        );
    }
}
