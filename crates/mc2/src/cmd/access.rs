//! Access commands: `exec`, `logs`.

use crate::cli::{ExecArgs, LogsArgs};
use crate::client::{api_error, checked_body, instance_url, resolve_instance_id};
use crate::context::Conn;
use anyhow::{Context, Result};
use std::io::IsTerminal;
use std::io::Write;

/// Run a command inside the instance's sandbox; mirror output and exit with
/// the command's exit code. Piped stdin is forwarded to the sandbox.
pub(crate) async fn exec_cmd(args: ExecArgs, conn: &Conn) -> Result<()> {
    let id = resolve_instance_id(conn, &args.instance).await?;
    // Surface an active disk condition before entering the VM, so the operator
    // sees why writes fail before hitting ENOSPC inside.
    if let Some(record) = crate::cmd::observe::fetch_instance(conn, &id).await {
        crate::cmd::observe::eprint_disk_conditions(&record);
    }
    let url = instance_url(&conn.url, &id, "/exec");
    let stdin = {
        if std::io::stdin().is_terminal() {
            None
        } else {
            use std::io::Read;
            let mut buf = Vec::new();
            std::io::stdin()
                .read_to_end(&mut buf)
                .context("read stdin")?;
            // Raw bytes: stdin may be binary (the REST envelope is base64).
            Some(buf)
        }
    };
    let client = reqwest::Client::new();
    let mut req = client.post(&url).json(&serde_json::json!({
        "cmd": args.cmd,
        "stdinBase64": stdin.as_deref().map(mc2_api::exec::encode_bytes),
    }));
    if let Some(t) = conn.token.as_deref().filter(|s| !s.is_empty()) {
        req = req.bearer_auth(t);
    }
    let res = req.send().await.with_context(|| format!("POST {url}"))?;
    let body = checked_body("exec", res).await?;
    let v: serde_json::Value = serde_json::from_str(&body)?;
    let (stdout, stderr, code) = decode_exec_response(&v)?;
    // stdout/stderr are raw base64: guest output need not be valid UTF-8, so it
    // is written through as bytes rather than lossily converted.
    if !stdout.is_empty() {
        let mut handle = std::io::stdout();
        handle.write_all(&stdout).context("write stdout")?;
        handle.flush().context("flush stdout")?;
    }
    if !stderr.is_empty() {
        let mut handle = std::io::stderr();
        handle.write_all(&stderr).context("write stderr")?;
        handle.flush().context("flush stderr")?;
    }
    if code != 0 {
        std::process::exit(code as i32);
    }
    Ok(())
}

/// Decode an exec response body into `(stdout, stderr, exit code)`.
///
/// Both streams are base64 (`mc2_api::exec`) because guest output is arbitrary
/// bytes; a missing or malformed field must not silently drop output.
fn decode_exec_response(v: &serde_json::Value) -> Result<(Vec<u8>, Vec<u8>, i64)> {
    let decode = |field: &str| -> Result<Vec<u8>> {
        match v[field].as_str() {
            Some(encoded) => {
                mc2_api::exec::decode_bytes(encoded).with_context(|| format!("decode {field}"))
            }
            None => Ok(Vec::new()),
        }
    };
    Ok((
        decode("stdoutBase64")?,
        decode("stderrBase64")?,
        v["exitCode"].as_i64().unwrap_or(0),
    ))
}

/// Print recent sandbox logs for an instance; `--follow` streams new entries.
pub(crate) async fn logs_cmd(args: LogsArgs, conn: &Conn) -> Result<()> {
    let id = resolve_instance_id(conn, &args.instance).await?;
    // Same pre-flight notice as `exec`: someone reading logs after a failed
    // write should see the cause and the fix.
    if let Some(record) = crate::cmd::observe::fetch_instance(conn, &id).await {
        crate::cmd::observe::eprint_disk_conditions(&record);
    }
    let mut url = instance_url(&conn.url, &id, "/logs");
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

    if !args.follow {
        let body = checked_body("logs", res).await?;
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

    // Follow: the response is an SSE stream, so the body cannot be buffered —
    // check the status directly and read an error body only on failure.
    let status = res.status();
    if !status.is_success() {
        let body = res.text().await.unwrap_or_default();
        return Err(api_error("logs", status, &body));
    }

    // Consume the SSE stream (`data: <json>` frames) as it arrives.
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The CLI half of the stdin/stdout byte round-trip: fields the server
    /// encoded (see `mc2_api::exec`) decode back to the original bytes, invalid
    /// UTF-8 included.
    #[test]
    fn decodes_base64_exec_fields() {
        let v = serde_json::json!({
            "stdoutBase64": mc2_api::exec::encode_bytes(&[0xff, 0x00, 0xfe]),
            "stderrBase64": mc2_api::exec::encode_bytes(b"boom"),
            "exitCode": 3,
        });
        let (stdout, stderr, code) = decode_exec_response(&v).unwrap();
        assert_eq!(stdout, vec![0xff, 0x00, 0xfe]);
        assert_eq!(stderr, b"boom".to_vec());
        assert_eq!(code, 3);

        // Absent fields → empty streams, exit 0.
        let (stdout, stderr, code) = decode_exec_response(&serde_json::json!({})).unwrap();
        assert!(stdout.is_empty() && stderr.is_empty());
        assert_eq!(code, 0);

        // Corrupt base64 is an error, never silently empty output.
        let bad = serde_json::json!({ "stdoutBase64": "not base64!!" });
        assert!(decode_exec_response(&bad).is_err());
    }
}
