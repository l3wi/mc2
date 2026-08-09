//! MicroCommandControl (MC2) — YAML-driven orchestration for microsandbox.
//!
//! ```text
//! mc2 server   # the orchestrator (single process)
//! mc2 up       # bring up a stack (reconcile desired state)
//! mc2 ps       # list instances
//! ```

use anyhow::{bail, Result};
use clap::{CommandFactory, Parser};
use context::{load as load_config, resolve, save, ClientMode, Conn};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

mod cli;
mod client;
mod cmd;
mod context;
mod help;
mod prompt;
mod setup;

use cli::{
    Cli, Commands, CompletionsArgs, ContextCmd, ContextCommands, DoctorArgs, NodeCmd, NodeCommands,
    SecretCmd, SecretCommands, SetupCmd, SshCmd, SshCommands,
};

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
    if help::try_show_grouped_top_level_help() {
        return Ok(());
    }
    let Cli {
        command,
        api,
        token,
        context,
        allow_insecure_http,
    } = Cli::parse();

    // Effective connection for operator commands (flags > env > context > default).
    let resolve = || {
        resolve_op(
            &api,
            token.as_deref(),
            context.as_deref(),
            allow_insecure_http,
        )
    };

    match command {
        Commands::Server(args) => mc2_server::run(args).await?,

        Commands::Up(args) => {
            let conn = resolve()?;
            cmd::stacks::up_cmd(args, &conn).await?
        }
        Commands::Down(args) => {
            let conn = resolve()?;
            cmd::stacks::down_cmd(args, &conn).await?
        }
        Commands::Config(args) => cmd::stacks::config_cmd(args)?,
        Commands::Exec(args) => {
            let conn = resolve()?;
            cmd::access::exec_cmd(args, &conn).await?
        }
        Commands::Logs(args) => {
            let conn = resolve()?;
            cmd::access::logs_cmd(args, &conn).await?
        }
        Commands::Status(args) => {
            let conn = resolve()?;
            cmd::observe::status_cmd(args, &conn).await?
        }
        Commands::Node(NodeCmd {
            command: NodeCommands::Ls(args),
        }) => {
            let conn = resolve()?;
            cmd::observe::node_ls(args, &conn).await?
        }
        Commands::Ps(args) => {
            let conn = resolve()?;
            cmd::observe::ps_cmd(args, &conn).await?
        }
        Commands::Network(args) => {
            let conn = resolve()?;
            cmd::observe::network_cmd(args, &conn).await?
        }
        Commands::Ingress(args) => {
            let conn = resolve()?;
            cmd::observe::ingress_cmd(args, &conn).await?
        }
        Commands::Secret(SecretCmd { command }) => match command {
            SecretCommands::Set(a) => {
                let conn = resolve()?;
                cmd::security::secret_set(a, &conn).await?
            }
            SecretCommands::Ls(a) => {
                let conn = resolve()?;
                cmd::security::secret_ls(a, &conn).await?
            }
            SecretCommands::Rm(a) => {
                let conn = resolve()?;
                cmd::security::secret_rm(a, &conn).await?
            }
        },
        Commands::Ssh(SshCmd { command }) => match command {
            SshCommands::AddKey(a) => {
                let conn = resolve()?;
                cmd::security::ssh_key_add(a, &conn).await?
            }
            SshCommands::ShowKey(a) => {
                let conn = resolve()?;
                cmd::security::ssh_key_show(a, &conn).await?
            }
            SshCommands::Keys(a) => {
                let conn = resolve()?;
                cmd::security::ssh_key_ls(a, &conn).await?
            }
            SshCommands::RmKey(a) => {
                let conn = resolve()?;
                cmd::security::ssh_key_rm(a, &conn).await?
            }
            SshCommands::Ls(a) => {
                let conn = resolve()?;
                cmd::security::ssh_endpoints_ls(a, &conn).await?
            }
            SshCommands::Show(a) => {
                let conn = resolve()?;
                cmd::security::ssh_instance_show(a, &conn).await?
            }
            SshCommands::Open(a) => {
                let conn = resolve()?;
                cmd::security::ssh_instance_open(a, &conn).await?
            }
            SshCommands::Close(a) => {
                let conn = resolve()?;
                cmd::security::ssh_instance_close(a, &conn).await?
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
            ContextCommands::Set(a) => context_set(&a.name, &api, token.as_deref())?,
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

/// Resolve the effective connection (flags > env > context > current > default)
/// and enforce the remote-plaintext guard.
fn resolve_op(
    api: &str,
    token: Option<&str>,
    context: Option<&str>,
    allow_insecure_http: bool,
) -> Result<Conn> {
    let allow = allow_insecure_http || context::env_truthy("MC2_ALLOW_INSECURE_HTTP");
    let cfg = load_config()?;
    let api_flag = if api.is_empty() { None } else { Some(api) };
    resolve(&cfg, api_flag, token, context, allow)
}

fn context_set(name: &str, url: &str, token: Option<&str>) -> Result<()> {
    let mut cfg = load_config()?;
    let url = url.trim().trim_end_matches('/').to_string();
    if url.is_empty() {
        bail!("context '{name}' needs --api <url> (or set MC2_API)");
    }
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
        Some(cli::SetupCommands::Server) => setup::run_server_wizard().await?,
        Some(cli::SetupCommands::Client) => setup::run_client_wizard().await?,
    }
    Ok(())
}

/// Generate shell completions to stdout.
fn completions_cmd(args: CompletionsArgs) -> Result<()> {
    let mut cmd = Cli::command();
    clap_complete::generate(args.shell, &mut cmd, "mc2", &mut std::io::stdout());
    Ok(())
}
