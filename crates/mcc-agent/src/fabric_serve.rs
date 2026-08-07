//! Same-node mediated service fabric dataplane (D13).
//!
//! - **expose**: track msb loopback publish host ports (allocated at create).
//! - **allow**: userspace L4 splice on `127.0.0.1:<service_port>` → backend publish;
//!   inject fabric DNS names into guest `/etc/hosts` → gateway IP.
//! - Never enables full Host/Private profiles (policy is create-time narrow rules).
//! - Splice/host inject reuse: only rebuild when edge config changes.

use mcc_api::agent::{FabricEdgeStatus, FabricExposeStatus, FabricObserved};
use mcc_runtime::DesiredSandbox;
use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::io::copy_bidirectional;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

#[derive(Debug, Clone)]
struct ExposeBinding {
    guest_port: u16,
    host_port: u16,
}

struct ActiveSplice {
    /// Stable key: `to_service:port → backend_instance:backend_host_port`
    config_key: String,
    status: FabricEdgeStatus,
    _handle: JoinHandle<()>,
}

/// Per-agent fabric state.
pub struct FabricTable {
    /// instance_id → expose bindings (after sandbox create).
    exposes: HashMap<String, Vec<ExposeBinding>>,
    /// instance_id → active client splices.
    splices: HashMap<String, Vec<ActiveSplice>>,
    /// Shared expose index: backend instance_id → guest_port → host_port
    publish_index: Arc<Mutex<HashMap<String, HashMap<u16, u16>>>>,
    /// instance_id → last successful hosts inject key (names+gw).
    hosts_key: HashMap<String, String>,
}

impl FabricTable {
    pub fn new() -> Self {
        Self {
            exposes: HashMap::new(),
            splices: HashMap::new(),
            publish_index: Arc::new(Mutex::new(HashMap::new())),
            hosts_key: HashMap::new(),
        }
    }

