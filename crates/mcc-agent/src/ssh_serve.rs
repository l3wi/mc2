//! Host-side SSH serve for instances — backend is **configurable**.
//!
//! | Backend | Behavior |
//! | ------- | -------- |
//! | `auto` | Use `msb-cli` when `msb ssh` is available; otherwise report Failed with config hint |
//! | `msb-cli` | Always spawn `msb ssh serve` (require capable CLI) |
//! | `disabled` | Never open listeners; desired SSH stays Closed/Failed with message |
//! | `sdk` | Reserved — in-process microsandbox `ssh` feature (blocked on crates.io today) |
//!
//! Configure on the agent:
//!   `--ssh-backend auto|msb-cli|disabled|sdk`  or  `MCC_SSH_BACKEND`
//!   `--msb-bin PATH`  or  `MCC_MSB_BIN`  (default: `msb` on PATH)

use mcc_api::agent::SshObserved;
use mcc_runtime::DesiredSandbox;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use tracing::{info, warn};

/// How this agent materializes host SSH endpoints.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SshBackend {
    /// Prefer msb CLI when it supports `ssh`; otherwise fail with a clear hint.
    #[default]
    Auto,
    /// Require `msb ssh serve` / `authorize`.
    MsbCli,
    /// Do not open SSH (control plane still tracks desired keys/ports).
    Disabled,
    /// In-process SDK (not available until microsandbox `ssh` feature resolves).
    Sdk,
}

impl SshBackend {
    pub fn parse(s: &str) -> Result<Self, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "auto" => Ok(Self::Auto),
            "msb-cli" | "msb_cli" | "msb" | "cli" => Ok(Self::MsbCli),
            "disabled" | "off" | "none" => Ok(Self::Disabled),
            "sdk" | "inprocess" | "embedded" => Ok(Self::Sdk),
            other => Err(format!(
                "unknown ssh backend {other:?} (want auto|msb-cli|disabled|sdk)"
            )),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::MsbCli => "msb-cli",
            Self::Disabled => "disabled",
            Self::Sdk => "sdk",
        }
    }
}

/// Agent-side SSH serve configuration.
#[derive(Debug, Clone)]
pub struct SshServeConfig {
    pub backend: SshBackend,
    /// Binary used for `msb-cli` backend (default `msb`).
    pub msb_bin: PathBuf,
}

impl Default for SshServeConfig {
    fn default() -> Self {
        Self {
            backend: SshBackend::Auto,
            msb_bin: PathBuf::from("msb"),
        }
    }
}

struct ActiveServe {
    config_hash: String,
    bind: String,
    port: u16,
    child: Mutex<Option<Child>>,
}

/// Per-agent map of instance_id → SSH listener process.
pub struct SshServeTable {
    cfg: SshServeConfig,
    active: HashMap<String, ActiveServe>,
    /// Cached capability probe for configured msb binary.
    msb_ssh_ok: Option<bool>,
}

impl SshServeTable {
    pub fn new(cfg: SshServeConfig) -> Self {
        info!(
            backend = cfg.backend.as_str(),
            msb_bin = %cfg.msb_bin.display(),
            "ssh serve config"
        );
        Self {
            cfg,
            active: HashMap::new(),
            msb_ssh_ok: None,
        }
    }

