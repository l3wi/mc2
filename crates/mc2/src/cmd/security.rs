//! Security commands: `secret` (set/ls/rm) and `ssh` (keys + instance endpoints).

use crate::cli::{
    ListArgs, OutputFormat, SecretRmArgs, SecretSetArgs, SshInstanceArgs, SshKeyAddArgs,
    SshKeyRmArgs, SshKeyShowArgs, SshOpenArgs,
};
use crate::client::{api_error, operator_get, urlencoding_simple};
use crate::context::Conn;
use anyhow::{bail, Context, Result};

pub(crate) async fn secret_set(args: SecretSetArgs, conn: &Conn) -> Result<()> {
    let value = match args.value {
        Some(v) => v,
        None => {
            use std::io::Read;
            let mut buf = String::new();
            std::io::stdin()
                .read_to_string(&mut buf)
                .context("read secret value from stdin")?;
            buf.trim_end().to_string()
        }
    };
    if value.is_empty() {
        bail!("secret value is empty (pass --value or stdin)");
    }
    let url = format!(
        "{}/v1/secrets/{}",
        conn.url.trim_end_matches('/'),
        urlencoding_simple(&args.name)
    );
    let client = reqwest::Client::new();
    let mut req = client.put(&url);
    if let Some(t) = conn.token.as_deref().filter(|s| !s.is_empty()) {
        req = req.bearer_auth(t);
    }
    let res = req
        .json(&serde_json::json!({ "value": value }))
        .send()
        .await
        .with_context(|| format!("PUT {url}"))?;
    let status = res.status();
    let body = res.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(api_error("secret set", status, &body));
    }
    // Body is metadata only (no value)
    println!("{body}");
    Ok(())
}

pub(crate) async fn secret_ls(args: ListArgs, conn: &Conn) -> Result<()> {
    let url = format!("{}/v1/secrets", conn.url.trim_end_matches('/'));
    let client = reqwest::Client::new();
    let res = operator_get(&client, &url, conn.token.as_deref())
        .send()
        .await
        .with_context(|| format!("GET {url}"))?;
    let status = res.status();
    let body = res.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(api_error("secret ls", status, &body));
    }
    if matches!(args.output, OutputFormat::Json) {
        println!("{body}");
        return Ok(());
    }
    let list: Vec<mc2_store::SecretMeta> =
        serde_json::from_str(&body).with_context(|| format!("parse: {body}"))?;
    if list.is_empty() {
        println!("No secrets.");
        return Ok(());
    }
    println!("{:<32} UPDATED", "NAME");
    for s in list {
        println!("{:<32} {}", s.name, s.updated_at);
    }
    Ok(())
}

pub(crate) async fn secret_rm(args: SecretRmArgs, conn: &Conn) -> Result<()> {
    let url = format!(
        "{}/v1/secrets/{}",
        conn.url.trim_end_matches('/'),
        urlencoding_simple(&args.name)
    );
    let client = reqwest::Client::new();
    let mut req = client.delete(&url);
    if let Some(t) = conn.token.as_deref().filter(|s| !s.is_empty()) {
        req = req.bearer_auth(t);
    }
    let res = req.send().await.with_context(|| format!("DELETE {url}"))?;
    let status = res.status();
    if status == reqwest::StatusCode::NO_CONTENT || status.is_success() {
        println!("deleted {}", args.name);
        return Ok(());
    }
    let body = res.text().await.unwrap_or_default();
    Err(api_error("secret rm", status, &body))
}

pub(crate) async fn ssh_key_add(args: SshKeyAddArgs, conn: &Conn) -> Result<()> {
    let public_key = if let Some(k) = args.key {
        k
    } else if let Some(path) = args.file {
        std::fs::read_to_string(&path).with_context(|| format!("read {path}"))?
    } else {
        bail!("pass --key or --file");
    };
    let url = format!(
        "{}/v1/ssh/keys/{}",
        conn.url.trim_end_matches('/'),
        urlencoding_simple(&args.name)
    );
    let client = reqwest::Client::new();
    let mut req = client.put(&url).json(&serde_json::json!({
        "publicKey": public_key.trim()
    }));
    if let Some(t) = conn.token.as_deref().filter(|s| !s.is_empty()) {
        req = req.bearer_auth(t);
    }
    let res = req.send().await.context("PUT ssh key")?;
    let status = res.status();
    let body = res.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(api_error("ssh key add", status, &body));
    }
    println!("{body}");
    Ok(())
}

pub(crate) async fn ssh_key_ls(args: ListArgs, conn: &Conn) -> Result<()> {
    let url = format!("{}/v1/ssh/keys", conn.url.trim_end_matches('/'));
    let client = reqwest::Client::new();
    let mut req = client.get(&url);
    if let Some(t) = conn.token.as_deref().filter(|s| !s.is_empty()) {
        req = req.bearer_auth(t);
    }
    let res = req.send().await.context("GET ssh keys")?;
    let status = res.status();
    let body = res.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(api_error("ssh key ls", status, &body));
    }
    if matches!(args.output, OutputFormat::Json) {
        println!("{body}");
        return Ok(());
    }
    let keys: Vec<mc2_store::SshAuthorizedKey> =
        serde_json::from_str(&body).with_context(|| format!("parse: {body}"))?;
    if keys.is_empty() {
        println!("No authorized keys.");
        return Ok(());
    }
    println!("{:<24} {:<52} NAME", "FINGERPRINT", "PUBLIC KEY");
    for k in keys {
        println!("{:<24} {:<52} {}", k.fingerprint, k.public_key, k.name);
    }
    Ok(())
}

