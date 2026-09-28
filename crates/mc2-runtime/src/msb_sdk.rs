//! Microsandbox backend using the official Rust SDK (`Sandbox::builder` / embed).
//!
//! Always uses the **local** backend. Host `MSB_API_KEY` / cloud profiles must not
//! hijack MC2 sandboxes.

use crate::networks::network_host_allow_ports;
use crate::restart::{action_for_phase, RestartAction, RestartPolicy};
use crate::spec::start_command_parts;
use crate::{
    DesiredSandbox, DiskUsage, DuplexStream, EnsureOutcome, EnsureRunning, ExecResult, LogLine,
    NodeRuntime, SandboxPhase, SandboxStatus, SshServer,
};
use anyhow::{Context, Result};
use async_trait::async_trait;
use futures::StreamExt;
use microsandbox::sandbox::SandboxStatus as MsbStatus;
use microsandbox::{set_default_backend, LocalBackend, NetworkPolicy, NetworkProfile, Sandbox};
use microsandbox_network::policy::{
    Action, Destination, DestinationGroup, Direction, PortRange, Protocol, Rule,
};
use std::collections::HashMap;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tracing::{debug, error, info, warn};

/// Ensure process-wide default is LocalBackend once per server process.
static LOCAL_BACKEND_INSTALLED: AtomicBool = AtomicBool::new(false);

/// Real microVM backend via the **microsandbox** crate (local libkrun only).
#[derive(Debug)]
pub struct MicrosandboxRuntime {
    /// MC2 volume root override (`--volume-dir` / `MC2_VOLUME_DIR`).
    /// `None` → `~/.mc2/volumes`. MC2 owns the directories under it and mounts
    /// `mc2-{stack}--{volume}` as a bind mount with an explicit quota.
    volume_dir: Option<PathBuf>,
    /// This install's id (`mc2.install` label / `list_owned` filter). Empty
    /// means "not configured" and refuses to create sandboxes, so a sandbox can
    /// never be created without the label ownership recovery depends on.
    install_id: String,
}

impl MicrosandboxRuntime {
    pub fn new(volume_dir: Option<PathBuf>) -> Self {
        Self {
            volume_dir,
            install_id: String::new(),
        }
    }

    /// Attach the install id that labels every sandbox this runtime creates.
    pub fn with_install_id(mut self, install_id: impl Into<String>) -> Self {
        self.install_id = install_id.into();
        self
    }

    /// The install id this runtime labels sandboxes with.
    pub fn install_id(&self) -> &str {
        &self.install_id
    }
}

/// Install the local backend process-wide, once.
///
/// MC2 owns its volume directories as bind mounts, so microsandbox's own
/// named-volume directory is unused and never overridden.
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

/// Map validated `network.profiles` values to msb profiles.
///
/// Closed set: `public` | `private` | `host` (stack validation rejects anything
/// else, and `none` is handled by `disable_network`). An unrecognised value
/// never becomes `Public` — it yields no profile at all, so the guest fails
/// closed. `Public` is the default **only** for an empty list.
fn network_profiles(desired: &DesiredSandbox) -> Vec<NetworkProfile> {
    let mut out = Vec::new();
    for p in &desired.spec.network.profiles {
        match p.as_str() {
            "public" => out.push(NetworkProfile::Public),
            "private" => out.push(NetworkProfile::Private),
            "host" => out.push(NetworkProfile::Host),
            "none" => {}
            other => {
                error!(
                    profile = %other,
                    "unknown network profile; refusing egress (never treated as public)"
                );
            }
        }
    }
    if desired.spec.network.profiles.is_empty() {
        out.push(NetworkProfile::Public);
    }
    out
}

