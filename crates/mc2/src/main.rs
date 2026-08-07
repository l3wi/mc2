//! MicroCommandControl (MC2) — dual-mode binary.
//!
//! ```text
//! mc2 server   # control plane
//! mc2 agent    # node agent (embeds microsandbox runtime)
//! mc2 apply    # operator: apply a stack (Phase 3+)
//! mc2 node ls  # list nodes
//! ```

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

#[derive(Debug, Parser)]
#[command(
    name = "mc2",
    about = "MC2 (MicroCommandControl) — self-hosted C2 for microsandbox microVMs",
    long_about = "MC2 (MicroCommandControl) is a K3s-shaped control plane for \
                  microsandbox microVMs. One dual-mode binary: server, agent, and operator CLI.",
    version
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Run the MicroCommandControl server (control plane)
    Server(mc2_server::ServerArgs),
    /// Run the MicroCommandControl agent (node worker)
    Agent(mc2_agent::AgentArgs),
    /// Apply a stack YAML (desired state)
    Apply(ApplyArgs),
    /// Node operations
    Node(NodeCmd),
    /// List instances (desired sandboxes)
    Ps(OperatorArgs),
    /// Cluster secrets (encrypted at rest; values never listed)
    Secret(SecretCmd),
    /// SSH authorized keys + endpoints
    Ssh(SshCmd),
    /// Check host readiness (hypervisor / msb / paths)
    Doctor(DoctorArgs),
    /// Show cluster / binary version info
    Version,
}

#[derive(Debug, Parser)]
struct SshCmd {
    #[command(subcommand)]
    command: SshCommands,
}

#[derive(Debug, Subcommand)]
enum SshCommands {
    /// Manage cluster authorized public keys
    Key(SshKeyCmd),
    /// List open SSH endpoints
    #[command(name = "ls", alias = "list")]
    Ls(OperatorArgs),
    /// Show SSH state for an instance
    Show(SshInstanceArgs),
    /// Open SSH on an instance (API override)
    Open(SshOpenArgs),
    /// Close SSH on an instance
    Close(SshInstanceArgs),
}

#[derive(Debug, Parser)]
struct SshKeyCmd {
    #[command(subcommand)]
    command: SshKeyCommands,
}

#[derive(Debug, Subcommand)]
enum SshKeyCommands {
    /// Add or replace an authorized public key
    Add(SshKeyAddArgs),
    #[command(name = "ls", alias = "list")]
    Ls(OperatorArgs),
    #[command(name = "rm", alias = "delete")]
    Rm(SshKeyRmArgs),
}

#[derive(Debug, Parser)]
struct SshKeyAddArgs {
    name: String,
    /// Public key line (or use --file)
    #[arg(long)]
    key: Option<String>,
    /// Path to .pub file
    #[arg(long)]
    file: Option<String>,
    #[command(flatten)]
    op: OperatorArgs,
}

#[derive(Debug, Parser)]
struct SshKeyRmArgs {
    name: String,
    #[command(flatten)]
    op: OperatorArgs,
}

#[derive(Debug, Parser)]
struct SshInstanceArgs {
    /// Instance id
    id: String,
    #[command(flatten)]
    op: OperatorArgs,
}

#[derive(Debug, Parser)]
struct SshOpenArgs {
    id: String,
    /// Authorized key names (repeatable)
    #[arg(long = "key", required = true)]
    keys: Vec<String>,
    #[arg(long, default_value = "127.0.0.1")]
    bind: String,
    #[arg(long, default_value_t = 0)]
    port: u16,
    #[command(flatten)]
    op: OperatorArgs,
}

#[derive(Debug, Parser)]
struct SecretCmd {
    #[command(subcommand)]
    command: SecretCommands,
}

#[derive(Debug, Subcommand)]
enum SecretCommands {
    /// Create or replace a secret value
    Set(SecretSetArgs),
    /// List secret names (values never shown)
    #[command(name = "ls", alias = "list")]
    Ls(OperatorArgs),
    /// Delete a secret
    #[command(name = "rm", alias = "delete")]
    Rm(SecretRmArgs),
}

