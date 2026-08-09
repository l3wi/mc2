//! Secret endpoints: names only, never values.

use crate::api::{ApiError, ApiResult};
use crate::secrets::{set_secret, SecretError};
use crate::AppState;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use mc2_store::SecretMeta;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct SecretBody {
    pub value: String,
}

/// List secret names only — never values.
pub async fn list_secrets(
    State(state): State<AppState>,
    _auth: crate::auth::AuthUser,
) -> ApiResult<Vec<SecretMeta>> {
    state
        .store
        .list_secret_meta()
        .await
        .map(Json)
        .map_err(ApiError::store)
}

/// Create or replace a secret. Body is never returned.
pub async fn put_secret(
    State(state): State<AppState>,
    _auth: crate::auth::AuthUser,
    Path(name): Path<String>,
    Json(body): Json<SecretBody>,
) -> ApiResult<SecretMeta> {
    match set_secret(
        state.store.clone(),
        state.secrets_key.as_ref(),
        &name,
        &body.value,
    )
    .await
    {
        Ok(meta) => Ok(Json(meta)),
        Err(SecretError::Validation(msg)) => Err(ApiError::bad_request(msg)),
        Err(SecretError::Other(e)) => Err(ApiError::internal(e.to_string())),
    }
}

pub async fn delete_secret(
    State(state): State<AppState>,
    _auth: crate::auth::AuthUser,
    Path(name): Path<String>,
) -> Result<StatusCode, ApiError> {
    match state.store.delete_secret(&name).await {
        Ok(true) => Ok(StatusCode::NO_CONTENT),
        Ok(false) => Err(ApiError::not_found(format!("secret not found: {name}"))),
        Err(e) => Err(ApiError::store(e)),
    }
}
