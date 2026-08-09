//! Instance endpoints: list, network status, exec, logs.

use crate::api::{ApiError, ApiResult};
use crate::AppState;
use axum::extract::{Path, Query, State};
use axum::response::sse::Event;
use axum::response::{IntoResponse, Response, Sse};
use axum::Json;
use futures::stream::{self, StreamExt};
use serde::Deserialize;
use serde_json::json;

#[derive(Debug, Deserialize)]
pub struct ExecBody {
    pub cmd: Vec<String>,
    #[serde(default)]
    pub stdin: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct LogsQuery {
    #[serde(default)]
    pub tail: Option<usize>,
    #[serde(default)]
    pub follow: bool,
}

pub async fn list_instances(
    State(state): State<AppState>,
    _auth: crate::auth::AuthUser,
) -> ApiResult<Vec<mc2_store::InstanceRecord>> {
    crate::apply::list_instance_views(state.store.clone())
        .await
        .map(Json)
        .map_err(ApiError::store)
}

/// Observed network status for one instance (agent-reported).
pub async fn get_instance_network(
    State(state): State<AppState>,
    _auth: crate::auth::AuthUser,
    Path(id): Path<String>,
) -> ApiResult<serde_json::Value> {
    match state.store.get_instance_network(&id).await {
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
        Ok(None) => Err(ApiError::not_found("no network status for instance")),
        Err(e) => Err(ApiError::store(e)),
    }
}

/// Server-wide network membership summary (default + named networks).
pub async fn list_networks(
    State(state): State<AppState>,
    _auth: crate::auth::AuthUser,
) -> ApiResult<crate::networks::NetworksView> {
    match crate::networks::build_networks_view(state.store.as_ref()).await {
        Ok(view) => Ok(Json(view)),
        Err(e) => Err(ApiError::store(e)),
    }
}

/// Run a command inside the instance's sandbox and return captured output.
pub async fn exec_instance(
    State(state): State<AppState>,
    _auth: crate::auth::AuthUser,
    Path(id): Path<String>,
    Json(body): Json<ExecBody>,
) -> ApiResult<serde_json::Value> {
    if body.cmd.is_empty() {
        return Err(ApiError::bad_request("cmd must not be empty"));
    }
    let inst = state
        .store
        .get_instance(&id)
        .await
        .map_err(ApiError::store)?;
    let Some(inst) = inst else {
        return Err(ApiError::not_found(format!("instance not found: {id}")));
    };
    let Some(runtime_id) = inst.runtime_id.as_deref() else {
        return Err(ApiError::bad_request(format!(
            "instance {id} has no sandbox runtime yet"
        )));
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
        Err(e) => Err(ApiError::internal(e.to_string())),
    }
}

/// Recent sandbox logs (runtime / exec / kernel) for an instance. With
/// `follow=true`, returns an SSE stream (tail snapshot first, then new entries
/// as they arrive).
pub async fn get_instance_logs(
    State(state): State<AppState>,
    _auth: crate::auth::AuthUser,
    Path(id): Path<String>,
    Query(params): Query<LogsQuery>,
) -> Result<Response, ApiError> {
    let inst = state
        .store
        .get_instance(&id)
        .await
        .map_err(ApiError::store)?;
    let Some(inst) = inst else {
        return Err(ApiError::not_found(format!("instance not found: {id}")));
    };
    let Some(runtime_id) = inst.runtime_id.as_deref() else {
        return Err(ApiError::bad_request(format!(
            "instance {id} has no sandbox runtime yet"
        )));
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
            Err(e) => Err(ApiError::internal(e.to_string())),
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
            Err(e) => return Err(ApiError::internal(e.to_string())),
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
        Err(e) => return Err(ApiError::internal(e.to_string())),
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

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use mc2_store::{MemoryStore, Store};
    use tower::ServiceExt;

    use crate::api::router;
    use crate::api::testing::{test_state, MockRuntime};

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

        let mut state = test_state(store.clone());
        state.runtime = std::sync::Arc::new(MockRuntime);
        let app = router(state);

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
}
