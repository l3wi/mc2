//! Operator REST API: router assembly and shared error/response types.

mod ingress;
mod instances;
mod meta;
mod secrets;
mod ssh;
mod stacks;
mod volumes;

use crate::limits::EXEC_BODY_LIMIT_BYTES;
use crate::AppState;
use axum::{
    extract::DefaultBodyLimit,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post, put},
    Json, Router,
};
use serde_json::json;
use tower_http::{timeout::TimeoutLayer, trace::TraceLayer};

pub fn router(state: AppState) -> Router {
    // Long-running routes: **no whole-request deadline**. `exec` runs a
    // process that can take minutes and `logs` (with `follow=true`) is an
    // infinite SSE stream. `exec` also gets an explicit, larger body limit
    // for its stdin payload; every other route keeps axum's 2 MiB default.
    let long_running = Router::new()
        .route(
            "/v1/instances/{id}/exec",
            post(instances::exec_instance).layer(DefaultBodyLimit::max(EXEC_BODY_LIMIT_BYTES)),
        )
        .route("/v1/instances/{id}/logs", get(instances::get_instance_logs));

    // Everything else answers within the request deadline (408 on expiry).
    let ordinary = Router::new()
        .route("/v1/status", get(meta::status))
        .route("/v1/nodes", get(meta::list_nodes))
        .route("/v1/stacks:apply", post(stacks::apply_stack))
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
        );

    assemble(state, ordinary, long_running)
}

