//! SSH endpoints: cluster key registry, instance overrides, open endpoints.

use crate::api::{ApiError, ApiResult};
use crate::AppState;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use mc2_store::{InstanceSshRecord, SshAuthorizedKey, StoreError};
use serde::Deserialize;
use serde_json::json;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SshKeyBody {
    pub public_key: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstanceSshPutBody {
    pub enabled: bool,
    #[serde(default)]
    pub bind: Option<String>,
    #[serde(default)]
    pub port: Option<u16>,
    #[serde(default)]
    pub user: Option<String>,
    #[serde(default)]
    pub sftp: Option<bool>,
    #[serde(default)]
    pub authorized_keys: Vec<String>,
}

pub async fn list_ssh_keys(
    State(state): State<AppState>,
    _auth: crate::auth::AuthUser,
) -> ApiResult<Vec<SshAuthorizedKey>> {
    state
        .store
        .list_ssh_keys()
        .await
        .map(Json)
        .map_err(ApiError::store)
}

pub async fn get_ssh_key(
    State(state): State<AppState>,
    _auth: crate::auth::AuthUser,
    Path(name): Path<String>,
) -> ApiResult<SshAuthorizedKey> {
    match state.store.get_ssh_key(&name).await {
        Ok(Some(k)) => Ok(Json(k)),
        Ok(None) => Err(ApiError::not_found(format!("ssh key not found: {name}"))),
        Err(e) => Err(ApiError::store(e)),
    }
}

pub async fn put_ssh_key(
    State(state): State<AppState>,
    _auth: crate::auth::AuthUser,
    Path(name): Path<String>,
    Json(body): Json<SshKeyBody>,
) -> ApiResult<SshAuthorizedKey> {
    match state.store.put_ssh_key(&name, &body.public_key).await {
        Ok(k) => Ok(Json(k)),
        Err(StoreError::InvalidArgument(msg)) => Err(ApiError::bad_request(msg)),
        Err(e) => Err(ApiError::store(e)),
    }
}

pub async fn delete_ssh_key(
    State(state): State<AppState>,
    _auth: crate::auth::AuthUser,
    Path(name): Path<String>,
) -> Result<StatusCode, ApiError> {
    match state.store.delete_ssh_key(&name).await {
        Ok(true) => Ok(StatusCode::NO_CONTENT),
        Ok(false) => Err(ApiError::not_found(format!("ssh key not found: {name}"))),
        Err(e) => Err(ApiError::store(e)),
    }
}

pub async fn get_instance_ssh(
    State(state): State<AppState>,
    _auth: crate::auth::AuthUser,
    Path(id): Path<String>,
) -> ApiResult<serde_json::Value> {
    if state
        .store
        .get_instance(&id)
        .await
        .map_err(ApiError::store)?
        .is_none()
    {
        return Err(ApiError::not_found(format!("instance not found: {id}")));
    }
    let rec = state
        .store
        .get_instance_ssh(&id)
        .await
        .map_err(ApiError::store)?;
    Ok(Json(ssh_view(&id, rec.as_ref())))
}

pub async fn put_instance_ssh(
    State(state): State<AppState>,
    _auth: crate::auth::AuthUser,
    Path(id): Path<String>,
    Json(body): Json<InstanceSshPutBody>,
) -> ApiResult<serde_json::Value> {
    if state
        .store
        .get_instance(&id)
        .await
        .map_err(ApiError::store)?
        .is_none()
    {
        return Err(ApiError::not_found(format!("instance not found: {id}")));
    }
    if body.enabled && body.authorized_keys.is_empty() {
        return Err(ApiError::bad_request(
            "authorizedKeys required when enabled",
        ));
    }
    for name in &body.authorized_keys {
        if state
            .store
            .get_ssh_key(name)
            .await
            .map_err(ApiError::store)?
            .is_none()
        {
            return Err(ApiError::bad_request(format!("ssh key not found: {name}")));
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
        .map_err(ApiError::store)?;
    Ok(Json(ssh_view(&id, Some(&rec))))
}

pub async fn delete_instance_ssh(
    State(state): State<AppState>,
    _auth: crate::auth::AuthUser,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    if state
        .store
        .get_instance(&id)
        .await
        .map_err(ApiError::store)?
        .is_none()
    {
        return Err(ApiError::not_found(format!("instance not found: {id}")));
    }
    let _ = state
        .store
        .clear_instance_ssh_override(&id)
        .await
        .map_err(ApiError::store)?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn list_ssh_endpoints(
    State(state): State<AppState>,
    _auth: crate::auth::AuthUser,
) -> ApiResult<serde_json::Value> {
    let rows = state
        .store
        .list_instance_ssh()
        .await
        .map_err(ApiError::store)?;
    let instances = state
        .store
        .list_instances()
        .await
        .map_err(ApiError::store)?;
    let nodes = state.store.list_nodes().await.map_err(ApiError::store)?;
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

fn ssh_view(instance_id: &str, rec: Option<&InstanceSshRecord>) -> serde_json::Value {
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
