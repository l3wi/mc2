//! REST client plumbing shared by the operator command handlers.

use crate::context::Conn;
use anyhow::{bail, Context, Result};
use percent_encoding::{utf8_percent_encode, AsciiSet, NON_ALPHANUMERIC};

/// Unified non-2xx error: prefer the server's JSON `error` (plus `message`)
/// over the raw body, so responses like the auth middleware's
/// `{"error":"unauthorized","message":"missing or invalid bearer token"}`
/// read as `…: 401 Unauthorized: unauthorized: missing or invalid bearer token`.
pub fn api_error(op: &str, status: reqwest::StatusCode, body: &str) -> anyhow::Error {
    let detail = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .map(|v| {
            let field = |k: &str| v.get(k).and_then(|e| e.as_str()).filter(|s| !s.is_empty());
            match (field("error"), field("message")) {
                (Some(e), Some(m)) if e != m => format!("{e}: {m}"),
                (Some(e), _) => e.to_string(),
                (None, Some(m)) => m.to_string(),
                (None, None) => body.to_string(),
            }
        })
        .unwrap_or_else(|| body.to_string());
    anyhow::anyhow!("{op} failed: {status}: {detail}")
}

/// Non-2xx → [`api_error`]; 2xx → `Ok(())`. The pure half of [`checked_body`],
/// usable wherever the status and body are already in hand.
pub fn check_status(op: &str, status: reqwest::StatusCode, body: &str) -> Result<()> {
    if status.is_success() {
        Ok(())
    } else {
        Err(api_error(op, status, body))
    }
}

/// Read a response body, mapping a non-2xx status to a readable error.
///
/// Every CLI request path goes through this (or [`check_status`]) so an error
/// status — 401 in particular — surfaces the server's message instead of
/// failing later on a JSON parse of the error body.
pub async fn checked_body(op: &str, res: reqwest::Response) -> Result<String> {
    let status = res.status();
    let body = res.text().await.unwrap_or_default();
    check_status(op, status, &body)?;
    Ok(body)
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

/// Characters left unencoded in a path segment: the identifier set MC2 names
/// use (`[A-Za-z0-9_.-]`). Everything else — `/`, space, and every non-ASCII
/// byte — is percent-encoded from its UTF-8 bytes (`café` → `caf%C3%A9`, not
/// the truncated `caf%E9` bug).
const PATH_SEGMENT_SAFE: &AsciiSet = &NON_ALPHANUMERIC.remove(b'-').remove(b'.').remove(b'_');

/// Percent-encode one URL path segment: names, ids and refs that reach the
/// server inside `/v1/...` paths.
pub fn urlencoding_simple(s: &str) -> String {
    utf8_percent_encode(s, PATH_SEGMENT_SAFE).to_string()
}

/// `{base}/v1/instances/{id}{suffix}` with a single percent-encoded id segment.
///
/// One builder for every instance-scoped request path (`/exec`, `/logs`,
/// `/network`, `/ssh`), so an id can never be interpolated unencoded.
pub fn instance_url(base: &str, id: &str, suffix: &str) -> String {
    format!(
        "{}/v1/instances/{}{suffix}",
        base.trim_end_matches('/'),
        urlencoding_simple(id)
    )
}

/// An instance reference: `<stack>/<service>/<ordinal>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InstanceRef<'a> {
    pub stack: &'a str,
    pub service: &'a str,
    pub ordinal: u32,
}

/// Parse `<stack>/<service>/<ordinal>`; `None` for a bare id/UUID (passed
/// through unchanged, no lookup needed).
pub fn parse_instance_ref(id_or_ref: &str) -> Result<Option<InstanceRef<'_>>> {
    if !id_or_ref.contains('/') {
        return Ok(None);
    }
    let parts: Vec<&str> = id_or_ref.splitn(3, '/').collect();
    if parts.len() != 3 {
        bail!("instance ref must be <stack>/<service>/<ordinal>, got {id_or_ref}");
    }
    let ordinal = parts[2]
        .parse::<u32>()
        .with_context(|| format!("ordinal must be a number, got {}", parts[2]))?;
    Ok(Some(InstanceRef {
        stack: parts[0],
        service: parts[1],
        ordinal,
    }))
}