#[derive(Debug, Parser)]
struct SecretSetArgs {
    /// Secret name (cluster-global)
    name: String,
    /// Secret value (prefer env MC2_SECRET_VALUE or stdin for scripts)
    #[arg(long, env = "MC2_SECRET_VALUE")]
    value: Option<String>,
    #[command(flatten)]
    op: OperatorArgs,
}

#[derive(Debug, Parser)]
struct SecretRmArgs {
    name: String,
    #[command(flatten)]
    op: OperatorArgs,
}

#[derive(Debug, Parser)]
struct DoctorArgs {
    /// Also try `msb version`
    #[arg(long, default_value_t = true)]
    msb: bool,
}

#[derive(Debug, Parser)]
struct ApplyArgs {
    /// Path to stack YAML
    #[arg(short = 'f', long = "file")]
    file: String,

    #[command(flatten)]
    op: OperatorArgs,
}

#[derive(Debug, Parser)]
struct NodeCmd {
    #[command(subcommand)]
    command: NodeCommands,
}

#[derive(Debug, Subcommand)]
enum NodeCommands {
    /// List registered nodes (`mc2 node ls`)
    #[command(name = "ls", alias = "list")]
    Ls(OperatorArgs),
}

#[derive(Debug, Parser)]
struct OperatorArgs {
    /// Control plane REST base URL
    #[arg(long, default_value = "http://127.0.0.1:7443", env = "MC2_API")]
    api: String,

    /// Operator API bearer token
    #[arg(long, env = "MC2_API_KEY")]
    token: Option<String>,
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer())
        .init();
}

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing();
    let cli = Cli::parse();

    match cli.command {
        Commands::Server(args) => mc2_server::run(args).await?,
        Commands::Agent(args) => mc2_agent::run(args).await?,
        Commands::Apply(args) => apply_cmd(args).await?,
        Commands::Node(NodeCmd {
            command: NodeCommands::Ls(args),
        }) => node_ls(args).await?,
        Commands::Ps(args) => ps_cmd(args).await?,
        Commands::Secret(SecretCmd { command }) => match command {
            SecretCommands::Set(a) => secret_set(a).await?,
            SecretCommands::Ls(a) => secret_ls(a).await?,
            SecretCommands::Rm(a) => secret_rm(a).await?,
        },
        Commands::Ssh(SshCmd { command }) => match command {
            SshCommands::Key(SshKeyCmd { command }) => match command {
                SshKeyCommands::Add(a) => ssh_key_add(a).await?,
                SshKeyCommands::Ls(a) => ssh_key_ls(a).await?,
                SshKeyCommands::Rm(a) => ssh_key_rm(a).await?,
            },
            SshCommands::Ls(a) => ssh_endpoints_ls(a).await?,
            SshCommands::Show(a) => ssh_instance_show(a).await?,
            SshCommands::Open(a) => ssh_instance_open(a).await?,
            SshCommands::Close(a) => ssh_instance_close(a).await?,
        },
        Commands::Doctor(args) => {
            let code = doctor_cmd(args)?;
            if code != 0 {
                std::process::exit(code);
            }
        }
        Commands::Version => {
            println!(
                "mc2 {} — MC2 (MicroCommandControl)",
                env!("CARGO_PKG_VERSION")
            );
            println!("api schema: {}", mc2_api::API_VERSION);
            if let Some(ep) = mc2_metrics::otlp_endpoint_from_env() {
                println!("otlp endpoint: {ep}");
            } else {
                println!(
                    "otlp endpoint: (unset — MC2_OTLP_ENDPOINT / OTEL_EXPORTER_OTLP_ENDPOINT)"
                );
            }
        }
    }

    mc2_metrics::shutdown();
    Ok(())
}

