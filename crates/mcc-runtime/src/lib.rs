//! Node runtime abstraction for MicroCommandControl agents.
//!
//! - [`MockRuntime`] — unit/CI (no hypervisor)
//! - [`MicrosandboxRuntime`] — **primary**: official `microsandbox` Rust SDK

mod mock;
mod msb_sdk;
mod naming;
mod spec;

pub use mock::MockRuntime;
pub use msb_sdk::MicrosandboxRuntime;
pub use naming::sandbox_name;
pub use spec::{desired_from_sync, DesiredSandbox, RuntimeKind, SandboxPhase};

use anyhow::Result;
use async_trait::async_trait;

/// Execution backend on a node.
#[async_trait]
pub trait NodeRuntime: Send + Sync {
    /// Human-readable backend name (`mock`, `msb`, …).
    fn kind(&self) -> RuntimeKind;

    /// Ensure sandbox exists and is running. Returns runtime id (msb name).
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
///
/// `auto` / `msb` → embedded microsandbox SDK.  
/// `mock` → fake runtime for CI.
pub fn select_runtime(kind: RuntimeKind) -> Result<Box<dyn NodeRuntime>> {
    match kind {
        RuntimeKind::Mock => Ok(Box::new(MockRuntime::new())),
        RuntimeKind::Auto | RuntimeKind::Msb => Ok(Box::new(MicrosandboxRuntime::new())),
    }
}
