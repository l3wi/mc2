//! Operator REST API.

use crate::apply::{apply_stack_yaml, list_instance_views};
use crate::auth::AuthUser;
use crate::ingress::build_ingress_routes_for_node;
use crate::secrets::set_secret;
use crate::AppState;
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{sse::Event, IntoResponse},
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
        .route("/v1/stacks/{name}", axum::routing::delete(delete_stack))
        .route("/v1/instances", get(list_instances))
        .route("/v1/instances/{id}/fabric", get(get_instance_fabric))
        .route(
            "/v1/instances/{id}/exec",
            axum::routing::post(exec_instance),
        )
        .route("/v1/instances/{id}/logs", get(get_instance_logs))
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

    let public_hostname = state
        .store
        .get_setting("public_hostname")
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "get_setting public_hostname");
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
        public_hostname,
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
        || m.contains("conflict")
        || m.contains("empty")
        || m.contains("expose")
        || m.contains("replicas")
        || m.contains("restartpolicy")
        || m.contains("apiversion")
        || m.contains("kind")
        || m.contains("metadata")
        || m.contains("parse")
        || m.contains("yaml")
}

/// Tear down a stack: delete instances + definition. `?volumes=true` also
/// removes the stack's named volumes (best-effort).
async fn delete_stack(
    State(state): State<AppState>,
    _auth: AuthUser,
    Path(name): Path<String>,
    Query(params): Query<StackDeleteQuery>,
) -> Result<StatusCode, (StatusCode, Json<serde_json::Value>)> {
    // Snapshot the YAML before deleting so `--volumes` knows the volume names.
    let stack = state.store.get_stack(&name).await.map_err(store_err)?;
    let Some(stack) = stack else {
        return Err((
            StatusCode::NOT_FOUND,
            Json(json!({ "error": format!("stack not found: {name}") })),
        ));
    };

    if !state.store.delete_stack(&name).await.map_err(store_err)? {
        return Err((
            StatusCode::NOT_FOUND,
            Json(json!({ "error": format!("stack not found: {name}") })),
        ));
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

#[derive(Debug, Deserialize)]
struct StackDeleteQuery {
    #[serde(default)]
    volumes: bool,
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

#[derive(Debug, Deserialize)]
struct ExecBody {
    cmd: Vec<String>,
    #[serde(default)]
    stdin: Option<String>,
}

/// Run a command inside the instance's sandbox and return captured output.
async fn exec_instance(
    State(state): State<AppState>,
    _auth: AuthUser,
    Path(id): Path<String>,
    Json(body): Json<ExecBody>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    if body.cmd.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "cmd must not be empty" })),
        ));
    }
    let inst = state.store.get_instance(&id).await.map_err(store_err)?;
    let Some(inst) = inst else {
        return Err((
            StatusCode::NOT_FOUND,
            Json(json!({ "error": format!("instance not found: {id}") })),
        ));
    };
    let Some(runtime_id) = inst.runtime_id.as_deref() else {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": format!("instance {id} has no sandbox runtime yet") })),
        ));
    };
    let stdin = body.stdin.unwrap_or_default().into_bytes();
    match state
        .runtime
        .exec_with_output(runtime_id, &body.cmd, &stdin)
        .await
    {
        Ok(out) => Ok(Json(json!({
            "instanceId": id,
            "exitCode": out.exit_code,
            "stdout": out.stdout,
            "stderr": out.stderr,
        }))),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )),
    }
}

#[derive(Debug, Deserialize)]
struct LogsQuery {
    #[serde(default)]
    tail: Option<usize>,
    #[serde(default)]
    follow: bool,
}

