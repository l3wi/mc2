//! Same-node mediated service network dataplane (D13).
//!
//! - **expose**: track msb loopback publish host ports (allocated at create).
//! - **edges**: one shared user-space L4 splice per exposed `(service, port)`
//!   on `127.0.0.1:<port>`; each connection **round-robins** across the Ready
//!   backend replicas (read per connection, so no restart on phase churn).
//!   All consumers route through the injected DNS gateway → the shared splice.
//! - DNS names injected into guest `/etc/hosts` → gateway IP.
//! - Never enables full Host/Private profiles (policy is create-time narrow rules).
//!
//! Port collision note: splices share the host loopback, so exposed guest
//! ports must be unique (validated within a stack; a cross-stack collision
//! makes the late splice report `Failed`).

use mc2_runtime::{
    DesiredSandbox, InstanceReport, NetworkEdgeStatus, NetworkExposeStatus, NetworkObserved,
};
use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
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

/// Shared backend registry: (service, guest_port) → ready host sockets.
type BackendRegistry = Arc<Mutex<HashMap<(String, u16), Vec<SocketAddr>>>>;

/// Per-node network state.
pub struct NetworkTable {
    /// instance_id → expose bindings (after sandbox create).
    exposes: HashMap<String, Vec<ExposeBinding>>,
    /// Shared expose index: backend instance_id → guest_port → host_port
    publish_index: Arc<Mutex<HashMap<String, HashMap<u16, u16>>>>,
    /// instance_id → last successful hosts inject key (names+gw).
    hosts_key: HashMap<String, String>,
    /// (service, guest_port) → shared splice task (one per exposed port).
    splices: HashMap<(String, u16), JoinHandle<()>>,
    /// (service, guest_port) whose splice failed to bind (port collision).
    failed_splices: HashSet<(String, u16)>,
    /// Ready backend host sockets per (service, guest_port); read per connection.
    backends: BackendRegistry,
    /// Round-robin cursor across the table.
    rr_counter: Arc<AtomicUsize>,
}

