//! Microsandbox backend using the official Rust SDK (`Sandbox::builder` / embed).
//!
//! Always uses the **local** backend. Host `MSB_API_KEY` / cloud profiles must not
//! hijack MCC agent sandboxes.

use crate::restart::{action_for_phase, RestartAction, RestartPolicy};
use crate::spec::start_command_parts;
use crate::{DesiredSandbox, NodeRuntime, SandboxPhase, SandboxStatus};
use anyhow::{Context, Result};
use async_trait::async_trait;
use microsandbox::sandbox::SandboxStatus as MsbStatus;
use microsandbox::{set_default_backend, LocalBackend, NetworkPolicy, NetworkProfile, Sandbox};
use std::net::IpAddr;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use tracing::{debug, info, warn};

/// Real microVM backend via the **microsandbox** crate (local libkrun only).
#[derive(Debug, Default)]
pub struct MicrosandboxRuntime;

/// Ensure process-wide default is LocalBackend once per agent process.
static LOCAL_BACKEND_INSTALLED: AtomicBool = AtomicBool::new(false);

impl MicrosandboxRuntime {
    pub fn new() -> Self {
        Self
    }
}

async fn ensure_local_backend() -> Result<()> {
    if LOCAL_BACKEND_INSTALLED.load(Ordering::SeqCst) {
        return Ok(());
    }
    // Install local even if MSB_API_KEY / cloud profile would otherwise win.
    let local = LocalBackend::new()
        .await
        .context("LocalBackend::new (open microsandbox local DB)")?;
    set_default_backend(local);
    LOCAL_BACKEND_INSTALLED.store(true, Ordering::SeqCst);
    info!("microsandbox: forced LocalBackend (ignores MSB_API_KEY / cloud profiles)");
    Ok(())
}

fn map_status(s: MsbStatus) -> SandboxPhase {
    match s {
        MsbStatus::Running | MsbStatus::Draining => SandboxPhase::Running,
        MsbStatus::Starting | MsbStatus::Created => SandboxPhase::Creating,
        MsbStatus::Stopped | MsbStatus::Paused => SandboxPhase::Stopped,
        MsbStatus::Crashed => SandboxPhase::Failed,
    }
}

fn network_profiles(desired: &DesiredSandbox) -> Vec<NetworkProfile> {
    let mut out = Vec::new();
    for p in &desired.spec.network.profiles {
        match p.as_str() {
            "public" => out.push(NetworkProfile::Public),
            "private" | "local" => out.push(NetworkProfile::Private),
            "host" | "any" => out.push(NetworkProfile::Host),
            "none" => {}
            other => {
                warn!(profile = %other, "unknown network profile; ignoring");
            }
        }
    }
    if out.is_empty() && !desired.spec.network.profiles.iter().any(|p| p == "none") {
        out.push(NetworkProfile::Public);
    }
    out
}

