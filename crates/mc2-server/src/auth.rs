//! Bearer token auth for the operator REST API.
//!
//! Auth is a router-level middleware ([`require_auth`]) applied to every `/v1`
//! route, so a handler cannot forget it. `/health` stays public. The bearer is
//! verified against the store on every request, so `mc2 server token rotate`
//! takes effect immediately.
//!
//! `--no-auth` is a per-process switch (`AppState::no_auth`): the token still
//! exists in the store; this process just skips the check.

use crate::AppState;
use axum::{
    extract::{Request, State},
    http::{header, HeaderMap, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;

/// Middleware: require a valid operator bearer token on protected routes.
pub async fn require_auth(State(state): State<AppState>, req: Request, next: Next) -> Response {
    if state.no_auth {
        // Explicit per-start opt-out (`--no-auth`), validated at startup.
        return next.run(req).await;
    }

    let Some(token) = extract_bearer(req.headers()) else {
        return unauthorized();
    };

    match state.store.verify_api_token(&token).await {
        Ok(true) => next.run(req).await,
        Ok(false) => unauthorized(),
        Err(e) => {
            tracing::error!(error = %e, "token verification failed");
            internal()
        }
    }
}

fn extract_bearer(headers: &HeaderMap) -> Option<String> {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(value: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(header::AUTHORIZATION, value.parse().unwrap());
        h
    }

    #[test]
    fn extracts_bearer_case_insensitively() {
        assert_eq!(
            extract_bearer(&headers("Bearer abc")).as_deref(),
            Some("abc")
        );
        assert_eq!(
            extract_bearer(&headers("bearer abc")).as_deref(),
            Some("abc")
        );
    }

    #[test]
    fn rejects_missing_empty_or_wrong_scheme() {
        assert!(extract_bearer(&HeaderMap::new()).is_none());
        assert!(extract_bearer(&headers("Bearer ")).is_none());
        assert!(extract_bearer(&headers("Basic abc")).is_none());
    }
}
