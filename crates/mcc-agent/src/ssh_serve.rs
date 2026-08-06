//! Host-side SSH serve for instances via `msb ssh serve` when available.
//!
//! Note: the microsandbox crate `ssh` feature currently cannot be enabled
//! (russh pins unpublished `ed25519-dalek = 3.0.0-pre.7`). The installed
//! `msb` CLI must expose `ssh serve` / `ssh authorize` (newer msb). Older
//! project-style CLIs will report phase Failed with a clear message.

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
    /// Cached capability probe (`msb ssh --help` succeeds).
    msb_ssh_ok: Option<bool>,
}

impl SshServeTable {
    pub fn new() -> Self {
        Self::default()
    }

    fn msb_supports_ssh(&mut self) -> bool {
        if let Some(ok) = self.msb_ssh_ok {
            return ok;
        }
        let ok = Command::new("msb")
            .args(["ssh", "--help"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        self.msb_ssh_ok = Some(ok);
        ok
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

        if !self.msb_supports_ssh() {
            self.close(&desired.instance_id);
            return SshObserved {
                phase: "Failed".into(),
                bind: String::new(),
                port: 0,
                message: "msb CLI has no `ssh` subcommand (upgrade msb, or wait for SDK ssh feature)".into(),
            };
        }

        if let Some(active) = self.active.get(&desired.instance_id) {
            if active.config_hash == desired.ssh.config_hash {
                let alive = process_alive(&active.child);
                if alive {
                    return SshObserved {
                        phase: "Open".into(),
                        bind: active.bind.clone(),
                        port: u32::from(active.port),
                        message: "msb ssh serve".into(),
                    };
                }
                warn!(
                    instance = %desired.instance_id,
                    "msb ssh serve process exited; will restart"
                );
            }
            self.close(&desired.instance_id);
        }

        match start_serve(desired) {
            Ok(active) => {
                // Confirm child still alive after short settle.
                std::thread::sleep(std::time::Duration::from_millis(300));
                if !process_alive(&active.child) {
                    warn!(instance = %desired.instance_id, "msb ssh serve exited immediately");
                    return SshObserved {
                        phase: "Failed".into(),
                        bind: String::new(),
                        port: 0,
                        message: "msb ssh serve exited immediately (check sandbox name / msb version)"
                            .into(),
                    };
                }
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

fn process_alive(child: &Mutex<Option<Child>>) -> bool {
    let Ok(mut g) = child.lock() else {
        return false;
    };
    let Some(ch) = g.as_mut() else {
        return false;
    };
    match ch.try_wait() {
        Ok(None) => true,           // still running
        Ok(Some(_)) => false,       // exited
        Err(_) => false,
    }
}

fn start_serve(desired: &DesiredSandbox) -> anyhow::Result<ActiveServe> {
    // Authorize keys (best-effort).
    for key in &desired.ssh.authorized_public_keys {
        let out = Command::new("msb")
            .args(["ssh", "authorize", "--key", key])
            .output();
        match out {
            Ok(o) if o.status.success() => {}
            Ok(o) => {
                let err = String::from_utf8_lossy(&o.stderr);
                warn!(%err, "msb ssh authorize failed");
            }
            Err(e) => warn!(error = %e, "msb ssh authorize spawn failed"),
        }
    }

    let bind_ip: std::net::IpAddr = desired
        .ssh
        .bind
        .parse()
        .unwrap_or_else(|_| std::net::IpAddr::from([127, 0, 0, 1]));

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
    let child = Command::new("msb")
        .args([
            "ssh",
            "serve",
            &name,
            "--host",
            &host,
            "--port",
            &port.to_string(),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| anyhow::anyhow!("spawn msb ssh serve: {e}"))?;

    Ok(ActiveServe {
        config_hash: desired.ssh.config_hash.clone(),
        bind: host,
        port,
        child: Mutex::new(Some(child)),
    })
}
