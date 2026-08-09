//! Ingress route listing (desired state; file readiness is agent-local).

use crate::api::{ApiError, ApiResult};
use crate::ingress::build_ingress_routes_for_node;
use crate::AppState;
use axum::extract::State;
use axum::Json;
use serde_json::json;

/// Desired Ingress routes derived from stack YAML + instance placement (D7).
/// Ready/file status is agent-local (`catalog.json` under `--ingress-config-dir`).
pub async fn list_ingress(
    State(state): State<AppState>,
    _auth: crate::auth::AuthUser,
) -> ApiResult<serde_json::Value> {
    let stacks = state.store.list_stacks().await.map_err(ApiError::store)?;
    let instances = state
        .store
        .list_instances()
        .await
        .map_err(ApiError::store)?;
    let stacks_yaml: Vec<(String, String)> =
        stacks.into_iter().map(|s| (s.name, s.raw_yaml)).collect();

    // Union of routes for every node that has instances (dedupe by route id).
    let mut by_id: std::collections::BTreeMap<String, serde_json::Value> =
        std::collections::BTreeMap::new();
    let mut node_ids: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for i in &instances {
        if let Some(ref n) = i.node_id {
            node_ids.insert(n.clone());
        }
    }
    for node_id in &node_ids {
        for r in build_ingress_routes_for_node(node_id, &stacks_yaml, &instances) {
            by_id.insert(
                r.id.clone(),
                json!({
                    "id": r.id,
                    "stack": r.stack,
                    "host": r.host,
                    "path": r.path,
                    "pathType": r.path_type,
                    "service": r.service,
                    "guestPort": r.guest_port,
                    "hostPort": r.host_port,
                    "bind": r.bind,
                    "tlsEnabled": r.tls_enabled,
                    "certResolver": r.cert_resolver,
                    "backendInstanceId": r.backend_instance_id,
                    "backendOrdinal": r.backend_ordinal,
                    "nodeId": node_id,
                }),
            );
        }
    }

    let routes: Vec<serde_json::Value> = by_id.into_values().collect();
    Ok(Json(json!({
        "routes": routes,
        "note": "file catalog readiness is on the agent (--ingress-config-dir); see catalog.json",
    })))
}
