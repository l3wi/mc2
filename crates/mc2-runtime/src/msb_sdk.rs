//! Microsandbox backend using the official Rust SDK (`Sandbox::builder` / embed).
//!
//! Always uses the **local** backend. Host `MSB_API_KEY` / cloud profiles must not
//! hijack MC2 agent sandboxes.

use crate::fabric::fabric_host_allow_ports;
use crate::restart::{action_for_phase, RestartAction, RestartPolicy};
use crate::spec::start_command_parts;
use crate::{DesiredSandbox, ExecResult, NodeRuntime, SandboxPhase, SandboxStatus};
use anyhow::{Context, Result};
use async_trait::async_trait;
use microsandbox::sandbox::SandboxStatus as MsbStatus;
use microsandbox::{set_default_backend, LocalBackend, NetworkPolicy, NetworkProfile, Sandbox};
use microsandbox_network::policy::{
    Action, Destination, DestinationGroup, Direction, PortRange, Protocol, Rule,
};
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use tracing::{debug, info, warn};

/// Ensure process-wide default is LocalBackend once per agent process.
static LOCAL_BACKEND_INSTALLED: AtomicBool = AtomicBool::new(false);

/// Real microVM backend via the **microsandbox** crate (local libkrun only).
#[derive(Debug)]
pub struct MicrosandboxRuntime {
    /// Named-volume root override (agent `--volume-dir`). `None` = msb default
    /// (`~/.microsandbox/volumes`). Applied process-wide on first backend init.
    volume_dir: Option<PathBuf>,
}

impl MicrosandboxRuntime {
    pub fn new(volume_dir: Option<PathBuf>) -> Self {
        Self { volume_dir }
    }
}

async fn ensure_local_backend(volume_dir: Option<&Path>) -> Result<()> {
    if LOCAL_BACKEND_INSTALLED.load(Ordering::SeqCst) {
        return Ok(());
    }
    // Install local even if MSB_API_KEY / cloud profile would otherwise win.
    let local = match volume_dir {
        Some(dir) => LocalBackend::builder()
            .volumes_dir(resolve_volume_dir(dir)?)
            .build()
            .await
            .with_context(|| format!("LocalBackend builder with volumes_dir={dir:?}"))?,
        None => LocalBackend::new()
            .await
            .context("LocalBackend::new (open microsandbox local DB)")?,
    };
    set_default_backend(local);
    LOCAL_BACKEND_INSTALLED.store(true, Ordering::SeqCst);
    info!(
        volume_dir = volume_dir.map(|p| p.display().to_string()),
        "microsandbox: forced LocalBackend (ignores MSB_API_KEY / cloud profiles)"
    );
    Ok(())
}

