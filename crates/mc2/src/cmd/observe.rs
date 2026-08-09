//! Observe commands: `ps`, `status`, `node ls`, `network`, `ingress`.

use crate::cli::{IngressArgs, ListArgs, NetworkArgs, OutputFormat, PsArgs, StatusArgs};
use crate::client::{api_error, operator_get, resolve_instance_id, urlencoding_simple};
use crate::context::Conn;
use anyhow::{bail, Context, Result};

pub(crate) async fn ps_cmd(args: PsArgs, conn: &Conn) -> Result<()> {
    let url = format!("{}/v1/instances", conn.url.trim_end_matches('/'));
    let client = reqwest::Client::new();
    let res = operator_get(&client, &url, conn.token.as_deref())
        .send()
        .await
        .with_context(|| format!("GET {url}"))?;
    let status = res.status();
    let body = res.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(api_error("ps", status, &body));
    }
    let mut instances: Vec<mc2_store::InstanceRecord> =
        serde_json::from_str(&body).with_context(|| format!("parse: {body}"))?;
    if let Some(ref stack) = args.stack {
        instances.retain(|i| i.stack == *stack);
    }
    if let Some(ref service) = args.service {
        instances.retain(|i| i.service == *service);
    }
    if matches!(args.output, OutputFormat::Json) {
        println!("{}", serde_json::to_string_pretty(&instances)?);
        return Ok(());
    }
    if instances.is_empty() {
        println!("No instances.");
        return Ok(());
    }
    let mut t = crate::table::Table::new()
        .header(["STACK", "SERVICE", "ORD", "PHASE", "NODE", "ID"])
        .right_align([2]);
    for i in instances {
        t = t.row([
            i.stack,
            i.service,
            i.ordinal.to_string(),
            i.phase,
            i.node_id.unwrap_or_else(|| "-".into()),
            i.id,
        ]);
    }
    print!("{}", t.render());
    Ok(())
}

pub(crate) async fn node_ls(args: ListArgs, conn: &Conn) -> Result<()> {
    let url = format!("{}/v1/nodes", conn.url.trim_end_matches('/'));
    let client = reqwest::Client::new();
    let res = operator_get(&client, &url, conn.token.as_deref())
        .send()
        .await
        .with_context(|| format!("GET {url}"))?;
    let status = res.status();
    let body = res.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(api_error("node ls", status, &body));
    }
    if matches!(args.output, OutputFormat::Json) {
        println!("{body}");
        return Ok(());
    }
    let nodes: Vec<mc2_api::NodeView> =
        serde_json::from_str(&body).with_context(|| format!("parse nodes JSON: {body}"))?;

    if nodes.is_empty() {
        println!("No nodes registered.");
        return Ok(());
    }

    let mut t = crate::table::Table::new()
        .header([
            "ID",
            "NAME",
            "STATUS",
            "CPU",
            "MEM_MiB",
            "ARCH",
            "LAST_HEARTBEAT",
        ])
        .right_align([3, 4]);
    for n in nodes {
        t = t.row([
            n.id,
            n.name,
            n.status,
            n.cpus.to_string(),
            n.memory_mib.to_string(),
            n.arch,
            n.last_heartbeat.unwrap_or_else(|| "-".into()),
        ]);
    }
    print!("{}", t.render());
    Ok(())
}