/// Returns process exit code (0 = ok for server/dev; 1 = agent hypervisor missing).
fn doctor_cmd(args: DoctorArgs) -> Result<i32> {
    let mut issues = 0u32;
    println!("mc2 doctor — host checks");
    println!("  version: {}", env!("CARGO_PKG_VERSION"));
    println!("  api: {}", mc2_api::API_VERSION);
    println!("  os: {} {}", std::env::consts::OS, std::env::consts::ARCH);

    #[cfg(target_os = "linux")]
    {
        let kvm = std::path::Path::new("/dev/kvm").exists();
        if kvm {
            println!("  /dev/kvm: yes");
        } else {
            println!("  /dev/kvm: MISSING (required for agent microVMs on Linux)");
            issues += 1;
        }
    }
    #[cfg(target_os = "macos")]
    {
        if std::env::consts::ARCH == "aarch64" {
            println!("  hypervisor: Apple Silicon HVF expected (agent machine)");
        } else {
            println!("  hypervisor: unsupported arch on macOS (Apple Silicon only)");
            issues += 1;
        }
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        println!("  platform: unsupported for agent (Linux KVM or macOS arm64)");
        issues += 1;
    }

    println!("  agent runtime: microsandbox Rust SDK only (embedded)");
    if let Some(ep) = mc2_metrics::otlp_endpoint_from_env() {
        println!("  otlp: {ep}");
    } else {
        println!("  otlp: unset (optional; MC2_OTLP_ENDPOINT or OTEL_EXPORTER_OTLP_ENDPOINT)");
    }

    if args.msb {
        match std::process::Command::new("msb").arg("version").output() {
            Ok(o) if o.status.success() => {
                let v = String::from_utf8_lossy(&o.stdout);
                let e = String::from_utf8_lossy(&o.stderr);
                println!(
                    "  msb CLI: ok — {} (optional host tooling; not used by agent)",
                    v.trim().lines().next().unwrap_or(e.trim())
                );
            }
            Ok(o) => println!("  msb CLI: failed ({}) — optional", o.status),
            Err(e) => println!("  msb CLI: not on PATH ({e}) — optional"),
        }
    }

    if issues == 0 {
        println!("doctor: ok");
        Ok(0)
    } else {
        println!("doctor: {issues} issue(s) — server-only hosts can ignore hypervisor warnings");
        Ok(1)
    }
}

