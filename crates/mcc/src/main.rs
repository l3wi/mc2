//! MicroCommandControl (MCC) — dual-mode binary.
//!
//! ```text
//! mcc server   # control plane
//! mcc agent    # node agent (embeds microsandbox runtime)
//! mcc apply    # operator: apply a stack (Phase 3+)
//! ```

use anyhow::Result;
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
    /// Show cluster / binary version info
    Version,
}

#[derive(Debug, Parser)]
struct ApplyArgs {
    /// Path to stack YAML
    #[arg(short = 'f', long = "file")]
    file: String,
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
            anyhow::bail!(
                "apply is not implemented yet (Phase 3). File: {}",
                args.file
            );
        }
        Commands::Version => {
            println!("mcc {} — MicroCommandControl", env!("CARGO_PKG_VERSION"));
            println!("api schema: {}", mcc_api::API_VERSION);
        }
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