/// Assemble the operator router.
///
/// The `ordinary` group is wrapped in the whole-request deadline; the
/// `long_running` group (`exec`, `logs`) never is. Everything under `/v1` sits
/// behind the auth middleware so a handler cannot forget the check; `/health`
/// stays public for liveness probes.
fn assemble(state: AppState, ordinary: Router<AppState>, long_running: Router<AppState>) -> Router {
    let timeout = state.http_limits.request_timeout;
    // `0` disables the whole-request deadline.
    let with_deadline = |router: Router<AppState>| -> Router<AppState> {
        if timeout.is_zero() {
            router
        } else {
            router.layer(TimeoutLayer::with_status_code(
                axum::http::StatusCode::REQUEST_TIMEOUT,
                timeout,
            ))
        }
    };
    let public = with_deadline(Router::new().route("/health", get(meta::health)));
    let protected = with_deadline(ordinary).merge(long_running).route_layer(
        axum::middleware::from_fn_with_state(state.clone(), crate::auth::require_auth),
    );

    Router::new()
        .merge(public)
        .merge(protected)
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
    use mc2_store::{hash_token, MemoryStore, NodeJoin, StackPlan, Store};
    use tower::ServiceExt;

    use crate::api::router;
    use crate::api::testing::{test_state, test_state_open};
    use tokio_util::sync::CancellationToken;

    #[tokio::test]
    async fn apply_network_validation_returns_400() {
        let store = MemoryStore::new();
        let app = router(test_state_open(store));
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
    async fn no_auth_state_allows_status_without_token() {
        let store = MemoryStore::new();
        store.init_cluster(&hash_token("secret")).await.unwrap();
        let app = router(test_state_open(store));
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
        store
            .set_setting("public_hostname", "mc2.example.com")
            .await
            .unwrap();
        let app = router(test_state_open(store));
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
        let inst = store
            .commit_stack_plan(&StackPlan::replicas(
                "demo",
                "{}",
                "yaml",
                vec![(
                    "web",
                    vec![r#"{"image":"x","cpus":1.0,"mem_limit":"512m"}"#.to_string()],
                )],
            ))
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

        let mut state = test_state_open(store);
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

    /// A5: every registered `/v1` route sits behind the auth middleware.
    #[tokio::test]
    async fn every_v1_route_requires_a_token() {
        use axum::http::Method;

        let store = MemoryStore::new();
        store.init_cluster(&hash_token("secret")).await.unwrap();
        let app = router(test_state(store));

        let routes: &[(Method, &str)] = &[
            (Method::GET, "/v1/status"),
            (Method::GET, "/v1/nodes"),
            (Method::POST, "/v1/stacks:apply"),
            (Method::DELETE, "/v1/stacks/demo"),
            (Method::GET, "/v1/instances"),
            (Method::GET, "/v1/instances/x/network"),
            (Method::GET, "/v1/networks"),
            (Method::POST, "/v1/instances/x/exec"),
            (Method::GET, "/v1/instances/x/logs"),
            (Method::GET, "/v1/ingress"),
            (Method::GET, "/v1/volumes"),
            (Method::GET, "/v1/secrets"),
            (Method::PUT, "/v1/secrets/DB_PASS"),
            (Method::DELETE, "/v1/secrets/DB_PASS"),
            (Method::GET, "/v1/ssh/keys"),
            (Method::PUT, "/v1/ssh/keys/laptop"),
            (Method::GET, "/v1/ssh/keys/laptop"),
            (Method::DELETE, "/v1/ssh/keys/laptop"),
            (Method::GET, "/v1/ssh/endpoints"),
            (Method::GET, "/v1/instances/x/ssh"),
            (Method::PUT, "/v1/instances/x/ssh"),
            (Method::DELETE, "/v1/instances/x/ssh"),
        ];

        for (method, uri) in routes {
            let res = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method(method.clone())
                        .uri(*uri)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(res.status(), StatusCode::UNAUTHORIZED, "{method} {uri}");
        }

        // `/health` stays public.
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

    /// A5: a valid token still reaches a previously-unguarded route.
    #[tokio::test]
    async fn valid_token_reaches_volumes() {
        let store = MemoryStore::new();
        store.init_cluster(&hash_token("secret")).await.unwrap();
        let app = router(test_state(store));
        let res = app
            .oneshot(
                Request::builder()
                    .uri("/v1/volumes")
                    .header("Authorization", "Bearer secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }

    /// Serve the full router on an ephemeral loopback port.
    async fn serve_app(state: AppState) -> (std::net::SocketAddr, CancellationToken) {
        let http_limits = state.http_limits;
        let app = router(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let cancel = CancellationToken::new();
        let cancel_serve = cancel.clone();
        tokio::spawn(async move {
            let _ = crate::http_serve::serve(listener, app, http_limits, cancel_serve).await;
        });
        (addr, cancel)
    }

    /// A10: the request deadline wraps the ordinary group, never the
    /// long-running group (`exec`, `logs`).
    #[tokio::test]
    async fn request_deadline_wraps_ordinary_routes_only() {
        use std::time::Duration;

        let mut state = test_state_open(MemoryStore::new());
        state.http_limits.request_timeout = Duration::from_millis(50);
        let slow = || {
            get(|| async {
                tokio::time::sleep(Duration::from_millis(300)).await;
                StatusCode::OK
            })
        };
        let app = assemble(
            state,
            Router::new().route("/ordinary", slow()),
            Router::new().route("/long-running", slow()),
        );

        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/ordinary")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::REQUEST_TIMEOUT);

        let res = app
            .oneshot(
                Request::builder()
                    .uri("/long-running")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }

    /// A10: with a stalled request body, an ordinary route hits the request
    /// deadline (`408`) while `exec` — exempt — keeps waiting for the body.
    #[tokio::test]
    async fn stalled_body_times_out_ordinary_routes_but_not_exec() {
        use std::time::Duration;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpStream;

        let mut state = test_state_open(MemoryStore::new());
        state.http_limits.request_timeout = Duration::from_millis(200);
        let (addr, _cancel) = serve_app(state).await;

        let mut ordinary = TcpStream::connect(addr).await.unwrap();
        ordinary
            .write_all(
                b"POST /v1/stacks:apply HTTP/1.1\r\nhost: mc2\r\ncontent-type: application/json\r\ncontent-length: 4096\r\n\r\n{",
            )
            .await
            .unwrap();
        let mut buf = [0u8; 256];
        let n = tokio::time::timeout(Duration::from_secs(5), ordinary.read(&mut buf))
            .await
            .expect("ordinary route should answer at the request deadline")
            .expect("read the response");
        let head = String::from_utf8_lossy(&buf[..n]);
        assert!(head.starts_with("HTTP/1.1 408"), "{head}");

        // `POST /v1/instances/{id}/exec` is exempt: the same stalled body gets
        // no answer at all inside the deadline window.
        let mut exec = TcpStream::connect(addr).await.unwrap();
        exec.write_all(
            b"POST /v1/instances/inst-1/exec HTTP/1.1\r\nhost: mc2\r\ncontent-type: application/json\r\ncontent-length: 4096\r\n\r\n{",
        )
        .await
        .unwrap();
        let quiet = tokio::time::timeout(Duration::from_millis(1000), exec.read(&mut buf)).await;
        assert!(
            quiet.is_err(),
            "the exec route must not answer within the request deadline"
        );
    }
}

#[cfg(test)]
pub(crate) mod testing {
    use crate::AppState;
    use std::sync::Arc;

    /// One `exec_with_output` call, captured for assertions.
    #[derive(Debug, Clone)]
    pub struct ExecCall {
        pub argv: Vec<String>,
        pub stdin: Vec<u8>,
    }

    /// Minimal NodeRuntime stub: exec returns a canned result (recording each
    /// call); the rest are unused by the REST handlers under test.
    #[derive(Default)]
    pub struct MockRuntime {
        pub exec_calls: tokio::sync::Mutex<Vec<ExecCall>>,
    }

    #[async_trait::async_trait]
    impl mc2_runtime::NodeRuntime for MockRuntime {
        async fn ensure_running(
            &self,
            _d: &mc2_runtime::DesiredSandbox,
        ) -> anyhow::Result<mc2_runtime::EnsureRunning> {
            unreachable!("not exercised")
        }
        async fn ensure_removed(&self, _id: &str) -> anyhow::Result<()> {
            unreachable!("not exercised")
        }
        async fn status(&self, _id: &str) -> anyhow::Result<mc2_runtime::SandboxStatus> {
            unreachable!("not exercised")
        }
        async fn list_owned(&self, _install_id: &str) -> anyhow::Result<Vec<String>> {
            unreachable!("not exercised")
        }
        async fn exec_command(&self, _id: &str, _argv: &[String]) -> anyhow::Result<i32> {
            Ok(7)
        }
        async fn exec_with_output(
            &self,
            _id: &str,
            argv: &[String],
            stdin: &[u8],
        ) -> anyhow::Result<mc2_runtime::ExecResult> {
            self.exec_calls.lock().await.push(ExecCall {
                argv: argv.to_vec(),
                stdin: stdin.to_vec(),
            });
            Ok(mc2_runtime::ExecResult {
                exit_code: 7,
                stdout: b"hello out".to_vec(),
                // Deliberately invalid UTF-8: it must survive the wire format.
                stderr: vec![0xff, 0xfe],
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
            http_limits: crate::HttpLimits::default(),
            no_auth: false,
            rest_port: 0,
            allow_host_profile: false,
            // Hermetic: no real host-port probing in REST tests.
            port_probe: |_p| Ok(()),
            apply_lock: Arc::new(tokio::sync::Mutex::new(())),
            liveness: Arc::new(crate::Liveness::with_stale_after(
                std::time::Duration::from_secs(30),
            )),
        };
        state.runtime = Arc::new(MockRuntime::default());
        state
    }

    /// Test AppState for the `--no-auth` (open) case.
    pub fn test_state_open(store: Arc<dyn mc2_store::Store>) -> AppState {
        let mut state = test_state(store);
        state.no_auth = true;
        state
    }
}
