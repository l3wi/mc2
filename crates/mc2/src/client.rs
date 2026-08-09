//! REST client plumbing shared by the operator command handlers.

use crate::context::Conn;
use anyhow::{bail, Context, Result};

/// Unified non-2xx error: prefer the server's `error` field over raw JSON.
pub fn api_error(op: &str, status: reqwest::StatusCode, body: &str) -> anyhow::Error {
    let detail = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(str::to_string))
        .unwrap_or_else(|| body.to_string());
    anyhow::anyhow!("{op} failed: {status}: {detail}")
}

pub fn operator_get(
    client: &reqwest::Client,
    url: &str,
    token: Option<&str>,
) -> reqwest::RequestBuilder {
    let mut req = client.get(url);
    if let Some(t) = token.filter(|s| !s.is_empty()) {
        req = req.bearer_auth(t);
    }
    req
}

pub fn operator_post(
    client: &reqwest::Client,
    url: &str,
    token: Option<&str>,
) -> reqwest::RequestBuilder {
    let mut req = client.post(url);
    if let Some(t) = token.filter(|s| !s.is_empty()) {
        req = req.bearer_auth(t);
    }
    req
}

pub fn urlencoding_simple(s: &str) -> String {
    // Secret names are identifiers; keep path-safe.
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.' {
                c.to_string()
            } else {
                format!("%{:02X}", c as u8)
            }
        })
        .collect()
}

/// Resolve `<stack>/<service>/<ordinal>` to an instance id; pass UUIDs through.
pub async fn resolve_instance_id(conn: &Conn, id_or_ref: &str) -> Result<String> {
    if !id_or_ref.contains('/') {
        return Ok(id_or_ref.to_string());
    }
    let parts: Vec<&str> = id_or_ref.splitn(3, '/').collect();
    if parts.len() != 3 {
        bail!("instance ref must be <stack>/<service>/<ordinal>, got {id_or_ref}");
    }
    let (stack, service, ordinal) = (parts[0], parts[1], parts[2]);
    let ordinal: u32 = ordinal
        .parse()
        .with_context(|| format!("ordinal must be a number, got {ordinal}"))?;
    let url = format!("{}/v1/instances", conn.url.trim_end_matches('/'));
    let client = reqwest::Client::new();
    let res = operator_get(&client, &url, conn.token.as_deref())
        .send()
        .await
        .with_context(|| format!("GET {url}"))?;
    let body = res.text().await.unwrap_or_default();
    let instances: Vec<mc2_store::InstanceRecord> =
        serde_json::from_str(&body).with_context(|| format!("parse: {body}"))?;
    instances
        .iter()
        .find(|i| i.stack == stack && i.service == service && i.ordinal == ordinal)
        .map(|i| i.id.clone())
        .ok_or_else(|| anyhow::anyhow!("no instance {stack}/{service}/{ordinal}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::ClientMode;

    #[test]
    fn api_error_prefers_server_error_field() {
        let e = api_error(
            "network",
            reqwest::StatusCode::NOT_FOUND,
            r#"{"error":"no network status"}"#,
        );
        assert_eq!(
            e.to_string(),
            "network failed: 404 Not Found: no network status"
        );
    }

    #[test]
    fn api_error_falls_back_to_raw_body() {
        let e = api_error("ps", reqwest::StatusCode::INTERNAL_SERVER_ERROR, "boom");
        assert!(e.to_string().contains("boom"));
    }

    #[tokio::test]
    async fn uuid_refs_pass_through_without_lookup() {
        let conn = Conn {
            url: "http://127.0.0.1:1".into(),
            token: None,
            context: None,
            mode: ClientMode::Local,
        };
        // No slash → no network call, returned verbatim.
        let id = resolve_instance_id(&conn, "6e6a2d3f-b4dc-4c09-a317-4aac91ff0c99")
            .await
            .unwrap();
        assert_eq!(id, "6e6a2d3f-b4dc-4c09-a317-4aac91ff0c99");
    }
}
