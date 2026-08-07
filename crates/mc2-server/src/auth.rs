//! Bearer token auth for operator REST API (optional when no API token configured).

use crate::AppState;
use axum::{
    extract::FromRequestParts,
    http::{header, request::Parts, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;

/// Extracted operator identity. When the cluster has no API token configured,
/// requests are allowed without a bearer.
#[derive(Debug, Clone)]
pub struct AuthUser;

impl FromRequestParts<AppState> for AuthUser {
    type Rejection = Response;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let required = match state.store.api_auth_required().await {
            Ok(r) => r,
            Err(e) => {
                tracing::error!(error = %e, "api_auth_required");
                return Err(internal());
            }
        };

        if !required {
            // Open cluster: no bearer required.
            return Ok(AuthUser);
        }

        let Some(token) = extract_bearer(&parts.headers) else {
            return Err(unauthorized());
        };

        match state.store.verify_api_token(&token).await {
            Ok(true) => Ok(AuthUser),
            Ok(false) => Err(unauthorized()),
            Err(e) => {
                tracing::error!(error = %e, "token verification failed");
                Err(internal())
            }
        }
    }
}

fn extract_bearer(headers: &http::HeaderMap) -> Option<String> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let token = value
        .strip_prefix("Bearer ")
        .or_else(|| value.strip_prefix("bearer "))?;
    if token.is_empty() {
        None
    } else {
        Some(token.to_string())
    }
}

fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(json!({
            "error": "unauthorized",
            "message": "missing or invalid bearer token"
        })),
    )
        .into_response()
}

fn internal() -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({ "error": "internal error" })),
    )
        .into_response()
}
