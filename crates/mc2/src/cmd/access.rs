//! Access commands: `exec`, `logs`.

use crate::cli::{ExecArgs, LogsArgs};
use crate::client::{api_error, resolve_instance_id, urlencoding_simple};
use crate::context::Conn;
use anyhow::{Context, Result};
use std::io::IsTerminal;

/// Run a command inside the instance's sandbox; mirror output and exit with
/// the command's exit code. Piped stdin is forwarded to the sandbox.
pub(crate) async fn exec_cmd(args: ExecArgs, conn: &Conn) -> Result<()> {
    let base = conn.url.trim_end_matches('/');
    let id = resolve_instance_id(conn, &args.instance).await?;
    let url = format!("{base}/v1/instances/{}/exec", urlencoding_simple(&id));
    let stdin = {
        if std::io::stdin().is_terminal() {
            None
        } else {
            use std::io::Read;
            let mut buf = Vec::new();
            std::io::stdin()
                .read_to_end(&mut buf)
                .context("read stdin")?;
            Some(String::from_utf8_lossy(&buf).to_string())
        }
    };
    let client = reqwest::Client::new();
    let mut req = client
        .post(&url)
        .json(&serde_json::json!({ "cmd": args.cmd, "stdin": stdin }));
    if let Some(t) = conn.token.as_deref().filter(|s| !s.is_empty()) {
        req = req.bearer_auth(t);
    }
    let res = req.send().await.with_context(|| format!("POST {url}"))?;
    let status = res.status();
    let body = res.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(api_error("exec", status, &body));
    }
    let v: serde_json::Value = serde_json::from_str(&body)?;
    if let Some(out) = v["stdout"].as_str() {
        print!("{out}");
    }
    if let Some(err) = v["stderr"].as_str() {
        eprint!("{err}");
    }
    let code = v["exitCode"].as_i64().unwrap_or(0);
    if code != 0 {
        std::process::exit(code as i32);
    }
    Ok(())
}

/// Print recent sandbox logs for an instance; `--follow` streams new entries.
pub(crate) async fn logs_cmd(args: LogsArgs, conn: &Conn) -> Result<()> {
    let base = conn.url.trim_end_matches('/');
    let id = resolve_instance_id(conn, &args.instance).await?;
    let mut url = format!("{base}/v1/instances/{}/logs", urlencoding_simple(&id));
    let mut params: Vec<String> = Vec::new();
    if let Some(tail) = args.tail {
        params.push(format!("tail={tail}"));
    }
    if args.follow {
        params.push("follow=true".into());
    }
    if !params.is_empty() {
        url.push_str(&format!("?{}", params.join("&")));
    }
    let client = reqwest::Client::new();
    let mut req = client.get(&url);
    if let Some(t) = conn.token.as_deref().filter(|s| !s.is_empty()) {
        req = req.bearer_auth(t);
    }
    let res = req.send().await.with_context(|| format!("GET {url}"))?;
    let status = res.status();
    if !status.is_success() {
        let body = res.text().await.unwrap_or_default();
        return Err(api_error("logs", status, &body));
    }

    if !args.follow {
        let body = res.text().await.unwrap_or_default();
        let v: serde_json::Value = serde_json::from_str(&body)?;
        let entries = v["entries"].as_array().cloned().unwrap_or_default();
        if entries.is_empty() {
            println!("No logs.");
            return Ok(());
        }
        for e in entries {
            print_log_entry(&e);
        }
        return Ok(());
    }

    // Follow: consume the SSE stream (`data: <json>` frames) as it arrives.
    use futures::StreamExt;
    let mut stream = res.bytes_stream();
    let mut buf: Vec<u8> = Vec::new();
    let mut saw_entry = false;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("read log stream")?;
        buf.extend_from_slice(&chunk);
        while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = buf.drain(..=pos).collect();
            let line = String::from_utf8_lossy(&line);
            if let Some(data) = line.strip_prefix("data:") {
                let data = data.trim();
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(data) {
                    saw_entry = true;
                    print_log_entry(&v);
                }
            }
        }
    }
    if !saw_entry {
        println!("No logs.");
    }
    Ok(())
}

fn print_log_entry(e: &serde_json::Value) {
    let data = e["data"].as_str().unwrap_or("").trim_end();
    if data.is_empty() {
        return;
    }
    let source = e["source"].as_str().unwrap_or("?");
    println!("[{source}] {data}");
}