async fn create_detached(desired: &DesiredSandbox) -> Result<()> {
    ensure_local_backend().await?;

    let cpus = desired.spec.resources.cpus.clamp(1, 255) as u8;
    let mem = desired.spec.resources.memory_mib.min(u32::MAX as u64) as u32;

    // Note: `.replace()` is not accepted by create_detached on local backend.
    // Callers must remove an existing sandbox first if recreation is needed.
    let mut b = Sandbox::builder(desired.runtime_id.clone())
        .image(desired.spec.image.as_str())
        .cpus(cpus)
        .memory(mem)
        .detached(true);

    let cmd = start_command_parts(&desired.spec);
    b = b.background_command(cmd);

    for (k, v) in &desired.spec.env {
        b = b.env(k, v);
    }

    for (k, v) in &desired.spec.labels {
        b = b.label(k, v);
    }
    b = b
        .label("mcc.stack", &desired.stack)
        .label("mcc.service", &desired.service)
        .label("mcc.ordinal", desired.ordinal.to_string());

    for p in &desired.spec.ports {
        let bind = IpAddr::from_str(&p.bind).unwrap_or_else(|_| IpAddr::from([127, 0, 0, 1]));
        if p.protocol.eq_ignore_ascii_case("udp") {
            b = b.port_udp_bind(bind, p.host, p.guest);
        } else {
            b = b.port_bind(bind, p.host, p.guest);
        }
    }

    let disable = desired.spec.network.profiles.iter().any(|p| p == "none");
    if disable {
        b = b.disable_network();
    } else {
        let profiles = network_profiles(desired);
        let policy = NetworkPolicy::from_profiles(profiles);
        b = b.network(|n| n.policy(policy));
    }

    // Host-side secret injection (guest sees placeholder only).
    for sec in &desired.secrets {
        if sec.allow_hosts.is_empty() {
            anyhow::bail!("secret env {:?} has empty allow_hosts (refused)", sec.env);
        }
        let env = sec.env.clone();
        let value = sec.value.clone();
        let hosts = sec.allow_hosts.clone();
        b = b.secret(move |s| {
            let mut s = s.env(env).value(value);
            for h in hosts {
                s = s.allow_host(h);
            }
            s
        });
    }

    info!(
        name = %desired.runtime_id,
        image = %desired.spec.image,
        cpus,
        memory_mib = mem,
        secrets = desired.secrets.len(),
        "creating detached microsandbox via SDK (local)"
    );

    b.create_detached()
        .await
        .with_context(|| format!("Sandbox::create_detached({})", desired.runtime_id))?;
    Ok(())
}

async fn observe(name: &str) -> Result<Option<MsbStatus>> {
    ensure_local_backend().await?;
    match Sandbox::get(name).await {
        Ok(handle) => Ok(Some(handle.status_snapshot())),
        Err(e) => {
            debug!(%name, error = %e, "sandbox get failed (treat as missing)");
            Ok(None)
        }
    }
}

#[async_trait]
impl NodeRuntime for MicrosandboxRuntime {
    async fn ensure_running(&self, desired: &DesiredSandbox) -> Result<SandboxStatus> {
        ensure_local_backend().await?;
        let name = desired.runtime_id.as_str();

        let policy = RestartPolicy::parse(&desired.spec.restart_policy);

        match observe(name).await? {
            Some(st) => {
                let phase = map_status(st);
                match phase {
                    SandboxPhase::Running => {
                        return Ok(SandboxStatus {
                            runtime_id: name.into(),
                            phase,
                            message: Some("microsandbox sdk (local)".into()),
                        });
                    }
                    SandboxPhase::Creating => {
                        return Ok(SandboxStatus {
                            runtime_id: name.into(),
                            phase,
                            message: Some("starting".into()),
                        });
                    }
                    other => match action_for_phase(policy, other) {
                        RestartAction::Leave => {
                            return Ok(SandboxStatus {
                                runtime_id: name.into(),
                                phase: other,
                                message: Some(format!(
                                    "left {} (restartPolicy={})",
                                    other.as_str(),
                                    desired.spec.restart_policy
                                )),
                            });
                        }
                        RestartAction::Recreate => {
                            warn!(%name, ?policy, "recreating sandbox per restartPolicy");
                            let _ = Sandbox::remove(name).await;
                            create_detached(desired).await?;
                        }
                        RestartAction::Start => {
                            info!(%name, ?other, "starting existing sandbox (detached)");
                            match Sandbox::start_detached(name).await {
                                Ok(_) => {}
                                Err(e) => {
                                    if policy == RestartPolicy::Never {
                                        return Ok(SandboxStatus {
                                            runtime_id: name.into(),
                                            phase: other,
                                            message: Some(format!("start failed: {e:#}")),
                                        });
                                    }
                                    warn!(%name, error = %e, "start_detached failed; recreating");
                                    let _ = Sandbox::remove(name).await;
                                    create_detached(desired).await?;
                                }
                            }
                        }
                    },
                }
            }
            None => {
                create_detached(desired).await?;
            }
        }

        let phase = match observe(name).await? {
            Some(st) => map_status(st),
            None => SandboxPhase::Creating,
        };

        Ok(SandboxStatus {
            runtime_id: name.into(),
            phase,
            message: Some("microsandbox sdk (local)".into()),
        })
    }

