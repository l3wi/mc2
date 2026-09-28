//! Host-mediated service network dataplane (L4 splice).
//!
//! - **expose**: track msb loopback publish host ports (allocated at create).
//! - **listeners**: one **held** user-space L4 splice per exposed guest port `P`
//!   on `127.0.0.1:P`, bound at the start of every reconcile pass — before any
//!   backend exists — and kept for as long as `P` is exposed by any desired
//!   instance. With no Ready backend a connection is accepted and closed.
//! - **edges**: each connection **round-robins** across the Ready backends of
//!   the port's owning `(stack, service)` (read per connection, so no restart on
//!   phase churn). All consumers route through the injected DNS gateway → the
//!   shared splice, so `P` is the whole identity of the edge.
//! - DNS names injected into guest `/etc/hosts` → gateway IP.
//! - Never enables full Host/Private profiles (policy is create-time narrow rules).
//!
//! Port claims are exclusive server-wide (enforced at apply), so a splice is
//! keyed by port alone. A **bind failure** is recorded and reported per port; a
//! consumer whose allowed port has no listener is failed closed by the node loop
//! (its create-time `Host:tcp:P` rule must never reach a foreign process).

use mc2_runtime::{
    DesiredSandbox, InstanceReport, NetworkAllowDesired, NetworkEdgeStatus, NetworkExposeStatus,
    NetworkObserved, NetworkPhase, NodeRuntime,
};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::copy_bidirectional;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

/// Consecutive accept failures before a splice task gives up; the next pass
/// re-binds it (see [`NetworkTable::ensure_listeners`]).
const MAX_ACCEPT_ERRORS: u32 = 8;

#[derive(Debug, Clone)]
struct ExposeBinding {
    guest_port: u16,
    host_port: u16,
}

/// Shared backend registry: exposed guest port → ready host sockets.
type BackendRegistry = Arc<Mutex<HashMap<u16, Vec<SocketAddr>>>>;

/// Exposed ports whose splice listener this process holds right now, shared
/// with the apply path. After `mc2 down` a stack's claim is gone but its
/// listener lives until the next pass releases it; apply must not mistake
/// that listener for a foreign process when the port is claimed again.
pub type HeldSplicePorts = Arc<parking_lot::Mutex<BTreeSet<u16>>>;

/// Per-node network state.
pub struct NetworkTable {
    /// instance_id → expose bindings (after sandbox create).
    exposes: HashMap<String, Vec<ExposeBinding>>,
    /// Shared expose index: backend instance_id → guest_port → host_port
    publish_index: Arc<Mutex<HashMap<String, HashMap<u16, u16>>>>,
    /// instance_id → last successful hosts inject key (names+gw).
    hosts_key: HashMap<String, String>,
    /// exposed guest port → held splice task (ports are exclusive server-wide).
    splices: HashMap<u16, JoinHandle<()>>,
    /// exposed guest port → why its splice could not bind (fail-closed signal).
    failed_splices: HashMap<u16, String>,
    /// Mirror of `splices`' keys for the apply path (see [`HeldSplicePorts`]).
    held: HeldSplicePorts,
    /// Ready backend host sockets per exposed guest port; read per connection.
    backends: BackendRegistry,
    /// Round-robin cursor across the table.
    rr_counter: Arc<AtomicUsize>,
    /// Process shutdown token: every splice listener selects on it so a cancel
    /// stops accepting immediately (D2).
    cancel: CancellationToken,
}

impl NetworkTable {
    pub fn new() -> Self {
        Self {
            exposes: HashMap::new(),
            publish_index: Arc::new(Mutex::new(HashMap::new())),
            hosts_key: HashMap::new(),
            splices: HashMap::new(),
            failed_splices: HashMap::new(),
            held: HeldSplicePorts::default(),
            backends: Arc::new(Mutex::new(HashMap::new())),
            rr_counter: Arc::new(AtomicUsize::new(0)),
            cancel: CancellationToken::new(),
        }
    }

    /// Install the process shutdown token. Called once, before the first pass,
    /// so held splice listeners stop accepting when `run()` cancels (D2).
    pub fn set_cancel(&mut self, cancel: CancellationToken) {
        self.cancel = cancel;
    }

    /// Publish the held-port set into `held` (shared with the apply path).
    pub fn share_held_ports(&mut self, held: HeldSplicePorts) {
        self.held = held;
        self.sync_held();
    }

