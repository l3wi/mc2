//! Server meta endpoints: health, cluster status, node listing.

use crate::api::{ApiError, ApiResult};
use crate::AppState;
use axum::extract::State;
use axum::Json;
use mc2_api::{ClusterStatus, NodeView};
use serde_json::json;

pub async fn health() -> impl axum::response::IntoResponse {
    Json(json!({
        "status": "ok",
        "service": "mc2-server",
    }))
}

pub async fn status(
    State(state): State<AppState>,
    _auth: crate::auth::AuthUser,
) -> ApiResult<ClusterStatus> {
    let counts = state.store.cluster_counts().await.map_err(|e| {
        tracing::error!(error = %e, "cluster_counts");
        ApiError::internal("store error")
    })?;

    let public_hostname = state
        .store
        .get_setting("public_hostname")
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "get_setting public_hostname");
            ApiError::internal("store error")
        })?;

    Ok(Json(ClusterStatus {
        version: state.version.to_string(),
        api_version: mc2_api::API_VERSION.to_string(),
        nodes_ready: counts.nodes_ready,
        nodes_total: counts.nodes_total,
        stacks: counts.stacks,
        instances: counts.instances,
        message: Some("control plane up".into()),
        public_hostname,
    }))
}

pub async fn list_nodes(
    State(state): State<AppState>,
    _auth: crate::auth::AuthUser,
) -> ApiResult<Vec<NodeView>> {
    let nodes = state.store.list_nodes().await.map_err(|e| {
        tracing::error!(error = %e, "list_nodes");
        ApiError::internal("store error")
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
