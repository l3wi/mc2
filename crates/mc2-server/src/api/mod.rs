//! Operator REST API: router assembly and shared error/response types.

mod ingress;
mod instances;
mod meta;
mod secrets;
mod ssh;
mod stacks;
mod volumes;

use crate::AppState;
use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, put},
    Json, Router,
};
use serde_json::json;
use tower_http::trace::TraceLayer;

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(meta::health))
        .route("/v1/status", get(meta::status))
        .route("/v1/nodes", get(meta::list_nodes))
        .route("/v1/stacks:apply", axum::routing::post(stacks::apply_stack))
        .route(
            "/v1/stacks/{name}",
            axum::routing::delete(stacks::delete_stack),
        )
        .route("/v1/instances", get(instances::list_instances))
        .route(
            "/v1/instances/{id}/network",
            get(instances::get_instance_network),
        )
        .route("/v1/networks", get(instances::list_networks))
        .route(
            "/v1/instances/{id}/exec",
            axum::routing::post(instances::exec_instance),
        )
        .route("/v1/instances/{id}/logs", get(instances::get_instance_logs))
        .route("/v1/ingress", get(ingress::list_ingress))
        .route("/v1/volumes", get(volumes::list_volumes))
        .route("/v1/secrets", get(secrets::list_secrets))
        .route(
            "/v1/secrets/{name}",
            put(secrets::put_secret).delete(secrets::delete_secret),
        )
        .route("/v1/ssh/keys", get(ssh::list_ssh_keys))
        .route(
            "/v1/ssh/keys/{name}",
            put(ssh::put_ssh_key)
                .get(ssh::get_ssh_key)
                .delete(ssh::delete_ssh_key),
        )
        .route("/v1/ssh/endpoints", get(ssh::list_ssh_endpoints))
        .route(
            "/v1/instances/{id}/ssh",
            get(ssh::get_instance_ssh)
                .put(ssh::put_instance_ssh)
                .delete(ssh::delete_instance_ssh),
        )
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

/// Unified REST error: status code + JSON `{ "error": msg }`.
///
/// Keeps handler plumbing to a single type instead of repeating
/// `(StatusCode, Json<Value>)` tuples everywhere.
#[derive(Debug)]
pub enum ApiError {
    BadRequest(String),
    NotFound(String),
    Internal(String),
}

impl ApiError {
    pub fn bad_request(msg: impl Into<String>) -> Self {
        Self::BadRequest(msg.into())
    }

    pub fn not_found(msg: impl Into<String>) -> Self {
        Self::NotFound(msg.into())
    }

    pub fn internal(msg: impl Into<String>) -> Self {
        Self::Internal(msg.into())
    }

    /// A store failure surfaces as a 500 (message kept for debugging).
    pub fn store(e: impl ToString) -> Self {
        Self::Internal(e.to_string())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, msg) = match self {
            Self::BadRequest(m) => (StatusCode::BAD_REQUEST, m),
            Self::NotFound(m) => (StatusCode::NOT_FOUND, m),
            Self::Internal(m) => (StatusCode::INTERNAL_SERVER_ERROR, m),
        };
        (status, Json(json!({ "error": msg }))).into_response()
    }
}

/// Alias for handlers returning a JSON body.
pub type ApiResult<T> = Result<Json<T>, ApiError>;

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use mc2_store::{hash_token, MemoryStore, NodeJoin, Store};
    use tower::ServiceExt;

    use crate::api::router;
    use crate::api::testing::test_state;

    #[tokio::test]
    async fn apply_network_validation_returns_400() {
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
    async fn status_reports_resources() {
        let store = MemoryStore::new();
        store.init_cluster("").await.unwrap();
        store.upsert_stack("demo", "{}", "yaml").await.unwrap();
        let inst = store
            .reconcile_service_replicas(
                "demo",
                "web",
                1,
                r#"{"image":"x","cpus":1.0,"mem_limit":"512m"}"#,
            )
            .await
            .unwrap();
        // Bind the instance so it counts toward reserved usage.
        store
            .bind_instance_to_node(&inst[0].id, "n1")
            .await
            .unwrap();
        store
            .update_instance_status(&inst[0].id, "Running", Some("demo-web-0"), None)
            .await
            .unwrap();

        let mut state = test_state(store);
        state.limits = crate::ResourceLimits {
            cpus: 4,
            memory_mib: 8192,
            disk_mib: 0,
        };
        let app = router(state);
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

        let r = &v["resources"];
        assert_eq!(r["limits"]["cpus"], 4);
        assert_eq!(r["limits"]["memoryMib"], 8192);
        assert_eq!(r["limits"]["diskMib"], 0);
        assert!(r["host"]["cpus"].as_u64().unwrap() > 0, "host cpu");
        assert_eq!(r["reserved"]["cpus"], 1);
        assert_eq!(r["reserved"]["memoryMib"], 512);
        assert!(r["mc2DiskUsedMib"].as_u64().is_some());
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
}

#[cfg(test)]
pub(crate) mod testing {
    use crate::AppState;
    use std::sync::Arc;

    /// Minimal NodeRuntime stub: exec returns a canned result; the rest are
    /// unused by the REST handlers under test.
    pub struct MockRuntime;

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

    /// Test AppState wired to an in-memory store + the runtime stub.
    pub fn test_state(store: Arc<dyn mc2_store::Store>) -> AppState {
        let mut state = AppState {
            store,
            data_dir: std::path::PathBuf::from("/tmp/mc2-test"),
            version: "0.1.0-test",
            secrets_key: Arc::new(mc2_store::SecretsKey::from_bytes([1u8; 32])),
            volume_dir: None,
            runtime: Arc::new(mc2_runtime::MicrosandboxRuntime::new(None)),
            limits: crate::ResourceLimits::default(),
        };
        state.runtime = Arc::new(MockRuntime);
        state
    }
}
