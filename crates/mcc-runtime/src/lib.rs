//! Node runtime abstraction for MicroCommandControl agents.
//!
//! - [`MockRuntime`] — Phase 3 / CI (no hypervisor)
//! - [`MsbCliRuntime`] — real microVMs via `msb` CLI (project + Sandboxfile)
//!
//! Optional SDK embed can land later behind a feature; CLI is the Phase 4 path.

mod mock;
mod msb_cli;
mod naming;
mod spec;

pub use mock::MockRuntime;
pub use msb_cli::MsbCliRuntime;
pub use naming::sandbox_name;
pub use spec::{desired_from_sync, DesiredSandbox, RuntimeKind, SandboxPhase};

use anyhow::Result;
use async_trait::async_trait;

/// Execution backend on a node.
#[async_trait]
pub trait NodeRuntime: Send + Sync {
    /// Human-readable backend name (`mock`, `msb-cli`, …).
    fn kind(&self) -> RuntimeKind;

    /// Ensure sandbox exists and is running. Returns runtime id (usually msb name).
    async fn ensure_running(&self, desired: &DesiredSandbox) -> Result<SandboxStatus>;

    /// Stop and remove a sandbox we own (scale-down / delete).
    async fn ensure_removed(&self, runtime_id: &str) -> Result<()>;

    /// Observe current phase without mutating.
    async fn status(&self, runtime_id: &str) -> Result<SandboxStatus>;

    /// List runtime ids known to this backend (best-effort).
    async fn list(&self) -> Result<Vec<String>>;
}

/// Observed sandbox state.
#[derive(Debug, Clone)]
pub struct SandboxStatus {
    pub runtime_id: String,
    pub phase: SandboxPhase,
    pub message: Option<String>,
}

/// Select a runtime for the agent.
pub fn select_runtime(
    kind: RuntimeKind,
    project_dir: impl Into<std::path::PathBuf>,
    msb_bin: Option<std::path::PathBuf>,
) -> Result<Box<dyn NodeRuntime>> {
    match kind {
        RuntimeKind::Mock => Ok(Box::new(MockRuntime::new())),
        RuntimeKind::MsbCli => {
            let bin = msb_bin.unwrap_or_else(|| std::path::PathBuf::from("msb"));
            Ok(Box::new(MsbCliRuntime::new(project_dir, bin)?))
        }
        RuntimeKind::Auto => {
            let bin = msb_bin.unwrap_or_else(|| std::path::PathBuf::from("msb"));
            if which_msb(&bin) {
                Ok(Box::new(MsbCliRuntime::new(project_dir, bin)?))
            } else {
                tracing::warn!("msb not found on PATH; using mock runtime");
                Ok(Box::new(MockRuntime::new()))
            }
        }
    }
}

fn which_msb(bin: &std::path::Path) -> bool {
    if bin.is_absolute() {
        return bin.is_file();
    }
    std::process::Command::new(bin)
        .arg("version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}