    async fn ensure_removed(&self, runtime_id: &str) -> Result<()> {
        ensure_local_backend().await?;
        let name = runtime_id;
        info!(%name, "stopping/removing microsandbox via SDK");

        if let Ok(handle) = Sandbox::get(name).await {
            let st = handle.status_snapshot();
            if matches!(
                st,
                MsbStatus::Running | MsbStatus::Draining | MsbStatus::Starting
            ) {
                if let Err(e) = handle.stop().await {
                    warn!(%name, error = %e, "stop failed");
                }
            }
        }

        match Sandbox::remove(name).await {
            Ok(()) => Ok(()),
            Err(e) => {
                let msg = e.to_string();
                if msg.to_ascii_lowercase().contains("not found")
                    || msg.to_ascii_lowercase().contains("no such")
                {
                    Ok(())
                } else {
                    Err(e).with_context(|| format!("Sandbox::remove({name})"))
                }
            }
        }
    }

    async fn status(&self, runtime_id: &str) -> Result<SandboxStatus> {
        ensure_local_backend().await?;
        let phase = match observe(runtime_id).await? {
            Some(st) => map_status(st),
            None => SandboxPhase::Stopped,
        };
        Ok(SandboxStatus {
            runtime_id: runtime_id.into(),
            phase,
            message: None,
        })
    }

    async fn list(&self) -> Result<Vec<String>> {
        ensure_local_backend().await?;
        let page = Sandbox::list().await.context("Sandbox::list")?;
        Ok(page
            .sandboxes
            .into_iter()
            .map(|h| h.name().to_string())
            .collect())
    }

    async fn exec_command(&self, runtime_id: &str, argv: &[String]) -> Result<i32> {
        ensure_local_backend().await?;
        if argv.is_empty() {
            anyhow::bail!("exec_command: empty argv");
        }
        let handle = Sandbox::get(runtime_id)
            .await
            .with_context(|| format!("Sandbox::get({runtime_id}) for exec"))?;
        let sb = handle
            .connect()
            .await
            .with_context(|| format!("SandboxHandle::connect({runtime_id}) for exec"))?;
        let cmd = argv[0].clone();
        let args: Vec<String> = argv[1..].to_vec();
        let output = sb
            .exec(cmd, args)
            .await
            .with_context(|| format!("Sandbox::exec({runtime_id})"))?;
        Ok(output.status().code)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mcc_api::{ResourceSpec, ServiceSpec};
    use std::collections::BTreeMap;

    #[test]
    fn maps_profiles() {
        let d = DesiredSandbox {
            instance_id: "i".into(),
            stack: "s".into(),
            service: "w".into(),
            ordinal: 0,
            runtime_id: "s-w-0".into(),
            spec: ServiceSpec {
                image: "alpine".into(),
                replicas: 1,
                resources: ResourceSpec {
                    cpus: 1,
                    memory_mib: 256,
                },
                ports: vec![],
                network: mcc_api::stack::NetworkSpec {
                    profiles: vec!["public".into(), "host".into()],
                },
                env: BTreeMap::new(),
                secrets: vec![],
                volumes: vec![],
                restart_policy: "on-failure".into(),
                health: None,
                labels: BTreeMap::new(),
                command: None,
                node_name: None,
                node_selector: BTreeMap::new(),
            },
            secrets: vec![],
        };
        let p = network_profiles(&d);
        assert!(p.contains(&NetworkProfile::Public));
        assert!(p.contains(&NetworkProfile::Host));
    }
}