    /// Reserve host ports for fabric exposes and merge into desired ports list.
    /// Call before `ensure_running` so msb create includes publishes.
    pub async fn prepare_exposes(&mut self, desired: &mut DesiredSandbox) -> Result<(), String> {
        if desired.fabric.exposes.is_empty() {
            self.exposes.remove(&desired.instance_id);
            let mut idx = self.publish_index.lock().await;
            idx.remove(&desired.instance_id);
            return Ok(());
        }

        // Reuse stable host ports across reconciles so msb recreate is not forced.
        let existing = self.exposes.get(&desired.instance_id).cloned();

        let mut bindings = Vec::new();
        for ex in &desired.fabric.exposes {
            let host_port = if let Some(b) = existing
                .as_ref()
                .and_then(|v| v.iter().find(|b| b.guest_port == ex.guest_port))
            {
                b.host_port
            } else if let Some(p) = desired
                .spec
                .ports
                .iter()
                .find(|p| p.guest == ex.guest_port && p.protocol.eq_ignore_ascii_case("tcp"))
            {
                p.host
            } else {
                reserve_ephemeral().await.map_err(|e| {
                    format!(
                        "fabric expose {}: bind 127.0.0.1 failed: {e}",
                        ex.guest_port
                    )
                })?
            };

            if !desired
                .spec
                .ports
                .iter()
                .any(|p| p.guest == ex.guest_port && p.protocol.eq_ignore_ascii_case("tcp"))
            {
                desired.spec.ports.push(mcc_api::PortSpec {
                    host: host_port,
                    guest: ex.guest_port,
                    protocol: "tcp".into(),
                    bind: "127.0.0.1".into(),
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
        self.exposes
            .insert(desired.instance_id.clone(), bindings);
        Ok(())
    }

    /// After sandbox is Running: inject DNS hosts + ensure client splices.
    pub async fn reconcile_running(&mut self, desired: &DesiredSandbox) -> FabricObserved {
        let mut observed = FabricObserved {
            exposes: vec![],
            edges: vec![],
            message: String::new(),
        };

        if let Some(binds) = self.exposes.get(&desired.instance_id) {
            for b in binds {
                observed.exposes.push(FabricExposeStatus {
                    guest_port: u32::from(b.guest_port),
                    host_port: u32::from(b.host_port),
                    phase: "Ready".into(),
                    message: String::new(),
                });
            }
        } else if !desired.fabric.exposes.is_empty() {
            for ex in &desired.fabric.exposes {
                observed.exposes.push(FabricExposeStatus {
                    guest_port: u32::from(ex.guest_port),
                    host_port: 0,
                    phase: "Failed".into(),
                    message: "expose not prepared before create".into(),
                });
            }
        }

        let mut edge_results: Vec<FabricEdgeStatus> = Vec::new();
        let mut kept: Vec<ActiveSplice> = Vec::new();

        let prev = self.splices.remove(&desired.instance_id).unwrap_or_default();
        let mut prev_by_key: HashMap<String, ActiveSplice> =
            prev.into_iter().map(|s| (s.config_key.clone(), s)).collect();

        for allow in &desired.fabric.allows {
            match self.plan_edge(allow).await {
                Ok((config_key, status, need_start, backend_host_port)) => {
                    let _ = status;
                    if let Some(existing) = prev_by_key.remove(&config_key) {
                        // Reuse live splice.
                        edge_results.push(existing.status.clone());
                        kept.push(existing);
                        continue;
                    }
                    if need_start {
                        match self
                            .start_splice(desired, allow, &config_key, backend_host_port)
                            .await
                        {
                            Ok(splice) => {
                                edge_results.push(splice.status.clone());
                                kept.push(splice);
                            }
                            Err(st) => edge_results.push(st),
                        }
                    } else {
                        edge_results.push(status);
                    }
                }
                Err(st) => edge_results.push(st),
            }
        }

        // Abort splices no longer desired.
        for (_k, old) in prev_by_key {
            old._handle.abort();
        }

        if !kept.is_empty() {
            self.splices.insert(desired.instance_id.clone(), kept);
        }

        observed.edges = edge_results;

        if let Err(e) = self.inject_hosts(desired, &observed).await {
            warn!(
                instance = %desired.instance_id,
                error = %e,
                "fabric hosts inject failed"
            );
            if observed.message.is_empty() {
                observed.message = format!("hosts inject: {e}");
            }
        }

        observed
    }

    pub async fn reconcile_not_running(&mut self, desired: &DesiredSandbox) -> FabricObserved {
        if let Some(old) = self.splices.remove(&desired.instance_id) {
            for s in old {
                s._handle.abort();
            }
        }
        self.hosts_key.remove(&desired.instance_id);

        let mut observed = FabricObserved::default();
        for ex in &desired.fabric.exposes {
            let host = self
                .exposes
                .get(&desired.instance_id)
                .and_then(|v| v.iter().find(|b| b.guest_port == ex.guest_port))
                .map(|b| b.host_port)
                .unwrap_or(0);
            observed.exposes.push(FabricExposeStatus {
                guest_port: u32::from(ex.guest_port),
                host_port: u32::from(host),
                phase: "Pending".into(),
                message: "sandbox not running".into(),
            });
        }
        for a in &desired.fabric.allows {
            observed.edges.push(FabricEdgeStatus {
                to_service: a.to_service.clone(),
                port: u32::from(a.port),
                phase: "Pending".into(),
                message: "sandbox not running".into(),
            });
        }
        observed
    }

    /// Host publish ports for this instance's fabric exposes (if prepared).
    pub fn expose_host_ports(&self, instance_id: &str) -> Option<Vec<u16>> {
        self.exposes.get(instance_id).map(|v| {
            v.iter().map(|b| b.host_port).collect()
        })
    }

    /// Drop splices/hosts for one instance (before force-recreate).
    /// Clears expose bindings so the next prepare allocates fresh host ports.
    pub async fn drop_instance(&mut self, instance_id: &str) {
        if let Some(sp) = self.splices.remove(instance_id) {
            for s in sp {
                s._handle.abort();
            }
        }
        self.hosts_key.remove(instance_id);
        self.exposes.remove(instance_id);
        let mut idx = self.publish_index.lock().await;
        idx.remove(instance_id);
    }

    pub async fn close_missing(&mut self, keep: &HashSet<String>) {
        let drop_ids: Vec<String> = self
            .exposes
            .keys()
            .chain(self.splices.keys())
            .filter(|id| !keep.contains(*id))
            .cloned()
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        for id in drop_ids {
            if let Some(sp) = self.splices.remove(&id) {
                for s in sp {
                    s._handle.abort();
                }
            }
            self.exposes.remove(&id);
            self.hosts_key.remove(&id);
            let mut idx = self.publish_index.lock().await;
            idx.remove(&id);
        }
    }

    /// Returns (config_key, status_if_not_starting, need_start, backend_host_port).
    async fn plan_edge(
        &self,
        allow: &mcc_runtime::FabricAllowDesired,
    ) -> Result<(String, FabricEdgeStatus, bool, u16), FabricEdgeStatus> {
        if !allow.backend_local {
            return Err(FabricEdgeStatus {
                to_service: allow.to_service.clone(),
                port: u32::from(allow.port),
                phase: "Failed".into(),
                message: format!(
                    "fabric allow {}:{}: cross-node fabric not supported (backend on node {}, local instance on this node)",
                    allow.to_service,
                    allow.port,
                    if allow.backend_node_id.is_empty() {
                        "unknown"
                    } else {
                        allow.backend_node_id.as_str()
                    }
                ),
            });
        }
        if allow.backend_instance_id.is_empty() {
            return Err(FabricEdgeStatus {
                to_service: allow.to_service.clone(),
                port: u32::from(allow.port),
                phase: "Pending".into(),
                message: format!(
                    "fabric allow {}:{}: waiting for backend instance",
                    allow.to_service, allow.port
                ),
            });
        }

        let backend_host = {
            let idx = self.publish_index.lock().await;
            idx.get(&allow.backend_instance_id)
                .and_then(|m| m.get(&allow.port).copied())
        };
        let Some(backend_host_port) = backend_host else {
            return Err(FabricEdgeStatus {
                to_service: allow.to_service.clone(),
                port: u32::from(allow.port),
                phase: "Pending".into(),
                message: format!(
                    "fabric allow {}:{}: waiting for backend expose on {}",
                    allow.to_service, allow.port, allow.backend_instance_id
                ),
            });
        };

        let config_key = format!(
            "{}:{}→{}:{}",
            allow.to_service, allow.port, allow.backend_instance_id, backend_host_port
        );
        let status = FabricEdgeStatus {
            to_service: allow.to_service.clone(),
            port: u32::from(allow.port),
            phase: "Ready".into(),
            message: String::new(),
        };
        Ok((config_key, status, true, backend_host_port))
    }

    async fn start_splice(
        &self,
        desired: &DesiredSandbox,
        allow: &mcc_runtime::FabricAllowDesired,
        config_key: &str,
        backend_host_port: u16,
    ) -> Result<ActiveSplice, FabricEdgeStatus> {
        let listener = match TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], allow.port))).await
        {
            Ok(l) => l,
            Err(e) => {
                return Err(FabricEdgeStatus {
                    to_service: allow.to_service.clone(),
                    port: u32::from(allow.port),
                    phase: "Failed".into(),
                    message: format!(
                        "fabric splice {}:{}: bind 127.0.0.1:{} failed: {e}",
                        allow.to_service, allow.port, allow.port
                    ),
                });
            }
        };
        let listen_port = listener.local_addr().map(|a| a.port()).unwrap_or(allow.port);
        let backend = backend_host_port;
        let to_label = allow.to_service.clone();
        let handle = tokio::spawn(async move {
            loop {
                let Ok((inbound, _)) = listener.accept().await else {
                    break;
                };
                let to = to_label.clone();
                tokio::spawn(async move {
                    let dest = SocketAddr::from(([127, 0, 0, 1], backend));
                    match TcpStream::connect(dest).await {
                        Ok(mut outbound) => {
                            let mut inbound = inbound;
                            let _ = copy_bidirectional(&mut inbound, &mut outbound).await;
                        }
                        Err(e) => {
                            debug!(error = %e, %to, backend, "fabric splice connect backend failed");
                        }
                    }
                });
            }
        });

        info!(
            client = %desired.runtime_id,
            to = %allow.to_service,
            listen_port,
            backend_host_port,
            "fabric edge Ready"
        );

        Ok(ActiveSplice {
            config_key: config_key.to_string(),
            status: FabricEdgeStatus {
                to_service: allow.to_service.clone(),
                port: u32::from(allow.port),
                phase: "Ready".into(),
                message: String::new(),
            },
            _handle: handle,
        })
    }