/// Cluster status: health + version + counts, mode/context-aware.
pub(crate) async fn status_cmd(args: StatusArgs, conn: &Conn) -> Result<()> {
    let base = conn.url.trim_end_matches('/');
    let client = reqwest::Client::new();
    let health = operator_get(&client, &format!("{base}/health"), None)
        .send()
        .await
        .with_context(|| format!("GET {base}/health"))?;
    if !health.status().is_success() {
        bail!(
            "status failed: {}: server unreachable at {base}",
            health.status()
        );
    }
    let res = operator_get(&client, &format!("{base}/v1/status"), conn.token.as_deref())
        .send()
        .await
        .with_context(|| format!("GET {base}/v1/status"))?;
    let status = res.status();
    let body = res.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(api_error("status", status, &body));
    }

    if matches!(args.output, OutputFormat::Json) {
        let mut v: serde_json::Value = serde_json::from_str(&body)?;
        if let Some(obj) = v.as_object_mut() {
            obj.insert("mode".into(), serde_json::json!(conn.mode.as_str()));
            if let Some(ref ctx) = conn.context {
                obj.insert("context".into(), serde_json::json!(ctx));
            }
            obj.insert(
                "auth".into(),
                serde_json::json!(if conn.token.is_some() {
                    "bearer"
                } else {
                    "none"
                }),
            );
        }
        println!("{}", serde_json::to_string_pretty(&v)?);
        return Ok(());
    }

    let v: serde_json::Value = serde_json::from_str(&body)?;
    let version = v["version"].as_str().unwrap_or("?");
    let ctx_suffix = conn
        .context
        .as_ref()
        .map(|c| format!(" (context: {c})"))
        .unwrap_or_default();
    println!("mc2 {version} — {}: {base}{ctx_suffix}", conn.mode.as_str());
    if conn.token.is_some() {
        println!("  auth: bearer ✓");
    } else {
        println!("  auth: none");
    }
    println!(
        "  server api: {}, version {}",
        v["api_version"].as_str().unwrap_or("?"),
        version
    );
    println!(
        "  nodes: {}/{} ready · stacks: {} · instances: {}",
        v["nodes_ready"].as_u64().unwrap_or(0),
        v["nodes_total"].as_u64().unwrap_or(0),
        v["stacks"].as_u64().unwrap_or(0),
        v["instances"].as_u64().unwrap_or(0),
    );
    if let Some(msg) = v["message"].as_str() {
        println!("  message: {msg}");
    }
    if let Some(res) = v.get("resources") {
        print_resources(res);
    }
    Ok(())
}

/// Print the `resources` block of `/v1/status` (budget + host + consumption).
fn print_resources(res: &serde_json::Value) {
    let lim = &res["limits"];
    let host = &res["host"];
    let used = &res["reserved"];
    // CPU: `0` = unlimited. Memory/disk: `0` = unlimited (no unit suffix).
    let unit = |n: &serde_json::Value| {
        let n = n.as_u64().unwrap_or(0);
        if n == 0 {
            "unlimited".to_string()
        } else {
            n.to_string()
        }
    };
    let mib = |n: &serde_json::Value| {
        let n = n.as_u64().unwrap_or(0);
        if n == 0 {
            "unlimited".to_string()
        } else {
            format!("{n} MiB")
        }
    };
    let body = crate::table::Table::new()
        .header(["METRIC", "CPU", "MEMORY", "DISK"])
        .right_align([1, 2, 3])
        .row([
            "Host".to_string(),
            host["cpus"].as_u64().unwrap_or(0).to_string(),
            format!("{} MiB", host["memoryMib"].as_u64().unwrap_or(0)),
            format!(
                "{:.1}/{:.1} GB",
                host["diskFreeMib"].as_u64().unwrap_or(0) as f64 / 1024.0,
                host["diskTotalMib"].as_u64().unwrap_or(0) as f64 / 1024.0
            ),
        ])
        .row([
            "Limits".to_string(),
            unit(&lim["cpus"]),
            mib(&lim["memoryMib"]),
            mib(&lim["diskMib"]),
        ])
        .row([
            "MC2".to_string(),
            used["cpus"].as_u64().unwrap_or(0).to_string(),
            format!("{} MiB", used["memoryMib"].as_u64().unwrap_or(0)),
            format!("{} MiB", res["mc2DiskUsedMib"].as_u64().unwrap_or(0)),
        ])
        .render();
    println!("  resources:");
    for line in body.lines() {
        println!("    {line}");
    }
}

