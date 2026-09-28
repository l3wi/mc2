//! Server meta endpoints: health, cluster status, node listing.

use crate::api::{ApiError, ApiResult};
use crate::AppState;
use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use mc2_api::{ClusterStatus, NodeView};
use serde_json::json;

/// Unauthenticated readiness probe (D1).
///
/// Answers `200` only while the node loop is live and has completed a reconcile
/// pass recently; otherwise `503` with the reason, so a service manager (or the
/// ingress health check) can act on an orchestrator that is up but no longer
/// converging.
pub async fn health(State(state): State<AppState>) -> impl axum::response::IntoResponse {
    match state.liveness.check() {
        Ok(()) => (
            StatusCode::OK,
            Json(json!({
                "status": "ok",
                "service": "mc2-server",
            })),
        ),
        Err(reason) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({
                "status": "degraded",
                "service": "mc2-server",
                "reason": reason,
            })),
        ),
    }
}

pub async fn status(State(state): State<AppState>) -> ApiResult<ClusterStatus> {
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
        resources: Some(resource_status(&state).await),
    }))
}

/// Build the resource budget + host/consumption snapshot for status.
async fn resource_status(state: &AppState) -> mc2_api::ResourceStatus {
    use mc2_api::{HostResources, ReservedResources, ResourceLimitsView};

    let instances = state.store.list_instances().await.unwrap_or_default();
    let (reserved_cpu, reserved_mem) = crate::scheduler::reserved_capacity(&instances);

    // Host disk: the filesystem hosting MC2's data/volumes.
    let disk_path = state
        .volume_dir
        .as_deref()
        .unwrap_or(state.data_dir.as_path());
    let (disk_total_mib, disk_free_mib) = crate::host_metrics::disk_usage_mib(disk_path);

    let mc2_disk_used_mib = crate::host_metrics::dir_size_mib(&state.data_dir)
        + state
            .volume_dir
            .as_deref()
            .map(crate::host_metrics::dir_size_mib)
            .unwrap_or(0);

    mc2_api::ResourceStatus {
        limits: ResourceLimitsView {
            cpus: state.limits.cpus,
            memory_mib: state.limits.memory_mib,
            disk_mib: state.limits.disk_mib,
        },
        host: HostResources {
            cpus: crate::host_metrics::host_cpus(),
            memory_mib: crate::host_metrics::host_memory_mib(),
            disk_total_mib,
            disk_free_mib,
        },
        reserved: ReservedResources {
            cpus: reserved_cpu,
            memory_mib: reserved_mem,
        },
        mc2_disk_used_mib,
    }
}

pub async fn list_nodes(State(state): State<AppState>) -> ApiResult<Vec<NodeView>> {
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
    use crate::api::testing::test_state;
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use std::sync::Arc;
    use std::time::Duration;
    use tower::ServiceExt;

    /// D1: `/health` is public and reflects node-loop readiness — 200 while a
    /// pass is current, 503 (with a reason) once it is stale, and 200 again
    /// after a pass without any token.
    #[tokio::test]
    async fn health_reflects_node_loop_readiness() {
        let store: Arc<dyn mc2_store::Store> = mc2_store::MemoryStore::new();
        let mut state = test_state(store);
        let liveness = Arc::new(crate::Liveness::with_stale_after(Duration::from_millis(40)));
        state.liveness = liveness.clone();
        let app = crate::router(state);

        let get = |app: axum::Router| {
            app.oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
        };

        let res = get(app.clone()).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK, "fresh loop is ready");

        tokio::time::sleep(Duration::from_millis(80)).await;
        let res = get(app.clone()).await.unwrap();
        assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body = res.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["status"], "degraded");
        assert!(
            v["reason"].as_str().is_some_and(|r| !r.is_empty()),
            "a degraded probe must carry a reason: {v}"
        );

        liveness.record_pass();
        let res = get(app.clone()).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK, "a pass refreshes readiness");

        liveness.mark_dead("node loop failed: boom");
        let res = get(app).await.unwrap();
        assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
    }
}