    async fn inject_hosts(
        &mut self,
        desired: &DesiredSandbox,
        observed: &FabricObserved,
    ) -> anyhow::Result<()> {
        let ready_names: Vec<(String, String)> = desired
            .fabric
            .allows
            .iter()
            .filter(|a| {
                observed.edges.iter().any(|e| {
                    e.to_service == a.to_service && e.port == u32::from(a.port) && e.phase == "Ready"
                })
            })
            .map(|a| (a.fqdn.clone(), a.short_name.clone()))
            .collect();
        if ready_names.is_empty() {
            return Ok(());
        }

        let handle = microsandbox::Sandbox::get(&desired.runtime_id).await?;
        let sb = handle.connect().await?;
        let out = sb
            .shell("awk '/^nameserver /{print $2; exit}' /etc/resolv.conf")
            .await?;
        let gw = out.stdout()?.trim().to_string();
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

        // Rebuild fabric host lines under a marker for idempotency.
        let mut entries = String::new();
        for (fqdn, short) in &ready_names {
            entries.push_str(&format!("{gw} {fqdn} {short}\n"));
        }
        // Escape for single-quoted shell heredoc is awkward; use printf lines.
        let mut script = String::from(
            "set -e; grep -v ' #mcc-fabric$' /etc/hosts > /tmp/hosts.mcc 2>/dev/null || true; ",
        );
        for (fqdn, short) in &ready_names {
            script.push_str(&format!(
                "printf '%s %s %s #mcc-fabric\\n' '{gw}' '{fqdn}' '{short}' >> /tmp/hosts.mcc; "
            ));
        }
        script.push_str("cp /tmp/hosts.mcc /etc/hosts");
        let _ = sb.shell(script).await?;
        self.hosts_key
            .insert(desired.instance_id.clone(), inject_key);
        debug!(
            runtime_id = %desired.runtime_id,
            gw = %gw,
            names = ready_names.len(),
            "fabric DNS names injected into /etc/hosts"
        );
        Ok(())
    }
}

async fn reserve_ephemeral() -> std::io::Result<u16> {
    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0))).await?;
    let p = listener.local_addr()?.port();
    drop(listener);
    Ok(p)
}
