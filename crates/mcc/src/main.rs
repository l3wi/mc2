//! MicroCommandControl (MCC) — dual-mode binary.
//!
//! ```text
//! mcc server   # control plane
//! mcc agent    # node agent (embeds microsandbox runtime)
//! mcc apply    # operator: apply a stack (Phase 3+)
//! mcc node ls  # list nodes
//! ```

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

#[derive(Debug, Parser)]
#[command(
    name = "mcc",
    about = "MicroCommandControl — self-hosted C2 for microsandbox microVMs",
    long_about = "MicroCommandControl (MCC) is a K3s-shaped control plane for \
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
    Server(mcc_server::ServerArgs),
    /// Run the MicroCommandControl agent (node worker)
    Agent(mcc_agent::AgentArgs),
    /// Apply a stack YAML (desired state)
    Apply(ApplyArgs),
    /// Node operations
    Node(NodeCmd),
    /// Show cluster / binary version info
    Version,
}

#[derive(Debug, Parser)]
struct ApplyArgs {
    /// Path to stack YAML
    #[arg(short = 'f', long = "file")]
    file: String,
}

#[derive(Debug, Parser)]
struct NodeCmd {
    #[command(subcommand)]
    command: NodeCommands,
}

#[derive(Debug, Subcommand)]
enum NodeCommands {
    /// List registered nodes (`mcc node ls`)
    #[command(name = "ls", alias = "list")]
    Ls(OperatorArgs),
}

#[derive(Debug, Parser)]
struct OperatorArgs {
    /// Control plane REST base URL
    #[arg(long, default_value = "http://127.0.0.1:7443", env = "MCC_API")]
    api: String,

    /// Operator API bearer token
    #[arg(long, env = "MCC_API_TOKEN")]
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
        Commands::Server(args) => mcc_server::run(args).await?,
        Commands::Agent(args) => mcc_agent::run(args).await?,
        Commands::Apply(args) => {
            bail!(
                "apply is not implemented yet (Phase 3). File: {}",
                args.file
            );
        }
        Commands::Node(NodeCmd {
            command: NodeCommands::Ls(args),
        }) => node_ls(args).await?,
        Commands::Version => {
            println!("mcc {} — MicroCommandControl", env!("CARGO_PKG_VERSION"));
            println!("api schema: {}", mcc_api::API_VERSION);
        }
    }

    Ok(())
}

async fn node_ls(args: OperatorArgs) -> Result<()> {
    let token = args
        .token
        .context("missing --token / MCC_API_TOKEN (operator API token from server bootstrap)")?;
    let url = format!("{}/v1/nodes", args.api.trim_end_matches('/'));
    let client = reqwest::Client::new();
    let res = client
        .get(&url)
        .bearer_auth(&token)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?;
    let status = res.status();
    let body = res.text().await.unwrap_or_default();
    if !status.is_success() {
        bail!("GET {url} failed: {status} {body}");
    }
    let nodes: Vec<mcc_api::NodeView> =
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
