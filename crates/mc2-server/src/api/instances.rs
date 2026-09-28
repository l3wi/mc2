//! Instance endpoints: list, network status, exec, logs.

use crate::api::{ApiError, ApiResult};
use crate::limits::SSE_KEEPALIVE_INTERVAL;
use crate::AppState;
use axum::extract::{Path, Query, State};
use axum::response::sse::{Event, KeepAlive};
use axum::response::{IntoResponse, Response, Sse};
use axum::Json;
use futures::stream::{self, StreamExt};
use serde::Deserialize;
use serde_json::json;

#[derive(Debug, Deserialize)]
pub struct ExecBody {
    pub cmd: Vec<String>,
    /// Raw stdin bytes, base64-encoded (`mc2_api::exec::decode_bytes`).
    #[serde(default, rename = "stdinBase64")]
    pub stdin_base64: Option<String>,
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
) -> ApiResult<Vec<mc2_store::InstanceRecord>> {
    crate::apply::list_instance_views(state.store.clone())
        .await
        .map(Json)
        .map_err(ApiError::store)
}

/// Observed network status for one instance (reported by the reconcile loop).
///
/// Each observed `expose` is annotated with its owning `stack/service` — the
/// server-wide owner of that port claim.
pub async fn get_instance_network(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<serde_json::Value> {
    let inst = state
        .store
        .get_instance(&id)
        .await
        .map_err(ApiError::store)?
        .ok_or_else(|| ApiError::not_found(format!("instance not found: {id}")))?;
    match state.store.get_instance_network(&id).await {
        Ok(Some(rec)) => {
            let mut observed: serde_json::Value =
                serde_json::from_str(&rec.observed_json).unwrap_or(json!({}));
            let owner = format!("{}/{}", inst.stack, inst.service);
            if let Some(exposes) = observed.get_mut("exposes").and_then(|v| v.as_array_mut()) {
                for ex in exposes {
                    if let Some(obj) = ex.as_object_mut() {
                        obj.insert("owner".into(), json!(owner));
                    }
                }
            }
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
) -> ApiResult<crate::networks::NetworksView> {
    match crate::networks::build_networks_view(state.store.as_ref()).await {
        Ok(view) => Ok(Json(view)),
        Err(e) => Err(ApiError::store(e)),
    }
}

/// Run a command inside the instance's sandbox and return captured output.
///
/// Exempt from the ordinary request deadline (a command can run for minutes).
/// stdin/stdout/stderr are arbitrary bytes, so they travel base64-encoded
/// (`stdinBase64`, `stdoutBase64`, `stderrBase64`); see `mc2_api::exec`.
/// The request body is capped at 16 MiB (`limits::EXEC_BODY_LIMIT_BYTES`), so
/// the largest raw stdin is ~12 MiB once base64 overhead is accounted for.
pub async fn exec_instance(
    State(state): State<AppState>,
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
    let stdin = match body.stdin_base64.as_deref() {
        Some(encoded) => mc2_api::exec::decode_bytes(encoded)
            .map_err(|_| ApiError::bad_request("stdinBase64 is not valid base64"))?,
        None => Vec::new(),
    };
    match state
        .runtime
        .exec_with_output(runtime_id, &body.cmd, &stdin)
        .await
    {
        Ok(out) => Ok(Json(json!({
            "instanceId": id,
            "exitCode": out.exit_code,
            "stdoutBase64": mc2_api::exec::encode_bytes(&out.stdout),
            "stderrBase64": mc2_api::exec::encode_bytes(&out.stderr),
        }))),
        Err(e) => Err(ApiError::internal(e.to_string())),
    }
}

/// Recent sandbox logs (runtime / exec / kernel) for an instance. With
/// `follow=true`, returns an SSE stream (tail snapshot first, then new entries
/// as they arrive).
///
/// The follow stream has **no total timeout** (the route is not wrapped in the
/// request deadline) and sends a comment keepalive every
/// [`SSE_KEEPALIVE_INTERVAL`] so an idle client or proxy can tell the stream is
/// still alive.
pub async fn get_instance_logs(
    State(state): State<AppState>,
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
        return match state.runtime.read_logs(runtime_id, params.tail).await {
            Ok(entries) => {
                let list: Vec<serde_json::Value> = entries.iter().map(entry_json).collect();
                Ok(Json(json!({ "instanceId": id, "entries": list })).into_response())
            }
            Err(e) => Err(ApiError::internal(e.to_string())),
        };
    }

    // Follow: tail snapshot (if requested), then resume from its cursor.
    let mut past_events: Vec<Event> = Vec::new();
    let resume = if let Some(n) = params.tail {
        match state.runtime.read_logs(runtime_id, Some(n)).await {
            Ok(entries) => {
                let cursor = entries.last().map(|e| e.cursor.clone());
                for e in &entries {
                    past_events.push(entry_event(e));
                }
                cursor
            }
            Err(e) => return Err(ApiError::internal(e.to_string())),
        }
    } else {
        None
    };
    let live = match state.runtime.log_stream(runtime_id, resume).await {
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
    Ok(Sse::new(entries)
        .keep_alive(KeepAlive::new().interval(SSE_KEEPALIVE_INTERVAL))
        .into_response())
}

fn entry_json(e: &mc2_runtime::LogLine) -> serde_json::Value {
    json!({
        "timestamp": &e.timestamp,
        "source": &e.source,
        "data": String::from_utf8_lossy(&e.data),
    })
}

fn entry_event(e: &mc2_runtime::LogLine) -> Event {
    Event::default().data(entry_json(e).to_string())
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use mc2_store::{MemoryStore, StackPlan, Store};
    use std::sync::Arc;
    use tower::ServiceExt;

    use crate::api::router;
    use crate::api::testing::{test_state_open, MockRuntime};

    #[tokio::test]
    async fn exec_runs_command_and_reports_errors() {
        let store = MemoryStore::new();
        store.init_cluster("").await.unwrap();
        let inst = store
            .commit_stack_plan(&StackPlan::replicas(
                "demo",
                "{}",
                "yaml",
                vec![("web", vec![r#"{}"#.to_string()])],
            ))
            .await
            .unwrap();
        store
            .update_instance_status(&inst[0].id, "Running", Some("demo-web-0"), None)
            .await
            .unwrap();

        let runtime = std::sync::Arc::new(MockRuntime::default());
        let mut state = test_state_open(store.clone());
        state.runtime = runtime.clone();
        let app = router(state);

        // Non-UTF-8 stdin must reach the runtime byte-exact.
        let raw_stdin: Vec<u8> = vec![0x00, 0xff, 0xfe, 0x80, b'o', b'k'];
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/v1/instances/{}/exec", inst[0].id))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&serde_json::json!({
                            "cmd": ["/bin/sh", "-c", "cat"],
                            "stdinBase64": mc2_api::exec::encode_bytes(&raw_stdin),
                        }))
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["exitCode"], 7);
        assert_eq!(
            mc2_api::exec::decode_bytes(v["stdoutBase64"].as_str().unwrap()).unwrap(),
            b"hello out"
        );
        // The runtime's invalid-UTF-8 stderr survives the wire format.
        assert_eq!(
            mc2_api::exec::decode_bytes(v["stderrBase64"].as_str().unwrap()).unwrap(),
            vec![0xff, 0xfe]
        );

        let calls = runtime.exec_calls.lock().await;
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].argv, ["/bin/sh", "-c", "cat"]);
        assert_eq!(calls[0].stdin, raw_stdin);
        drop(calls);

        // Malformed base64 → 400, not a 500 or a silently empty stdin.
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/v1/instances/{}/exec", inst[0].id))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&serde_json::json!({
                            "cmd": ["cat"],
                            "stdinBase64": "not base64!!",
                        }))
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);

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

    fn log_line(timestamp: &str, source: &str, data: &str, cursor: &str) -> mc2_runtime::LogLine {
        mc2_runtime::LogLine {
            timestamp: timestamp.into(),
            source: source.into(),
            data: data.as_bytes().to_vec(),
            cursor: cursor.into(),
        }
    }

    /// A store with one Running `demo/web` instance whose runtime id is
    /// `demo-web-0` (the fake runtime ignores it).
    async fn store_with_running_web() -> (Arc<dyn Store>, String) {
        let store = MemoryStore::new();
        store.init_cluster("").await.unwrap();
        let inst = store
            .commit_stack_plan(&StackPlan::replicas(
                "demo",
                "{}",
                "yaml",
                vec![("web", vec![r#"{}"#.to_string()])],
            ))
            .await
            .unwrap();
        store
            .update_instance_status(&inst[0].id, "Running", Some("demo-web-0"), None)
            .await
            .unwrap();
        (store, inst[0].id.clone())
    }

    /// F5: the logs handler reads through `NodeRuntime`, so a fake supplies the
    /// lines and the JSON shape is unchanged.
    #[tokio::test]
    async fn logs_reads_sandbox_entries_through_the_runtime() {
        let (store, instance_id) = store_with_running_web().await;
        let runtime = Arc::new(MockRuntime {
            log_lines: vec![
                log_line(
                    "2026-01-01T00:00:00+00:00",
                    "stdout",
                    "hello from sandbox",
                    "c1",
                ),
                log_line("2026-01-01T00:00:01+00:00", "system", "runtime note", "c2"),
            ],
            ..Default::default()
        });
        let mut state = test_state_open(store);
        state.runtime = runtime.clone();
        let app = router(state);

        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/v1/instances/{instance_id}/logs?tail=5"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let entries = v["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 2, "{v}");
        assert_eq!(entries[0]["timestamp"], "2026-01-01T00:00:00+00:00", "{v}");
        assert_eq!(entries[0]["source"], "stdout", "{v}");
        assert!(
            entries[0]["data"]
                .as_str()
                .unwrap()
                .contains("hello from sandbox"),
            "{v}"
        );
        assert_eq!(entries[1]["source"], "system", "{v}");

        // `tail` is the runtime's: only the newest line survives.
        let res = app
            .oneshot(
                Request::builder()
                    .uri(format!("/v1/instances/{instance_id}/logs?tail=1"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let entries = v["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 1, "{v}");
        assert_eq!(entries[0]["source"], "system", "{v}");
    }

    /// F5: the `follow` SSE path also goes through the runtime — the tail
    /// snapshot is emitted, then the stream resumes from its cursor.
    #[tokio::test]
    async fn logs_follow_streams_the_tail_then_resumes() {
        let (store, instance_id) = store_with_running_web().await;
        let runtime = Arc::new(MockRuntime {
            log_lines: vec![
                log_line("2026-01-01T00:00:00+00:00", "stdout", "first", "c1"),
                log_line("2026-01-01T00:00:01+00:00", "stdout", "second", "c2"),
            ],
            ..Default::default()
        });
        let mut state = test_state_open(store);
        state.runtime = runtime.clone();
        let app = router(state);

        // Tail 1: exactly the newest line, then a resume at the end yields none.
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/v1/instances/{instance_id}/logs?tail=1&follow=true"
                    ))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        let text = String::from_utf8(bytes.to_vec()).unwrap();
        assert_eq!(text.matches("data: ").count(), 1, "{text}");
        assert!(text.contains("second"), "{text}");
        assert!(!text.contains("first"), "{text}");
    }

    #[tokio::test]
    async fn instance_network_view_names_the_port_owner() {
        let store = MemoryStore::new();
        store.init_cluster("").await.unwrap();
        let inst = store
            .commit_stack_plan(&StackPlan::replicas(
                "shop",
                "{}",
                "yaml",
                vec![("db", vec![r#"{}"#.to_string()])],
            ))
            .await
            .unwrap();
        store
            .update_instance_network_observed(
                &inst[0].id,
                "Ready",
                r#"{"exposes":[{"guestPort":5432,"hostPort":41000,"phase":"Ready","message":""}],"edges":[],"message":""}"#,
                None,
            )
            .await
            .unwrap();

        let app = router(test_state_open(store));
        let res = app
            .oneshot(
                Request::builder()
                    .uri(format!("/v1/instances/{}/network", inst[0].id))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["observed"]["exposes"][0]["guestPort"], 5432);
        assert_eq!(
            v["observed"]["exposes"][0]["owner"], "shop/db",
            "each exposed port names its owning stack/service: {v}"
        );
    }
}