/// Create the override volume root and resolve symlinks (e.g. macOS
/// `/tmp` → `/private/tmp`): msb refuses to follow symlinks when mounting,
/// so the backend must receive a canonical path.
fn resolve_volume_dir(dir: &Path) -> Result<PathBuf> {
    std::fs::create_dir_all(dir).with_context(|| format!("create volume dir {}", dir.display()))?;
    std::fs::canonicalize(dir).with_context(|| format!("canonicalize volume dir {}", dir.display()))
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

/// Build msb network policy: base profiles + **narrow** Host TCP ports for fabric
/// allows. Does **not** add `NetworkProfile::Host` or `Private` for fabric (D13/Q2).
fn build_network_policy(desired: &DesiredSandbox) -> NetworkPolicy {
    let profiles = network_profiles(desired);
    let mut policy = NetworkPolicy::from_profiles(profiles);
    let fabric_ports = fabric_host_allow_ports(&desired.fabric);
    // Prepend so first-match-wins before broader profile rules.
    for port in fabric_ports.into_iter().rev() {
        policy.rules.insert(
            0,
            Rule {
                direction: Direction::Egress,
                destination: Destination::Group(DestinationGroup::Host),
                protocols: vec![Protocol::Tcp],
                ports: vec![PortRange::single(port)],
                action: Action::Allow,
            },
        );
    }
    if !desired.fabric.allows.is_empty() {
        debug!(
            runtime_id = %desired.runtime_id,
            allows = desired.fabric.allows.len(),
            "fabric: installed narrow Host:tcp:port egress rules (not Host profile)"
        );
    }
    policy
}

async fn create_detached(desired: &DesiredSandbox, volume_dir: Option<&Path>) -> Result<()> {
    ensure_local_backend(volume_dir).await?;

    let cpus = (desired.spec.cpus.clamp(1.0, 255.0)) as u8;
    let mem = desired.spec.mem_limit_mib.min(u32::MAX as u64) as u32;

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
        .label("mc2.stack", &desired.stack)
        .label("mc2.service", &desired.service)
        .label("mc2.ordinal", desired.ordinal.to_string());

    // Node-local named directory volumes: create-or-reuse under the backend
    // volumes dir; data survives sandbox removal (msb retains named volumes).
    let mounts = crate::volume_mount_plan(desired);
    for (guest, msb_name) in mounts {
        b = b.volume(guest, move |m| {
            m.named_with(msb_name, |v| v.ensure_exists().directory())
        });
    }
    for p in &desired.spec.ports {
        // Ports always publish on loopback (fabric/ingress only, BYO Traefik).
        let bind = IpAddr::from([127, 0, 0, 1]);
        if p.protocol.eq_ignore_ascii_case("udp") {
            b = b.port_udp_bind(bind, p.published, p.target);
        } else {
            b = b.port_bind(bind, p.published, p.target);
        }
    }

    let disable = desired.spec.network.profiles.iter().any(|p| p == "none");
    if disable {
        b = b.disable_network();
    } else {
        let policy = build_network_policy(desired);
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
        volumes = desired.spec.volumes.len(),
    );

    b.create_detached()
        .await
        .with_context(|| format!("Sandbox::create_detached({})", desired.runtime_id))?;
    Ok(())
}

async fn observe(name: &str, volume_dir: Option<&Path>) -> Result<Option<MsbStatus>> {
    ensure_local_backend(volume_dir).await?;
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
        ensure_local_backend(self.volume_dir.as_deref()).await?;
        let name = desired.runtime_id.as_str();

        let policy = RestartPolicy::parse(&desired.spec.restart);

        match observe(name, self.volume_dir.as_deref()).await? {
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
                                    "left {} (restart={})",
                                    other.as_str(),
                                    desired.spec.restart
                                )),
                            });
                        }
                        RestartAction::Recreate => {
                            warn!(%name, ?policy, "recreating sandbox per restartPolicy");
                            let _ = Sandbox::remove(name).await;
                            create_detached(desired, self.volume_dir.as_deref()).await?;
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
                                    create_detached(desired, self.volume_dir.as_deref()).await?;
                                }
                            }
                        }
                    },
                }
            }
            None => {
                create_detached(desired, self.volume_dir.as_deref()).await?;
            }
        }

        let phase = match observe(name, self.volume_dir.as_deref()).await? {
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
        ensure_local_backend(self.volume_dir.as_deref()).await?;
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
        ensure_local_backend(self.volume_dir.as_deref()).await?;
        let phase = match observe(runtime_id, self.volume_dir.as_deref()).await? {
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
        ensure_local_backend(self.volume_dir.as_deref()).await?;
        let page = Sandbox::list().await.context("Sandbox::list")?;
        Ok(page
            .sandboxes
            .into_iter()
            .map(|h| h.name().to_string())
            .collect())
    }

    async fn exec_command(&self, runtime_id: &str, argv: &[String]) -> Result<i32> {
        Ok(self
            .exec_with_output(runtime_id, argv, &[])
            .await?
            .exit_code)
    }

    async fn exec_with_output(
        &self,
        runtime_id: &str,
        argv: &[String],
        stdin: &[u8],
    ) -> Result<ExecResult> {
        ensure_local_backend(self.volume_dir.as_deref()).await?;
        if argv.is_empty() {
            anyhow::bail!("exec: empty argv");
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
        let input = stdin.to_vec();
        let mut exec = sb
            .exec_stream_with(cmd, |e| e.args(args).stdin_bytes(input))
            .await
            .with_context(|| format!("Sandbox::exec({runtime_id})"))?;
        let output = exec
            .collect()
            .await
            .with_context(|| format!("Sandbox::exec collect({runtime_id})"))?;
        Ok(ExecResult {
            exit_code: output.status().code,
            stdout: output.stdout().unwrap_or_default(),
            stderr: output.stderr().unwrap_or_default(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mc2_api::ServiceSpec;
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
                scale: 1,
                cpus: 1.0,
                mem_limit_mib: 256,
                ports: vec![],
                network: mc2_api::stack::NetworkSpec {
                    profiles: vec!["public".into(), "host".into()],
                },
                env: BTreeMap::new(),
                secrets: vec![],
                volumes: vec![],
                restart: "on-failure".into(),
                healthcheck: None,
                labels: BTreeMap::new(),
                command: None,
                node_name: None,
                node_selector: BTreeMap::new(),
                ssh: None,
                expose: vec![],
                networks: vec![],
            },
            secrets: vec![],
            ssh: Default::default(),
            fabric: Default::default(),
        };
        let p = network_profiles(&d);
        assert!(p.contains(&NetworkProfile::Public));
        assert!(p.contains(&NetworkProfile::Host));
    }

    #[test]
    fn fabric_adds_narrow_host_port_not_profile() {
        use crate::fabric::{DesiredFabric, FabricAllowDesired};
        let mut d = DesiredSandbox {
            instance_id: "i".into(),
            stack: "shop".into(),
            service: "web".into(),
            ordinal: 0,
            runtime_id: "shop-web-0".into(),
            spec: ServiceSpec {
                image: "alpine".into(),
                scale: 1,
                cpus: 1.0,
                mem_limit_mib: 256,
                ports: vec![],
                network: mc2_api::stack::NetworkSpec {
                    profiles: vec!["public".into()],
                },
                env: BTreeMap::new(),
                secrets: vec![],
                volumes: vec![],
                restart: "on-failure".into(),
                healthcheck: None,
                labels: BTreeMap::new(),
                command: None,
                node_name: None,
                node_selector: BTreeMap::new(),
                ssh: None,
                expose: vec![],
                networks: vec![],
            },
            secrets: vec![],
            ssh: Default::default(),
            fabric: DesiredFabric {
                exposes: vec![],
                allows: vec![FabricAllowDesired {
                    to_service: "db".into(),
                    port: 5432,
                    protocol: "tcp".into(),
                    fqdn: "db.shop.svc.mc2".into(),
                    short_name: "db".into(),
                    backend_instance_id: "x".into(),
                    backend_node_id: "n".into(),
                    backend_local: true,
                    backend_ordinal: 0,
                }],
            },
        };
        let policy = build_network_policy(&d);
        // Narrow Host:5432 rule present; profile set is still Public-only (no Host profile).
        let profiles = network_profiles(&d);
        assert!(!profiles.contains(&NetworkProfile::Host));
        assert!(!profiles.contains(&NetworkProfile::Private));
        assert!(policy.rules.iter().any(|r| {
            r.action == Action::Allow
                && matches!(r.destination, Destination::Group(DestinationGroup::Host))
                && r.ports.iter().any(|p| p.start == 5432 && p.end == 5432)
        }));
        d.fabric = Default::default();
    }
}
