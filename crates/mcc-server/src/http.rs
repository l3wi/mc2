//! Operator REST API (Phase 1: health + status).

use crate::auth::AuthUser;
use crate::AppState;
use axum::{extract::State, http::StatusCode, response::IntoResponse, routing::get, Json, Router};
use mcc_api::ClusterStatus;
use serde_json::json;
use tower_http::trace::TraceLayer;

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/v1/status", get(status))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

async fn health() -> impl IntoResponse {
    Json(json!({
        "status": "ok",
        "service": "mcc-server",
    }))
}

async fn status(
    State(state): State<AppState>,
    _auth: AuthUser,
) -> Result<Json<ClusterStatus>, (StatusCode, Json<serde_json::Value>)> {
    let counts = state.store.cluster_counts().await.map_err(|e| {
        tracing::error!(error = %e, "cluster_counts");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": "store error" })),
        )
    })?;

    Ok(Json(ClusterStatus {
        version: state.version.to_string(),
        api_version: mcc_api::API_VERSION.to_string(),
        nodes_ready: counts.nodes_ready,
        nodes_total: counts.nodes_total,
        stacks: counts.stacks,
        instances: counts.instances,
        message: Some("Phase 1: control plane up".into()),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use mcc_store::{hash_token, MemoryStore, Store};
    use tower::ServiceExt;

    fn test_state(store: std::sync::Arc<dyn Store>) -> AppState {
        AppState {
            store,
            data_dir: std::path::PathBuf::from("/tmp/mcc-test"),
            version: "0.1.0-test",
        }
    }

    #[tokio::test]
    async fn health_no_auth() {
        let store = MemoryStore::new();
        let app = router(test_state(store));
        let res = app
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn status_requires_auth() {
        let store = MemoryStore::new();
        store
            .init_cluster(&hash_token("secret"), &hash_token("join"))
            .await
            .unwrap();
        let app = router(test_state(store));

        let unauth = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/v1/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(unauth.status(), StatusCode::UNAUTHORIZED);

        let auth = app
            .oneshot(
                Request::builder()
                    .uri("/v1/status")
                    .header("Authorization", "Bearer secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(auth.status(), StatusCode::OK);
        let body = auth.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["api_version"], "mcc/v1");
    }
}
