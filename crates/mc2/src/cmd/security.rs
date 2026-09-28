//! Security commands: `secret` (set/ls/rm) and `ssh` (keys + instance endpoints).

use crate::cli::{
    ListArgs, OutputFormat, SecretRmArgs, SecretSetArgs, SshInstanceArgs, SshKeyAddArgs,
    SshKeyRmArgs, SshKeyShowArgs, SshOpenArgs,
};
use crate::client::{
    checked_body, instance_url, operator_get, resolve_instance_id, urlencoding_simple,
};
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
    // Body is metadata only (no value)
    println!("{}", checked_body("secret set", res).await?);
    Ok(())
}

pub(crate) async fn secret_ls(args: ListArgs, conn: &Conn) -> Result<()> {
    let url = format!("{}/v1/secrets", conn.url.trim_end_matches('/'));
    let client = reqwest::Client::new();
    let res = operator_get(&client, &url, conn.token.as_deref())
        .send()
        .await
        .with_context(|| format!("GET {url}"))?;
    let body = checked_body("secret ls", res).await?;
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
    let mut t = crate::table::Table::new().header(["NAME", "UPDATED"]);
    for s in list {
        t = t.row([s.name, s.updated_at]);
    }
    print!("{}", t.render());
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
    checked_body("secret rm", res).await?;
    println!("deleted {}", args.name);
    Ok(())
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
    println!("{}", checked_body("ssh key add", res).await?);
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
    let body = checked_body("ssh key ls", res).await?;
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
    let mut t = crate::table::Table::new().header(["FINGERPRINT", "PUBLIC KEY", "NAME"]);
    for k in keys {
        t = t.row([k.fingerprint, k.public_key, k.name]);
    }
    print!("{}", t.render());
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
    checked_body("ssh key rm", res).await?;
    println!("deleted {}", args.name);
    Ok(())
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
    println!("{}", checked_body("ssh key show", res).await?);
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
    let body = checked_body("ssh ls", res).await?;
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
    // REF is the copyable `<stack>/<service>/<ordinal>` form every instance
    // command accepts; the full id is printed untruncated alongside it.
    let mut t =
        crate::table::Table::new().header(["REF", "INSTANCE", "PHASE", "NODE", "BIND:PORT"]);
    for e in endpoints {
        let bind = e["bind"].as_str().unwrap_or("-");
        let port = e["port"].as_u64().unwrap_or(0);
        t = t.row([
            ssh_endpoint_ref(&e),
            e["instanceId"].as_str().unwrap_or("-").to_string(),
            e["phase"].as_str().unwrap_or("-").to_string(),
            e["nodeName"].as_str().unwrap_or("-").to_string(),
            format!("{bind}:{port}"),
        ]);
    }
    print!("{}", t.render());
    Ok(())
}

/// Copyable `<stack>/<service>/<ordinal>` reference for an SSH endpoint row
/// (`-` when the endpoint's instance row is gone), the form every other
/// instance command accepts.
fn ssh_endpoint_ref(e: &serde_json::Value) -> String {
    match (
        e["stack"].as_str(),
        e["service"].as_str(),
        e["ordinal"].as_u64(),
    ) {
        (Some(stack), Some(service), Some(ordinal)) => format!("{stack}/{service}/{ordinal}"),
        _ => "-".to_string(),
    }
}

pub(crate) async fn ssh_instance_show(args: SshInstanceArgs, conn: &Conn) -> Result<()> {
    // Accept `<stack>/<service>/<ordinal>` refs like `exec`/`logs` do.
    let id = resolve_instance_id(conn, &args.id).await?;
    let url = instance_url(&conn.url, &id, "/ssh");
    let client = reqwest::Client::new();
    let res = operator_get(&client, &url, conn.token.as_deref())
        .send()
        .await
        .with_context(|| format!("GET {url}"))?;
    println!("{}", checked_body("ssh show", res).await?);
    Ok(())
}

