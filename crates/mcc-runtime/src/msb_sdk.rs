//! Microsandbox backend using the official Rust SDK (`Sandbox::builder` / embed).

use crate::spec::start_command_parts;
use crate::{DesiredSandbox, NodeRuntime, SandboxPhase, SandboxStatus};
use anyhow::{Context, Result};
use async_trait::async_trait;
use microsandbox::sandbox::SandboxStatus as MsbStatus;
use microsandbox::{NetworkPolicy, NetworkProfile, Sandbox};
use std::net::IpAddr;
use std::str::FromStr;
use tracing::{debug, info, warn};

/// Real microVM backend via the **microsandbox** crate (not the `msb` CLI).
#[derive(Debug, Default)]
pub struct MicrosandboxRuntime;

impl MicrosandboxRuntime {
    pub fn new() -> Self {
        Self
    }
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
    let cpus = desired.spec.resources.cpus.clamp(1, 255) as u8;
    let mem = desired.spec.resources.memory_mib.min(u32::MAX as u64) as u32;

    // Note: `.replace()` is not accepted by create_detached on local backend.
    // Callers must remove an existing sandbox first if recreation is needed.
    let mut b = Sandbox::builder(desired.runtime_id.clone())
        .image(desired.spec.image.as_str())
        .cpus(cpus)
        .memory(mem)
        .detached(true);

    // Guest command for detached run (replaces image CMD; keeps ENTRYPOINT).
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

    info!(
        name = %desired.runtime_id,
        image = %desired.spec.image,
        cpus,
        memory_mib = mem,
        "creating detached microsandbox via SDK"
    );

    b.create_detached()
        .await
        .with_context(|| format!("Sandbox::create_detached({})", desired.runtime_id))?;
    Ok(())
}

async fn observe(name: &str) -> Result<Option<MsbStatus>> {
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
        let name = desired.runtime_id.as_str();

        match observe(name).await? {
            Some(st) => {
                let phase = map_status(st);
                match phase {
                    SandboxPhase::Running => {
                        return Ok(SandboxStatus {
                            runtime_id: name.into(),
                            phase,
                            message: Some("microsandbox sdk".into()),
                        });
                    }
                    SandboxPhase::Creating => {
                        return Ok(SandboxStatus {
                            runtime_id: name.into(),
                            phase,
                            message: Some("starting".into()),
                        });
                    }
                    SandboxPhase::Failed => {
                        // Replace crashed sandbox
                        warn!(%name, "sandbox crashed; recreating");
                        let _ = Sandbox::remove(name).await;
                        create_detached(desired).await?;
                    }
                    SandboxPhase::Stopped | SandboxPhase::Pending | SandboxPhase::Unknown => {
                        info!(%name, ?phase, "starting existing sandbox (detached)");
                        match Sandbox::start_detached(name).await {
                            Ok(_) => {}
                            Err(e) => {
                                warn!(%name, error = %e, "start_detached failed; recreating");
                                let _ = Sandbox::remove(name).await;
                                create_detached(desired).await?;
                            }
                        }
                    }
                }
            }
            None => {
                create_detached(desired).await?;
            }
        }

        // Refresh status after create/start
        let phase = match observe(name).await? {
            Some(st) => map_status(st),
            None => SandboxPhase::Creating,
        };

        Ok(SandboxStatus {
            runtime_id: name.into(),
            phase,
            message: Some("microsandbox sdk".into()),
        })
    }

    async fn ensure_removed(&self, runtime_id: &str) -> Result<()> {
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
                // Missing is fine
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
        let page = Sandbox::list().await.context("Sandbox::list")?;
        // SandboxPage API — try common field names via debug if needed
        Ok(page_names(page))
    }
}

fn page_names(page: microsandbox::SandboxPage) -> Vec<String> {
    // SandboxPage exposes items/sandboxes; use Display/debug-friendly access.
    // From SDK: typically `.items` or iterator — check via public API.
    page_names_impl(page)
}

fn page_names_impl(page: microsandbox::SandboxPage) -> Vec<String> {
    // Prefer documented accessors; fall back empty if shape differs at compile time.
    #[allow(unused_mut)]
    let mut names = Vec::new();
    // `SandboxPage` in 0.6.x: public field `sandboxes: Vec<SandboxHandle>`
    // and each handle has `.name()`.
    for h in page.sandboxes {
        names.push(h.name().to_string());
    }
    names
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
        };
        let p = network_profiles(&d);
        assert!(p.contains(&NetworkProfile::Public));
        assert!(p.contains(&NetworkProfile::Host));
    }
}
