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
pub use naming::{
    default_volume_root, dir_size_bytes, dir_size_mib, ensure_volume_dir, parse_volume_name,
    remaining_quota_mib, sandbox_name, volume_bind_plan, volume_name, volume_root, VolumeMountPlan,
};
pub use restart::{action_for_phase, backoff_secs, RestartAction, RestartPolicy};
pub mod networks;
pub use networks::{
    network_host_allow_ports, DesiredNetwork, NetworkAllowDesired, NetworkEdgeStatus,
    NetworkExposeDesired, NetworkExposeStatus, NetworkObserved, NetworkPhase,
};
pub mod ingress {
    pub use crate::ingress_render::*;
}
pub use ingress_render::{
    render_catalog_json, render_traefik_dynamic, DesiredIngressRoute, ReadyIngressRoute,
};
pub use spec::{
    DesiredSandbox, DesiredSsh, DiskUsage, InjectedSecret, InstanceReport, SandboxPhase,
    SshObserved, SshPhase,
};
pub use spec_hash::desired_recreate_hash;

use anyhow::Result;
use async_trait::async_trait;
use futures::stream::BoxStream;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncWrite};

/// Execution backend on a node (microsandbox SDK only).
#[async_trait]
pub trait NodeRuntime: Send + Sync {
    /// Ensure sandbox exists and is running, reporting **what was done** so the
    /// controller can count every restart attempt (B10).
    async fn ensure_running(&self, desired: &DesiredSandbox) -> Result<EnsureRunning>;

    /// Stop and remove a sandbox we own (scale-down / delete).
    async fn ensure_removed(&self, runtime_id: &str) -> Result<()>;

    /// Observe current phase without mutating.
    async fn status(&self, runtime_id: &str) -> Result<SandboxStatus>;

    /// Runtime ids of the sandboxes this **install** owns.
    ///
    /// Every sandbox MC2 creates is labelled `mc2.install=<install_id>`; the
    /// backend must return exactly the sandboxes carrying that label, so the
    /// node can recover ownership after a restart and never touch a foreign
    /// workload (B2). Best-effort in the same sense as the rest of the trait:
    /// an error means the pass fails rather than anything being removed on a
    /// guess.
    async fn list_owned(&self, install_id: &str) -> Result<Vec<String>>;

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

    /// Directory holding MC2-owned volume directories (bind-mount sources).
    ///
    /// Defaults to `~/.mc2/volumes` when the backend has no explicit
    /// `--volume-dir`.
    fn volume_root(&self) -> PathBuf {
        naming::volume_root(None)
    }

    /// Observed root-disk usage per running sandbox, keyed by runtime id.
    ///
    /// Best-effort: a backend without metrics returns an empty map (the default
    /// implementation), and disk conditions are simply not reported.
    async fn root_disk_usage(&self) -> Result<HashMap<String, DiskUsage>> {
        Ok(HashMap::new())
    }

    /// Run a shell `script` inside the guest and return its stdout.
    ///
    /// Used for host-file injection (the service-network `/etc/hosts` lines):
    /// the script runs through the guest shell, and a backend failure is an
    /// `Err`. Kept deliberately narrow so `mc2-server` never names an SDK type
    /// to reach into a guest.
    async fn guest_shell(&self, runtime_id: &str, script: &str) -> Result<String>;

    /// Read recent captured log lines for a sandbox (snapshot, chronological).
    ///
    /// `tail` keeps only the last N lines. Errors (e.g. no sandbox) are
    /// returned, matching the previous direct-SDK behaviour.
    async fn read_logs(&self, runtime_id: &str, tail: Option<usize>) -> Result<Vec<LogLine>>;

    /// Stream captured log lines for a sandbox, resuming after `from` (an
    /// opaque cursor taken from a previous [`LogLine::cursor`]) when given.
    ///
    /// Items are `Result`s so a mid-stream backend failure ends the stream
    /// without discarding lines already delivered. The stream follows: it stays
    /// open past the current end and yields new lines as they are produced.
    async fn log_stream(
        &self,
        runtime_id: &str,
        from: Option<String>,
    ) -> Result<BoxStream<'static, Result<LogLine>>>;

    /// Open an in-process SSH server for `runtime_id`, authorizing
    /// `authorized_public_keys` for `user`, with SFTP enabled when `sftp`.
    ///
    /// MC2 owns the host listener and its admission control; the backend only
    /// produces the endpoint that serves one accepted connection.
    async fn ssh_server(
        &self,
        runtime_id: &str,
        user: &str,
        authorized_public_keys: &[String],
        sftp: bool,
    ) -> Result<Arc<dyn SshServer>>;
}

/// A captured guest log line, mapped from the backend's log entry.
#[derive(Debug, Clone)]
pub struct LogLine {
    /// Capture time, RFC 3339 (the wire format the logs endpoint emits).
    pub timestamp: String,
    /// Lowercase source tag: `stdout` | `stderr` | `output` | `system`.
    pub source: String,
    /// Raw line bytes (may not be valid UTF-8; the wire form is lossy).
    pub data: Vec<u8>,
    /// Opaque per-source resume handle, passed back to
    /// [`NodeRuntime::log_stream`] as `from`.
    pub cursor: String,
}

/// Ordered duplex stream bound: the byte-stream shape an SSH session needs.
///
/// A helper trait because a trait object may name only one non-auto trait;
/// `dyn DuplexStreamIo` still implements `AsyncRead`/`AsyncWrite`/`Unpin`/`Send`
/// through these supertraits.
pub trait DuplexStreamIo: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> DuplexStreamIo for T {}

/// A boxed duplex byte stream (one accepted host socket) for
/// [`SshServer::serve`]. Boxing keeps the concrete socket type — and every SDK
/// stream type — out of the server crate.
pub type DuplexStream = Box<dyn DuplexStreamIo>;

/// An in-process SSH endpoint for one sandbox.
///
/// Produced by [`NodeRuntime::ssh_server`]; MC2 hands it each accepted TCP
/// connection. Object-safe so it can be shared as `Arc<dyn SshServer>` across
/// the accept loop.
#[async_trait]
pub trait SshServer: Send + Sync {
    /// Serve one SSH connection over the ordered duplex `stream`.
    async fn serve(&self, stream: DuplexStream) -> Result<()>;
}

/// Captured guest command output.
///
/// Both streams are raw bytes: a guest process is free to emit invalid UTF-8,
/// and the `mc2 exec` wire format keeps it byte-exact (base64 over JSON).
#[derive(Debug, Clone, Default)]
pub struct ExecResult {
    pub exit_code: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

/// Observed sandbox state.
#[derive(Debug, Clone)]
pub struct SandboxStatus {
    pub runtime_id: String,
    pub phase: SandboxPhase,
    pub message: Option<String>,
}

/// What [`NodeRuntime::ensure_running`] did to the sandbox.
///
/// The controller must see every restart the runtime performs on its own
/// (B10): a backend that quietly recreates a crashed sandbox would otherwise
/// restart it on every reconcile pass, with the restart policy's backoff never
/// advancing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnsureOutcome {
    /// The sandbox was already up; nothing was created or started.
    AlreadyRunning,
    /// No sandbox existed; a new one was created.
    Created,
    /// An existing sandbox was restarted: a stopped one started, or a
    /// crashed/failed one removed and recreated.
    Restarted,
    /// The sandbox is not running and the restart policy left it that way.
    Left,
}

/// Result of [`NodeRuntime::ensure_running`]: the observed status plus what the
/// call did to get there.
#[derive(Debug, Clone)]
pub struct EnsureRunning {
    pub status: SandboxStatus,
    pub outcome: EnsureOutcome,
}
