//! Operator REST API.

use crate::apply::{apply_stack_yaml, list_instance_views};
use crate::auth::AuthUser;
use crate::ingress::build_ingress_routes_for_node;
use crate::secrets::set_secret;
use crate::AppState;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, put},
    Json, Router,
};
use mc2_api::{ClusterStatus, NodeView};
use mc2_store::SecretMeta;
use serde::Deserialize;
use serde_json::json;
use tower_http::trace::TraceLayer;

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/v1/status", get(status))
        .route("/v1/nodes", get(list_nodes))
        .route("/v1/stacks:apply", axum::routing::post(apply_stack))
        .route("/v1/instances", get(list_instances))
        .route("/v1/instances/{id}/fabric", get(get_instance_fabric))
        .route("/v1/ingress", get(list_ingress))
        .route("/v1/secrets", get(list_secrets))
        .route("/v1/secrets/{name}", put(put_secret).delete(delete_secret))
        .route("/v1/ssh/keys", get(list_ssh_keys))
        .route(
            "/v1/ssh/keys/{name}",
            put(put_ssh_key).get(get_ssh_key).delete(delete_ssh_key),
        )
        .route("/v1/ssh/endpoints", get(list_ssh_endpoints))
        .route(
            "/v1/instances/{id}/ssh",
            get(get_instance_ssh)
                .put(put_instance_ssh)
                .delete(delete_instance_ssh),
        )
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

#[derive(Debug, Deserialize)]
struct ApplyBody {
    yaml: String,
}

#[derive(Debug, Deserialize)]
struct SecretBody {
    value: String,
}

async fn health() -> impl IntoResponse {
    Json(json!({
        "status": "ok",
        "service": "mc2-server",
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
        api_version: mc2_api::API_VERSION.to_string(),
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

async fn apply_stack(
    State(state): State<AppState>,
    _auth: AuthUser,
    Json(body): Json<ApplyBody>,
) -> Result<Json<crate::ApplyResult>, (StatusCode, Json<serde_json::Value>)> {
    match apply_stack_yaml(state.store.clone(), &body.yaml).await {
        Ok(r) => Ok(Json(r)),
        Err(e) => {
            let msg = e.to_string();
            let code = if is_stack_client_error(&msg) {
                StatusCode::BAD_REQUEST
            } else {
                StatusCode::INTERNAL_SERVER_ERROR
            };
            Err((code, Json(json!({ "error": msg }))))
        }
    }
}

/// Stack YAML / apply validation problems → 400 (not 500).
fn is_stack_client_error(msg: &str) -> bool {
    let m = msg.to_ascii_lowercase();
    m.contains("invalid")
        || m.contains("unsupported")
        || m.contains("required")
        || m.contains("require")
        || m.contains("ingress")
        || m.contains("must ")
        || m.contains("must be")
        || m.contains("must not")
        || m.contains("not a service")
        || m.contains("not defined")
        || m.contains("duplicate")
        || m.contains("empty")
        || m.contains("expose")
        || m.contains("allow")
        || m.contains("replicas")
        || m.contains("restartpolicy")
        || m.contains("apiversion")
        || m.contains("kind")
        || m.contains("metadata")
        || m.contains("parse")
        || m.contains("yaml")
}

async fn list_instances(
    State(state): State<AppState>,
    _auth: AuthUser,
) -> Result<Json<Vec<mc2_store::InstanceRecord>>, (StatusCode, Json<serde_json::Value>)> {
    list_instance_views(state.store.clone())
        .await
        .map(Json)
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": e.to_string() })),
            )
        })
}

/// Observed fabric status for one instance (agent-reported).
async fn get_instance_fabric(
    State(state): State<AppState>,
    _auth: AuthUser,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    match state.store.get_instance_fabric(&id).await {
        Ok(Some(rec)) => {
            let observed: serde_json::Value =
                serde_json::from_str(&rec.observed_json).unwrap_or(json!({}));
            Ok(Json(json!({
                "instanceId": rec.instance_id,
                "phase": rec.phase,
                "message": rec.message,
                "updatedAt": rec.updated_at,
                "observed": observed,
            })))
        }
        Ok(None) => Err((
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "no fabric status for instance" })),
        )),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )),
    }
}

