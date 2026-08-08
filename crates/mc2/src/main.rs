//! MicroCommandControl (MC2) — YAML-driven orchestration for microsandbox.
//!
//! ```text
//! mc2 server   # the orchestrator (single process)
//! mc2 apply    # operator: apply a stack
//! mc2 node ls  # show the local node
//! ```

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

#[derive(Debug, Parser)]
#[command(
    name = "mc2",
    about = "MC2 (MicroCommandControl) — YAML-driven orchestration for microsandbox microVMs",
    long_about = "MC2 (MicroCommandControl) runs microsandbox microVMs from desired stack YAML. \
                  One binary: the orchestrator (server) and operator CLI.",
    version
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Run the MC2 orchestrator (single process)
    Server(mc2_server::ServerArgs),
    /// Apply a stack YAML (desired state)
    Apply(ApplyArgs),
    /// Cluster status (health + version + counts)
    Status(StatusArgs),
    /// Node operations
    Node(NodeCmd),
    /// List instances (desired sandboxes)
    Ps(PsArgs),
    /// Observed fabric status for one instance
    Fabric(FabricArgs),
    /// Desired ingress routes
    Ingress(IngressArgs),
    /// Secrets (encrypted at rest; values never listed)
    Secret(SecretCmd),
    /// SSH authorized keys + endpoints
    Ssh(SshCmd),
    /// Check host readiness (hypervisor / msb / paths)
    Doctor(DoctorArgs),
    /// Generate shell completions
    Completions(CompletionsArgs),
    /// Show version info
    Version,
}

#[derive(Debug, Parser)]
struct SshCmd {
    #[command(subcommand)]
    command: SshCommands,
}

#[derive(Debug, Subcommand)]
enum SshCommands {
    /// Manage authorized public keys
    Key(SshKeyCmd),
    /// List open SSH endpoints
    #[command(name = "ls", alias = "list")]
    Ls(ListArgs),
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
    /// Show one authorized public key
    Show(SshKeyShowArgs),
    #[command(name = "ls", alias = "list")]
    Ls(ListArgs),
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
    Ls(ListArgs),
    #[command(name = "rm", alias = "delete")]
    Rm(SecretRmArgs),
}

#[derive(Debug, Parser)]
struct SecretSetArgs {
    /// Secret name
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
    /// Path to stack YAML. Boilerplate is optional: apiVersion, kind, and
    /// metadata.name (defaults to the file name) are filled in when missing.
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
    Ls(ListArgs),
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

/// Output format for listing commands.
#[derive(Debug, Clone, Copy, Default, ValueEnum)]
pub enum OutputFormat {
    /// Human-readable table (default)
    #[default]
    Table,
    /// Machine-readable JSON
    Json,
}

/// Common listing args: connection + output format.
#[derive(Debug, Parser)]
struct ListArgs {
    #[command(flatten)]
    op: OperatorArgs,
    /// Output format
    #[arg(short = 'o', long, value_enum, default_value_t = OutputFormat::Table)]
    output: OutputFormat,
}

#[derive(Debug, Parser)]
struct StatusArgs {
    #[command(flatten)]
    op: OperatorArgs,
    /// Output format
    #[arg(short = 'o', long, value_enum, default_value_t = OutputFormat::Table)]
    output: OutputFormat,
}

#[derive(Debug, Parser)]
struct PsArgs {
    #[command(flatten)]
    op: OperatorArgs,
    /// Output format
    #[arg(short = 'o', long, value_enum, default_value_t = OutputFormat::Table)]
    output: OutputFormat,
    /// Only instances of this stack
    #[arg(long)]
    stack: Option<String>,
    /// Only instances of this service
    #[arg(long)]
    service: Option<String>,
}

#[derive(Debug, Parser)]
struct FabricArgs {
    /// Instance id, or <stack>/<service>/<ordinal> (e.g. demo/web/0)
    id: String,
    #[command(flatten)]
    op: OperatorArgs,
    /// Output format
    #[arg(short = 'o', long, value_enum, default_value_t = OutputFormat::Table)]
    output: OutputFormat,
}

#[derive(Debug, Parser)]
struct IngressArgs {
    #[command(flatten)]
    op: OperatorArgs,
    /// Output format
    #[arg(short = 'o', long, value_enum, default_value_t = OutputFormat::Table)]
    output: OutputFormat,
}

#[derive(Debug, Parser)]
struct SshKeyShowArgs {
    name: String,
    #[command(flatten)]
    op: OperatorArgs,
}

#[derive(Debug, Parser)]
struct CompletionsArgs {
    /// Shell to generate completions for
    #[arg(value_enum)]
    shell: clap_complete::Shell,
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

