//! Node runtime for MicroCommandControl agents.
//!
//! Sole backend: official [`microsandbox`](https://docs.rs/microsandbox) Rust SDK.

mod ingress_render;
mod msb_sdk;
mod naming;
mod restart;
mod spec;
mod spec_hash;

pub use msb_sdk::MicrosandboxRuntime;
pub use naming::sandbox_name;
pub use restart::{action_for_phase, backoff_secs, RestartAction, RestartPolicy};
pub mod fabric;
pub use fabric::{
    fabric_expose_guest_ports, fabric_from_proto, fabric_host_allow_ports, DesiredFabric,
    FabricAllowDesired, FabricExposeDesired,
};
pub mod ingress {
    pub use crate::ingress_render::*;
}
pub use ingress_render::{
    make_route_id, normalize_path, render_caddyfile, render_catalog_json, render_traefik_dynamic,
    DesiredIngressRoute, ReadyIngressRoute,
};
pub use spec::{desired_from_sync, DesiredSandbox, DesiredSsh, InjectedSecret, SandboxPhase};
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
}

/// Observed sandbox state.
#[derive(Debug, Clone)]
pub struct SandboxStatus {
    pub runtime_id: String,
    pub phase: SandboxPhase,
    pub message: Option<String>,
}

/// Construct the only supported runtime: embedded microsandbox SDK.
pub fn default_runtime() -> MicrosandboxRuntime {
    MicrosandboxRuntime::new()
}
