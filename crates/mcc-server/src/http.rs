//! Operator REST API.

use crate::auth::AuthUser;
use crate::AppState;
use axum::{extract::State, http::StatusCode, response::IntoResponse, routing::get, Json, Router};
use mcc_api::{ClusterStatus, NodeView};
use serde_json::json;
use tower_http::trace::TraceLayer;

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/v1/status", get(status))
        .route("/v1/nodes", get(list_nodes))
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
        message: Some("control plane up".into()),
    }))
}

async fn list_nodes(
    State(state): State<AppState>,
    _auth: AuthUser,
) -> Result<Json<Vec<NodeView>>, (StatusCode, Json<serde_json::Value>)> {
    let nodes = state.store.list_nodes().await.map_err(|e| {
        tracing::error!(error = %e, "list_nodes");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": "store error" })),
        )
    })?;

    let views: Vec<NodeView> = nodes
        .into_iter()
        .map(|n| {
            let labels = serde_json::from_str(&n.labels_json).unwrap_or(json!({}));
            NodeView {
                id: n.id,
                name: n.name,
                arch: n.arch,
                cpus: n.cpus,
                memory_mib: n.memory_mib,
                status: n.status,
                last_heartbeat: n.last_heartbeat,
                labels,
                created_at: n.created_at,
            }
        })
        .collect();

    Ok(Json(views))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use mcc_store::{hash_token, MemoryStore, NodeJoin, Store};
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

    #[tokio::test]
    async fn list_nodes_returns_joined() {
        let store = MemoryStore::new();
        store
            .init_cluster(&hash_token("secret"), &hash_token("join"))
            .await
            .unwrap();
        store
            .upsert_node_join(NodeJoin {
                name: "n1".into(),
                labels_json: r#"{"role":"worker"}"#.into(),
                arch: "aarch64".into(),
                cpus: 2,
                memory_mib: 4096,
                node_token_hash: hash_token("nt"),
            })
            .await
            .unwrap();

        let app = router(test_state(store));
        let res = app
            .oneshot(
                Request::builder()
                    .uri("/v1/nodes")
                    .header("Authorization", "Bearer secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body = res.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v.as_array().unwrap().len(), 1);
        assert_eq!(v[0]["name"], "n1");
        assert_eq!(v[0]["status"], "Ready");
        assert!(v[0].get("node_token_hash").is_none());
    }
}