/// Desired Ingress routes derived from stack YAML + instance placement (D7).
/// Ready/file status is agent-local (`catalog.json` under `--ingress-config-dir`).
async fn list_ingress(
    State(state): State<AppState>,
    _auth: AuthUser,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let stacks = state.store.list_stacks().await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
    })?;
    let instances = state.store.list_instances().await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
    })?;
    let stacks_yaml: Vec<(String, String)> = stacks
        .into_iter()
        .map(|s| (s.name, s.raw_yaml))
        .collect();

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
                    "caddyTls": r.caddy_tls,
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

/// List secret names only — never values.
async fn list_secrets(
    State(state): State<AppState>,
    _auth: AuthUser,
) -> Result<Json<Vec<SecretMeta>>, (StatusCode, Json<serde_json::Value>)> {
    state.store.list_secret_meta().await.map(Json).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
    })
}

/// Create or replace a secret. Body is never returned.
async fn put_secret(
    State(state): State<AppState>,
    _auth: AuthUser,
    Path(name): Path<String>,
    Json(body): Json<SecretBody>,
) -> Result<Json<SecretMeta>, (StatusCode, Json<serde_json::Value>)> {
    match set_secret(
        state.store.clone(),
        state.secrets_key.as_ref(),
        &name,
        &body.value,
    )
    .await
    {
        Ok(meta) => Ok(Json(meta)),
        Err(e) => {
            let msg = e.to_string();
            let code = if msg.contains("required") || msg.contains("empty") {
                StatusCode::BAD_REQUEST
            } else {
                StatusCode::INTERNAL_SERVER_ERROR
            };
            Err((code, Json(json!({ "error": msg }))))
        }
    }
}

async fn delete_secret(
    State(state): State<AppState>,
    _auth: AuthUser,
    Path(name): Path<String>,
) -> Result<StatusCode, (StatusCode, Json<serde_json::Value>)> {
    match state.store.delete_secret(&name).await {
        Ok(true) => Ok(StatusCode::NO_CONTENT),
        Ok(false) => Err((
            StatusCode::NOT_FOUND,
            Json(json!({ "error": format!("secret not found: {name}") })),
        )),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )),
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SshKeyBody {
    public_key: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct InstanceSshPutBody {
    enabled: bool,
    #[serde(default)]
    bind: Option<String>,
    #[serde(default)]
    port: Option<u16>,
    #[serde(default)]
    user: Option<String>,
    #[serde(default)]
    sftp: Option<bool>,
    #[serde(default)]
    authorized_keys: Vec<String>,
}

async fn list_ssh_keys(
    State(state): State<AppState>,
    _auth: AuthUser,
) -> Result<Json<Vec<mc2_store::SshAuthorizedKey>>, (StatusCode, Json<serde_json::Value>)> {
    state.store.list_ssh_keys().await.map(Json).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
    })
}

async fn get_ssh_key(
    State(state): State<AppState>,
    _auth: AuthUser,
    Path(name): Path<String>,
) -> Result<Json<mc2_store::SshAuthorizedKey>, (StatusCode, Json<serde_json::Value>)> {
    match state.store.get_ssh_key(&name).await {
        Ok(Some(k)) => Ok(Json(k)),
        Ok(None) => Err((
            StatusCode::NOT_FOUND,
            Json(json!({ "error": format!("ssh key not found: {name}") })),
        )),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )),
    }
}