/// Server-wide network membership summary. `mc2 network` lists all networks;
/// `mc2 network <name>` shows one network's instances and their ports;
/// `mc2 network <stack>/<service>/<ordinal>` shows one instance's observed
/// network connectivity (exposes/edges).
pub(crate) async fn network_cmd(args: NetworkArgs, conn: &Conn) -> Result<()> {
    // An instance reference (contains `/`) shows observed per-instance connectivity.
    let Some(target) = args.network.clone() else {
        return network_summary_cmd(args, conn).await;
    };
    if target.contains('/') {
        return network_instance_cmd(args, conn, &target).await;
    }

    let url = format!("{}/v1/networks", conn.url.trim_end_matches('/'));
    let client = reqwest::Client::new();
    let res = operator_get(&client, &url, conn.token.as_deref())
        .send()
        .await
        .with_context(|| format!("GET {url}"))?;
    let status = res.status();
    let body = res.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(api_error("network", status, &body));
    }
    let v: serde_json::Value = serde_json::from_str(&body)?;
    let all = v["networks"].as_array().cloned().unwrap_or_default();
    let selected: Vec<serde_json::Value> = all
        .into_iter()
        .filter(|n| n["name"] == target.as_str())
        .collect();
    if matches!(args.output, OutputFormat::Json) {
        println!("{}", serde_json::to_string_pretty(&selected)?);
        return Ok(());
    }
    let Some(net) = selected.first() else {
        let known: Vec<String> = v["networks"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|n| n["name"].as_str().map(str::to_string))
            .collect();
        bail!(
            "network {target:?} not found (known networks: {})",
            if known.is_empty() {
                "none".to_string()
            } else {
                known.join(", ")
            }
        );
    };
    print_network_detail(net);
    Ok(())
}

/// `mc2 network` with no argument: table of every network.
pub(crate) async fn network_summary_cmd(args: NetworkArgs, conn: &Conn) -> Result<()> {
    let url = format!("{}/v1/networks", conn.url.trim_end_matches('/'));
    let client = reqwest::Client::new();
    let res = operator_get(&client, &url, conn.token.as_deref())
        .send()
        .await
        .with_context(|| format!("GET {url}"))?;
    let status = res.status();
    let body = res.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(api_error("network", status, &body));
    }
    let v: serde_json::Value = serde_json::from_str(&body)?;
    let all = v["networks"].as_array().cloned().unwrap_or_default();
    if matches!(args.output, OutputFormat::Json) {
        println!("{}", serde_json::to_string_pretty(&all)?);
        return Ok(());
    }
    print_network_summary(&all);
    Ok(())
}

/// `mc2 network <stack>/<service>/<ordinal>`: observed connectivity for one instance.
pub(crate) async fn network_instance_cmd(
    args: NetworkArgs,
    conn: &Conn,
    target: &str,
) -> Result<()> {
    let id = resolve_instance_id(conn, target).await?;
    let url = format!(
        "{}/v1/instances/{}/network",
        conn.url.trim_end_matches('/'),
        urlencoding_simple(&id)
    );
    let client = reqwest::Client::new();
    let res = operator_get(&client, &url, conn.token.as_deref())
        .send()
        .await
        .with_context(|| format!("GET {url}"))?;
    let status = res.status();
    let body = res.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(api_error("network", status, &body));
    }
    if matches!(args.output, OutputFormat::Json) {
        println!("{body}");
        return Ok(());
    }
    let v: serde_json::Value = serde_json::from_str(&body)?;
    println!(
        "instance {} — network {}",
        v["instanceId"].as_str().unwrap_or("-"),
        v["phase"].as_str().unwrap_or("-")
    );
    if let Some(msg) = v["message"].as_str().filter(|m| !m.is_empty()) {
        println!("  message: {msg}");
    }
    let observed = v
        .get("observed")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    for ex in observed["exposes"].as_array().into_iter().flatten() {
        println!(
            "  expose guest:{} -> host:{} [{}]",
            ex["guestPort"].as_u64().unwrap_or(0),
            ex["hostPort"].as_u64().unwrap_or(0),
            ex["phase"].as_str().unwrap_or("-")
        );
    }
    for edge in observed["edges"].as_array().into_iter().flatten() {
        println!(
            "  reach {}:{} [{}]",
            edge["toService"].as_str().unwrap_or("-"),
            edge["port"].as_u64().unwrap_or(0),
            edge["phase"].as_str().unwrap_or("-")
        );
    }
    Ok(())
}

