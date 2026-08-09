//! Stack lifecycle commands: `up`, `down` (alias `rm`), `config`.

use crate::cli::{ConfigArgs, DownArgs, UpArgs};
use crate::client::{api_error, operator_post, urlencoding_simple};
use crate::context::Conn;
use anyhow::{Context, Result};

/// Bring up a stack: publish desired state and converge (idempotent).
pub(crate) async fn up_cmd(args: UpArgs, conn: &Conn) -> Result<()> {
    // Token optional when server was bootstrapped with --no-auth.
    let raw = std::fs::read_to_string(&args.file).with_context(|| format!("read {}", args.file))?;
    let yaml = fill_stack_defaults(&raw, &args.file);
    let url = format!("{}/v1/stacks:apply", conn.url.trim_end_matches('/'));
    let client = reqwest::Client::new();
    let res = operator_post(&client, &url, conn.token.as_deref())
        .json(&serde_json::json!({ "yaml": yaml }))
        .send()
        .await
        .with_context(|| format!("POST {url}"))?;
    let status = res.status();
    let body = res.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(api_error("up", status, &body));
    }
    println!("{body}");
    print_ssh_endpoints(&body);
    Ok(())
}

/// After `mc2 up`, print the declared SSH front ends and their ingress
/// entrypoints (auto host ports resolve on first reconcile → `mc2 ssh ls`).
fn print_ssh_endpoints(body: &str) {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(body) else {
        return;
    };
    let Some(ssh) = v["ssh"].as_array() else {
        return;
    };
    if ssh.is_empty() {
        return;
    }
    println!("ssh:");
    for e in ssh {
        let port = e["port"]
            .as_u64()
            .map(|p| p.to_string())
            .unwrap_or_else(|| "auto".into());
        let replicas = e["replicas"].as_u64().unwrap_or(1);
        println!(
            "  {}{}  {}:{}",
            e["service"].as_str().unwrap_or("-"),
            if replicas > 1 {
                format!(" ×{replicas}")
            } else {
                String::new()
            },
            e["bind"].as_str().unwrap_or("127.0.0.1"),
            port
        );
        if let Some(ep) = e["entrypoint"].as_str().filter(|s| !s.is_empty()) {
            println!("    ingress entrypoint: {ep}");
        }
        if e["port"].as_u64().is_none() {
            println!("    host port auto — see `mc2 ssh ls` after reconcile");
        }
    }
}

/// Tear down a stack; `--volumes` also deletes its named volumes.
pub(crate) async fn down_cmd(args: DownArgs, conn: &Conn) -> Result<()> {
    let mut url = format!(
        "{}/v1/stacks/{}",
        conn.url.trim_end_matches('/'),
        urlencoding_simple(&args.stack)
    );
    if args.volumes {
        url.push_str("?volumes=true");
    }
    let client = reqwest::Client::new();
    let mut req = client.delete(&url);
    if let Some(t) = conn.token.as_deref().filter(|s| !s.is_empty()) {
        req = req.bearer_auth(t);
    }
    let res = req.send().await.with_context(|| format!("DELETE {url}"))?;
    let status = res.status();
    if status == reqwest::StatusCode::NO_CONTENT || status.is_success() {
        if args.volumes {
            println!("stack '{}' down (volumes deleted)", args.stack);
        } else {
            println!("stack '{}' down", args.stack);
        }
        return Ok(());
    }
    let body = res.text().await.unwrap_or_default();
    Err(api_error("down", status, &body))
}

/// Validate and print the normalized stack config (what `mc2 up` would send).
pub(crate) fn config_cmd(args: ConfigArgs) -> Result<()> {
    let raw = std::fs::read_to_string(&args.file).with_context(|| format!("read {}", args.file))?;
    let yaml = fill_stack_defaults(&raw, &args.file);
    mc2_api::parse_stack_yaml(&yaml).map_err(|e| anyhow::anyhow!("invalid stack: {e}"))?;
    print!("{yaml}");
    Ok(())
}