async fn put_ssh_key(
    State(state): State<AppState>,
    _auth: AuthUser,
    Path(name): Path<String>,
    Json(body): Json<SshKeyBody>,
) -> Result<Json<mc2_store::SshAuthorizedKey>, (StatusCode, Json<serde_json::Value>)> {
    match state.store.put_ssh_key(&name, &body.public_key).await {
        Ok(k) => Ok(Json(k)),
        Err(e) => {
            let msg = e.to_string();
            let code =
                if msg.contains("unsupported") || msg.contains("empty") || msg.contains("short") {
                    StatusCode::BAD_REQUEST
                } else {
                    StatusCode::INTERNAL_SERVER_ERROR
                };
            Err((code, Json(json!({ "error": msg }))))
        }
    }
}

async fn delete_ssh_key(
    State(state): State<AppState>,
    _auth: AuthUser,
    Path(name): Path<String>,
) -> Result<StatusCode, (StatusCode, Json<serde_json::Value>)> {
    match state.store.delete_ssh_key(&name).await {
        Ok(true) => Ok(StatusCode::NO_CONTENT),
        Ok(false) => Err((
            StatusCode::NOT_FOUND,
            Json(json!({ "error": format!("ssh key not found: {name}") })),
        )),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )),
    }
}

async fn get_instance_ssh(
    State(state): State<AppState>,
    _auth: AuthUser,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    if state
        .store
        .get_instance(&id)
        .await
        .map_err(store_err)?
        .is_none()
    {
        return Err((
            StatusCode::NOT_FOUND,
            Json(json!({ "error": format!("instance not found: {id}") })),
        ));
    }
    let rec = state.store.get_instance_ssh(&id).await.map_err(store_err)?;
    Ok(Json(ssh_view(&id, rec.as_ref())))
}

async fn put_instance_ssh(
    State(state): State<AppState>,
    _auth: AuthUser,
    Path(id): Path<String>,
    Json(body): Json<InstanceSshPutBody>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    if state
        .store
        .get_instance(&id)
        .await
        .map_err(store_err)?
        .is_none()
    {
        return Err((
            StatusCode::NOT_FOUND,
            Json(json!({ "error": format!("instance not found: {id}") })),
        ));
    }
    if body.enabled && body.authorized_keys.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "authorizedKeys required when enabled" })),
        ));
    }
    for name in &body.authorized_keys {
        if state
            .store
            .get_ssh_key(name)
            .await
            .map_err(store_err)?
            .is_none()
        {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": format!("ssh key not found: {name}") })),
            ));
        }
    }
    let desired = crate::ssh::desired_from_put(
        &id,
        body.enabled,
        body.bind,
        body.port,
        body.user,
        body.sftp,
        body.authorized_keys,
    );
    let rec = state
        .store
        .put_instance_ssh_desired(&desired)
        .await
        .map_err(store_err)?;
    Ok(Json(ssh_view(&id, Some(&rec))))
}