pub(crate) async fn ssh_key_rm(args: SshKeyRmArgs, conn: &Conn) -> Result<()> {
    let url = format!(
        "{}/v1/ssh/keys/{}",
        conn.url.trim_end_matches('/'),
        urlencoding_simple(&args.name)
    );
    let client = reqwest::Client::new();
    let mut req = client.delete(&url);
    if let Some(t) = conn.token.as_deref().filter(|s| !s.is_empty()) {
        req = req.bearer_auth(t);
    }
    let res = req.send().await.context("DELETE ssh key")?;
    let status = res.status();
    if status == reqwest::StatusCode::NO_CONTENT || status.is_success() {
        println!("deleted {}", args.name);
        return Ok(());
    }
    let body = res.text().await.unwrap_or_default();
    Err(api_error("ssh key rm", status, &body))
}

/// Show one authorized public key.
pub(crate) async fn ssh_key_show(args: SshKeyShowArgs, conn: &Conn) -> Result<()> {
    let url = format!(
        "{}/v1/ssh/keys/{}",
        conn.url.trim_end_matches('/'),
        urlencoding_simple(&args.name)
    );
    let client = reqwest::Client::new();
    let mut req = client.get(&url);
    if let Some(t) = conn.token.as_deref().filter(|s| !s.is_empty()) {
        req = req.bearer_auth(t);
    }
    let res = req.send().await.context("GET ssh key")?;
    let status = res.status();
    let body = res.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(api_error("ssh key show", status, &body));
    }
    println!("{body}");
    Ok(())
}

pub(crate) async fn ssh_endpoints_ls(args: ListArgs, conn: &Conn) -> Result<()> {
    let url = format!("{}/v1/ssh/endpoints", conn.url.trim_end_matches('/'));
    let client = reqwest::Client::new();
    let mut req = client.get(&url);
    if let Some(t) = conn.token.as_deref().filter(|s| !s.is_empty()) {
        req = req.bearer_auth(t);
    }
    let res = req.send().await.context("GET ssh endpoints")?;
    let status = res.status();
    let body = res.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(api_error("ssh ls", status, &body));
    }
    if matches!(args.output, OutputFormat::Json) {
        println!("{body}");
        return Ok(());
    }
    // Shape: { "endpoints": [...] } from list_ssh_endpoints.
    let v: serde_json::Value =
        serde_json::from_str(&body).with_context(|| format!("parse: {body}"))?;
    let endpoints: Vec<serde_json::Value> = v
        .get("endpoints")
        .and_then(|e| e.as_array())
        .cloned()
        .unwrap_or_default();
    if endpoints.is_empty() {
        println!("No open SSH endpoints.");
        return Ok(());
    }
    println!(
        "{:<18} {:<16} {:<8} {:<22} BIND:PORT",
        "INSTANCE", "STACK/SERVICE", "PHASE", "NODE"
    );
    for e in endpoints {
        let bind = e["bind"].as_str().unwrap_or("-");
        let port = e["port"].as_u64().unwrap_or(0);
        println!(
            "{:<18} {:<16} {:<8} {:<22} {}:{}",
            e["instanceId"]
                .as_str()
                .unwrap_or("-")
                .chars()
                .take(18)
                .collect::<String>(),
            format!(
                "{}/{}",
                e["stack"].as_str().unwrap_or("-"),
                e["service"].as_str().unwrap_or("-")
            ),
            e["phase"].as_str().unwrap_or("-"),
            e["nodeName"].as_str().unwrap_or("-"),
            bind,
            port
        );
    }
    Ok(())
}

pub(crate) async fn ssh_instance_show(args: SshInstanceArgs, conn: &Conn) -> Result<()> {
    let url = format!(
        "{}/v1/instances/{}/ssh",
        conn.url.trim_end_matches('/'),
        urlencoding_simple(&args.id)
    );
    let client = reqwest::Client::new();
    let mut req = client.get(&url);
    if let Some(t) = conn.token.as_deref().filter(|s| !s.is_empty()) {
        req = req.bearer_auth(t);
    }
    let res = req.send().await.context("GET instance ssh")?;
    let status = res.status();
    let body = res.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(api_error("ssh show", status, &body));
    }
    println!("{body}");
    Ok(())
}

pub(crate) async fn ssh_instance_open(args: SshOpenArgs, conn: &Conn) -> Result<()> {
    let url = format!(
        "{}/v1/instances/{}/ssh",
        conn.url.trim_end_matches('/'),
        urlencoding_simple(&args.id)
    );
    let client = reqwest::Client::new();
    let mut req = client.put(&url).json(&serde_json::json!({
        "enabled": true,
        "bind": args.bind,
        "port": args.port,
        "authorizedKeys": args.keys,
    }));
    if let Some(t) = conn.token.as_deref().filter(|s| !s.is_empty()) {
        req = req.bearer_auth(t);
    }
    let res = req.send().await.context("PUT instance ssh open")?;
    let status = res.status();
    let body = res.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(api_error("ssh open", status, &body));
    }
    println!("{body}");
    Ok(())
}

pub(crate) async fn ssh_instance_close(args: SshInstanceArgs, conn: &Conn) -> Result<()> {
    let url = format!(
        "{}/v1/instances/{}/ssh",
        conn.url.trim_end_matches('/'),
        urlencoding_simple(&args.id)
    );
    let client = reqwest::Client::new();
    let mut req = client.put(&url).json(&serde_json::json!({
        "enabled": false,
        "authorizedKeys": []
    }));
    if let Some(t) = conn.token.as_deref().filter(|s| !s.is_empty()) {
        req = req.bearer_auth(t);
    }
    let res = req.send().await.context("PUT instance ssh close")?;
    let status = res.status();
    let body = res.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(api_error("ssh close", status, &body));
    }
    println!("{body}");
    Ok(())
}