    fn sync_held(&self) {
        *self.held.lock() = self.splices.keys().copied().collect();
    }

    /// Abort every held splice listener. Called as the node loop exits.
    pub fn shutdown(&mut self) {
        for (port, handle) in self.splices.drain() {
            handle.abort();
            debug!(port, "network splice stopped (shutdown)");
        }
        self.sync_held();
    }

    /// Reserve host ports for network exposes and merge into desired ports list.
    /// Call before `ensure_running` so msb create includes publishes.
    pub async fn prepare_exposes(&mut self, desired: &mut DesiredSandbox) -> Result<(), String> {
        if desired.network.exposes.is_empty() {
            self.exposes.remove(&desired.instance_id);
            let mut idx = self.publish_index.lock().await;
            idx.remove(&desired.instance_id);
            return Ok(());
        }

        // Reuse stable host ports across reconciles so msb recreate is not forced.
        let existing = self.exposes.get(&desired.instance_id).cloned();

        let mut bindings = Vec::new();
        for ex in &desired.network.exposes {
            let host_port = if let Some(b) = existing
                .as_ref()
                .and_then(|v| v.iter().find(|b| b.guest_port == ex.guest_port))
            {
                b.host_port
            } else if let Some(p) = desired
                .spec
                .ports
                .iter()
                .find(|p| p.target == ex.guest_port && p.protocol.eq_ignore_ascii_case("tcp"))
            {
                p.published
            } else {
                reserve_ephemeral().await.map_err(|e| {
                    format!(
                        "network expose {}: bind 127.0.0.1 failed: {e}",
                        ex.guest_port
                    )
                })?
            };

            if !desired
                .spec
                .ports
                .iter()
                .any(|p| p.target == ex.guest_port && p.protocol.eq_ignore_ascii_case("tcp"))
            {
                desired.spec.ports.push(mc2_api::PortSpec {
                    published: host_port,
                    target: ex.guest_port,
                    protocol: "tcp".into(),
                    hostname: None,
                });
            }
            bindings.push(ExposeBinding {
                guest_port: ex.guest_port,
                host_port,
            });
        }

        {
            let mut idx = self.publish_index.lock().await;
            let mut map = HashMap::new();
            for b in &bindings {
                map.insert(b.guest_port, b.host_port);
            }
            idx.insert(desired.instance_id.clone(), map);
        }
        self.exposes.insert(desired.instance_id.clone(), bindings);
        Ok(())
    }