async fn delete_instance_ssh(
    State(state): State<AppState>,
    _auth: AuthUser,
    Path(id): Path<String>,
) -> Result<StatusCode, (StatusCode, Json<serde_json::Value>)> {
    if state
        .store
        .get_instance(&id)
        .await
        .map_err(store_err)?
        .is_none()
    {
        return Err((
            StatusCode::NOT_FOUND,
            Json(json!({ "error": format!("instance not found: {id}") })),
        ));
    }
    let _ = state
        .store
        .clear_instance_ssh_override(&id)
        .await
        .map_err(store_err)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn list_ssh_endpoints(
    State(state): State<AppState>,
    _auth: AuthUser,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let rows = state.store.list_instance_ssh().await.map_err(store_err)?;
    let instances = state.store.list_instances().await.map_err(store_err)?;
    let nodes = state.store.list_nodes().await.map_err(store_err)?;
    let mut endpoints = Vec::new();
    for r in rows {
        if r.phase != "Open" {
            continue;
        }
        let inst = instances.iter().find(|i| i.id == r.instance_id);
        let node_name = inst
            .and_then(|i| i.node_id.as_ref())
            .and_then(|nid| nodes.iter().find(|n| n.id == *nid))
            .map(|n| n.name.clone());
        let port = r.port.unwrap_or(0);
        let bind = r.bind.clone().unwrap_or_else(|| "127.0.0.1".into());
        endpoints.push(json!({
            "instanceId": r.instance_id,
            "stack": inst.map(|i| i.stack.clone()),
            "service": inst.map(|i| i.service.clone()),
            "ordinal": inst.map(|i| i.ordinal),
            "nodeId": inst.and_then(|i| i.node_id.clone()),
            "nodeName": node_name,
            "phase": r.phase,
            "bind": bind,
            "port": port,
            "connectHint": format!("ssh -p {port} root@{bind}"),
        }));
    }
    Ok(Json(json!({ "endpoints": endpoints })))
}

fn ssh_view(instance_id: &str, rec: Option<&mc2_store::InstanceSshRecord>) -> serde_json::Value {
    match rec {
        None => json!({
            "instanceId": instance_id,
            "desired": false,
            "phase": "Closed",
            "bind": null,
            "port": null,
            "message": null,
        }),
        Some(r) => {
            let keys: Vec<String> = r
                .desired_key_names_json
                .as_ref()
                .and_then(|j| serde_json::from_str(j).ok())
                .unwrap_or_default();
            json!({
                "instanceId": r.instance_id,
                "hasOverride": r.has_override,
                "desired": r.desired,
                "desiredBind": r.desired_bind,
                "desiredPort": r.desired_port,
                "desiredUser": r.desired_user,
                "desiredSftp": r.desired_sftp,
                "authorizedKeys": keys,
                "phase": r.phase,
                "bind": r.bind,
                "port": r.port,
                "message": r.message,
                "updatedAt": r.updated_at,
            })
        }
    }
}

fn store_err(e: impl ToString) -> (StatusCode, Json<serde_json::Value>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({ "error": e.to_string() })),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use mc2_store::{hash_token, MemoryStore, NodeJoin, Store};
    use tower::ServiceExt;

    fn test_state(store: std::sync::Arc<dyn Store>) -> AppState {
        AppState {
            store,
            data_dir: std::path::PathBuf::from("/tmp/mc2-test"),
            version: "0.1.0-test",
            secrets_key: std::sync::Arc::new(mc2_store::SecretsKey::from_bytes([1u8; 32])),
        }
    }

    #[test]
    fn stack_client_errors_map_to_400_keywords() {
        assert!(is_stack_client_error(
            "service b: allow to a:9 requires that service to expose port 9"
        ));
        assert!(is_stack_client_error(
            "service web: allow.to \"nosuch\" is not a service in this stack"
        ));
        assert!(is_stack_client_error("invalid stack YAML: ..."));
        assert!(!is_stack_client_error("database locked"));
        assert!(!is_stack_client_error("connection reset by peer"));
    }

    #[tokio::test]
    async fn apply_fabric_validation_returns_400() {
        let store = MemoryStore::new();
        store.init_cluster("", "").await.unwrap();
        let app = router(test_state(store));
        let body = serde_json::json!({
            "yaml": r#"
apiVersion: mc2/v1
kind: Stack
metadata:
  name: bad
services:
  a:
    image: alpine
  b:
    image: alpine
    allow:
      - to: a
        port: 9
"#
        });
        let res = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/stacks:apply")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(
            v["error"].as_str().unwrap_or("").contains("expose"),
            "{v}"
        );
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
        assert_eq!(v["api_version"], "mc2/v1");
    }

    #[tokio::test]
    async fn open_cluster_status_without_token() {
        let store = MemoryStore::new();
        store.init_cluster("", "").await.unwrap();
        let app = router(test_state(store));
        let res = app
            .oneshot(
                Request::builder()
                    .uri("/v1/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
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
