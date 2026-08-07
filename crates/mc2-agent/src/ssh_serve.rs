//! Host-side SSH serve for instances via the **microsandbox SDK only**.
//!
//! No `msb` CLI subprocess. Uses `Sandbox::ssh().server_with(...).serve(stream)`
//! over a host TCP listener (default bind 127.0.0.1, auto port when port=0).

use mc2_api::agent::SshObserved;
use mc2_runtime::DesiredSandbox;
use microsandbox::Sandbox;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tracing::{info, warn};

struct ActiveServe {
    config_hash: String,
    bind: String,
    port: u16,
    shutdown: Option<oneshot::Sender<()>>,
    join: JoinHandle<()>,
}

/// Per-agent map of instance_id → in-process SSH accept loop.
#[derive(Default)]
pub struct SshServeTable {
    active: HashMap<String, ActiveServe>,
}

impl SshServeTable {
    pub fn new() -> Self {
        info!("ssh serve: microsandbox SDK (in-process)");
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
            self.close(&desired.instance_id).await;
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
            if active.config_hash == desired.ssh.config_hash && !active.join.is_finished() {
                return SshObserved {
                    phase: "Open".into(),
                    bind: active.bind.clone(),
                    port: u32::from(active.port),
                    message: "sdk".into(),
                };
            }
            self.close(&desired.instance_id).await;
        }

        match start_serve_sdk(desired).await {
            Ok(active) => {
                let obs = SshObserved {
                    phase: "Open".into(),
                    bind: active.bind.clone(),
                    port: u32::from(active.port),
                    message: "sdk".into(),
                };
                info!(
                    instance = %desired.instance_id,
                    runtime_id = %desired.runtime_id,
                    bind = %active.bind,
                    port = active.port,
                    "ssh serve open (sdk)"
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

    pub async fn close(&mut self, instance_id: &str) {
        if let Some(mut active) = self.active.remove(instance_id) {
            if let Some(tx) = active.shutdown.take() {
                let _ = tx.send(());
            }
            active.join.abort();
            info!(instance = %instance_id, "ssh serve closed");
        }
    }

    pub async fn close_missing(&mut self, keep: &std::collections::HashSet<String>) {
        let stale: Vec<String> = self
            .active
            .keys()
            .filter(|k| !keep.contains(*k))
            .cloned()
            .collect();
        for id in stale {
            self.close(&id).await;
        }
    }
}

async fn start_serve_sdk(desired: &DesiredSandbox) -> anyhow::Result<ActiveServe> {
    let bind_ip: std::net::IpAddr = desired
        .ssh
        .bind
        .parse()
        .unwrap_or_else(|_| std::net::IpAddr::from([127, 0, 0, 1]));

    let listener = if desired.ssh.port == 0 {
        TcpListener::bind(SocketAddr::new(bind_ip, 0)).await?
    } else {
        TcpListener::bind(SocketAddr::new(bind_ip, desired.ssh.port)).await?
    };
    let local = listener.local_addr()?;
    let port = local.port();
    let bind = desired.ssh.bind.clone();

    let handle = Sandbox::get(&desired.runtime_id)
        .await
        .map_err(|e| anyhow::anyhow!("Sandbox::get({}): {e}", desired.runtime_id))?;
    let sb = handle
        .connect()
        .await
        .map_err(|e| anyhow::anyhow!("Sandbox::connect({}): {e}", desired.runtime_id))?;

    let keys = desired.ssh.authorized_public_keys.clone();
    let user = desired.ssh.user.clone();
    let sftp = desired.ssh.sftp;
    let server = sb
        .ssh()
        .server_with(|opts| {
            let mut o = opts.user(user).sftp(sftp);
            for k in keys {
                o = o.authorized_key(k);
            }
            o
        })
        .await
        .map_err(|e| anyhow::anyhow!("ssh server_with: {e}"))?;

    let (tx, mut rx) = oneshot::channel::<()>();
    let server = Arc::new(server);
    let join = tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = &mut rx => break,
                acc = listener.accept() => {
                    match acc {
                        Ok((stream, peer)) => {
                            let srv = server.clone();
                            tokio::spawn(async move {
                                if let Err(e) = srv.serve(stream).await {
                                    warn!(%peer, error = %e, "ssh connection ended");
                                }
                            });
                        }
                        Err(e) => {
                            warn!(error = %e, "ssh accept failed");
                            break;
                        }
                    }
                }
            }
        }
    });

    Ok(ActiveServe {
        config_hash: desired.ssh.config_hash.clone(),
        bind,
        port,
        shutdown: Some(tx),
        join,
    })
}