        Commands::Apply(args) => apply_cmd(args).await?,
        Commands::Status(args) => status_cmd(args).await?,
        Commands::Node(NodeCmd {
            command: NodeCommands::Ls(args),
        }) => node_ls(args).await?,
        Commands::Ps(args) => ps_cmd(args).await?,
        Commands::Fabric(args) => fabric_cmd(args).await?,
        Commands::Ingress(args) => ingress_cmd(args).await?,
        Commands::Secret(SecretCmd { command }) => match command {
            SecretCommands::Set(a) => secret_set(a).await?,
            SecretCommands::Ls(a) => secret_ls(a).await?,
            SecretCommands::Rm(a) => secret_rm(a).await?,
        },
        Commands::Ssh(SshCmd { command }) => match command {
            SshCommands::Key(SshKeyCmd { command }) => match command {
                SshKeyCommands::Add(a) => ssh_key_add(a).await?,
                SshKeyCommands::Show(a) => ssh_key_show(a).await?,
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
        Commands::Completions(args) => completions_cmd(args)?,
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

/// Returns process exit code (0 = ok; 1 = hypervisor missing).
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
            println!("  /dev/kvm: MISSING (required for microVMs on Linux)");
            issues += 1;
        }
    }
    #[cfg(target_os = "macos")]
    {
        if std::env::consts::ARCH == "aarch64" {
            println!("  hypervisor: Apple Silicon HVF expected");
        } else {
            println!("  hypervisor: unsupported arch on macOS (Apple Silicon only)");
            issues += 1;
        }
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        println!("  platform: unsupported for microVMs (Linux KVM or macOS arm64)");
        issues += 1;
    }

    println!("  runtime: microsandbox Rust SDK (embedded)");
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
                    "  msb CLI: ok — {} (optional host tooling)",
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

/// Unified non-2xx error: prefer the server's `error` field over raw JSON.
fn api_error(op: &str, status: reqwest::StatusCode, body: &str) -> anyhow::Error {
    let detail = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(str::to_string))
        .unwrap_or_else(|| body.to_string());
    anyhow::anyhow!("{op} failed: {status}: {detail}")
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
        return Err(api_error("secret set", status, &body));
    }
    // Body is metadata only (no value)
    println!("{body}");
    Ok(())
}

async fn secret_ls(args: ListArgs) -> Result<()> {
    let url = format!("{}/v1/secrets", args.op.api.trim_end_matches('/'));
    let client = reqwest::Client::new();
    let res = operator_get(&client, &url, args.op.token.as_deref())
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
    Err(api_error("secret rm", status, &body))
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
        return Err(api_error("ssh key add", status, &body));
    }
    println!("{body}");
    Ok(())
}

