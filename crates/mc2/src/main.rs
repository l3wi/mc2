//! MicroCommandControl (MC2) — YAML-driven orchestration for microsandbox.
//!
//! ```text
//! mc2 server   # the orchestrator (single process)
//! mc2 apply    # operator: apply a stack
//! mc2 node ls  # show the local node
//! ```

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use context::{load as load_config, resolve, save, ClientMode, Conn};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

mod context;
mod prompt;
mod setup;

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
    /// Bring up a stack: publish desired state and converge (idempotent)
    Up(UpArgs),
    /// Tear down a stack (instances + definition; volumes retained)
    Down(DownArgs),
    /// Tear down a stack, optionally deleting its named volumes
    Rm(RmArgs),
    /// Validate and print a normalized stack config
    Config(ConfigArgs),
    /// Run a command inside an instance's sandbox
    Exec(ExecArgs),
    /// Print recent sandbox logs for an instance
    Logs(LogsArgs),
    /// Cluster status (health + version + counts)
    Status(StatusArgs),
    /// Node operations
    Node(NodeCmd),
    /// List instances (desired sandboxes)
    Ps(PsArgs),
    /// Server-wide network membership summary (default + named networks)
    Network(NetworkArgs),
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
    /// Manage named API contexts (local/remote)
    Context(ContextCmd),
    /// Interactive setup wizard (server / client)
    Setup(SetupCmd),
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
struct UpArgs {
    /// Path to stack YAML. Boilerplate is optional: apiVersion, kind, and
    /// metadata.name (defaults to the file name) are filled in when missing.
    #[arg(short = 'f', long = "file")]
    file: String,

    #[command(flatten)]
    op: OperatorArgs,
}

#[derive(Debug, Parser)]
struct DownArgs {
    /// Stack name
    stack: String,

    #[command(flatten)]
    op: OperatorArgs,
}

#[derive(Debug, Parser)]
struct RmArgs {
    /// Stack name
    stack: String,

    /// Also delete the stack's named volumes (default: retained)
    #[arg(long)]
    volumes: bool,

    #[command(flatten)]
    op: OperatorArgs,
}

#[derive(Debug, Parser)]
struct ConfigArgs {
    /// Path to stack YAML
    #[arg(short = 'f', long = "file")]
    file: String,
}

#[derive(Debug, Parser)]
struct ExecArgs {
    /// Instance id, or <stack>/<service>/<ordinal> (e.g. demo/web/0)
    instance: String,
    /// Command to run in the sandbox
    #[arg(required = true, num_args = 1..)]
    cmd: Vec<String>,
    #[command(flatten)]
    op: OperatorArgs,
}

#[derive(Debug, Parser)]
struct LogsArgs {
    /// Instance id, or <stack>/<service>/<ordinal> (e.g. demo/web/0)
    instance: String,
    /// Show only the last N entries
    #[arg(long)]
    tail: Option<usize>,
    /// Keep streaming new entries as they arrive
    #[arg(long)]
    follow: bool,
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
    /// Control plane REST base URL (overrides MC2_API and the current context)
    #[arg(long, default_value = "", env = "MC2_API", hide_default_value = true)]
    api: String,

    /// Operator API bearer token
    #[arg(long, env = "MC2_API_KEY")]
    token: Option<String>,

    /// Named context from ~/.mc2/config.toml (overrides the current context)
    #[arg(long, env = "MC2_CONTEXT")]
    context: Option<String>,

    /// Allow plaintext http:// for a remote (non-loopback) control plane.
    /// Prefer https:// + TLS; the operator token travels unencrypted otherwise.
    #[arg(long)]
    allow_insecure_http: bool,
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
struct NetworkArgs {
    /// Network name, or <stack>/<service>/<ordinal> for per-instance connectivity (all networks when omitted)
    network: Option<String>,
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

#[derive(Debug, Parser)]
struct ContextCmd {
    #[command(subcommand)]
    command: ContextCommands,
}

#[derive(Debug, Subcommand)]
enum ContextCommands {
    /// List contexts (name, URL, mode) and the current one
    Ls,
    /// Switch the current context
    Use(UseContextArgs),
    /// Create or update a context (upsert)
    Set(SetContextArgs),
}

#[derive(Debug, Parser)]
struct UseContextArgs {
    /// Context name
    name: String,
}

#[derive(Debug, Parser)]
struct SetContextArgs {
    /// Context name
    name: String,
    /// Control plane REST base URL
    #[arg(long)]
    api: String,
    /// Operator API bearer token (stored in the 0600 config file)
    #[arg(long)]
    token: Option<String>,
}

#[derive(Debug, Parser)]
struct SetupCmd {
    #[command(subcommand)]
    command: Option<SetupCommands>,
}

#[derive(Debug, Subcommand)]
enum SetupCommands {
    /// Interactive server setup (run on the VPS)
    Server,
    /// Interactive client setup (connect to a server)
    Client,
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