    /// Bind (and hold) one splice listener per exposed port in the desired set.
    ///
    /// Runs **before** the per-instance loop and regardless of backend phase:
    /// an exposed port is claimed from apply until the stack is removed, so a
    /// client sees accept-then-close while no backend is Ready rather than
    /// connection-refused (and a foreign host process can never take the port
    /// while the claim lives). Stale listeners are aborted; a listener whose
    /// task finished is re-bound. A bind failure is recorded per port and makes
    /// consumers fail closed in the node loop.
    pub async fn ensure_listeners(&mut self, desired: &[DesiredSandbox]) {
        let mut wanted: BTreeSet<u16> = BTreeSet::new();
        for d in desired {
            for ex in &d.network.exposes {
                wanted.insert(ex.guest_port);
            }
        }

        // Abort listeners for ports no longer exposed by any desired instance.
        let stale: Vec<u16> = self
            .splices
            .keys()
            .copied()
            .filter(|p| !wanted.contains(p))
            .collect();
        for port in stale {
            if let Some(h) = self.splices.remove(&port) {
                h.abort();
                info!(port, "network splice stopped (port no longer exposed)");
            }
        }
        self.failed_splices.retain(|port, _| wanted.contains(port));

        for &port in &wanted {
            // Keep a live listener; re-bind when the task gave up earlier.
            if self.splices.get(&port).is_some_and(|h| !h.is_finished()) {
                continue;
            }
            self.splices.remove(&port);
            match TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], port))).await {
                Ok(listener) => {
                    let backends = self.backends.clone();
                    let counter = self.rr_counter.clone();
                    let handle = tokio::spawn(splice_loop(
                        listener,
                        port,
                        backends,
                        counter,
                        self.cancel.clone(),
                    ));
                    self.splices.insert(port, handle);
                    self.failed_splices.remove(&port);
                    info!(port, "network shared splice listening");
                }
                Err(e) => {
                    let cause = format!("bind 127.0.0.1:{port} failed: {e}");
                    warn!(port, error = %e, "network splice bind failed");
                    self.failed_splices.insert(port, cause);
                }
            }
        }
        self.sync_held();
    }

    /// Rebuild the shared backend registry from the current desired set and the
    /// phases observed this pass. Running backends only; a port with no backend
    /// stays listening but accepts-and-closes. Swapping the map re-targets live
    /// splices without restarting them.
    pub async fn update_backends(
        &mut self,
        desired: &[DesiredSandbox],
        reports: &[InstanceReport],
    ) {
        let phases: HashMap<&str, &str> = reports
            .iter()
            .map(|r| (r.instance_id.as_str(), r.phase.as_str()))
            .collect();

        let mut new_backends: HashMap<u16, Vec<SocketAddr>> = HashMap::new();
        {
            let idx = self.publish_index.lock().await;
            for d in desired {
                if d.network.exposes.is_empty() {
                    continue;
                }
                if phases.get(d.instance_id.as_str()) != Some(&"Running") {
                    continue;
                }
                let Some(hosts) = idx.get(&d.instance_id) else {
                    continue;
                };
                for ex in &d.network.exposes {
                    if let Some(host) = hosts.get(&ex.guest_port) {
                        new_backends
                            .entry(ex.guest_port)
                            .or_default()
                            .push(SocketAddr::from(([127, 0, 0, 1], *host)));
                    }
                }
            }
        }
        // Deterministic round-robin order.
        for v in new_backends.values_mut() {
            v.sort_by_key(|a| a.port());
        }

        let mut b = self.backends.lock().await;
        *b = new_backends;
    }

    /// Allowed ports with no live listener we bound, with the reason.
    ///
    /// The node loop refuses to create such an instance: a create-time
    /// `Host:tcp:P` egress rule cannot be narrowed later, so a VM whose rule
    /// could reach a foreign process on `P` must never start (A3, fail closed).
    pub fn listener_gaps(&self, allows: &[NetworkAllowDesired]) -> Vec<(u16, String)> {
        let ports: BTreeSet<u16> = allows.iter().map(|a| a.port).collect();
        ports
            .into_iter()
            .filter_map(|port| {
                if self.splices.get(&port).is_some_and(|h| !h.is_finished()) {
                    return None;
                }
                let cause = self.failed_splices.get(&port).cloned().unwrap_or_else(|| {
                    format!("no service exposing port {port} is scheduled on this node")
                });
                Some((port, cause))
            })
            .collect()
    }

    /// After sandbox is Running: inject DNS hosts + report network status.
    pub async fn reconcile_running(
        &mut self,
        desired: &DesiredSandbox,
        runtime: &Arc<dyn NodeRuntime>,
    ) -> NetworkObserved {
        let mut observed = NetworkObserved {
            exposes: vec![],
            edges: vec![],
            message: String::new(),
        };

        if let Some(binds) = self.exposes.get(&desired.instance_id) {
            for b in binds {
                observed.exposes.push(NetworkExposeStatus {
                    guest_port: b.guest_port,
                    host_port: b.host_port,
                    phase: NetworkPhase::Ready.as_str().into(),
                    message: String::new(),
                });
            }
        } else if !desired.network.exposes.is_empty() {
            for ex in &desired.network.exposes {
                observed.exposes.push(NetworkExposeStatus {
                    guest_port: ex.guest_port,
                    host_port: 0,
                    phase: NetworkPhase::Failed.as_str().into(),
                    message: "expose not prepared before create".into(),
                });
            }
        }

        let backends = self.backends.lock().await;
        for a in &desired.network.allows {
            let (phase, message) = if let Some(cause) = self.failed_splices.get(&a.port) {
                (NetworkPhase::Failed.as_str(), cause.clone())
            } else {
                let has = self.splices.get(&a.port).is_some_and(|h| !h.is_finished());
                let up = backends
                    .get(&a.port)
                    .map(|v| !v.is_empty())
                    .unwrap_or(false);
                if has && up {
                    (NetworkPhase::Ready.as_str(), String::new())
                } else {
                    (
                        NetworkPhase::Pending.as_str(),
                        "waiting for backend".to_string(),
                    )
                }
            };
            observed.edges.push(NetworkEdgeStatus {
                to_service: a.to_service.clone(),
                port: a.port,
                phase: phase.into(),
                message,
            });
        }
        drop(backends);

        if let Err(e) = self.inject_hosts(desired, &observed, runtime).await {
            warn!(
                instance = %desired.instance_id,
                error = %e,
                "network hosts inject failed"
            );
            if observed.message.is_empty() {
                observed.message = format!("hosts inject: {e}");
            }
        }

        observed
    }

    pub async fn reconcile_not_running(&mut self, desired: &DesiredSandbox) -> NetworkObserved {
        self.hosts_key.remove(&desired.instance_id);

        let mut observed = NetworkObserved::default();
        for ex in &desired.network.exposes {
            let host = self
                .exposes
                .get(&desired.instance_id)
                .and_then(|v| v.iter().find(|b| b.guest_port == ex.guest_port))
                .map(|b| b.host_port)
                .unwrap_or(0);
            observed.exposes.push(NetworkExposeStatus {
                guest_port: ex.guest_port,
                host_port: host,
                phase: NetworkPhase::Pending.as_str().into(),
                message: "sandbox not running".into(),
            });
        }
        for a in &desired.network.allows {
            observed.edges.push(NetworkEdgeStatus {
                to_service: a.to_service.clone(),
                port: a.port,
                phase: NetworkPhase::Pending.as_str().into(),
                message: "sandbox not running".into(),
            });
        }
        observed
    }

    /// Host publish ports for this instance's network exposes (if prepared).
    pub fn expose_host_ports(&self, instance_id: &str) -> Option<Vec<u16>> {
        self.exposes
            .get(instance_id)
            .map(|v| v.iter().map(|b| b.host_port).collect())
    }

    /// Clear per-instance expose state before force-recreate. Shared listeners
    /// are untouched; `update_backends` re-targets on the next cycle.
    pub async fn drop_instance(&mut self, instance_id: &str) {
        self.hosts_key.remove(instance_id);
        self.exposes.remove(instance_id);
        let mut idx = self.publish_index.lock().await;
        idx.remove(instance_id);
    }

    pub async fn close_missing(&mut self, keep: &HashSet<String>) {
        let drop_ids: Vec<String> = self
            .exposes
            .keys()
            .chain(self.hosts_key.keys())
            .filter(|id| !keep.contains(*id))
            .cloned()
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        for id in drop_ids {
            self.exposes.remove(&id);
            self.hosts_key.remove(&id);
            let mut idx = self.publish_index.lock().await;
            idx.remove(&id);
        }
    }

    async fn inject_hosts(
        &mut self,
        desired: &DesiredSandbox,
        observed: &NetworkObserved,
        runtime: &Arc<dyn NodeRuntime>,
    ) -> anyhow::Result<()> {
        let ready_names: Vec<(String, String)> = desired
            .network
            .allows
            .iter()
            .filter(|a| {
                observed
                    .edges
                    .iter()
                    .any(|e| e.to_service == a.to_service && e.port == a.port && e.phase == "Ready")
            })
            .map(|a| (a.fqdn.clone(), a.short_name.clone()))
            .collect();
        if ready_names.is_empty() {
            return Ok(());
        }

        let gw = runtime
            .guest_shell(
                &desired.runtime_id,
                "awk '/^nameserver /{print $2; exit}' /etc/resolv.conf",
            )
            .await?
            .trim()
            .to_string();
        if gw.is_empty() {
            anyhow::bail!("empty gateway from resolv.conf");
        }

        let mut key_parts: Vec<String> = ready_names
            .iter()
            .map(|(f, s)| format!("{f}+{s}"))
            .collect();
        key_parts.sort();
        let inject_key = format!("{gw}|{}", key_parts.join(","));
        if self.hosts_key.get(&desired.instance_id) == Some(&inject_key) {
            return Ok(());
        }

        // Rebuild network host lines under a marker for idempotency.
        let mut entries = String::new();
        for (fqdn, short) in &ready_names {
            entries.push_str(&format!("{gw} {fqdn} {short}\n"));
        }
        // Escape for single-quoted shell heredoc is awkward; use printf lines.
        let mut script = String::from(
            "set -e; grep -v ' #mc2-network$' /etc/hosts > /tmp/hosts.mc2 2>/dev/null || true; ",
        );
        for (fqdn, short) in &ready_names {
            script.push_str(&format!(
                "printf '%s %s %s #mc2-network\\n' '{gw}' '{fqdn}' '{short}' >> /tmp/hosts.mc2; "
            ));
        }
        script.push_str("cp /tmp/hosts.mc2 /etc/hosts");
        let _ = runtime.guest_shell(&desired.runtime_id, &script).await?;
        self.hosts_key
            .insert(desired.instance_id.clone(), inject_key);
        debug!(
            runtime_id = %desired.runtime_id,
            gw = %gw,
            names = ready_names.len(),
            "network DNS names injected into /etc/hosts"
        );
        Ok(())
    }
}