    fn msb_supports_ssh(&mut self) -> bool {
        if let Some(ok) = self.msb_ssh_ok {
            return ok;
        }
        let ok = Command::new(&self.cfg.msb_bin)
            .args(["ssh", "--help"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        self.msb_ssh_ok = Some(ok);
        if !ok {
            warn!(
                msb_bin = %self.cfg.msb_bin.display(),
                "msb binary has no `ssh` subcommand — set MCC_SSH_BACKEND=disabled or upgrade msb / use a path that supports SSH"
            );
        }
        ok
    }

    fn effective_backend(&mut self) -> SshBackend {
        match self.cfg.backend {
            SshBackend::Auto => {
                if self.msb_supports_ssh() {
                    SshBackend::MsbCli
                } else {
                    SshBackend::Disabled
                }
            }
            other => other,
        }
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

        let backend = self.effective_backend();
        match backend {
            SshBackend::Disabled => {
                self.close(&desired.instance_id);
                let msg = if self.cfg.backend == SshBackend::Auto {
                    format!(
                        "ssh backend=auto: {} has no `ssh` subcommand (set MCC_SSH_BACKEND=msb-cli after upgrading msb, or MCC_SSH_BACKEND=disabled)",
                        self.cfg.msb_bin.display()
                    )
                } else {
                    "ssh backend=disabled on this agent".into()
                };
                SshObserved {
                    phase: "Failed".into(),
                    bind: String::new(),
                    port: 0,
                    message: msg,
                }
            }
            SshBackend::Sdk => {
                self.close(&desired.instance_id);
                SshObserved {
                    phase: "Failed".into(),
                    bind: String::new(),
                    port: 0,
                    message: "ssh backend=sdk not available (microsandbox crate ssh feature blocked on crates.io; use msb-cli or wait for upstream)".into(),
                }
            }
            SshBackend::MsbCli | SshBackend::Auto => {
                // Auto already resolved to MsbCli when capable; if forced MsbCli, re-check.
                if backend == SshBackend::MsbCli && !self.msb_supports_ssh() {
                    self.close(&desired.instance_id);
                    return SshObserved {
                        phase: "Failed".into(),
                        bind: String::new(),
                        port: 0,
                        message: format!(
                            "ssh backend=msb-cli but {} lacks `ssh` (upgrade msb or set MCC_MSB_BIN / MCC_SSH_BACKEND)",
                            self.cfg.msb_bin.display()
                        ),
                    };
                }
                self.reconcile_msb_cli(desired).await
            }
        }
    }

    async fn reconcile_msb_cli(&mut self, desired: &DesiredSandbox) -> SshObserved {
        if let Some(active) = self.active.get(&desired.instance_id) {
            if active.config_hash == desired.ssh.config_hash {
                let alive = process_alive(&active.child);
                if alive {
                    return SshObserved {
                        phase: "Open".into(),
                        bind: active.bind.clone(),
                        port: u32::from(active.port),
                        message: format!("msb-cli ({})", self.cfg.msb_bin.display()),
                    };
                }
                warn!(
                    instance = %desired.instance_id,
                    "msb ssh serve process exited; will restart"
                );
            }
            self.close(&desired.instance_id);
        }

        match start_serve_msb(&self.cfg.msb_bin, desired) {
            Ok(active) => {
                std::thread::sleep(std::time::Duration::from_millis(300));
                if !process_alive(&active.child) {
                    warn!(instance = %desired.instance_id, "msb ssh serve exited immediately");
                    return SshObserved {
                        phase: "Failed".into(),
                        bind: String::new(),
                        port: 0,
                        message:
                            "msb ssh serve exited immediately (check sandbox name / msb version)"
                                .into(),
                    };
                }
                let obs = SshObserved {
                    phase: "Open".into(),
                    bind: active.bind.clone(),
                    port: u32::from(active.port),
                    message: format!("msb-cli ({})", self.cfg.msb_bin.display()),
                };
                info!(
                    instance = %desired.instance_id,
                    bind = %active.bind,
                    port = active.port,
                    "ssh serve open (msb-cli)"
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
        Ok(None) => true,
        Ok(Some(_)) => false,
        Err(_) => false,
    }
}

fn start_serve_msb(msb_bin: &PathBuf, desired: &DesiredSandbox) -> anyhow::Result<ActiveServe> {
    for key in &desired.ssh.authorized_public_keys {
        let out = Command::new(msb_bin)
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
    let child = Command::new(msb_bin)
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
        .map_err(|e| {
            anyhow::anyhow!(
                "spawn `{} ssh serve`: {e} (set MCC_MSB_BIN or MCC_SSH_BACKEND)",
                msb_bin.display()
            )
        })?;

    Ok(ActiveServe {
        config_hash: desired.ssh.config_hash.clone(),
        bind: host,
        port,
        child: Mutex::new(Some(child)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_backends() {
        assert_eq!(SshBackend::parse("auto").unwrap(), SshBackend::Auto);
        assert_eq!(SshBackend::parse("msb-cli").unwrap(), SshBackend::MsbCli);
        assert_eq!(SshBackend::parse("disabled").unwrap(), SshBackend::Disabled);
        assert_eq!(SshBackend::parse("sdk").unwrap(), SshBackend::Sdk);
        assert!(SshBackend::parse("nope").is_err());
    }
}