/// Recent sandbox logs (runtime / exec / kernel) for an instance. With
/// `follow=true`, returns an SSE stream (tail snapshot first, then new entries
/// as they arrive).
async fn get_instance_logs(
    State(state): State<AppState>,
    _auth: AuthUser,
    Path(id): Path<String>,
    Query(params): Query<LogsQuery>,
) -> Result<axum::response::Response, (StatusCode, Json<serde_json::Value>)> {
    use axum::response::sse::Sse;
    use futures::stream::{self, StreamExt};

    let inst = state.store.get_instance(&id).await.map_err(store_err)?;
    let Some(inst) = inst else {
        return Err((
            StatusCode::NOT_FOUND,
            Json(json!({ "error": format!("instance not found: {id}") })),
        ));
    };
    let Some(runtime_id) = inst.runtime_id.as_deref() else {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": format!("instance {id} has no sandbox runtime yet") })),
        ));
    };

    if !params.follow {
        let opts = microsandbox::logs::LogOptions {
            tail: params.tail,
            ..Default::default()
        };
        return match microsandbox::logs::read_logs(runtime_id, &opts).await {
            Ok(entries) => {
                let list: Vec<serde_json::Value> =
                    entries.into_iter().map(|e| entry_json(&e)).collect();
                Ok(Json(json!({ "instanceId": id, "entries": list })).into_response())
            }
            Err(e) => Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": e.to_string() })),
            )),
        };
    }

    // Follow: tail snapshot (if requested), then resume from its cursor.
    let mut past_events: Vec<Event> = Vec::new();
    let resume = if let Some(n) = params.tail {
        let opts = microsandbox::logs::LogOptions {
            tail: Some(n),
            ..Default::default()
        };
        match microsandbox::logs::read_logs(runtime_id, &opts).await {
            Ok(entries) => {
                let cursor = entries.last().map(|e| e.cursor.clone());
                for e in entries {
                    past_events.push(entry_event(&e));
                }
                cursor
            }
            Err(e) => {
                return Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "error": e.to_string() })),
                ))
            }
        }
    } else {
        None
    };
    let start = match resume {
        Some(cursor) => microsandbox::logs::LogStreamStart::From(cursor),
        None => microsandbox::logs::LogStreamStart::Beginning,
    };
    let stream_opts = microsandbox::logs::LogStreamOptions {
        start,
        follow: true,
        ..Default::default()
    };
    let live = match microsandbox::logs::log_stream(runtime_id, &stream_opts).await {
        Ok(s) => s,
        Err(e) => {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": e.to_string() })),
            ))
        }
    };

    let entries: futures::stream::BoxStream<'static, Result<Event, std::convert::Infallible>> =
        stream::iter(past_events)
            .map(Ok::<Event, std::convert::Infallible>)
            .chain(live.filter_map(|r| async move {
                r.ok()
                    .map(|e| Ok::<Event, std::convert::Infallible>(entry_event(&e)))
            }))
            .boxed();
    Ok(Sse::new(entries).into_response())
}

fn entry_json(e: &microsandbox::logs::LogEntry) -> serde_json::Value {
    json!({
        "timestamp": e.timestamp.to_rfc3339(),
        "source": format!("{:?}", e.source).to_ascii_lowercase(),
        "data": String::from_utf8_lossy(&e.data),
    })
}