/// (stacks, services, instances, active) for a network view value.
fn network_counts(n: &serde_json::Value) -> (usize, usize, usize, usize) {
    use std::collections::BTreeSet;
    let mut stacks: BTreeSet<String> = BTreeSet::new();
    let mut services: BTreeSet<(String, String)> = BTreeSet::new();
    let mut active = 0usize;
    for i in n["instances"].as_array().into_iter().flatten() {
        stacks.insert(i["stack"].as_str().unwrap_or("").to_string());
        services.insert((
            i["stack"].as_str().unwrap_or("").to_string(),
            i["service"].as_str().unwrap_or("").to_string(),
        ));
        if i["phase"] == "Running" {
            active += 1;
        }
    }
    (
        stacks.len(),
        services.len(),
        n["instances"].as_array().map(|a| a.len()).unwrap_or(0),
        active,
    )
}

fn print_network_summary(nets: &[serde_json::Value]) {
    if nets.is_empty() {
        println!("No networks.");
        return;
    }
    let mut t = crate::table::Table::new().header([
        "NETWORK",
        "KIND",
        "STACKS",
        "SERVICES",
        "INSTANCES",
        "ACTIVE",
    ]);
    for n in nets {
        let (stacks, services, instances, active) = network_counts(n);
        t = t.row([
            n["name"].as_str().unwrap_or("-").to_string(),
            n["kind"].as_str().unwrap_or("-").to_string(),
            stacks.to_string(),
            services.to_string(),
            instances.to_string(),
            active.to_string(),
        ]);
    }
    print!("{}", t.render());
}

fn print_network_detail(n: &serde_json::Value) {
    let (stacks, services, instances, active) = network_counts(n);
    let plural = |x: usize| if x == 1 { "" } else { "s" };
    println!(
        "network '{}' ({}) — {} stack{}, {} service{}, {} instance{} ({} active)",
        n["name"].as_str().unwrap_or("-"),
        n["kind"].as_str().unwrap_or("-"),
        stacks,
        plural(stacks),
        services,
        plural(services),
        instances,
        plural(instances),
        active
    );
    for i in n["instances"].as_array().into_iter().flatten() {
        let expose: Vec<String> = i["exposePorts"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|p| p.as_u64())
            .map(|p| p.to_string())
            .collect();
        let ports: Vec<String> = i["ports"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|p| {
                format!(
                    "{}->{}",
                    p["published"].as_u64().unwrap_or(0),
                    p["target"].as_u64().unwrap_or(0)
                )
            })
            .collect();
        let health = if i["healthy"].as_bool().unwrap_or(false) {
            "  healthy"
        } else {
            ""
        };
        println!(
            "  {}/{}  {}  {}{}",
            i["stack"].as_str().unwrap_or("-"),
            i["service"].as_str().unwrap_or("-"),
            i["ordinal"].as_u64().unwrap_or(0),
            i["phase"].as_str().unwrap_or("-"),
            health
        );
        if !expose.is_empty() {
            println!("      expose: {}", expose.join(", "));
        }
        if !ports.is_empty() {
            println!("      host ports: {}", ports.join(", "));
        }
    }
}

/// Desired ingress routes.
pub(crate) async fn ingress_cmd(args: IngressArgs, conn: &Conn) -> Result<()> {
    let url = format!("{}/v1/ingress", conn.url.trim_end_matches('/'));
    let client = reqwest::Client::new();
    let res = operator_get(&client, &url, conn.token.as_deref())
        .send()
        .await
        .with_context(|| format!("GET {url}"))?;
    let status = res.status();
    let body = res.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(api_error("ingress", status, &body));
    }
    if matches!(args.output, OutputFormat::Json) {
        println!("{body}");
        return Ok(());
    }
    let v: serde_json::Value = serde_json::from_str(&body)?;
    let routes = v["routes"].as_array().cloned().unwrap_or_default();
    if routes.is_empty() {
        println!("No ingress routes.");
        return Ok(());
    }
    let mut t =
        crate::table::Table::new().header(["HOST", "PATH", "SERVICE", "GUEST", "HOST_PORT", "ID"]);
    for r in routes {
        t = t.row([
            r["host"].as_str().unwrap_or("-").to_string(),
            r["path"].as_str().unwrap_or("/").to_string(),
            r["service"].as_str().unwrap_or("-").to_string(),
            r["guestPort"].as_u64().unwrap_or(0).to_string(),
            r["hostPort"].as_u64().unwrap_or(0).to_string(),
            r["id"].as_str().unwrap_or("-").to_string(),
        ]);
    }
    print!("{}", t.render());
    Ok(())
}