/// Build msb network policy: base profiles + **narrow** Host TCP ports for network
/// reachability. Does **not** add `NetworkProfile::Host` or `Private` for network.
fn build_network_policy(desired: &DesiredSandbox) -> NetworkPolicy {
    let profiles = network_profiles(desired);
    let mut policy = NetworkPolicy::from_profiles(profiles);
    let network_ports = network_host_allow_ports(&desired.network);
    // Prepend so first-match-wins before broader profile rules.
    for port in network_ports.into_iter().rev() {
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
    if !desired.network.allows.is_empty() {
        debug!(
            runtime_id = %desired.runtime_id,
            allows = desired.network.allows.len(),
            "network: installed narrow Host:tcp:port egress rules (not Host profile)"
        );
    }
    policy
}

async fn create_detached(
    desired: &DesiredSandbox,
    volume_dir: Option<&Path>,
    install_id: &str,
) -> Result<()> {
    if install_id.is_empty() {
        anyhow::bail!(
            "runtime has no install id; refusing to create {} \
             (without the mc2.install label the node could never recover ownership of it)",
            desired.runtime_id
        );
    }
    ensure_local_backend().await?;

    let cpus = desired.spec.cpus.clamp(1, 255) as u8;
    let mem = desired.spec.mem_limit_mib.min(u32::MAX as u64) as u32;
    let root_disk_mib = desired.spec.root_disk_mib().clamp(1, u32::MAX as u64) as u32;

    // Note: `.replace()` is not accepted by create_detached on local backend.
    // Callers must remove an existing sandbox first if recreation is needed.
    let mut b = Sandbox::builder(desired.runtime_id.clone())
        .image(desired.spec.image.as_str())
        // Guest writable root disk (`services.<name>.storage_opt.size`). Always
        // explicit, so a spec change is a deliberate recreate rather than a
        // silent fallback to microsandbox's own 4 GiB default.
        .root_disk(root_disk_mib)
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
        .label("mc2.ordinal", desired.ordinal.to_string())
        // Ownership marker: `list_owned` filters on it, so a node only ever
        // adopts/removes sandboxes this install created (B2).
        .label("mc2.install", install_id);

    // MC2-owned volume directories, mounted as bind mounts with an explicit
    // per-start quota. microsandbox charges a bind mount's quota as growth
    // *beyond* the directory's existing contents, so passing
    // `size − current usage` makes the declared size an absolute cap that
    // survives restarts and can be resized (`mc2 up`) with the data kept.
    // A quota is never omitted: without one microsandbox would silently apply
    // its own 4 GiB default. Usage is measured fresh before every create.
    let root = crate::ensure_volume_dir(&crate::volume_root(volume_dir))
        .with_context(|| format!("create volume root for {volume_dir:?}"))?;
    let mut usage: HashMap<String, u64> = HashMap::new();
    for m in &desired.spec.volumes {
        let dir = crate::volume_name(&desired.stack, &m.name);
        let used = crate::dir_size_mib(&root.join(&dir));
        usage.insert(dir, used);
    }
    for plan in crate::volume_bind_plan(desired, &root, &usage) {
        // The host directory must exist before canonicalizing: microsandbox
        // refuses to follow symlinks on a bind-mount host path.
        let host = crate::ensure_volume_dir(&plan.host)
            .with_context(|| format!("create volume dir {}", plan.host.display()))?;
        debug!(
            runtime_id = %desired.runtime_id,
            guest = %plan.guest,
            host = %host.display(),
            quota_mib = plan.quota_mib,
            "volume: bind mount with guest-write quota"
        );
        let guest = plan.guest;
        let quota = plan.quota_mib;
        b = b.volume(guest, move |m| m.bind(host).quota(quota));
    }
    for p in &desired.spec.ports {
        // Ports always publish on loopback (network/ingress only, BYO Traefik).
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
        root_disk_mib,
        secrets = desired.secrets.len(),
        volumes = desired.spec.volumes.len(),
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
    async fn ensure_running(&self, desired: &DesiredSandbox) -> Result<EnsureRunning> {
        ensure_local_backend().await?;
        let name = desired.runtime_id.as_str();

        let policy = RestartPolicy::parse(&desired.spec.restart);

        // B10: every restart this backend performs (starting a Stopped sandbox,
        // recreating a Crashed one) is reported as `Restarted` so the
        // controller counts it against the restart-policy backoff.
        let outcome = match observe(name).await? {
            Some(st) => {
                let phase = map_status(st);
                match phase {
                    SandboxPhase::Running => {
                        return Ok(EnsureRunning {
                            status: SandboxStatus {
                                runtime_id: name.into(),
                                phase,
                                message: Some("microsandbox sdk (local)".into()),
                            },
                            outcome: EnsureOutcome::AlreadyRunning,
                        });
                    }
                    SandboxPhase::Creating => {
                        return Ok(EnsureRunning {
                            status: SandboxStatus {
                                runtime_id: name.into(),
                                phase,
                                message: Some("starting".into()),
                            },
                            outcome: EnsureOutcome::AlreadyRunning,
                        });
                    }
                    other => match action_for_phase(policy, other) {
                        RestartAction::Leave => {
                            return Ok(EnsureRunning {
                                status: SandboxStatus {
                                    runtime_id: name.into(),
                                    phase: other,
                                    message: Some(format!(
                                        "left {} (restart={})",
                                        other.as_str(),
                                        desired.spec.restart
                                    )),
                                },
                                outcome: EnsureOutcome::Left,
                            });
                        }
                        RestartAction::Recreate => {
                            warn!(%name, ?policy, "recreating sandbox per restartPolicy");
                            let _ = Sandbox::remove(name).await;
                            create_detached(desired, self.volume_dir.as_deref(), &self.install_id)
                                .await?;
                            EnsureOutcome::Restarted
                        }
                        RestartAction::Start => {
                            info!(%name, ?other, "starting existing sandbox (detached)");
                            match Sandbox::start_detached(name).await {
                                Ok(_) => {}
                                Err(e) => {
                                    if policy == RestartPolicy::Never {
                                        return Ok(EnsureRunning {
                                            status: SandboxStatus {
                                                runtime_id: name.into(),
                                                phase: other,
                                                message: Some(format!("start failed: {e:#}")),
                                            },
                                            outcome: EnsureOutcome::Left,
                                        });
                                    }
                                    warn!(%name, error = %e, "start_detached failed; recreating");
                                    let _ = Sandbox::remove(name).await;
                                    create_detached(
                                        desired,
                                        self.volume_dir.as_deref(),
                                        &self.install_id,
                                    )
                                    .await?;
                                }
                            }
                            EnsureOutcome::Restarted
                        }
                    },
                }
            }
            None => {
                create_detached(desired, self.volume_dir.as_deref(), &self.install_id).await?;
                EnsureOutcome::Created
            }
        };

        let phase = match observe(name).await? {
            Some(st) => map_status(st),
            None => SandboxPhase::Creating,
        };

        Ok(EnsureRunning {
            status: SandboxStatus {
                runtime_id: name.into(),
                phase,
                message: Some("microsandbox sdk (local)".into()),
            },
            outcome,
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

    /// Only this install's sandboxes: the SDK matches the `mc2.install` label
    /// server-side and paginates, so a foreign (unlabelled) workload is never
    /// even returned.
    async fn list_owned(&self, install_id: &str) -> Result<Vec<String>> {
        if install_id.is_empty() {
            anyhow::bail!("runtime has no install id; refusing to list sandboxes");
        }
        ensure_local_backend().await?;
        let mut owned = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let after = cursor.clone();
            let page = Sandbox::list_with(move |list| {
                let list = list.label("mc2.install", install_id);
                match after {
                    Some(c) => list.cursor(c),
                    None => list,
                }
            })
            .await
            .with_context(|| format!("Sandbox::list(mc2.install={install_id})"))?;
            owned.extend(page.sandboxes.iter().map(|h| h.name().to_string()));
            match page.next_cursor {
                Some(c) => cursor = Some(c),
                None => break,
            }
        }
        Ok(owned)
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
        ensure_local_backend().await?;
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
            // Raw bytes, not `stdout()`/`stderr()`: those return
            // `Result<String, FromUtf8Error>` and would drop non-UTF-8 output.
            stdout: output.stdout_bytes().to_vec(),
            stderr: output.stderr_bytes().to_vec(),
        })
    }

    fn volume_root(&self) -> PathBuf {
        crate::volume_root(self.volume_dir.as_deref())
    }

    /// Root-disk usage from the microsandbox live metrics registry (one shared
    /// memory read for every running sandbox).
    ///
    /// A sandbox whose sample has no `upper_used_bytes` (metrics disabled or
    /// not yet sampled) is skipped rather than reported as empty.
    async fn root_disk_usage(&self) -> Result<HashMap<String, DiskUsage>> {
        ensure_local_backend().await?;
        let all = microsandbox::all_sandbox_metrics()
            .await
            .context("microsandbox::all_sandbox_metrics")?;
        Ok(all
            .into_iter()
            .filter_map(|(name, m)| {
                let used = m.upper_used_bytes?;
                let capacity = m.upper_free_bytes.map(|free| used.saturating_add(free));
                Some((
                    name,
                    DiskUsage {
                        used_mib: used / (1024 * 1024),
                        capacity_mib: capacity.map(|b| b / (1024 * 1024)),
                    },
                ))
            })
            .collect())
    }

    /// Run a shell script in the guest and return stdout.
    ///
    /// Both `/etc/hosts` injection steps (read the DNS gateway, rewrite the
    /// file) go through this; the caller owns the script text.
    async fn guest_shell(&self, runtime_id: &str, script: &str) -> Result<String> {
        ensure_local_backend().await?;
        let handle = Sandbox::get(runtime_id)
            .await
            .with_context(|| format!("Sandbox::get({runtime_id}) for shell"))?;
        let sb = handle
            .connect()
            .await
            .with_context(|| format!("SandboxHandle::connect({runtime_id}) for shell"))?;
        let out = sb
            .shell(script)
            .await
            .with_context(|| format!("Sandbox::shell({runtime_id})"))?;
        Ok(out.stdout()?)
    }

    async fn read_logs(&self, runtime_id: &str, tail: Option<usize>) -> Result<Vec<LogLine>> {
        ensure_local_backend().await?;
        let opts = microsandbox::logs::LogOptions {
            tail,
            ..Default::default()
        };
        let entries = microsandbox::logs::read_logs(runtime_id, &opts)
            .await
            .with_context(|| format!("logs::read_logs({runtime_id})"))?;
        Ok(entries.into_iter().map(map_log_entry).collect())
    }

    async fn log_stream(
        &self,
        runtime_id: &str,
        from: Option<String>,
    ) -> Result<futures::stream::BoxStream<'static, Result<LogLine>>> {
        ensure_local_backend().await?;
        let start = match from {
            Some(cursor) => microsandbox::logs::LogStreamStart::From(
                cursor.parse().context("parse log cursor")?,
            ),
            None => microsandbox::logs::LogStreamStart::Beginning,
        };
        let opts = microsandbox::logs::LogStreamOptions {
            start,
            follow: true,
            ..Default::default()
        };
        let stream = microsandbox::logs::log_stream(runtime_id, &opts)
            .await
            .with_context(|| format!("logs::log_stream({runtime_id})"))?;
        Ok(stream
            .map(|r| r.map(map_log_entry).map_err(anyhow::Error::from))
            .boxed())
    }

    async fn ssh_server(
        &self,
        runtime_id: &str,
        user: &str,
        authorized_public_keys: &[String],
        sftp: bool,
    ) -> Result<Arc<dyn SshServer>> {
        ensure_local_backend().await?;
        let handle = Sandbox::get(runtime_id)
            .await
            .map_err(|e| anyhow::anyhow!("Sandbox::get({runtime_id}): {e}"))?;
        let sb = handle
            .connect()
            .await
            .map_err(|e| anyhow::anyhow!("Sandbox::connect({runtime_id}): {e}"))?;

        let keys = authorized_public_keys.to_vec();
        let user = user.to_string();
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
        Ok(Arc::new(MsbSshServer { server }))
    }
}

/// Map an SDK log entry onto the MC2-owned [`LogLine`] wire shape.
fn map_log_entry(e: microsandbox::logs::LogEntry) -> LogLine {
    LogLine {
        timestamp: e.timestamp.to_rfc3339(),
        source: format!("{:?}", e.source).to_ascii_lowercase(),
        data: e.data.to_vec(),
        cursor: e.cursor.to_string(),
    }
}

/// Adapter: the SDK's reusable SSH endpoint behind MC2's [`SshServer`].
struct MsbSshServer {
    server: microsandbox::SshServer,
}

#[async_trait]
impl SshServer for MsbSshServer {
    async fn serve(&self, stream: DuplexStream) -> Result<()> {
        self.server
            .serve(stream)
            .await
            .map_err(|e| anyhow::anyhow!("ssh server: {e}"))
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
            runtime_id: "s--w--0".into(),
            spec: ServiceSpec {
                image: "alpine".into(),
                scale: 1,
                cpus: 1,
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
                storage_opt: None,
                expose: vec![],
                networks: vec![],
                depends_on: BTreeMap::new(),
            },
            secrets: vec![],
            ssh: Default::default(),
            network: Default::default(),
        };
        let p = network_profiles(&d);
        assert!(p.contains(&NetworkProfile::Public));
        assert!(p.contains(&NetworkProfile::Host));
    }

    #[test]
    fn network_adds_narrow_host_port_not_profile() {
        use crate::networks::{DesiredNetwork, NetworkAllowDesired};
        let mut d = DesiredSandbox {
            instance_id: "i".into(),
            stack: "shop".into(),
            service: "web".into(),
            ordinal: 0,
            runtime_id: "shop--web--0".into(),
            spec: ServiceSpec {
                image: "alpine".into(),
                scale: 1,
                cpus: 1,
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
                storage_opt: None,
                expose: vec![],
                networks: vec![],
                depends_on: BTreeMap::new(),
            },
            secrets: vec![],
            ssh: Default::default(),
            network: DesiredNetwork {
                exposes: vec![],
                allows: vec![NetworkAllowDesired {
                    to_service: "db".into(),
                    port: 5432,
                    protocol: "tcp".into(),
                    fqdn: "db.shop.svc.mc2".into(),
                    short_name: "db".into(),
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
        d.network = Default::default();
    }

    fn profiled(profiles: &[&str]) -> DesiredSandbox {
        DesiredSandbox {
            instance_id: "i".into(),
            stack: "s".into(),
            service: "w".into(),
            ordinal: 0,
            runtime_id: "s--w--0".into(),
            spec: ServiceSpec {
                image: "alpine".into(),
                scale: 1,
                cpus: 1,
                mem_limit_mib: 256,
                ports: vec![],
                network: mc2_api::stack::NetworkSpec {
                    profiles: profiles.iter().map(|p| (*p).to_string()).collect(),
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
                storage_opt: None,
                expose: vec![],
                networks: vec![],
                depends_on: BTreeMap::new(),
            },
            secrets: vec![],
            ssh: Default::default(),
            network: Default::default(),
        }
    }

    #[test]
    fn empty_profiles_default_to_public() {
        let p = network_profiles(&profiled(&[]));
        assert_eq!(p.len(), 1);
        assert!(p.contains(&NetworkProfile::Public));
    }

    #[test]
    fn closed_profile_set_maps_without_aliases() {
        let private = network_profiles(&profiled(&["private"]));
        assert_eq!(private.len(), 1);
        assert!(private.contains(&NetworkProfile::Private));

        let host = network_profiles(&profiled(&["host"]));
        assert_eq!(host.len(), 1);
        assert!(host.contains(&NetworkProfile::Host));

        // `none` contributes no profile at all (disable_network is separate).
        assert!(network_profiles(&profiled(&["none"])).is_empty());
    }

    #[test]
    fn unknown_profile_never_falls_back_to_public() {
        // Aliases (`local`/`any`) and typos must not grant any egress.
        for bad in ["local", "any", "privte"] {
            let got = network_profiles(&profiled(&[bad]));
            assert!(got.is_empty(), "{bad} must map to no profile");
            assert!(
                !got.contains(&NetworkProfile::Public),
                "{bad} must never become Public"
            );
        }
    }

    /// B2: a runtime with no install id must not create or list sandboxes — an
    /// unlabelled sandbox could never be recovered (or safely cleaned up) after
    /// a restart, so this fails before any backend is touched.
    #[tokio::test]
    async fn refuses_to_create_or_list_without_an_install_id() {
        let rt = MicrosandboxRuntime::new(None);
        assert!(rt.list_owned("").await.is_err());
        let d = profiled(&["public"]);
        assert!(create_detached(&d, None, "").await.is_err());
    }
}