async fn ssh_key_ls(args: ListArgs) -> Result<()> {
    let url = format!("{}/v1/ssh/keys", args.op.api.trim_end_matches('/'));
    let client = reqwest::Client::new();
    let mut req = client.get(&url);
    if let Some(t) = args.op.token.as_deref().filter(|s| !s.is_empty()) {
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
    Err(api_error("ssh key rm", status, &body))
}

async fn ssh_endpoints_ls(args: ListArgs) -> Result<()> {
    let url = format!("{}/v1/ssh/endpoints", args.op.api.trim_end_matches('/'));
    let client = reqwest::Client::new();
    let mut req = client.get(&url);
    if let Some(t) = args.op.token.as_deref().filter(|s| !s.is_empty()) {
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
        return Err(api_error("ssh show", status, &body));
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
        return Err(api_error("ssh open", status, &body));
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
        return Err(api_error("ssh close", status, &body));
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
    let raw = std::fs::read_to_string(&args.file).with_context(|| format!("read {}", args.file))?;
    let yaml = fill_stack_defaults(&raw, &args.file);
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
        return Err(api_error("apply", status, &body));
    }
    println!("{body}");
    Ok(())
}

/// Fill omissible boilerplate for local DX: `apiVersion`, `kind`, and
/// `metadata.name` (derived from the file name). Only missing fields are
/// filled; present values are never rewritten (wrong values still fail server
/// validation loudly). Non-YAML or non-mapping input passes through for the
/// server to reject. Note: filling re-serializes the document, dropping YAML
/// comments — complete documents are returned verbatim.
fn fill_stack_defaults(raw: &str, file_path: &str) -> String {
    let Ok(mut doc) = serde_yaml::from_str::<serde_yaml::Value>(raw) else {
        return raw.to_string();
    };
    let Some(map) = doc.as_mapping_mut() else {
        return raw.to_string();
    };
    let mut changed = ensure_scalar(map, "apiVersion", mc2_api::API_VERSION);
    changed |= ensure_scalar(map, "kind", "Stack");
    changed |= ensure_stack_name(map, file_path);
    if changed {
        serde_yaml::to_string(&doc).unwrap_or_else(|_| raw.to_string())
    } else {
        raw.to_string()
    }
}

fn ensure_scalar(map: &mut serde_yaml::Mapping, key: &str, value: &str) -> bool {
    let key = serde_yaml::Value::String(key.into());
    if map
        .get(&key)
        .and_then(|v| v.as_str())
        .is_some_and(|s| !s.trim().is_empty())
    {
        return false;
    }
    map.insert(key, serde_yaml::Value::String(value.into()));
    true
}

fn ensure_stack_name(map: &mut serde_yaml::Mapping, file_path: &str) -> bool {
    let meta_key = serde_yaml::Value::String("metadata".into());
    let name_key = serde_yaml::Value::String("name".into());
    let has_name = |meta: &serde_yaml::Mapping| {
        meta.get(&name_key)
            .and_then(|v| v.as_str())
            .is_some_and(|s| !s.trim().is_empty())
    };
    match map.get_mut(&meta_key) {
        Some(serde_yaml::Value::Mapping(meta)) => {
            if has_name(meta) {
                return false;
            }
            meta.insert(
                name_key,
                serde_yaml::Value::String(stack_name_from_path(file_path)),
            );
            true
        }
        // metadata present but malformed: leave it for server validation.
        Some(_) => false,
        None => {
            let mut meta = serde_yaml::Mapping::new();
            meta.insert(
                name_key,
                serde_yaml::Value::String(stack_name_from_path(file_path)),
            );
            map.insert(meta_key, serde_yaml::Value::Mapping(meta));
            true
        }
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

async fn ps_cmd(args: PsArgs) -> Result<()> {
    let url = format!("{}/v1/instances", args.op.api.trim_end_matches('/'));
    let client = reqwest::Client::new();
    let res = operator_get(&client, &url, args.op.token.as_deref())
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

async fn node_ls(args: ListArgs) -> Result<()> {
    let url = format!("{}/v1/nodes", args.op.api.trim_end_matches('/'));
    let client = reqwest::Client::new();
    let res = operator_get(&client, &url, args.op.token.as_deref())
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

/// Cluster status: health + version + counts.
async fn status_cmd(args: StatusArgs) -> Result<()> {
    let base = args.op.api.trim_end_matches('/');
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
    let res = operator_get(
        &client,
        &format!("{base}/v1/status"),
        args.op.token.as_deref(),
    )
    .send()
    .await
    .with_context(|| format!("GET {base}/v1/status"))?;
    let status = res.status();
    let body = res.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(api_error("status", status, &body));
    }
    if matches!(args.output, OutputFormat::Json) {
        println!("{body}");
        return Ok(());
    }
    let v: serde_json::Value = serde_json::from_str(&body)?;
    println!("mc2 {} — {}", v["version"].as_str().unwrap_or("?"), base);
    println!("  api schema: {}", v["api_version"].as_str().unwrap_or("?"));
    println!(
        "  nodes: {}/{} ready",
        v["nodes_ready"].as_u64().unwrap_or(0),
        v["nodes_total"].as_u64().unwrap_or(0)
    );
    println!("  stacks: {}", v["stacks"].as_u64().unwrap_or(0));
    println!("  instances: {}", v["instances"].as_u64().unwrap_or(0));
    if let Some(msg) = v["message"].as_str() {
        println!("  message: {msg}");
    }
    Ok(())
}

/// Resolve `<stack>/<service>/<ordinal>` to an instance id; pass UUIDs through.
async fn resolve_instance_id(op: &OperatorArgs, id_or_ref: &str) -> Result<String> {
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
    let url = format!("{}/v1/instances", op.api.trim_end_matches('/'));
    let client = reqwest::Client::new();
    let res = operator_get(&client, &url, op.token.as_deref())
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

/// Observed fabric status for one instance.
async fn fabric_cmd(args: FabricArgs) -> Result<()> {
    let id = resolve_instance_id(&args.op, &args.id).await?;
    let url = format!(
        "{}/v1/instances/{}/fabric",
        args.op.api.trim_end_matches('/'),
        urlencoding_simple(&id)
    );
    let client = reqwest::Client::new();
    let res = operator_get(&client, &url, args.op.token.as_deref())
        .send()
        .await
        .with_context(|| format!("GET {url}"))?;
    let status = res.status();
    let body = res.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(api_error("fabric", status, &body));
    }
    if matches!(args.output, OutputFormat::Json) {
        println!("{body}");
        return Ok(());
    }
    let v: serde_json::Value = serde_json::from_str(&body)?;
    println!(
        "instance {} — fabric {}",
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
            "  allow {}:{} [{}]",
            edge["toService"].as_str().unwrap_or("-"),
            edge["port"].as_u64().unwrap_or(0),
            edge["phase"].as_str().unwrap_or("-")
        );
    }
    Ok(())
}

/// Desired ingress routes.
async fn ingress_cmd(args: IngressArgs) -> Result<()> {
    let url = format!("{}/v1/ingress", args.op.api.trim_end_matches('/'));
    let client = reqwest::Client::new();
    let res = operator_get(&client, &url, args.op.token.as_deref())
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
    println!(
        "{:<24} {:<12} {:<10} {:<8} {:<8} ID",
        "HOST", "PATH", "SERVICE", "GUEST", "HOST_PORT"
    );
    for r in routes {
        println!(
            "{:<24} {:<12} {:<10} {:<8} {:<8} {}",
            r["host"].as_str().unwrap_or("-"),
            r["path"].as_str().unwrap_or("/"),
            r["service"].as_str().unwrap_or("-"),
            r["guestPort"].as_u64().unwrap_or(0),
            r["hostPort"].as_u64().unwrap_or(0),
            r["id"].as_str().unwrap_or("-")
        );
    }
    Ok(())
}

/// Show one authorized public key.
async fn ssh_key_show(args: SshKeyShowArgs) -> Result<()> {
    let url = format!(
        "{}/v1/ssh/keys/{}",
        args.op.api.trim_end_matches('/'),
        urlencoding_simple(&args.name)
    );
    let client = reqwest::Client::new();
    let mut req = client.get(&url);
    if let Some(t) = args.op.token.as_deref().filter(|s| !s.is_empty()) {
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

/// Generate shell completions to stdout.
fn completions_cmd(args: CompletionsArgs) -> Result<()> {
    use clap::CommandFactory;
    let mut cmd = Cli::command();
    clap_complete::generate(args.shell, &mut cmd, "mc2", &mut std::io::stdout());
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

    #[test]
    fn minimal_stack_gets_boilerplate_and_passes_validation() {
        let raw = "services:\n  web:\n    image: alpine:3.20\n";
        let filled = fill_stack_defaults(raw, "demo.yaml");
        let doc = mc2_api::parse_stack_yaml(&filled).unwrap();
        assert_eq!(doc.metadata.name, "demo");
        assert_eq!(doc.api_version, mc2_api::API_VERSION);
        assert_eq!(doc.kind, "Stack");
    }

    #[test]
    fn complete_stack_passes_through_verbatim() {
        let raw = "apiVersion: mc2/v1\nkind: Stack\nmetadata:\n  name: x\nservices:\n  web:\n    image: alpine:3.20\n";
        assert_eq!(fill_stack_defaults(raw, "whatever.yaml"), raw);
    }

    #[test]
    fn metadata_without_name_gets_file_stem() {
        let raw = "apiVersion: mc2/v1\nkind: Stack\nmetadata: {}\nservices:\n  web:\n    image: alpine:3.20\n";
        let doc = mc2_api::parse_stack_yaml(&fill_stack_defaults(raw, "My Stack!.yaml")).unwrap();
        assert_eq!(doc.metadata.name, "my-stack");
    }

    #[test]
    fn generic_stack_yaml_takes_parent_dir_name() {
        let raw = "services:\n  web:\n    image: alpine:3.20\n";
        let doc = mc2_api::parse_stack_yaml(&fill_stack_defaults(
            raw,
            "examples/01-hello-service/stack.yaml",
        ))
        .unwrap();
        assert_eq!(doc.metadata.name, "01-hello-service");
    }

    #[test]
    fn wrong_api_version_is_left_for_server() {
        let raw = "apiVersion: wrong/v9\nkind: Stack\nmetadata:\n  name: x\nservices:\n  web:\n    image: alpine\n";
        assert_eq!(fill_stack_defaults(raw, "x.yaml"), raw);
    }

    #[test]
    fn invalid_yaml_passes_through() {
        let raw = "not: [valid";
        assert_eq!(fill_stack_defaults(raw, "x.yaml"), raw);
    }

    #[test]
    fn api_error_prefers_server_error_field() {
        let e = api_error(
            "fabric",
            reqwest::StatusCode::NOT_FOUND,
            r#"{"error":"no fabric status"}"#,
        );
        assert_eq!(
            e.to_string(),
            "fabric failed: 404 Not Found: no fabric status"
        );
    }

    #[test]
    fn api_error_falls_back_to_raw_body() {
        let e = api_error("ps", reqwest::StatusCode::INTERNAL_SERVER_ERROR, "boom");
        assert!(e.to_string().contains("boom"));
    }

    #[tokio::test]
    async fn uuid_refs_pass_through_without_lookup() {
        let op = OperatorArgs {
            api: "http://127.0.0.1:1".into(),
            token: None,
        };
        // No slash → no network call, returned verbatim.
        let id = resolve_instance_id(&op, "6e6a2d3f-b4dc-4c09-a317-4aac91ff0c99")
            .await
            .unwrap();
        assert_eq!(id, "6e6a2d3f-b4dc-4c09-a317-4aac91ff0c99");
    }
}
