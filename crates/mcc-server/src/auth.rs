//! Bearer token auth for operator REST API.

use crate::AppState;
use axum::{
    extract::{FromRequestParts, Request},
    http::{header, request::Parts, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;

/// Extracted and verified API bearer token (opaque presence).
#[derive(Debug, Clone)]
pub struct AuthUser;

impl FromRequestParts<AppState> for AuthUser {
    type Rejection = Response;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let token = extract_bearer(&parts.headers).ok_or_else(unauthorized)?;

        match state.store.verify_api_token(&token).await {
            Ok(true) => Ok(AuthUser),
            Ok(false) => Err(unauthorized()),
            Err(e) => {
                tracing::error!(error = %e, "token verification failed");
                Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "error": "internal error" })),
                )
                    .into_response())
            }
        }
    }
}

/// Middleware alternative if we need layer-style auth later.
#[allow(dead_code)]
pub async fn require_auth(state: AppState, req: Request, next: Next) -> Response {
    let token = match extract_bearer(req.headers()) {
        Some(t) => t,
        None => return unauthorized(),
    };
    match state.store.verify_api_token(&token).await {
        Ok(true) => next.run(req).await,
        Ok(false) => unauthorized(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
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
        Json(json!({ "error": "unauthorized", "message": "missing or invalid bearer token" })),
    )
        .into_response()
}
