//! MicroCommandControl node agent.
//!
//! Phase 0: CLI args + stub.
//! Phase 2: join, heartbeat, sync desired instances.
//! Phase 4: microsandbox runtime embed.

use anyhow::Result;
use clap::Parser;
use tracing::info;

/// Arguments for `mcc agent`.
#[derive(Debug, Clone, Parser)]
pub struct AgentArgs {
    /// Control plane gRPC/REST endpoint (e.g. https://cp.lab:7443)
    #[arg(long, env = "MCC_SERVER")]
    pub server: Option<String>,

    /// Join token (bootstrap); later replaced by node credential
    #[arg(long, env = "MCC_JOIN_TOKEN")]
    pub token: Option<String>,

    /// Node name (defaults to hostname)
    #[arg(long, env = "MCC_NODE_NAME")]
    pub name: Option<String>,

    /// Log and exit without connecting
    #[arg(long, hide = true)]
    pub dry_run: bool,
}

/// Run the agent.
pub async fn run(args: AgentArgs) -> Result<()> {
    let name = args
        .name
        .clone()
        .or_else(hostname)
        .unwrap_or_else(|| "unknown".into());

    info!(
        node = %name,
        server = ?args.server,
        has_token = args.token.is_some(),
        api = mcc_api::API_VERSION,
        "MicroCommandControl agent starting (Phase 0 stub)"
    );

    if args.dry_run {
        info!("dry_run: not connecting");
        return Ok(());
    }

    anyhow::bail!("mcc agent join/sync is Phase 2. For Phase 0 smoke: `mcc agent --dry-run`");
}

fn hostname() -> Option<String> {
    std::env::var("HOSTNAME")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| {
            // Best-effort without extra crates in Phase 0
            std::fs::read_to_string("/etc/hostname")
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn dry_run_ok() {
        let args = AgentArgs {
            server: Some("https://127.0.0.1:7443".into()),
            token: Some("test".into()),
            name: Some("test-node".into()),
            dry_run: true,
        };
        run(args).await.expect("dry_run should succeed");
    }
}