fn operator_get(
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

fn operator_post(
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

async fn secret_set(args: SecretSetArgs) -> Result<()> {
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
        args.op.api.trim_end_matches('/'),
        urlencoding_simple(&args.name)
    );
    let client = reqwest::Client::new();
    let mut req = client.put(&url);
    if let Some(t) = args.op.token.as_deref().filter(|s| !s.is_empty()) {
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
        bail!("secret set failed: {status} {body}");
    }
    // Body is metadata only (no value)
    println!("{body}");
    Ok(())
}

async fn secret_ls(args: OperatorArgs) -> Result<()> {
    let url = format!("{}/v1/secrets", args.api.trim_end_matches('/'));
    let client = reqwest::Client::new();
    let res = operator_get(&client, &url, args.token.as_deref())
        .send()
        .await
        .with_context(|| format!("GET {url}"))?;
    let status = res.status();
    let body = res.text().await.unwrap_or_default();
    if !status.is_success() {
        bail!("secret ls failed: {status} {body}");
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

async fn secret_rm(args: SecretRmArgs) -> Result<()> {
    let url = format!(
        "{}/v1/secrets/{}",
        args.op.api.trim_end_matches('/'),
        urlencoding_simple(&args.name)
    );
    let client = reqwest::Client::new();
    let mut req = client.delete(&url);
    if let Some(t) = args.op.token.as_deref().filter(|s| !s.is_empty()) {
        req = req.bearer_auth(t);
    }
    let res = req.send().await.with_context(|| format!("DELETE {url}"))?;
    let status = res.status();
    if status == reqwest::StatusCode::NO_CONTENT || status.is_success() {
        println!("deleted {}", args.name);
        return Ok(());
    }
    let body = res.text().await.unwrap_or_default();
    bail!("secret rm failed: {status} {body}");
}

async fn ssh_key_add(args: SshKeyAddArgs) -> Result<()> {
    let public_key = if let Some(k) = args.key {
        k
    } else if let Some(path) = args.file {
        std::fs::read_to_string(&path).with_context(|| format!("read {path}"))?
    } else {
        bail!("pass --key or --file");
    };
    let url = format!(
        "{}/v1/ssh/keys/{}",
        args.op.api.trim_end_matches('/'),
        urlencoding_simple(&args.name)
    );
    let client = reqwest::Client::new();
    let mut req = client.put(&url).json(&serde_json::json!({
        "publicKey": public_key.trim()
    }));
    if let Some(t) = args.op.token.as_deref().filter(|s| !s.is_empty()) {
        req = req.bearer_auth(t);
    }
    let res = req.send().await.context("PUT ssh key")?;
    let status = res.status();
    let body = res.text().await.unwrap_or_default();
    if !status.is_success() {
        bail!("ssh-key add failed: {status} {body}");
    }
    println!("{body}");
    Ok(())
}

async fn ssh_key_ls(args: OperatorArgs) -> Result<()> {
    let url = format!("{}/v1/ssh/keys", args.api.trim_end_matches('/'));
    let client = reqwest::Client::new();
    let mut req = client.get(&url);
    if let Some(t) = args.token.as_deref().filter(|s| !s.is_empty()) {
        req = req.bearer_auth(t);
    }
    let res = req.send().await.context("GET ssh keys")?;
    let status = res.status();
    let body = res.text().await.unwrap_or_default();
    if !status.is_success() {
        bail!("ssh-key ls failed: {status} {body}");
    }
    println!("{body}");
    Ok(())
}

async fn ssh_key_rm(args: SshKeyRmArgs) -> Result<()> {
    let url = format!(
        "{}/v1/ssh/keys/{}",
        args.op.api.trim_end_matches('/'),
        urlencoding_simple(&args.name)
    );
    let client = reqwest::Client::new();
    let mut req = client.delete(&url);
    if let Some(t) = args.op.token.as_deref().filter(|s| !s.is_empty()) {
        req = req.bearer_auth(t);
    }
    let res = req.send().await.context("DELETE ssh key")?;
    let status = res.status();
    if status == reqwest::StatusCode::NO_CONTENT || status.is_success() {
        println!("deleted {}", args.name);
        return Ok(());
    }
    let body = res.text().await.unwrap_or_default();
    bail!("ssh-key rm failed: {status} {body}");
}

async fn ssh_endpoints_ls(args: OperatorArgs) -> Result<()> {
    let url = format!("{}/v1/ssh/endpoints", args.api.trim_end_matches('/'));
    let client = reqwest::Client::new();
    let mut req = client.get(&url);
    if let Some(t) = args.token.as_deref().filter(|s| !s.is_empty()) {
        req = req.bearer_auth(t);
    }
    let res = req.send().await.context("GET ssh endpoints")?;
    let status = res.status();
    let body = res.text().await.unwrap_or_default();
    if !status.is_success() {
        bail!("ssh ls failed: {status} {body}");
    }
    println!("{body}");
    Ok(())
}

async fn ssh_instance_show(args: SshInstanceArgs) -> Result<()> {
    let url = format!(
        "{}/v1/instances/{}/ssh",
        args.op.api.trim_end_matches('/'),
        urlencoding_simple(&args.id)
    );
    let client = reqwest::Client::new();
    let mut req = client.get(&url);
    if let Some(t) = args.op.token.as_deref().filter(|s| !s.is_empty()) {
        req = req.bearer_auth(t);
    }
    let res = req.send().await.context("GET instance ssh")?;
    let status = res.status();
    let body = res.text().await.unwrap_or_default();
    if !status.is_success() {
        bail!("ssh show failed: {status} {body}");
    }
    println!("{body}");
    Ok(())
}

async fn ssh_instance_open(args: SshOpenArgs) -> Result<()> {
    let url = format!(
        "{}/v1/instances/{}/ssh",
        args.op.api.trim_end_matches('/'),
        urlencoding_simple(&args.id)
    );
    let client = reqwest::Client::new();
    let mut req = client.put(&url).json(&serde_json::json!({
        "enabled": true,
        "bind": args.bind,
        "port": args.port,
        "authorizedKeys": args.keys,
    }));
    if let Some(t) = args.op.token.as_deref().filter(|s| !s.is_empty()) {
        req = req.bearer_auth(t);
    }
    let res = req.send().await.context("PUT instance ssh open")?;
    let status = res.status();
    let body = res.text().await.unwrap_or_default();
    if !status.is_success() {
        bail!("ssh open failed: {status} {body}");
    }
    println!("{body}");
    Ok(())
}

async fn ssh_instance_close(args: SshInstanceArgs) -> Result<()> {
    let url = format!(
        "{}/v1/instances/{}/ssh",
        args.op.api.trim_end_matches('/'),
        urlencoding_simple(&args.id)
    );
    let client = reqwest::Client::new();
    let mut req = client.put(&url).json(&serde_json::json!({
        "enabled": false,
        "authorizedKeys": []
    }));
    if let Some(t) = args.op.token.as_deref().filter(|s| !s.is_empty()) {
        req = req.bearer_auth(t);
    }
    let res = req.send().await.context("PUT instance ssh close")?;
    let status = res.status();
    let body = res.text().await.unwrap_or_default();
    if !status.is_success() {
        bail!("ssh close failed: {status} {body}");
    }
    println!("{body}");
    Ok(())
}

fn urlencoding_simple(s: &str) -> String {
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

async fn apply_cmd(args: ApplyArgs) -> Result<()> {
    // Token optional when server was bootstrapped with --no-auth.
    let yaml =
        std::fs::read_to_string(&args.file).with_context(|| format!("read {}", args.file))?;
    let url = format!("{}/v1/stacks:apply", args.op.api.trim_end_matches('/'));
    let client = reqwest::Client::new();
    let res = operator_post(&client, &url, args.op.token.as_deref())
        .json(&serde_json::json!({ "yaml": yaml }))
        .send()
        .await
        .with_context(|| format!("POST {url}"))?;
    let status = res.status();
    let body = res.text().await.unwrap_or_default();
    if !status.is_success() {
        bail!("apply failed: {status} {body}");
    }
    println!("{body}");
    Ok(())
}

async fn ps_cmd(args: OperatorArgs) -> Result<()> {
    let url = format!("{}/v1/instances", args.api.trim_end_matches('/'));
    let client = reqwest::Client::new();
    let res = operator_get(&client, &url, args.token.as_deref())
        .send()
        .await
        .with_context(|| format!("GET {url}"))?;
    let status = res.status();
    let body = res.text().await.unwrap_or_default();
    if !status.is_success() {
        bail!("ps failed: {status} {body}");
    }
    let instances: Vec<mc2_store::InstanceRecord> =
        serde_json::from_str(&body).with_context(|| format!("parse: {body}"))?;
    if instances.is_empty() {
        println!("No instances.");
        return Ok(());
    }
    println!(
        "{:<8} {:<12} {:<6} {:<10} {:<36} ID",
        "STACK", "SERVICE", "ORD", "PHASE", "NODE"
    );
    for i in instances {
        println!(
            "{:<8} {:<12} {:<6} {:<10} {:<36} {}",
            i.stack,
            i.service,
            i.ordinal,
            i.phase,
            i.node_id.unwrap_or_else(|| "-".into()),
            i.id
        );
    }
    Ok(())
}

async fn node_ls(args: OperatorArgs) -> Result<()> {
    let url = format!("{}/v1/nodes", args.api.trim_end_matches('/'));
    let client = reqwest::Client::new();
    let res = operator_get(&client, &url, args.token.as_deref())
        .send()
        .await
        .with_context(|| format!("GET {url}"))?;
    let status = res.status();
    let body = res.text().await.unwrap_or_default();
    if !status.is_success() {
        bail!("GET {url} failed: {status} {body}");
    }
    let nodes: Vec<mc2_api::NodeView> =
        serde_json::from_str(&body).with_context(|| format!("parse nodes JSON: {body}"))?;

    if nodes.is_empty() {
        println!("No nodes registered.");
        return Ok(());
    }

    println!(
        "{:<36} {:<16} {:<10} {:>4} {:>8} {:<10} LAST_HEARTBEAT",
        "ID", "NAME", "STATUS", "CPU", "MEM_MiB", "ARCH"
    );
    for n in nodes {
        println!(
            "{:<36} {:<16} {:<10} {:>4} {:>8} {:<10} {}",
            n.id,
            n.name,
            n.status,
            n.cpus,
            n.memory_mib,
            n.arch,
            n.last_heartbeat.unwrap_or_else(|| "-".into())
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_parses_help() {
        Cli::command().debug_assert();
    }
}