        Commands::Up(mut args) => {
            resolve_op(&mut args.op)?;
            up_cmd(args).await?
        }
        Commands::Down(mut args) => {
            resolve_op(&mut args.op)?;
            down_cmd(args).await?
        }
        Commands::Rm(mut args) => {
            resolve_op(&mut args.op)?;
            rm_cmd(args).await?
        }
        Commands::Config(args) => config_cmd(args)?,
        Commands::Exec(mut args) => {
            resolve_op(&mut args.op)?;
            exec_cmd(args).await?
        }
        Commands::Logs(mut args) => {
            resolve_op(&mut args.op)?;
            logs_cmd(args).await?
        }
        Commands::Status(mut args) => {
            let conn = resolve_op(&mut args.op)?;
            status_cmd(args, &conn).await?
        }
        Commands::Node(NodeCmd {
            command: NodeCommands::Ls(mut args),
        }) => {
            resolve_op(&mut args.op)?;
            node_ls(args).await?
        }
        Commands::Ps(mut args) => {
            resolve_op(&mut args.op)?;
            ps_cmd(args).await?
        }
        Commands::Network(mut args) => {
            resolve_op(&mut args.op)?;
            network_cmd(args).await?
        }
        Commands::Ingress(mut args) => {
            resolve_op(&mut args.op)?;
            ingress_cmd(args).await?
        }
        Commands::Secret(SecretCmd { command }) => match command {
            SecretCommands::Set(mut a) => {
                resolve_op(&mut a.op)?;
                secret_set(a).await?
            }
            SecretCommands::Ls(mut a) => {
                resolve_op(&mut a.op)?;
                secret_ls(a).await?
            }
            SecretCommands::Rm(mut a) => {
                resolve_op(&mut a.op)?;
                secret_rm(a).await?
            }
        },
        Commands::Ssh(SshCmd { command }) => match command {
            SshCommands::Key(SshKeyCmd { command }) => match command {
                SshKeyCommands::Add(mut a) => {
                    resolve_op(&mut a.op)?;
                    ssh_key_add(a).await?
                }
                SshKeyCommands::Show(mut a) => {
                    resolve_op(&mut a.op)?;
                    ssh_key_show(a).await?
                }
                SshKeyCommands::Ls(mut a) => {
                    resolve_op(&mut a.op)?;
                    ssh_key_ls(a).await?
                }
                SshKeyCommands::Rm(mut a) => {
                    resolve_op(&mut a.op)?;
                    ssh_key_rm(a).await?
                }
            },
            SshCommands::Ls(mut a) => {
                resolve_op(&mut a.op)?;
                ssh_endpoints_ls(a).await?
            }
            SshCommands::Show(mut a) => {
                resolve_op(&mut a.op)?;
                ssh_instance_show(a).await?
            }
            SshCommands::Open(mut a) => {
                resolve_op(&mut a.op)?;
                ssh_instance_open(a).await?
            }
            SshCommands::Close(mut a) => {
                resolve_op(&mut a.op)?;
                ssh_instance_close(a).await?
            }
        },
        Commands::Doctor(args) => {
            let code = doctor_cmd(args)?;
            if code != 0 {
                std::process::exit(code);
            }
        }
        Commands::Completions(args) => completions_cmd(args)?,
        Commands::Context(ContextCmd { command }) => match command {
            ContextCommands::Ls => context_ls()?,
            ContextCommands::Use(a) => context_use(&a.name)?,
            ContextCommands::Set(a) => context_set(&a.name, &a.api, a.token.as_deref())?,
        },
        Commands::Setup(args) => setup_cmd(args).await?,
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

/// Resolve the effective connection (flags > env > context > current > default),
/// enforce the remote-plaintext guard, then rewrite `op` so downstream handlers
/// see the final URL + token. Returns the resolved connection for mode-aware
/// commands (`mc2 status`).
fn resolve_op(op: &mut OperatorArgs) -> Result<Conn> {
    let allow_insecure_http =
        op.allow_insecure_http || context::env_truthy("MC2_ALLOW_INSECURE_HTTP");
    let cfg = load_config()?;
    let api_flag = if op.api.is_empty() {
        None
    } else {
        Some(op.api.as_str())
    };
    let conn = resolve(
        &cfg,
        api_flag,
        op.token.as_deref(),
        op.context.as_deref(),
        allow_insecure_http,
    )?;
    op.api = conn.url.clone();
    op.token = conn.token.clone();
    Ok(conn)
}

fn context_set(name: &str, url: &str, token: Option<&str>) -> Result<()> {
    let mut cfg = load_config()?;
    let url = url.trim_end_matches('/').to_string();
    if context::mode_of(&url) == ClientMode::Remote
        && url.to_ascii_lowercase().starts_with("http://")
    {
        eprintln!(
            "warning: context '{name}' uses plaintext http:// for a remote endpoint; \
             connecting will require --allow-insecure-http / MC2_ALLOW_INSECURE_HTTP=1"
        );
    }
    cfg.set_context(name, &url, token);
    save(&cfg)?;
    println!("context '{name}' set ({url})");
    Ok(())
}

fn context_use(name: &str) -> Result<()> {
    let mut cfg = load_config()?;
    let url = cfg
        .contexts
        .get(name)
        .map(|c| c.url.clone())
        .ok_or_else(|| {
            let available = if cfg.contexts.is_empty() {
                "(none configured)".to_string()
            } else {
                cfg.contexts.keys().cloned().collect::<Vec<_>>().join(", ")
            };
            anyhow::anyhow!("context '{name}' not found (available: {available})")
        })?;
    cfg.set_current(name)?;
    save(&cfg)?;
    println!("using context '{name}' ({url})");
    Ok(())
}

fn context_ls() -> Result<()> {
    let cfg = load_config()?;
    if cfg.contexts.is_empty() {
        println!(
            "No contexts configured. Default: local {}",
            context::DEFAULT_API_URL
        );
        println!("Create one: mc2 context set <name> --api <url> [--token <key>]");
        return Ok(());
    }
    println!("{:<3} {:<16} {:<44} MODE", "CUR", "NAME", "URL");
    for (name, entry) in &cfg.contexts {
        let cur = if cfg.current.as_deref() == Some(name.as_str()) {
            "*"
        } else {
            ""
        };
        println!(
            "{cur:<3} {name:<16} {:<44} {}",
            entry.url,
            context::mode_of(&entry.url).as_str()
        );
    }
    match cfg.current.as_deref() {
        None => println!(
            "\nno current context; default: local {}",
            context::DEFAULT_API_URL
        ),
        Some(name) if !cfg.contexts.contains_key(name) => {
            println!("\ncurrent context '{name}' is not defined (config drift)")
        }
        Some(_) => {}
    }
    Ok(())
}

async fn setup_cmd(args: SetupCmd) -> Result<()> {
    match args.command {
        None => setup::run().await?,
        Some(SetupCommands::Server) => setup::run_server_wizard().await?,
        Some(SetupCommands::Client) => setup::run_client_wizard().await?,
    }
    Ok(())
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

async fn up_cmd(args: UpArgs) -> Result<()> {
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

/// Tear down a stack (instances + definition); named volumes retained.
async fn down_cmd(args: DownArgs) -> Result<()> {
    let url = format!(
        "{}/v1/stacks/{}",
        args.op.api.trim_end_matches('/'),
        urlencoding_simple(&args.stack)
    );
    let client = reqwest::Client::new();
    let mut req = client.delete(&url);
    if let Some(t) = args.op.token.as_deref().filter(|s| !s.is_empty()) {
        req = req.bearer_auth(t);
    }
    let res = req.send().await.with_context(|| format!("DELETE {url}"))?;
    let status = res.status();
    if status == reqwest::StatusCode::NO_CONTENT || status.is_success() {
        println!("stack '{}' down", args.stack);
        return Ok(());
    }
    let body = res.text().await.unwrap_or_default();
    Err(api_error("down", status, &body))
}

/// Tear down a stack; `--volumes` also deletes its named volumes.
async fn rm_cmd(args: RmArgs) -> Result<()> {
    let mut url = format!(
        "{}/v1/stacks/{}",
        args.op.api.trim_end_matches('/'),
        urlencoding_simple(&args.stack)
    );
    if args.volumes {
        url.push_str("?volumes=true");
    }
    let client = reqwest::Client::new();
    let mut req = client.delete(&url);
    if let Some(t) = args.op.token.as_deref().filter(|s| !s.is_empty()) {
        req = req.bearer_auth(t);
    }
    let res = req.send().await.with_context(|| format!("DELETE {url}"))?;
    let status = res.status();
    if status == reqwest::StatusCode::NO_CONTENT || status.is_success() {
        if args.volumes {
            println!("stack '{}' removed (volumes deleted)", args.stack);
        } else {
            println!("stack '{}' removed", args.stack);
        }
        return Ok(());
    }
    let body = res.text().await.unwrap_or_default();
    Err(api_error("rm", status, &body))
}

/// Validate and print the normalized stack config (what `mc2 up` would send).
fn config_cmd(args: ConfigArgs) -> Result<()> {
    let raw = std::fs::read_to_string(&args.file).with_context(|| format!("read {}", args.file))?;
    let yaml = fill_stack_defaults(&raw, &args.file);
    mc2_api::parse_stack_yaml(&yaml).map_err(|e| anyhow::anyhow!("invalid stack: {e}"))?;
    print!("{yaml}");
    Ok(())
}

/// Run a command inside the instance's sandbox; mirror output and exit with
/// the command's exit code. Piped stdin is forwarded to the sandbox.
async fn exec_cmd(args: ExecArgs) -> Result<()> {
    let base = args.op.api.trim_end_matches('/');
    let id = resolve_instance_id(&args.op, &args.instance).await?;
    let url = format!("{base}/v1/instances/{}/exec", urlencoding_simple(&id));
    let stdin = {
        use std::io::IsTerminal;
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
    if let Some(t) = args.op.token.as_deref().filter(|s| !s.is_empty()) {
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
async fn logs_cmd(args: LogsArgs) -> Result<()> {
    let base = args.op.api.trim_end_matches('/');
    let id = resolve_instance_id(&args.op, &args.instance).await?;
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
    if let Some(t) = args.op.token.as_deref().filter(|s| !s.is_empty()) {
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

/// Cluster status: health + version + counts, mode/context-aware.
async fn status_cmd(args: StatusArgs, conn: &Conn) -> Result<()> {
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

/// Server-wide network membership summary. `mc2 network` lists all networks;
/// `mc2 network <name>` shows one network's instances and their ports;
/// `mc2 network <stack>/<service>/<ordinal>` shows one instance's observed
/// network connectivity (exposes/edges).
async fn network_cmd(args: NetworkArgs) -> Result<()> {
    // An instance reference (contains `/`) shows observed per-instance connectivity.
    let Some(target) = args.network.clone() else {
        return network_summary_cmd(args).await;
    };
    if target.contains('/') {
        return network_instance_cmd(args, &target).await;
    }

    let url = format!("{}/v1/networks", args.op.api.trim_end_matches('/'));
    let client = reqwest::Client::new();
    let res = operator_get(&client, &url, args.op.token.as_deref())
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
async fn network_summary_cmd(args: NetworkArgs) -> Result<()> {
    let url = format!("{}/v1/networks", args.op.api.trim_end_matches('/'));
    let client = reqwest::Client::new();
    let res = operator_get(&client, &url, args.op.token.as_deref())
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
async fn network_instance_cmd(args: NetworkArgs, target: &str) -> Result<()> {
    let id = resolve_instance_id(&args.op, target).await?;
    let url = format!(
        "{}/v1/instances/{}/network",
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
    println!(
        "{:<16} {:<8} {:<7} {:<9} {:<10} {:<6}",
        "NETWORK", "KIND", "STACKS", "SERVICES", "INSTANCES", "ACTIVE"
    );
    for n in nets {
        let (stacks, services, instances, active) = network_counts(n);
        println!(
            "{:<16} {:<8} {:<7} {:<9} {:<10} {:<6}",
            n["name"].as_str().unwrap_or("-"),
            n["kind"].as_str().unwrap_or("-"),
            stacks,
            services,
            instances,
            active
        );
    }
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
        let op = OperatorArgs {
            api: "http://127.0.0.1:1".into(),
            token: None,
            context: None,
            allow_insecure_http: false,
        };
        // No slash → no network call, returned verbatim.
        let id = resolve_instance_id(&op, "6e6a2d3f-b4dc-4c09-a317-4aac91ff0c99")
            .await
            .unwrap();
        assert_eq!(id, "6e6a2d3f-b4dc-4c09-a317-4aac91ff0c99");
    }
}