pub(crate) async fn ssh_instance_open(args: SshOpenArgs, conn: &Conn) -> Result<()> {
    let id = resolve_instance_id(conn, &args.id).await?;
    // Surface an active disk condition before opening a session into the VM.
    if let Some(record) = crate::cmd::observe::fetch_instance(conn, &id).await {
        crate::cmd::observe::eprint_disk_conditions(&record);
    }
    let url = instance_url(&conn.url, &id, "/ssh");
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
    println!("{}", checked_body("ssh open", res).await?);
    Ok(())
}

pub(crate) async fn ssh_instance_close(args: SshInstanceArgs, conn: &Conn) -> Result<()> {
    let id = resolve_instance_id(conn, &args.id).await?;
    let url = instance_url(&conn.url, &id, "/ssh");
    let client = reqwest::Client::new();
    let mut req = client.put(&url).json(&serde_json::json!({
        "enabled": false,
        "authorizedKeys": []
    }));
    if let Some(t) = conn.token.as_deref().filter(|s| !s.is_empty()) {
        req = req.bearer_auth(t);
    }
    let res = req.send().await.context("PUT instance ssh close")?;
    println!("{}", checked_body("ssh close", res).await?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::ssh_endpoint_ref;
    use crate::cli::{Cli, Commands, SshCommands};
    use clap::Parser;

    fn parse_ssh(args: &[&str]) -> SshCommands {
        match Cli::try_parse_from(args)
            .expect("ssh args should parse")
            .command
        {
            Commands::Ssh(cmd) => cmd.command,
            other => panic!("expected an ssh command, got {other:?}"),
        }
    }

    #[test]
    fn ssh_show_close_accept_instance_refs() {
        // A `<stack>/<service>/<ordinal>` ref is one argv token; resolution
        // against the server happens later (client::resolve_instance_id).
        match parse_ssh(&["mc2", "ssh", "show", "demo/web/0"]) {
            SshCommands::Show(a) => assert_eq!(a.id, "demo/web/0"),
            other => panic!("expected show, got {other:?}"),
        }
        match parse_ssh(&[
            "mc2",
            "ssh",
            "close",
            "6e6a2d3f-b4dc-4c09-a317-4aac91ff0c99",
        ]) {
            SshCommands::Close(a) => assert_eq!(a.id, "6e6a2d3f-b4dc-4c09-a317-4aac91ff0c99"),
            other => panic!("expected close, got {other:?}"),
        }
    }

    #[test]
    fn ssh_open_accepts_a_ref_and_its_flags() {
        match parse_ssh(&[
            "mc2",
            "ssh",
            "open",
            "demo/web/0",
            "--key",
            "laptop",
            "--port",
            "2222",
        ]) {
            SshCommands::Open(a) => {
                assert_eq!(a.id, "demo/web/0");
                assert_eq!(a.keys, vec!["laptop".to_string()]);
                assert_eq!(a.bind, "127.0.0.1");
                assert_eq!(a.port, 2222);
            }
            other => panic!("expected open, got {other:?}"),
        }
    }

    #[test]
    fn ssh_endpoint_ref_includes_the_ordinal() {
        // Shapes come from the server's `GET /v1/ssh/endpoints`.
        let e = serde_json::json!({
            "instanceId": "6e6a2d3f-b4dc-4c09-a317-4aac91ff0c99",
            "stack": "demo",
            "service": "web",
            "ordinal": 2,
        });
        assert_eq!(ssh_endpoint_ref(&e), "demo/web/2");
        // No instance row → no copyable ref.
        assert_eq!(
            ssh_endpoint_ref(&serde_json::json!({
                "instanceId": "x",
                "stack": null,
                "service": null,
                "ordinal": null,
            })),
            "-"
        );
    }

    #[test]
    fn ssh_refs_resolve_to_path_segments_not_slashes() {
        // The parsed ref is what the client turns into an instance lookup and
        // then a single (percent-encoded) path segment.
        let r = crate::client::parse_instance_ref("demo/web/0")
            .unwrap()
            .unwrap();
        assert_eq!((r.stack, r.service, r.ordinal), ("demo", "web", 0));
        assert_eq!(
            crate::client::urlencoding_simple("demo/web/0"),
            "demo%2Fweb%2F0"
        );
    }
}
