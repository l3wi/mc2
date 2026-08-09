//! Node runtime for MicroCommandControl (embedded microsandbox SDK backend).
//!
//! Sole backend: official [`microsandbox`](https://docs.rs/microsandbox) Rust SDK.

mod ingress_render;
mod msb_sdk;
mod naming;
mod restart;
mod spec;
mod spec_hash;

pub use msb_sdk::MicrosandboxRuntime;
pub use naming::{sandbox_name, volume_mount_plan, volume_name};
pub use restart::{action_for_phase, backoff_secs, RestartAction, RestartPolicy};
pub mod networks;
pub use networks::{
    network_host_allow_ports, DesiredNetwork, NetworkAllowDesired, NetworkEdgeStatus,
    NetworkExposeDesired, NetworkExposeStatus, NetworkObserved,
};
pub mod ingress {
    pub use crate::ingress_render::*;
}
pub use ingress_render::{
    render_catalog_json, render_traefik_dynamic, DesiredIngressRoute, ReadyIngressRoute,
};
pub use spec::{
    DesiredSandbox, DesiredSsh, InjectedSecret, InstanceReport, SandboxPhase, SshObserved,
};
pub use spec_hash::desired_recreate_hash;

use anyhow::Result;
use async_trait::async_trait;

/// Execution backend on a node (microsandbox SDK only).
#[async_trait]
pub trait NodeRuntime: Send + Sync {
    /// Ensure sandbox exists and is running. Returns runtime id (sandbox name).
    async fn ensure_running(&self, desired: &DesiredSandbox) -> Result<SandboxStatus>;

    /// Stop and remove a sandbox we own (scale-down / delete).
    async fn ensure_removed(&self, runtime_id: &str) -> Result<()>;

    /// Observe current phase without mutating.
    async fn status(&self, runtime_id: &str) -> Result<SandboxStatus>;

    /// List runtime ids known to this backend (best-effort).
    async fn list(&self) -> Result<Vec<String>>;

    /// Run a guest command (health probes). Returns process exit code.
    async fn exec_command(&self, runtime_id: &str, argv: &[String]) -> Result<i32>;

    /// Run a guest command, feeding `stdin` (may be empty), and capture its
    /// output (`mc2 exec`).
    async fn exec_with_output(
        &self,
        runtime_id: &str,
        argv: &[String],
        stdin: &[u8],
    ) -> Result<ExecResult>;
}

/// Captured guest command output.
#[derive(Debug, Clone, Default)]
pub struct ExecResult {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

/// Observed sandbox state.
#[derive(Debug, Clone)]
pub struct SandboxStatus {
    pub runtime_id: String,
    pub phase: SandboxPhase,
    pub message: Option<String>,
}