fn entry_event(e: &microsandbox::logs::LogEntry) -> Event {
    Event::default().data(entry_json(e).to_string())
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

    /// Minimal NodeRuntime stub: exec returns a canned result; the rest are
    /// unused by the REST handlers under test.
    struct MockRuntime;

    #[async_trait::async_trait]
    impl mc2_runtime::NodeRuntime for MockRuntime {
        async fn ensure_running(
            &self,
            _d: &mc2_runtime::DesiredSandbox,
        ) -> anyhow::Result<mc2_runtime::SandboxStatus> {
            unreachable!("not exercised")
        }
        async fn ensure_removed(&self, _id: &str) -> anyhow::Result<()> {
            unreachable!("not exercised")
        }
        async fn status(&self, _id: &str) -> anyhow::Result<mc2_runtime::SandboxStatus> {
            unreachable!("not exercised")
        }
        async fn list(&self) -> anyhow::Result<Vec<String>> {
            unreachable!("not exercised")
        }
        async fn exec_command(&self, _id: &str, _argv: &[String]) -> anyhow::Result<i32> {
            Ok(7)
        }
        async fn exec_with_output(
            &self,
            _id: &str,
            _argv: &[String],
            _stdin: &[u8],
        ) -> anyhow::Result<mc2_runtime::ExecResult> {
            Ok(mc2_runtime::ExecResult {
                exit_code: 7,
                stdout: "hello out".into(),
                stderr: "hello err".into(),
            })
        }
    }

    fn test_state(store: std::sync::Arc<dyn Store>) -> AppState {
        let mut state = AppState {
            store,
            data_dir: std::path::PathBuf::from("/tmp/mc2-test"),
            version: "0.1.0-test",
            secrets_key: std::sync::Arc::new(mc2_store::SecretsKey::from_bytes([1u8; 32])),
            volume_dir: None,
            runtime: std::sync::Arc::new(mc2_runtime::MicrosandboxRuntime::new(None)),
        };
        state.runtime = std::sync::Arc::new(MockRuntime);
        state
    }

    #[test]
    fn stack_client_errors_map_to_400_keywords() {
        assert!(is_stack_client_error(
            "service web: duplicate expose.port 8080"
        ));
        assert!(is_stack_client_error(
            "service web: expose protocol must be tcp in v1 (got udp)"
        ));
        assert!(is_stack_client_error(
            "published host port 5001 conflicts with another allocation in this stack"
        ));
        assert!(is_stack_client_error("invalid stack YAML: ..."));
        assert!(!is_stack_client_error("database locked"));
        assert!(!is_stack_client_error("connection reset by peer"));
    }

    #[tokio::test]
    async fn apply_fabric_validation_returns_400() {
        let store = MemoryStore::new();
        store.init_cluster("").await.unwrap();
        let app = router(test_state(store));
        let body = serde_json::json!({
            "yaml": r#"
name: bad
services:
  a:
    image: alpine
    expose:
      - port: 8080
      - port: 8080
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
        assert!(v["error"].as_str().unwrap_or("").contains("expose"), "{v}");
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
        store.init_cluster(&hash_token("secret")).await.unwrap();
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
        store.init_cluster("").await.unwrap();
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
    async fn status_reports_public_hostname() {
        let store = MemoryStore::new();
        store.init_cluster("").await.unwrap();
        store
            .set_setting("public_hostname", "mc2.example.com")
            .await
            .unwrap();
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
        let body = res.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["publicHostname"], "mc2.example.com");
    }

    #[tokio::test]
    async fn list_nodes_returns_joined() {
        let store = MemoryStore::new();
        store.init_cluster(&hash_token("secret")).await.unwrap();
        store
            .upsert_local_node(NodeJoin {
                name: "n1".into(),
                labels_json: r#"{"role":"worker"}"#.into(),
                arch: "aarch64".into(),
                cpus: 2,
                memory_mib: 4096,
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

    #[tokio::test]
    async fn logs_reads_sandbox_entries() {
        // MSB_HOME is process-global but only this test reads it (no other
        // mc2-server test touches the SDK log registry).
        let home = tempfile::tempdir().unwrap();
        std::env::set_var("MSB_HOME", home.path());
        let name = "demo-web-0";
        let log_dir = home.path().join("sandboxes").join(name).join("logs");
        std::fs::create_dir_all(&log_dir).unwrap();
        std::fs::write(
            log_dir.join("exec.log"),
            r#"{"t":"2026-01-01T00:00:00Z","s":"stdout","d":"hello from sandbox","id":1}
"#,
        )
        .unwrap();

        let store = MemoryStore::new();
        store.init_cluster("").await.unwrap();
        store.upsert_stack("demo", "{}", "yaml").await.unwrap();
        let inst = store
            .reconcile_service_replicas("demo", "web", 1, r#"{}"#)
            .await
            .unwrap();
        store
            .update_instance_status(&inst[0].id, "Running", Some(name), None)
            .await
            .unwrap();

        let app = router(test_state(store));
        let res = app
            .oneshot(
                Request::builder()
                    .uri(format!("/v1/instances/{}/logs?tail=5", inst[0].id))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let entries = v["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 1, "{v}");
        assert!(
            entries[0]["data"]
                .as_str()
                .unwrap()
                .contains("hello from sandbox"),
            "{v}"
        );

        std::env::remove_var("MSB_HOME");
    }

    #[tokio::test]
    async fn exec_runs_command_and_reports_errors() {
        let store = MemoryStore::new();
        store.init_cluster("").await.unwrap();
        store.upsert_stack("demo", "{}", "yaml").await.unwrap();
        let inst = store
            .reconcile_service_replicas("demo", "web", 1, r#"{}"#)
            .await
            .unwrap();
        store
            .update_instance_status(&inst[0].id, "Running", Some("demo-web-0"), None)
            .await
            .unwrap();
        let app = router(test_state(store));

        // Success: mirrors canned output + exit code.
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/v1/instances/{}/exec", inst[0].id))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&serde_json::json!({ "cmd": ["echo", "hi"] })).unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["exitCode"], 7);
        assert_eq!(v["stdout"], "hello out");

        // Empty command → 400.
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/v1/instances/{}/exec", inst[0].id))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&serde_json::json!({ "cmd": [] })).unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);

        // Missing instance → 404.
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/instances/00000000-0000-0000-0000-000000000000/exec")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&serde_json::json!({ "cmd": ["ls"] })).unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND);

        // Instance with no runtime yet → 400.
        let store2 = MemoryStore::new();
        store2.init_cluster("").await.unwrap();
        store2.upsert_stack("demo", "{}", "yaml").await.unwrap();
        let pending = store2
            .reconcile_service_replicas("demo", "web", 1, r#"{}"#)
            .await
            .unwrap();
        let app2 = router(test_state(store2));
        let res = app2
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/v1/instances/{}/exec", pending[0].id))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&serde_json::json!({ "cmd": ["ls"] })).unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    }
}