/// Fill omissible compose boilerplate for local DX: a top-level `name:` when
/// missing (derived from the file name). Only missing fields are filled;
/// present values are never rewritten. Note: filling re-serializes the
/// document, dropping YAML comments — complete documents are returned verbatim.
fn fill_stack_defaults(raw: &str, file_path: &str) -> String {
    let Ok(mut doc) = serde_yaml::from_str::<serde_yaml::Value>(raw) else {
        return raw.to_string();
    };
    let Some(map) = doc.as_mapping_mut() else {
        return raw.to_string();
    };
    let mut changed = false;
    let name_key = serde_yaml::Value::String("name".into());
    let has_name = map
        .get(&name_key)
        .and_then(|v| v.as_str())
        .is_some_and(|s| !s.trim().is_empty());
    if !has_name {
        map.insert(
            name_key,
            serde_yaml::Value::String(stack_name_from_path(file_path)),
        );
        changed = true;
    }
    if changed {
        serde_yaml::to_string(&doc).unwrap_or_else(|_| raw.to_string())
    } else {
        raw.to_string()
    }
}

/// Stack name from the file path: the sanitized stem, or — when the stem is
/// the generic `stack` (e.g. `examples/01-hello-service/stack.yaml`) — the
/// sanitized parent directory name. Result is lowercase, volume-safe charset,
/// no `--` (the volume namespace separator).
fn stack_name_from_path(file_path: &str) -> String {
    let path = std::path::Path::new(file_path);
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("stack");
    let source = if stem.eq_ignore_ascii_case("stack") {
        path.parent()
            .and_then(|p| p.file_name())
            .and_then(|s| s.to_str())
            .filter(|s| !s.is_empty())
            .unwrap_or(stem)
    } else {
        stem
    };
    let sanitized = sanitize_stack_name(source);
    if sanitized.is_empty() {
        "stack".into()
    } else {
        sanitized
    }
}

/// Lowercase, volume-safe charset; collapse `--` and trim edge dashes.
fn sanitize_stack_name(source: &str) -> String {
    let mut out: String = source
        .to_ascii_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '-'
            }
        })
        .collect();
    while out.contains("--") {
        out = out.replace("--", "-");
    }
    out.trim_matches('-').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minimal_stack_gets_name_and_passes_validation() {
        let raw = "services:\n  web:\n    image: alpine:3.20\n";
        let filled = fill_stack_defaults(raw, "demo.yaml");
        let doc = mc2_api::parse_stack_yaml(&filled).unwrap();
        assert_eq!(doc.name, "demo");
    }

    #[test]
    fn complete_compose_stack_passes_through_verbatim() {
        let raw = "name: x\nservices:\n  web:\n    image: alpine:3.20\n";
        assert_eq!(fill_stack_defaults(raw, "whatever.yaml"), raw);
    }

    #[test]
    fn name_missing_gets_file_stem() {
        let raw = "services:\n  web:\n    image: alpine:3.20\n";
        let doc = mc2_api::parse_stack_yaml(&fill_stack_defaults(raw, "My Stack!.yaml")).unwrap();
        assert_eq!(doc.name, "my-stack");
    }

    #[test]
    fn generic_stack_yaml_takes_parent_dir_name() {
        let raw = "services:\n  web:\n    image: alpine:3.20\n";
        let doc = mc2_api::parse_stack_yaml(&fill_stack_defaults(
            raw,
            "examples/01-hello-service/stack.yaml",
        ))
        .unwrap();
        assert_eq!(doc.name, "01-hello-service");
    }

    #[test]
    fn old_k8s_form_is_rejected_at_parse() {
        // Canonical compose parser: unknown k8s keys are rejected outright.
        let raw = "apiVersion: mc2/v1\nkind: Stack\nmetadata:\n  name: x\nservices:\n  web:\n    image: alpine\n";
        let filled = fill_stack_defaults(raw, "x.yaml");
        let err = mc2_api::parse_stack_yaml(&filled).unwrap_err();
        assert!(err.contains("unknown field"), "{err}");
    }

    #[test]
    fn invalid_yaml_passes_through() {
        let raw = "not: [valid";
        assert_eq!(fill_stack_defaults(raw, "x.yaml"), raw);
    }
}
