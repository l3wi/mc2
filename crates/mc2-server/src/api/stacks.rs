//! Stack endpoints: apply (up) and delete (down).

use crate::api::{ApiError, ApiResult};
use crate::apply::{apply_stack_yaml, ApplyConfig, ApplyError};
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
    Json(body): Json<ApplyBody>,
) -> ApiResult<crate::ApplyResult> {
    // Serialize applies (and stack deletes): planning reads the store (expose
    // claims, port allocations, the instance diff) and the commit writes it, so
    // concurrent applies could otherwise race a claim.
    let _guard = state.apply_lock.lock().await;
    let cfg = ApplyConfig {
        limits: state.limits,
        data_dir: state.data_dir.clone(),
        volume_dir: state.volume_dir.clone(),
        rest_port: state.rest_port,
        allow_host_profile: state.allow_host_profile,
        port_probe: state.port_probe,
    };
    match apply_stack_yaml(state.store.clone(), &cfg, &body.yaml).await {
        Ok(r) => Ok(Json(r)),
        Err(
            ApplyError::Validation(msg) | ApplyError::Allocation(msg) | ApplyError::Capacity(msg),
        ) => Err(ApiError::bad_request(msg)),
        Err(ApplyError::Other(e)) => Err(ApiError::internal(e.to_string())),
    }
}

/// Tear down a stack: delete instances + definition. `?volumes=true` also
/// removes the stack's named volumes (best-effort).
pub async fn delete_stack(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Query(params): Query<StackDeleteQuery>,
) -> Result<StatusCode, ApiError> {
    // Same lock as apply: both mutate stack/instance rows the claim checks read.
    let _guard = state.apply_lock.lock().await;
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
            let root = &state.volume_dir;
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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use mc2_store::{ClaimKind, HostPortClaim, MemoryStore, PortProtocol, StackPlan, Store};
    use tower::ServiceExt;

    use crate::api::router;
    use crate::api::testing::test_state_open;

    /// ApplyConfig with no limits, for tests that exercise validation only.
    fn unlimited_cfg() -> ApplyConfig {
        let data_dir = tempfile::tempdir().unwrap().path().to_path_buf();
        ApplyConfig {
            limits: crate::ResourceLimits::default(),
            volume_dir: data_dir.join("volumes"),
            data_dir,
            rest_port: 0,
            allow_host_profile: false,
            port_probe: crate::probe_host_loopback_port,
        }
    }

    #[tokio::test]
    async fn parse_errors_classify_as_validation() {
        let store: std::sync::Arc<dyn Store> = mc2_store::MemoryStore::new();
        let err = apply_stack_yaml(store, &unlimited_cfg(), "name: x\nservices: {")
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
        let err = apply_stack_yaml(store, &unlimited_cfg(), yaml)
            .await
            .unwrap_err();
        assert!(matches!(err, ApplyError::Validation(_)), "{err:?}");
    }

    #[tokio::test]
    async fn delete_stack_tears_down_and_reports_missing() {
        let store = MemoryStore::new();
        store.init_cluster("").await.unwrap();
        store
            .commit_stack_plan(&StackPlan::replicas(
                "demo",
                "{}",
                "yaml",
                vec![("web", vec![r#"{"image":"x"}"#.to_string()])],
            ))
            .await
            .unwrap();
        let app = router(test_state_open(store.clone()));

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
        store
            .commit_stack_plan(&StackPlan::replicas("demo", "{}", yaml, []))
            .await
            .unwrap();

        let root = tempfile::tempdir().unwrap();
        let volume_path = root.path().join("mc2-demo--data");
        std::fs::create_dir_all(volume_path.join("sub")).unwrap();
        std::fs::write(volume_path.join("sub/keep.txt"), "x").unwrap();

        let mut state = test_state_open(store);
        state.volume_dir = root.path().to_path_buf();
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

    /// A port-allocation conflict is a user error: the REST layer must answer
    /// 400, not 500 (C5).
    #[tokio::test]
    async fn allocation_errors_classify_as_bad_request() {
        let store = MemoryStore::new();
        store.init_cluster("").await.unwrap();
        store
            .commit_stack_plan(&StackPlan::replicas(
                "app",
                "{}",
                "yaml",
                vec![(
                    "web",
                    vec![r#"{"image":"alpine","ports":[{"published":8080,"target":80},{"published":8081,"target":81}]}"#.to_string()],
                )],
            ))
            .await
            .unwrap();

        let app = router(test_state_open(store));
        let body = serde_json::json!({
            "yaml": "name: app\nservices:\n  web:\n    image: alpine\n    ports: [\"8080:80\",\"8080:81\"]\n"
        });
        let res = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/stacks:apply")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    }

    /// C4: a host-port clash the store's constraint catches is still a user
    /// error — the REST layer answers 400 with the reason, never 500.
    #[tokio::test]
    async fn store_host_port_conflict_is_a_bad_request() {
        let store = MemoryStore::new();
        store.init_cluster("").await.unwrap();
        // A claim with no matching instance spec: what a concurrent apply that
        // won the race leaves behind for the pre-commit scans to miss, so the
        // database constraint is the only thing that can reject it.
        store
            .commit_stack_plan(
                &StackPlan::replicas("ghost", "{}", "yaml", []).with_host_ports(vec![
                    HostPortClaim {
                        port: 8080,
                        protocol: PortProtocol::Tcp,
                        service: "web".into(),
                        ordinal: Some(0),
                        kind: ClaimKind::Publish,
                    },
                ]),
            )
            .await
            .unwrap();

        let app = router(test_state_open(store));
        let body = serde_json::json!({
            "yaml": "name: app\nservices:\n  web:\n    image: alpine\n    ports: [\"8080:80\"]\n"
        });
        let res = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/stacks:apply")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let msg = v["error"].as_str().unwrap_or("");
        assert!(msg.contains("8080/tcp"), "{v}");
        assert!(msg.contains("ghost/web"), "names the owner: {v}");
    }
}