/// Shared splice task: bind once, forward each connection round-robin across
/// the current Ready backends for `port`. Reads the registry per connection so
/// replica changes take effect without restarting the listener. With no backend
/// the connection is accepted and closed. A transient accept error is retried
/// with backoff; after `MAX_ACCEPT_ERRORS` in a row the task ends so the next
/// reconcile re-binds a fresh listener.
///
/// `cancel` is the process shutdown token: once it fires the listener stops
/// accepting and the task returns (D2). Connections already accepted are not
/// touched — the process exits shortly after.
async fn splice_loop(
    listener: TcpListener,
    port: u16,
    backends: BackendRegistry,
    counter: Arc<AtomicUsize>,
    cancel: CancellationToken,
) {
    let mut consecutive_errors = 0u32;
    loop {
        let accepted = tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                debug!(port, "network splice stopping (shutdown)");
                return;
            }
            accepted = listener.accept() => accepted,
        };
        match accepted {
            Ok((inbound, _)) => {
                consecutive_errors = 0;
                let backends = backends.clone();
                let counter = counter.clone();
                tokio::spawn(async move {
                    let dest = {
                        let b = backends.lock().await;
                        match b.get(&port) {
                            Some(v) if !v.is_empty() => {
                                let idx = counter.fetch_add(1, Ordering::Relaxed) % v.len();
                                v[idx]
                            }
                            _ => return,
                        }
                    };
                    if let Ok(mut outbound) = TcpStream::connect(dest).await {
                        let mut inbound = inbound;
                        let _ = copy_bidirectional(&mut inbound, &mut outbound).await;
                    }
                });
            }
            Err(e) => {
                consecutive_errors += 1;
                warn!(
                    port,
                    error = %e,
                    attempt = consecutive_errors,
                    "network splice accept failed"
                );
                if consecutive_errors >= MAX_ACCEPT_ERRORS {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50u64 << consecutive_errors.min(5))).await;
            }
        }
    }
}