/// Resolve `<stack>/<service>/<ordinal>` to an instance id; pass UUIDs through.
pub async fn resolve_instance_id(conn: &Conn, id_or_ref: &str) -> Result<String> {
    let Some(r) = parse_instance_ref(id_or_ref)? else {
        return Ok(id_or_ref.to_string());
    };
    let url = format!("{}/v1/instances", conn.url.trim_end_matches('/'));
    let client = reqwest::Client::new();
    let res = operator_get(&client, &url, conn.token.as_deref())
        .send()
        .await
        .with_context(|| format!("GET {url}"))?;
    let body = checked_body("instance lookup", res).await?;
    let instances: Vec<mc2_store::InstanceRecord> =
        serde_json::from_str(&body).with_context(|| format!("parse: {body}"))?;
    instances
        .iter()
        .find(|i| i.stack == r.stack && i.service == r.service && i.ordinal == r.ordinal)
        .map(|i| i.id.clone())
        .ok_or_else(|| anyhow::anyhow!("no instance {}/{}/{}", r.stack, r.service, r.ordinal))
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

    #[test]
    fn api_error_includes_json_message() {
        // The auth middleware's 401 body: both `error` and `message`.
        let e = api_error(
            "ps",
            reqwest::StatusCode::UNAUTHORIZED,
            r#"{"error":"unauthorized","message":"missing or invalid bearer token"}"#,
        );
        assert_eq!(
            e.to_string(),
            "ps failed: 401 Unauthorized: unauthorized: missing or invalid bearer token"
        );
    }

    #[test]
    fn check_status_maps_401_json_body_to_readable_error() {
        let err = check_status(
            "exec",
            reqwest::StatusCode::UNAUTHORIZED,
            r#"{"error":"unauthorized","message":"missing or invalid bearer token"}"#,
        )
        .unwrap_err();
        assert_eq!(
            err.to_string(),
            "exec failed: 401 Unauthorized: unauthorized: missing or invalid bearer token"
        );
        // A 401 with a non-JSON body still carries the status and the body.
        let err = check_status("exec", reqwest::StatusCode::UNAUTHORIZED, "nope").unwrap_err();
        assert_eq!(err.to_string(), "exec failed: 401 Unauthorized: nope");
        // 2xx passes.
        assert!(check_status("exec", reqwest::StatusCode::OK, "{}").is_ok());
        assert!(check_status("rm", reqwest::StatusCode::NO_CONTENT, "").is_ok());
    }

    #[test]
    fn urlencoding_encodes_utf8_bytes_and_separators() {
        // Non-ASCII is encoded from its UTF-8 bytes, not truncated to one.
        assert_eq!(urlencoding_simple("café"), "caf%C3%A9");
        // `/` must never survive: the id is a single path segment.
        assert_eq!(urlencoding_simple("a/b"), "a%2Fb");
        assert_eq!(urlencoding_simple("a b"), "a%20b");
        assert_eq!(urlencoding_simple("naïve name"), "na%C3%AFve%20name");
        // The identifier set is left verbatim.
        assert_eq!(
            urlencoding_simple("6e6a2d3f-b4dc-4c09-a317-4aac91ff0c99"),
            "6e6a2d3f-b4dc-4c09-a317-4aac91ff0c99"
        );
        assert_eq!(urlencoding_simple("volume_data.1"), "volume_data.1");
        // `%` is itself encoded, so the result round-trips.
        assert_eq!(urlencoding_simple("100%"), "100%25");
    }

    #[test]
    fn instance_url_builds_an_encoded_path_segment() {
        assert_eq!(
            instance_url("http://127.0.0.1:7443/", "café/x", "/ssh"),
            "http://127.0.0.1:7443/v1/instances/caf%C3%A9%2Fx/ssh"
        );
        assert_eq!(
            instance_url("http://h:1", "abc", ""),
            "http://h:1/v1/instances/abc"
        );
    }

    #[test]
    fn parse_instance_ref_accepts_refs_and_rejects_malformed() {
        let r = parse_instance_ref("shop/web/0").unwrap().unwrap();
        assert_eq!(
            (r.stack, r.service, r.ordinal),
            ("shop", "web", 0),
            "three-part ref"
        );
        // A bare id/UUID (no slash) is passed through without a lookup.
        assert!(parse_instance_ref("6e6a2d3f").unwrap().is_none());
        // Wrong arity and a non-numeric ordinal are refusals, not lookups.
        assert!(parse_instance_ref("shop/web").is_err());
        assert!(parse_instance_ref("shop/web/x").is_err());
        assert!(parse_instance_ref("shop/web/0/extra").is_err());
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
