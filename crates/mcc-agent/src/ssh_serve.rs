//! Host-side SSH serve for instances.
//!
//! Full protocol is provided by `msb ssh serve` (official external-client path).
//! The microsandbox crate `ssh` feature currently fails to resolve on crates.io
//! (`russh` pins `ed25519-dalek = 3.0.0-pre.7`, not published). When that is
//! fixed we can switch to in-process `Sandbox::ssh().server_with(...)`.

use mcc_api::agent::SshObserved;
use mcc_runtime::DesiredSandbox;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use tracing::{info, warn};

struct ActiveServe {
    config_hash: String,
    bind: String,
    port: u16,
    child: Mutex<Option<Child>>,
}

/// Per-agent map of instance_id → SSH listener process.
#[derive(Default)]
pub struct SshServeTable {
    active: HashMap<String, ActiveServe>,
}

impl SshServeTable {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn reconcile(
        &mut self,
        desired: &DesiredSandbox,
        sandbox_running: bool,
    ) -> SshObserved {
        let want = desired.ssh.enabled
            && sandbox_running
            && !desired.ssh.authorized_public_keys.is_empty();

        if !want {
            self.close(&desired.instance_id);
            return SshObserved {
                phase: "Closed".into(),
                bind: String::new(),
                port: 0,
                message: if desired.ssh.enabled && !sandbox_running {
                    "waiting for Running".into()
                } else if desired.ssh.enabled && desired.ssh.authorized_public_keys.is_empty() {
                    "no authorized keys".into()
                } else {
                    String::new()
                },
            };
        }

        if let Some(active) = self.active.get(&desired.instance_id) {
            if active.config_hash == desired.ssh.config_hash {
                // Still running?
                let alive = active
                    .child
                    .lock()
                    .ok()
                    .and_then(|mut c| c.as_mut().map(|ch| ch.try_wait().ok() == Some(None)))
                    .unwrap_or(false);
                if alive {
                    return SshObserved {
                        phase: "Open".into(),
                        bind: active.bind.clone(),
                        port: u32::from(active.port),
                        message: "msb ssh serve".into(),
                    };
                }
            }
            self.close(&desired.instance_id);
        }

        match start_serve(desired) {
            Ok(active) => {
                let obs = SshObserved {
                    phase: "Open".into(),
                    bind: active.bind.clone(),
                    port: u32::from(active.port),
                    message: "msb ssh serve".into(),
                };
                info!(
                    instance = %desired.instance_id,
                    bind = %active.bind,
                    port = active.port,
                    "ssh serve open (msb)"
                );
                self.active.insert(desired.instance_id.clone(), active);
                obs
            }
            Err(e) => {
                warn!(instance = %desired.instance_id, error = %e, "ssh serve failed");
                SshObserved {
                    phase: "Failed".into(),
                    bind: String::new(),
                    port: 0,
                    message: e.to_string(),
                }
            }
        }
    }

    pub fn close(&mut self, instance_id: &str) {
        if let Some(active) = self.active.remove(instance_id) {
            if let Ok(mut g) = active.child.lock() {
                if let Some(mut child) = g.take() {
                    let _ = child.kill();
                    let _ = child.wait();
                }
            }
            info!(instance = %instance_id, "ssh serve closed");
        }
    }

    pub fn close_missing(&mut self, keep: &std::collections::HashSet<String>) {
        let stale: Vec<String> = self
            .active
            .keys()
            .filter(|k| !keep.contains(*k))
            .cloned()
            .collect();
        for id in stale {
            self.close(&id);
        }
    }
}

fn start_serve(desired: &DesiredSandbox) -> anyhow::Result<ActiveServe> {
    // Ensure msb CLI is available.
    let msb = which_msb().ok_or_else(|| {
        anyhow::anyhow!("msb CLI not on PATH (required for SSH serve until SDK ssh feature builds)")
    })?;

    // Authorize keys into msb home (cluster keys → host authorized_keys).
    for key in &desired.ssh.authorized_public_keys {
        let status = Command::new(&msb)
            .args(["ssh", "authorize", "--key", key])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        if let Ok(s) = status {
            if !s.success() {
                warn!("msb ssh authorize failed for a key (continuing)");
            }
        }
    }

    let bind_ip: std::net::IpAddr = desired
        .ssh
        .bind
        .parse()
        .unwrap_or_else(|_| std::net::IpAddr::from([127, 0, 0, 1]));

    // Pick free port if auto.
    let port = if desired.ssh.port == 0 {
        let listener = std::net::TcpListener::bind(SocketAddr::new(bind_ip, 0))?;
        let p = listener.local_addr()?.port();
        drop(listener);
        p
    } else {
        desired.ssh.port
    };

    let host = desired.ssh.bind.clone();
    let name = desired.runtime_id.clone();
    let child = Command::new(&msb)
        .args([
            "ssh",
            "serve",
            &name,
            "--host",
            &host,
            "--port",
            &port.to_string(),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| anyhow::anyhow!("spawn msb ssh serve: {e}"))?;

    // Brief settle: if process dies immediately, surface failure.
    std::thread::sleep(std::time::Duration::from_millis(200));
    // can't try_wait after move easily — store and check on next reconcile

    Ok(ActiveServe {
        config_hash: desired.ssh.config_hash.clone(),
        bind: host,
        port,
        child: Mutex::new(Some(child)),
    })
}

fn which_msb() -> Option<String> {
    Command::new("msb")
        .arg("version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .ok()
        .filter(|s| s.success())
        .map(|_| "msb".into())
}