async fn reserve_ephemeral() -> std::io::Result<u16> {
    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0))).await?;
    let p = listener.local_addr()?.port();
    drop(listener);
    Ok(p)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mc2_runtime::{DesiredNetwork, NetworkAllowDesired, NetworkExposeDesired};
    use std::collections::BTreeMap;
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn allow(to: &str, port: u16) -> NetworkAllowDesired {
        NetworkAllowDesired {
            to_service: to.into(),
            port,
            protocol: "tcp".into(),
            fqdn: format!("{to}.shop.svc.mc2"),
            short_name: to.into(),
        }
    }

    fn desired(
        instance_id: &str,
        service: &str,
        guest_port: u16,
        allow_edge: Option<(&str, u16)>,
    ) -> DesiredSandbox {
        use mc2_api::ServiceSpec;
        let allows = allow_edge.map(|(to, p)| allow(to, p)).into_iter().collect();
        DesiredSandbox {
            instance_id: instance_id.into(),
            stack: "shop".into(),
            service: service.into(),
            ordinal: 0,
            runtime_id: format!("{service}-{instance_id}"),
            spec: ServiceSpec {
                image: "x".into(),
                scale: 1,
                cpus: 1,
                mem_limit_mib: 512,
                ports: vec![],
                network: Default::default(),
                env: Default::default(),
                secrets: vec![],
                volumes: vec![],
                restart: "on-failure".into(),
                healthcheck: None,
                labels: Default::default(),
                command: None,
                node_name: None,
                node_selector: Default::default(),
                ssh: None,
                storage_opt: None,
                expose: vec![mc2_api::ExposeSpec {
                    port: guest_port,
                    protocol: "tcp".into(),
                    name: None,
                }],
                networks: vec![],
                depends_on: BTreeMap::new(),
            },
            secrets: vec![],
            ssh: Default::default(),
            network: DesiredNetwork {
                exposes: vec![NetworkExposeDesired {
                    guest_port,
                    protocol: "tcp".into(),
                }],
                allows,
            },
        }
    }

    fn report(instance_id: &str, phase: &str) -> InstanceReport {
        InstanceReport {
            instance_id: instance_id.into(),
            phase: phase.into(),
            message: String::new(),
            runtime_id: String::new(),
            ssh: None,
            network: None,
        }
    }

    /// Shell-only fake runtime: records every `guest_shell` script and answers
    /// the DNS-gateway probe with a fixed address.
    struct ShellRuntime {
        scripts: tokio::sync::Mutex<Vec<String>>,
    }

    #[async_trait::async_trait]
    impl NodeRuntime for ShellRuntime {
        async fn ensure_running(
            &self,
            _d: &DesiredSandbox,
        ) -> anyhow::Result<mc2_runtime::EnsureRunning> {
            anyhow::bail!("unused")
        }
        async fn ensure_removed(&self, _id: &str) -> anyhow::Result<()> {
            anyhow::bail!("unused")
        }
        async fn status(&self, _id: &str) -> anyhow::Result<mc2_runtime::SandboxStatus> {
            anyhow::bail!("unused")
        }
        async fn list_owned(&self, _install_id: &str) -> anyhow::Result<Vec<String>> {
            anyhow::bail!("unused")
        }
        async fn exec_command(&self, _id: &str, _argv: &[String]) -> anyhow::Result<i32> {
            anyhow::bail!("unused")
        }
        async fn exec_with_output(
            &self,
            _id: &str,
            _argv: &[String],
            _stdin: &[u8],
        ) -> anyhow::Result<mc2_runtime::ExecResult> {
            anyhow::bail!("unused")
        }
        async fn guest_shell(&self, _id: &str, script: &str) -> anyhow::Result<String> {
            self.scripts.lock().await.push(script.to_string());
            Ok("10.0.2.2\n".into())
        }
        async fn read_logs(
            &self,
            _id: &str,
            _tail: Option<usize>,
        ) -> anyhow::Result<Vec<mc2_runtime::LogLine>> {
            anyhow::bail!("unused")
        }
        async fn log_stream(
            &self,
            _id: &str,
            _from: Option<String>,
        ) -> anyhow::Result<futures::stream::BoxStream<'static, anyhow::Result<mc2_runtime::LogLine>>>
        {
            anyhow::bail!("unused")
        }
        async fn ssh_server(
            &self,
            _id: &str,
            _user: &str,
            _keys: &[String],
            _sftp: bool,
        ) -> anyhow::Result<Arc<dyn mc2_runtime::SshServer>> {
            anyhow::bail!("unused")
        }
    }

    /// F5: `/etc/hosts` injection reaches the guest through
    /// `NodeRuntime::guest_shell` (the fake records the scripts), keeping the
    /// `#mc2-network` marker lines and the gateway probe unchanged.
    #[tokio::test]
    async fn hosts_injection_uses_the_runtime_guest_shell() {
        let mut table = NetworkTable::new();
        let d = desired("i-web-0", "web", 8080, Some(("db", 5432)));
        let observed = NetworkObserved {
            exposes: vec![],
            edges: vec![NetworkEdgeStatus {
                to_service: "db".into(),
                port: 5432,
                phase: "Ready".into(),
                message: String::new(),
            }],
            message: String::new(),
        };
        let fake = Arc::new(ShellRuntime {
            scripts: tokio::sync::Mutex::new(Vec::new()),
        });
        let runtime: Arc<dyn NodeRuntime> = fake.clone();

        table
            .inject_hosts(&d, &observed, &runtime)
            .await
            .expect("inject");

        let scripts = fake.scripts.lock().await;
        assert_eq!(scripts.len(), 2, "{scripts:?}");
        // 1: read the DNS gateway from the guest's resolv.conf.
        assert!(scripts[0].contains("nameserver"), "{}", scripts[0]);
        // 2: rebuild the marker lines and install the file.
        assert!(
            scripts[1].contains("grep -v ' #mc2-network$' /etc/hosts"),
            "{}",
            scripts[1]
        );
        assert!(
            scripts[1]
                .contains("printf '%s %s %s #mc2-network\\n' '10.0.2.2' 'db.shop.svc.mc2' 'db'"),
            "{}",
            scripts[1]
        );
        assert!(
            scripts[1].contains("cp /tmp/hosts.mc2 /etc/hosts"),
            "{}",
            scripts[1]
        );
        drop(scripts);

        // Same gateway + names: only the probe re-runs; the rewrite is skipped.
        table
            .inject_hosts(&d, &observed, &runtime)
            .await
            .expect("inject again");
        let scripts = fake.scripts.lock().await;
        assert_eq!(scripts.len(), 3, "{scripts:?}");
        assert!(!scripts[2].contains("#mc2-network"), "{}", scripts[2]);
    }

    /// Echo backend that prefixes its own listen port, so round-robin is observable.
    async fn echo_backend() -> u16 {
        let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            loop {
                let Ok((mut s, _)) = listener.accept().await else {
                    break;
                };
                let port = port.to_string();
                let data = port.as_bytes().to_vec();
                tokio::spawn(async move {
                    let _ = s.write_all(&data).await;
                    let _ = s.shutdown().await;
                });
            }
        });
        port
    }

    /// Connect and read; returns the bytes received (empty = accepted then closed).
    async fn read_from(port: u16) -> Vec<u8> {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        let mut buf = [0u8; 8];
        match tokio::time::timeout(Duration::from_secs(2), stream.read(&mut buf)).await {
            Ok(Ok(n)) => buf[..n].to_vec(),
            _ => Vec::new(),
        }
    }

    #[tokio::test]
    async fn ensure_listeners_binds_without_backend_and_connection_is_closed() {
        let mut table = NetworkTable::new();
        let guest_port = reserve_ephemeral().await.unwrap();
        let d = desired("i-db-0", "db", guest_port, Some(("db", guest_port)));

        // Bound before any backend exists (phase Pending).
        table.ensure_listeners(std::slice::from_ref(&d)).await;
        assert!(
            table.splices.contains_key(&guest_port),
            "listener held with no Running backend"
        );
        assert!(table.failed_splices.is_empty());
        assert!(table.listener_gaps(&d.network.allows).is_empty());

        // A connection is accepted and closed (no backend to forward to).
        assert_eq!(read_from(guest_port).await.len(), 0);

        // Still accepted-and-closed after a non-Running report.
        table
            .update_backends(std::slice::from_ref(&d), &[report("i-db-0", "Creating")])
            .await;
        assert_eq!(read_from(guest_port).await.len(), 0);
    }

    /// D2: cancelling the process token stops a held splice listener from
    /// accepting (real loopback socket).
    #[tokio::test]
    async fn cancel_stops_a_held_splice_listener() {
        let cancel = CancellationToken::new();
        let mut table = NetworkTable::new();
        table.set_cancel(cancel.clone());
        let guest_port = reserve_ephemeral().await.unwrap();
        let d = desired("i-db-0", "db", guest_port, None);
        table.ensure_listeners(std::slice::from_ref(&d)).await;
        assert!(table.splices.contains_key(&guest_port));

        // Accepted-and-closed while held (no Running backend).
        assert!(read_from(guest_port).await.is_empty());

        cancel.cancel();
        // The listener is dropped; connections are refused.
        let mut refused = false;
        for _ in 0..100 {
            if TcpStream::connect(("127.0.0.1", guest_port)).await.is_err() {
                refused = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(refused, "the splice must stop accepting after cancel");
    }

    #[tokio::test]
    async fn shared_splice_round_robins_across_ready_replicas() {
        let mut table = NetworkTable::new();
        let guest_port = reserve_ephemeral().await.unwrap();

        // Two Ready "db" replicas on distinct host publish ports, same guest port.
        let b1 = echo_backend().await;
        let b2 = echo_backend().await;
        let d1 = desired("i-db-0", "db", guest_port, None);
        let d2 = desired("i-db-1", "db", guest_port, None);

        // Seed the publish index + exposes as prepare_exposes would.
        table.prepare_exposes(&mut d1.clone()).await.unwrap();
        table.prepare_exposes(&mut d2.clone()).await.unwrap();
        // Override host ports to our echo backends so we can observe routing.
        {
            let mut idx = table.publish_index.lock().await;
            idx.insert("i-db-0".into(), HashMap::from([(guest_port, b1)]));
            idx.insert("i-db-1".into(), HashMap::from([(guest_port, b2)]));
        }
        table.exposes.remove("i-db-0");
        table.exposes.remove("i-db-1");

        table.ensure_listeners(&[d1.clone(), d2.clone()]).await;
        table
            .update_backends(
                &[d1, d2],
                &[report("i-db-0", "Running"), report("i-db-1", "Running")],
            )
            .await;
        assert!(table.failed_splices.is_empty());

        // Two connections → each lands on a different backend.
        let mut seen = std::collections::HashSet::new();
        for _ in 0..2 {
            let buf = read_from(guest_port).await;
            let port: u16 = std::str::from_utf8(&buf).unwrap().trim().parse().unwrap();
            seen.insert(port);
        }
        assert_eq!(
            seen.len(),
            2,
            "expected both replicas to receive traffic: {seen:?}"
        );
        assert!(seen.contains(&b1));
        assert!(seen.contains(&b2));

        table.drop_instance("i-db-0").await;
    }

    #[tokio::test]
    async fn stale_listener_aborted_when_port_no_longer_exposed() {
        let mut table = NetworkTable::new();
        let guest_port = reserve_ephemeral().await.unwrap();
        let d = desired("i-db-0", "db", guest_port, None);

        table.ensure_listeners(&[d]).await;
        assert!(table.splices.contains_key(&guest_port));

        // Nothing exposes the port any more → listener aborted, port released.
        table.ensure_listeners(&[]).await;
        assert!(table.splices.is_empty());
        let mut released = false;
        for _ in 0..50 {
            if std::net::TcpListener::bind(("127.0.0.1", guest_port)).is_ok() {
                released = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(
            released,
            "the held port must be released once no instance exposes it"
        );
    }

    #[tokio::test]
    async fn finished_listener_is_rebound_on_next_pass() {
        let mut table = NetworkTable::new();
        let guest_port = reserve_ephemeral().await.unwrap();
        let d = desired("i-db-0", "db", guest_port, None);

        table.ensure_listeners(std::slice::from_ref(&d)).await;
        table.splices.get(&guest_port).unwrap().abort();
        for _ in 0..50 {
            if table
                .splices
                .get(&guest_port)
                .is_some_and(|h| h.is_finished())
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(table.splices.get(&guest_port).unwrap().is_finished());

        // Next pass re-binds a fresh listener rather than leaving the port dead.
        table.ensure_listeners(&[d]).await;
        assert!(!table.splices.get(&guest_port).unwrap().is_finished());
        assert_eq!(read_from(guest_port).await.len(), 0);
    }

    #[tokio::test]
    async fn bind_failure_is_recorded_and_reported_as_a_listener_gap() {
        let mut table = NetworkTable::new();
        let guest_port = reserve_ephemeral().await.unwrap();
        // A foreign process holds the loopback port.
        let squatter = std::net::TcpListener::bind(("127.0.0.1", guest_port)).unwrap();

        let d = desired("i-app-0", "app", guest_port, Some(("db", guest_port)));
        table.ensure_listeners(std::slice::from_ref(&d)).await;
        assert!(
            table.failed_splices.contains_key(&guest_port),
            "bind failure recorded"
        );
        let gaps = table.listener_gaps(&d.network.allows);
        assert_eq!(gaps.len(), 1);
        assert_eq!(gaps[0].0, guest_port);
        assert!(gaps[0].1.contains("bind"), "{}", gaps[0].1);

        // Once the port is free the next pass binds it and clears the failure.
        drop(squatter);
        table.ensure_listeners(std::slice::from_ref(&d)).await;
        assert!(table.failed_splices.is_empty());
        assert!(table.listener_gaps(&d.network.allows).is_empty());
    }

    #[tokio::test]
    async fn listener_gap_reported_for_a_port_no_instance_exposes() {
        let table = NetworkTable::new();
        let d = desired("i-app-0", "app", 8080, Some(("db", 5432)));
        let gaps = table.listener_gaps(&d.network.allows);
        assert_eq!(gaps.len(), 1);
        assert_eq!(gaps[0].0, 5432);
        assert!(gaps[0].1.contains("no service exposing"), "{}", gaps[0].1);
    }
}