impl NetworkTable {
    pub fn new() -> Self {
        Self {
            exposes: HashMap::new(),
            publish_index: Arc::new(Mutex::new(HashMap::new())),
            hosts_key: HashMap::new(),
            splices: HashMap::new(),
            failed_splices: HashSet::new(),
            backends: Arc::new(Mutex::new(HashMap::new())),
            rr_counter: Arc::new(AtomicUsize::new(0)),
        }
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

    /// Rebuild the shared backend registry and reconcile splice tasks.
    ///
    /// Called once per reconcile after every instance's exposes are prepared
    /// and observed. One splice per exposed (service, port); stale splices are
    /// aborted, new ones started, and the backends map is swapped so running
    /// splices re-target without restarting.
    pub async fn reconcile_splices(
        &mut self,
        desired: &[DesiredSandbox],
        reports: &[InstanceReport],
    ) {
        let phases: HashMap<&str, &str> = reports
            .iter()
            .map(|r| (r.instance_id.as_str(), r.phase.as_str()))
            .collect();

        let mut new_backends: HashMap<(String, u16), Vec<SocketAddr>> = HashMap::new();
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
                            .entry((d.service.clone(), ex.guest_port))
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

        // Abort splices whose (service, port) is no longer served.
        let keep: HashSet<(String, u16)> = new_backends.keys().cloned().collect();
        let stale: Vec<(String, u16)> = self
            .splices
            .keys()
            .filter(|k| !keep.contains(k))
            .cloned()
            .collect();
        for k in stale {
            if let Some(h) = self.splices.remove(&k) {
                h.abort();
            }
        }

        // Start missing splices.
        for key in new_backends.keys() {
            if self.splices.contains_key(key) {
                continue;
            }
            let (service, port) = key.clone();
            match TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], port))).await {
                Ok(listener) => {
                    let backends = self.backends.clone();
                    let counter = self.rr_counter.clone();
                    let handle =
                        tokio::spawn(splice_loop(listener, key.clone(), backends, counter));
                    self.splices.insert(key.clone(), handle);
                    self.failed_splices.remove(key);
                    info!(service, port, "network shared splice started");
                }
                Err(e) => {
                    warn!(service, port, error = %e, "network splice bind failed (port collision?)");
                    self.failed_splices.insert(key.clone());
                }
            }
        }

        {
            let mut b = self.backends.lock().await;
            *b = new_backends;
        }
    }

    /// After sandbox is Running: inject DNS hosts + report network status.
    pub async fn reconcile_running(&mut self, desired: &DesiredSandbox) -> NetworkObserved {
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
                    phase: "Ready".into(),
                    message: String::new(),
                });
            }
        } else if !desired.network.exposes.is_empty() {
            for ex in &desired.network.exposes {
                observed.exposes.push(NetworkExposeStatus {
                    guest_port: ex.guest_port,
                    host_port: 0,
                    phase: "Failed".into(),
                    message: "expose not prepared before create".into(),
                });
            }
        }

        let backends = self.backends.lock().await;
        for a in &desired.network.allows {
            let key = (a.to_service.clone(), a.port);
            let (phase, message) = if self.failed_splices.contains(&key) {
                (
                    "Failed",
                    "network splice could not bind port (collision?)".to_string(),
                )
            } else {
                let has = self.splices.contains_key(&key);
                let up = backends.get(&key).map(|v| !v.is_empty()).unwrap_or(false);
                if has && up {
                    ("Ready", String::new())
                } else {
                    ("Pending", "waiting for backend".to_string())
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

        if let Err(e) = self.inject_hosts(desired, &observed).await {
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
                phase: "Pending".into(),
                message: "sandbox not running".into(),
            });
        }
        for a in &desired.network.allows {
            observed.edges.push(NetworkEdgeStatus {
                to_service: a.to_service.clone(),
                port: a.port,
                phase: "Pending".into(),
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

    /// Clear per-instance expose state before force-recreate. Shared splices
    /// are untouched; `reconcile_splices` re-targets on the next cycle.
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
        let _ = sb.shell(script).await?;
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
/// the current Ready backends for `key`. Reads the registry per connection so
/// replica changes take effect without restarting the listener.
async fn splice_loop(
    listener: TcpListener,
    key: (String, u16),
    backends: BackendRegistry,
    counter: Arc<AtomicUsize>,
) {
    loop {
        let Ok((inbound, _)) = listener.accept().await else {
            break;
        };
        let key = key.clone();
        let backends = backends.clone();
        let counter = counter.clone();
        tokio::spawn(async move {
            let dest = {
                let b = backends.lock().await;
                match b.get(&key) {
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

    fn desired(
        instance_id: &str,
        service: &str,
        guest_port: u16,
        allow: Option<(&str, u16)>,
    ) -> DesiredSandbox {
        use mc2_api::ServiceSpec;
        let mut allows = Vec::new();
        if let Some((to, port)) = allow {
            allows.push(NetworkAllowDesired {
                to_service: to.into(),
                port,
                protocol: "tcp".into(),
                fqdn: format!("{to}.shop.svc.mc2"),
                short_name: to.into(),
                backend_instance_id: String::new(),
                backend_node_id: String::new(),
                backend_local: true,
                backend_ordinal: 0,
            });
        }
        DesiredSandbox {
            instance_id: instance_id.into(),
            stack: "shop".into(),
            service: service.into(),
            ordinal: 0,
            runtime_id: format!("{service}-{instance_id}"),
            spec: ServiceSpec {
                image: "x".into(),
                scale: 1,
                cpus: 1.0,
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

        let reports = vec![
            InstanceReport {
                instance_id: "i-db-0".into(),
                phase: "Running".into(),
                message: String::new(),
                runtime_id: String::new(),
                ssh: None,
                network: None,
            },
            InstanceReport {
                instance_id: "i-db-1".into(),
                phase: "Running".into(),
                message: String::new(),
                runtime_id: String::new(),
                ssh: None,
                network: None,
            },
        ];
        table.reconcile_splices(&[d1, d2], &reports).await;
        assert!(table.failed_splices.is_empty());

        // Two connections → each lands on a different backend.
        let mut seen = std::collections::HashSet::new();
        for _ in 0..2 {
            let mut stream = TcpStream::connect(("127.0.0.1", guest_port)).await.unwrap();
            let mut buf = [0u8; 8];
            let n = tokio::time::timeout(Duration::from_secs(2), stream.read(&mut buf))
                .await
                .unwrap()
                .unwrap();
            let port: u16 = std::str::from_utf8(&buf[..n])
                .unwrap()
                .trim()
                .parse()
                .unwrap();
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
    async fn no_splice_when_no_ready_backend() {
        let mut table = NetworkTable::new();
        let d = desired("i-db-0", "db", 5432, None);
        table.prepare_exposes(&mut d.clone()).await.unwrap();
        let reports = vec![InstanceReport {
            instance_id: "i-db-0".into(),
            phase: "Creating".into(),
            message: String::new(),
            runtime_id: String::new(),
            ssh: None,
            network: None,
        }];
        table.reconcile_splices(&[d], &reports).await;
        assert!(table.splices.is_empty());
        assert!(table.failed_splices.is_empty());
    }
}
