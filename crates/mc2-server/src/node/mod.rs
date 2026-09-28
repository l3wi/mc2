//! Local node reconcile loop: drive the embedded microsandbox runtime from
//! the store's desired set, and write observed phases back to the store.
//!
//! Replaces the former `mc2-agent` gRPC client/server pair with direct calls.
//!
//! The embedded node is a **singleton**: it is registered (with a stable id)
//! before the REST listener accepts, stays Ready for as long as the process
//! runs, and owns its sandboxes through the `mc2.install` label rather than
//! through in-memory state, so a restart adopts or cleans up exactly what this
//! install left behind (B2/B8).

mod disk;
mod health;
mod lifecycle;
mod report;
mod state;

use crate::desired::{build_desired_set, DesiredSet};
use crate::ingress_files::{warn_ingress_dir_unset, SelfIngressRoute};
use crate::liveness::Liveness;
use anyhow::{Context, Result};
use mc2_runtime::{InstanceReport, NodeRuntime};
use mc2_store::{InstancePhase, NodeHeartbeat, SecretsKey, Store};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use self::report::persist_instance_report;
use self::state::{InstanceLiveness, NodeRuntimeState, PassErrors};

/// Configuration for the local node loop.
#[derive(Debug, Clone)]
pub struct NodeConfig {
    /// Display name (hostname / `--node-name`). Changing it must not change the
    /// node's identity — that lives in the store (B8).
    pub name: String,
    /// This install's random id: labels every sandbox and scopes ownership
    /// recovery to sandboxes this install created (B2).
    pub install_id: String,
    pub labels_json: String,
    pub cpus: u32,
    pub memory_mib: u64,
    pub reconcile_interval: Duration,
    pub ingress_config_dir: Option<PathBuf>,
    /// Public hostname to publish the control plane through the ingress catalog.
    pub public_hostname: Option<String>,
    /// Traefik cert resolver for the control-plane TLS route.
    pub public_tls_cert_resolver: String,
    /// REST listener port (backend for the control-plane self-route).
    pub rest_port: u16,
}

/// Register/refresh the local node row; returns its stable node_id.
///
/// Called before the REST listener serves, so an apply can never see a node
/// list without the embedded node in it.
pub async fn ensure_local_node(store: Arc<dyn Store>, cfg: &NodeConfig) -> Result<String> {
    let rec = store
        .upsert_local_node(mc2_store::NodeJoin {
            name: cfg.name.clone(),
            labels_json: cfg.labels_json.clone(),
            arch: std::env::consts::ARCH.to_string(),
            cpus: cfg.cpus,
            memory_mib: cfg.memory_mib,
        })
        .await
        .context("upsert local node")?;
    Ok(rec.id)
}

/// The reconcile loop. Runs until `cancel` fires.
pub async fn run(
    store: Arc<dyn Store>,
    secrets_key: Arc<SecretsKey>,
    runtime: Arc<dyn NodeRuntime>,
    node_id: String,
    cfg: NodeConfig,
    cancel: CancellationToken,
    liveness: Arc<Liveness>,
) -> Result<()> {
    let self_route = cfg.public_hostname.as_ref().map(|host| SelfIngressRoute {
        host: host.clone(),
        port: cfg.rest_port,
        tls_cert_resolver: cfg.public_tls_cert_resolver.clone(),
    });

    info!(
        node = %cfg.name,
        node_id = %node_id,
        install_id = %cfg.install_id,
        runtime = "microsandbox-sdk",
        ssh = "sdk",
        ingress_dir = ?cfg.ingress_config_dir,
        api = mc2_api::API_VERSION,
        "local node reconcile loop starting"
    );

    let mut node_id = node_id;
    let interval = cfg.reconcile_interval.max(Duration::from_secs(1));
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    let mut rt_state = NodeRuntimeState::new(
        cfg.ingress_config_dir
            .clone()
            .map(|d| crate::ingress_files::IngressFileWriter::new(d, cfg.name.clone())),
        self_route,
    );
    // Propagate the process cancellation token to the listeners this loop owns
    // so shutdown stops accepting without waiting for the next pass (D2).
    rt_state.network_table.set_cancel(cancel.clone());
    rt_state.ssh_table.set_cancel(cancel.clone());

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                info!("local node shutting down");
                break;
            }
            _ = ticker.tick() => {
                // The node is Ready for as long as this process runs; the
                // heartbeat keeps the row's capacity and timestamp current.
                let heartbeat = NodeHeartbeat {
                    cpus: cfg.cpus,
                    memory_mib: cfg.memory_mib,
                    status: "Ready".into(),
                };
                if let Err(e) = store.touch_node(&node_id, heartbeat).await {
                    warn!(node_id = %node_id, error = %e, "node heartbeat failed; re-registering");
                    match ensure_local_node(store.clone(), &cfg).await {
                        Ok(id) => node_id = id,
                        Err(e) => {
                            mc2_metrics::record_reconcile(false);
                            warn!(error = %e, "re-registering the local node failed");
                            continue;
                        }
                    }
                }

                match reconcile(
                    store.clone(),
                    secrets_key.as_ref(),
                    &node_id,
                    &cfg,
                    &runtime,
                    &mut rt_state,
                )
                .await
                {
                    Ok(()) => {
                        mc2_metrics::record_reconcile(true);
                        // Readiness (D1): only a fully successful pass refreshes
                        // it, so a wedged loop lets `/health` degrade.
                        liveness.record_pass();
                    }
                    Err(e) => {
                        mc2_metrics::record_reconcile(false);
                        warn!(error = %e, error_full = format!("{e:#}"), "reconcile failed");
                    }
                }
            }
        }
    }

    // Stop the SSH/splice listeners this loop holds; the runtime sandboxes stay
    // running (they are detached; adoption on restart handles them).
    rt_state.network_table.shutdown();
    rt_state.ssh_table.close_all().await;
    Ok(())
}

/// One reconcile pass: recover ownership, build the desired set, GC, reconcile
/// every instance, then publish the observed state.
async fn reconcile(
    store: Arc<dyn Store>,
    secrets_key: &SecretsKey,
    node_id: &str,
    cfg: &NodeConfig,
    runtime: &Arc<dyn NodeRuntime>,
    rt: &mut NodeRuntimeState,
) -> Result<()> {
    // B2: ownership comes from the install label, never from an empty in-memory
    // set. A pass that cannot recover it does nothing at all.
    ensure_owned_seeded(runtime, &cfg.install_id, rt).await?;

    let DesiredSet {
        sandboxes: mut desired,
        failures,
        ingress_routes,
    } = build_desired_set(store.clone(), secrets_key, node_id).await?;

    // GC keep-set: desired instances plus the ones whose desired state could not
    // be resolved — a resolution failure must never delete a running sandbox
    // (B4).
    let mut keep_ids: HashSet<String> = desired.iter().map(|d| d.runtime_id.clone()).collect();
    keep_ids.extend(failures.iter().map(|f| f.runtime_id.clone()));

    gc_orphans(runtime, rt, &keep_ids).await;

    // B13/B2: per-sandbox runtime state is kept only for instances we still
    // desire or still own (a failed removal is retried until it succeeds);
    // everything else is pruned so it cannot grow without bound or leak stale
    // restart backoff into a later instance reusing the name.
    let stale_state: Vec<String> = rt
        .rt_state
        .keys()
        .filter(|rid| !keep_ids.contains(*rid) && !rt.owned.contains(*rid))
        .cloned()
        .collect();
    for rid in stale_state {
        rt.rt_state.remove(&rid);
    }

    let mut errors = PassErrors::default();
    let mut reports: Vec<InstanceReport> = Vec::new();
    let now = Instant::now();

    // Providers first so expose publish index is populated before client edges.
    desired.sort_by(|a, b| {
        let ae = a.network.exposes.is_empty();
        let be = b.network.exposes.is_empty();
        ae.cmp(&be) // false (has expose) sorts before true
            .then_with(|| a.service.cmp(&b.service))
    });

    // Seed per-instance liveness from the store; the same reduction is applied
    // to the observations this cycle makes below, so in-cycle dependencies start
    // fast and replica order never decides a service's liveness (B11).
    let mut live = live_from_store(&store).await?;

    // B4: an instance whose desired state could not be resolved is reported
    // Failed with the message and is not started; dependents see it as not
    // running. Its sandbox (if any) stays owned — see `keep_ids` above.
    for f in &failures {
        warn!(
            instance_id = %f.instance_id,
            error = %f.message,
            "desired state unresolved; reporting Failed"
        );
        reports.push(f.failure_report());
        live.observe(
            &f.instance_id,
            &f.stack,
            &f.service,
            InstancePhase::Failed.as_str(),
            false,
        );
    }

    // Hold one splice listener per exposed port in the desired set — before any
    // backend exists and regardless of phase — so the exclusive port claim is
    // real for the stack's whole lifetime (accept-then-close instead of
    // connection-refused, and no foreign process can take the port).
    rt.network_table.ensure_listeners(desired.as_slice()).await;

    for d in &mut desired {
        match rt
            .reconcile_instance(&store, runtime, d, now, &mut live, &mut errors)
            .await
        {
            Ok(report) => reports.push(report),
            Err(e) => {
                warn!(
                    instance_id = %d.instance_id,
                    error = %e,
                    "instance reconcile aborted; retrying next pass"
                );
                errors.record(e);
            }
        }
    }

    let keep_instances: HashSet<String> = desired.iter().map(|d| d.instance_id.clone()).collect();
    rt.ssh_table.close_missing(&keep_instances).await;
    // Rebuild the shared backend registry from the current desired set + observed
    // phases (Running backends only); listeners stay held across phase churn.
    rt.network_table
        .update_backends(desired.as_slice(), &reports)
        .await;
    rt.network_table.close_missing(&keep_instances).await;

    publish_observed(
        &store,
        rt,
        &desired,
        &ingress_routes,
        runtime,
        &mut reports,
        &mut errors,
    )
    .await;

    errors.into_result()
}

/// B2: seed `rt.owned` from the sandboxes carrying this install's label.
///
/// Errors abort the pass: adopting or removing sandboxes on an incomplete view
/// of what we own is exactly the failure this exists to prevent.
async fn ensure_owned_seeded(
    runtime: &Arc<dyn NodeRuntime>,
    install_id: &str,
    rt: &mut NodeRuntimeState,
) -> Result<()> {
    if rt.owned_seeded {
        return Ok(());
    }
    let owned = runtime
        .list_owned(install_id)
        .await
        .context("list sandboxes owned by this install")?;
    info!(
        install_id,
        count = owned.len(),
        "recovered sandbox ownership from the install label"
    );
    rt.owned.extend(owned);
    rt.owned_seeded = true;
    Ok(())
}

/// B2: remove owned sandboxes that are no longer desired (scale-down, delete,
/// orphans from an earlier run).
///
/// Ownership is dropped only after `ensure_removed` succeeds — the runtime
/// treats an already-gone sandbox as success — so a transient failure keeps the
/// id owned and retried on every later pass.
async fn gc_orphans(
    runtime: &Arc<dyn NodeRuntime>,
    rt: &mut NodeRuntimeState,
    keep_ids: &HashSet<String>,
) {
    let stale: Vec<String> = rt.owned.difference(keep_ids).cloned().collect();
    for rid in stale {
        match runtime.ensure_removed(&rid).await {
            Ok(()) => {
                info!(runtime_id = %rid, "removed sandbox that is no longer desired");
                rt.owned.remove(&rid);
                rt.rt_state.remove(&rid);
            }
            Err(e) => {
                warn!(
                    runtime_id = %rid,
                    error = %e,
                    "ensure_removed failed; keeping ownership and retrying next pass"
                );
            }
        }
    }
}

/// Seed the `depends_on` liveness map from the stored observed phases.
///
/// Stored and current observations are reduced by the same [`InstanceLiveness::observe`]
/// (B11).
async fn live_from_store(store: &Arc<dyn Store>) -> Result<InstanceLiveness> {
    let mut live = InstanceLiveness::default();
    for inst in store
        .list_instances()
        .await
        .context("list instances for deps")?
    {
        live.observe(
            &inst.id,
            &inst.stack,
            &inst.service,
            &inst.phase,
            inst.healthy,
        );
    }
    Ok(live)
}

/// Ingress catalog, disk-limit annotations and the observed-state writes.
///
/// Split out of the pass body so the reshape of `reports` (disk conditions) and
/// the persistence of it (D4) stay in one place.
async fn publish_observed(
    store: &Arc<dyn Store>,
    rt: &mut NodeRuntimeState,
    desired: &[mc2_runtime::DesiredSandbox],
    ingress_routes: &[mc2_runtime::DesiredIngressRoute],
    runtime: &Arc<dyn NodeRuntime>,
    reports: &mut [InstanceReport],
    errors: &mut PassErrors,
) {
    // Ingress file catalog (same-node BYO Traefik).
    let mut phases: HashMap<String, String> = HashMap::new();
    for r in reports.iter() {
        phases.insert(r.instance_id.clone(), r.phase.clone());
    }
    if let Some(writer) = rt.ingress_writer.as_mut() {
        match writer
            .reconcile(ingress_routes, &phases, rt.self_route.as_ref())
            .await
        {
            Ok(st) if st.wrote => {
                info!(
                    ready = st.ready,
                    pending = st.pending,
                    "ingress catalog updated"
                );
            }
            Ok(_) => {}
            Err(e) => warn!(error = %e, "ingress catalog render failed"),
        }
    } else {
        warn_ingress_dir_unset(ingress_routes.len());
    }

    // Disk-limit conditions (A9): root-disk usage from msb metrics plus cached
    // volume-directory walks, appended to each report's message (short NOTES in
    // `mc2 ps`, full text + fix printed by `mc2 exec`/`ssh`/`logs`).
    {
        let root_disk_usage = runtime.root_disk_usage().await.unwrap_or_default();
        disk::annotate_reports(reports, desired, &root_disk_usage, &runtime.volume_root());
    }

    // D4: every observed-state write is checked; a failure is logged with the
    // instance and operation and makes the pass count as failed.
    for r in reports.iter() {
        persist_instance_report(store, &rt.rt_state, r, errors).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::lifecycle::waiting_deps;
    use crate::node::state::{InstanceLiveness, ServiceLive};
    use mc2_store::{
        ClusterCounts, ClusterMeta, InstanceNetworkRecord, InstanceRecord, InstanceSshRecord,
        MemoryStore, NodeJoin, NodeRecord, SecretBlob, SecretMeta, SshAuthorizedKey, StackPlan,
        StackRecord, StoreError,
    };
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn node_cfg(install_id: &str) -> NodeConfig {
        NodeConfig {
            name: "n1".into(),
            install_id: install_id.into(),
            labels_json: "{}".into(),
            cpus: 8,
            memory_mib: 8192,
            reconcile_interval: Duration::from_secs(10),
            ingress_config_dir: None,
            public_hostname: None,
            public_tls_cert_resolver: "le".into(),
            rest_port: 0,
        }
    }

    fn sandbox(
        depends_on: BTreeMap<String, mc2_api::DependsOnSpec>,
    ) -> mc2_runtime::DesiredSandbox {
        mc2_runtime::DesiredSandbox {
            instance_id: "i1".into(),
            stack: "demo".into(),
            service: "web".into(),
            ordinal: 0,
            runtime_id: "demo--web--0".into(),
            spec: mc2_api::ServiceSpec {
                image: "alpine".into(),
                scale: 1,
                cpus: 1,
                mem_limit_mib: 512,
                ports: vec![],
                network: Default::default(),
                env: BTreeMap::new(),
                secrets: vec![],
                volumes: vec![],
                restart: "no".into(),
                healthcheck: None,
                labels: BTreeMap::new(),
                command: None,
                node_name: None,
                node_selector: BTreeMap::new(),
                ssh: None,
                storage_opt: None,
                expose: vec![],
                networks: vec![],
                depends_on,
            },
            secrets: vec![],
            ssh: Default::default(),
            network: Default::default(),
        }
    }

    fn dep(name: &str, condition: &str) -> BTreeMap<String, mc2_api::DependsOnSpec> {
        BTreeMap::from([(
            name.to_string(),
            mc2_api::DependsOnSpec {
                condition: condition.into(),
            },
        )])
    }

    /// A liveness map holding one observation for `service`.
    fn live(stack: &str, service: &str, running: bool, healthy: bool) -> InstanceLiveness {
        let mut l = InstanceLiveness::default();
        l.observe(
            &format!("{stack}--{service}--0"),
            stack,
            service,
            if running { "Running" } else { "Failed" },
            healthy,
        );
        l
    }

    #[test]
    fn service_started_gates_on_running() {
        let d = sandbox(dep("db", "service_started"));
        assert!(waiting_deps(&d, &live("demo", "db", false, false)).is_some());
        assert!(waiting_deps(&d, &live("demo", "db", true, false)).is_none());
    }

    #[test]
    fn service_healthy_requires_running_and_healthy() {
        let d = sandbox(dep("db", "service_healthy"));
        assert!(waiting_deps(&d, &live("demo", "db", true, false)).is_some());
        assert!(waiting_deps(&d, &live("demo", "db", false, true)).is_some());
        assert!(waiting_deps(&d, &live("demo", "db", true, true)).is_none());
    }

    #[test]
    fn waiting_message_lists_unsatisfied_deps() {
        let mut deps = dep("db", "service_healthy");
        deps.insert("cache".into(), mc2_api::DependsOnSpec::service_started());
        let d = sandbox(deps);
        let msg = waiting_deps(&d, &live("demo", "db", true, false)).unwrap();
        assert!(msg.contains("db (service_healthy)"), "{msg}");
        assert!(msg.contains("cache (service_started)"), "{msg}");
    }

    /// B11: a service is live if *any* replica is — one Failed replica (first or
    /// last) must not hide a healthy one.
    #[test]
    fn service_liveness_reduces_over_replicas_in_any_order() {
        let observe_all = |order: [(bool, bool); 2]| {
            let mut l = InstanceLiveness::default();
            for (i, (running, healthy)) in order.into_iter().enumerate() {
                l.observe(
                    &format!("db--{i}"),
                    "demo",
                    "db",
                    if running { "Running" } else { "Failed" },
                    healthy,
                );
            }
            l.service("demo", "db")
        };
        let healthy_first = observe_all([(true, true), (false, false)]);
        let healthy_last = observe_all([(false, false), (true, true)]);
        assert_eq!(healthy_first, healthy_last);
        assert!(healthy_first.running && healthy_first.healthy);
        // Only a running replica can make the service healthy.
        assert_eq!(
            observe_all([(false, true), (false, false)]),
            ServiceLive::default()
        );
    }

    /// B11: this pass's observation of an instance replaces the seeded one, so a
    /// service that just went down is not reported live from stale store state.
    #[test]
    fn current_observation_replaces_the_seeded_one() {
        let mut l = live("demo", "web", true, true);
        l.observe("demo--web--0", "demo", "web", "Failed", false);
        assert_eq!(l.service("demo", "web"), ServiceLive::default());
    }

    fn plain_spec() -> String {
        spec_json(&[], None, None)
    }

    /// The spec every helper starts from: alpine, no ports, no ssh, restart `no`.
    fn base_spec() -> mc2_api::ServiceSpec {
        mc2_api::ServiceSpec {
            image: "alpine".into(),
            scale: 1,
            cpus: 1,
            mem_limit_mib: 512,
            ports: vec![],
            network: Default::default(),
            env: BTreeMap::new(),
            secrets: vec![],
            volumes: vec![],
            restart: "no".into(),
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
        }
    }

    fn spec_json(
        expose: &[u16],
        healthcheck: Option<mc2_api::HealthcheckSpec>,
        secret: Option<(&str, &str)>,
    ) -> String {
        let mut spec = base_spec();
        spec.expose = expose
            .iter()
            .map(|&p| mc2_api::ExposeSpec {
                port: p,
                protocol: "tcp".into(),
                name: None,
            })
            .collect();
        spec.healthcheck = healthcheck;
        spec.secrets = secret
            .map(|(name, env)| {
                vec![mc2_api::SecretRef {
                    name: name.into(),
                    env: env.into(),
                    allow_hosts: vec!["api.example.com".into()],
                }]
            })
            .unwrap_or_default();
        serde_json::to_string(&spec).unwrap()
    }

    /// Spec with a `restart` policy, an optional healthcheck and `depends_on`.
    fn spec_json_deps(
        restart: &str,
        healthcheck: Option<mc2_api::HealthcheckSpec>,
        depends_on: BTreeMap<String, mc2_api::DependsOnSpec>,
    ) -> String {
        let mut spec = base_spec();
        spec.restart = restart.into();
        spec.healthcheck = healthcheck;
        spec.depends_on = depends_on;
        serde_json::to_string(&spec).unwrap()
    }

    /// A healthcheck that probes `true` every second with 3 retries.
    fn healthcheck_spec() -> mc2_api::HealthcheckSpec {
        mc2_api::HealthcheckSpec {
            test: Some(vec!["true".into()]),
            interval_seconds: 1,
            timeout_seconds: 1,
            retries: 3,
            start_period_seconds: 0,
            disable: false,
        }
    }

    /// A `NodeRuntime` that records what the node does to it.
    #[derive(Default)]
    struct FakeRuntime {
        /// Sandboxes listed as owned (as if recovered from `mc2.install`).
        owned: Vec<String>,
        /// `ensure_removed` fails this many times per id, then succeeds.
        remove_failures: tokio::sync::Mutex<HashMap<String, u32>>,
        removed: tokio::sync::Mutex<Vec<String>>,
        created: AtomicUsize,
        /// `exec_command` never returns when set.
        hang_exec: bool,
    }

    impl FakeRuntime {
        fn with_owned(owned: &[&str]) -> Self {
            Self {
                owned: owned.iter().map(|s| (*s).to_string()).collect(),
                ..Default::default()
            }
        }

        async fn fail_removals(&self, id: &str, times: u32) {
            self.remove_failures
                .lock()
                .await
                .insert(id.to_string(), times);
        }

        async fn removed(&self) -> Vec<String> {
            self.removed.lock().await.clone()
        }

        fn created_count(&self) -> usize {
            self.created.load(Ordering::SeqCst)
        }
    }

    #[async_trait::async_trait]
    impl NodeRuntime for FakeRuntime {
        async fn ensure_running(
            &self,
            d: &mc2_runtime::DesiredSandbox,
        ) -> anyhow::Result<mc2_runtime::EnsureRunning> {
            self.created.fetch_add(1, Ordering::SeqCst);
            Ok(mc2_runtime::EnsureRunning {
                status: mc2_runtime::SandboxStatus {
                    runtime_id: d.runtime_id.clone(),
                    phase: mc2_runtime::SandboxPhase::Running,
                    message: None,
                },
                outcome: mc2_runtime::EnsureOutcome::Created,
            })
        }
        async fn ensure_removed(&self, id: &str) -> anyhow::Result<()> {
            self.removed.lock().await.push(id.to_string());
            let mut failures = self.remove_failures.lock().await;
            if let Some(left) = failures.get_mut(id) {
                if *left > 0 {
                    *left -= 1;
                    anyhow::bail!("injected remove failure for {id}");
                }
            }
            Ok(())
        }
        async fn status(&self, _id: &str) -> anyhow::Result<mc2_runtime::SandboxStatus> {
            anyhow::bail!("unused")
        }
        async fn list_owned(&self, _install_id: &str) -> anyhow::Result<Vec<String>> {
            Ok(self.owned.clone())
        }
        async fn exec_command(&self, _id: &str, _argv: &[String]) -> anyhow::Result<i32> {
            if self.hang_exec {
                return std::future::pending::<anyhow::Result<i32>>().await;
            }
            Ok(0)
        }
        async fn exec_with_output(
            &self,
            _id: &str,
            _argv: &[String],
            _stdin: &[u8],
        ) -> anyhow::Result<mc2_runtime::ExecResult> {
            anyhow::bail!("unused")
        }
        async fn guest_shell(&self, _id: &str, _script: &str) -> anyhow::Result<String> {
            anyhow::bail!("unused")
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
        ) -> anyhow::Result<std::sync::Arc<dyn mc2_runtime::SshServer>> {
            anyhow::bail!("unused")
        }
    }

    async fn store_with_node() -> (Arc<MemoryStore>, String) {
        let store = MemoryStore::new();
        store.init_cluster("").await.unwrap();
        let node = store
            .upsert_local_node(NodeJoin {
                name: "n1".into(),
                labels_json: "{}".into(),
                arch: "aarch64".into(),
                cpus: 8,
                memory_mib: 8192,
            })
            .await
            .unwrap();
        (store, node.id)
    }

    /// Seed a stack's instances and bind them to `node_id`.
    ///
    /// One plan per stack: `commit_stack_plan` removes every stored instance the
    /// plan does not mention, so seeding a service on its own would prune its
    /// siblings.
    async fn seed_stack(
        store: &MemoryStore,
        node_id: &str,
        stack: &str,
        services: &[(&str, String)],
    ) -> Vec<mc2_store::InstanceRecord> {
        let plan = StackPlan::replicas(
            stack,
            "{}",
            "yaml",
            services.iter().map(|(s, spec)| (*s, vec![spec.clone()])),
        );
        let insts = store.commit_stack_plan(&plan).await.unwrap();
        for i in &insts {
            store.bind_instance_to_node(&i.id, node_id).await.unwrap();
        }
        insts
    }

    /// Seed exactly one instance for `service` in `stack` and bind it.
    async fn bound_instance(
        store: &MemoryStore,
        node_id: &str,
        stack: &str,
        service: &str,
        spec: &str,
    ) -> mc2_store::InstanceRecord {
        let mut insts = seed_stack(store, node_id, stack, &[(service, spec.to_string())]).await;
        insts.remove(0)
    }

    fn key() -> SecretsKey {
        SecretsKey::from_bytes([1u8; 32])
    }

    async fn pass(
        store: &Arc<MemoryStore>,
        node_id: &str,
        install_id: &str,
        runtime: &Arc<dyn NodeRuntime>,
        rt: &mut NodeRuntimeState,
    ) -> Result<()> {
        reconcile(
            store.clone() as Arc<dyn Store>,
            &key(),
            node_id,
            &node_cfg(install_id),
            runtime,
            rt,
        )
        .await
    }

    /// A3/B7: a consumer whose allowed port has no listener (a foreign process
    /// holds it) is reported Failed and never created.
    #[tokio::test]
    async fn consumer_with_unbindable_allowed_port_fails_closed() {
        let (store, node_id) = store_with_node().await;

        // A foreign process holds the guest port `db` exposes.
        let squatter = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = squatter.local_addr().unwrap().port();

        let insts = seed_stack(
            &store,
            &node_id,
            "shop",
            &[
                ("db", spec_json(&[port], None, None)),
                ("app", plain_spec()),
            ],
        )
        .await;
        let app = insts.iter().find(|i| i.service == "app").unwrap();

        let runtime = Arc::new(FakeRuntime::default());
        let dyn_runtime: Arc<dyn NodeRuntime> = runtime.clone();
        let mut rt_state = NodeRuntimeState::new(None, None);
        pass(&store, &node_id, "install-1", &dyn_runtime, &mut rt_state)
            .await
            .unwrap();

        assert_eq!(
            runtime.created_count(),
            0,
            "no VM may be created while an allowed port has no listener"
        );
        let app_rec = store.get_instance(&app.id).await.unwrap().unwrap();
        assert_eq!(app_rec.phase, "Failed");
        let msg = app_rec.message.unwrap_or_default();
        assert!(msg.contains(&port.to_string()), "{msg}");
        assert!(msg.contains("no listener"), "{msg}");
    }

    /// B6b: a guest exec that never returns must not stall the reconcile pass —
    /// the probe runs in its own task, so the other instances still reconcile.
    #[tokio::test]
    async fn hung_health_probe_does_not_block_reconcile() {
        let (store, node_id) = store_with_node().await;

        let health = mc2_api::HealthcheckSpec {
            test: Some(vec!["/bin/sh".into(), "-c".into(), "true".into()]),
            interval_seconds: 30,
            timeout_seconds: 1,
            retries: 3,
            start_period_seconds: 0,
            disable: false,
        };
        let insts = seed_stack(
            &store,
            &node_id,
            "shop",
            &[
                ("web", spec_json(&[], Some(health), None)),
                ("solo", plain_spec()),
            ],
        )
        .await;
        let web = insts.iter().find(|i| i.service == "web").unwrap();
        let solo = insts.iter().find(|i| i.service == "solo").unwrap();
        assert!(insts.len() == 2, "both instances are seeded");

        let runtime: Arc<dyn NodeRuntime> = Arc::new(FakeRuntime {
            hang_exec: true,
            ..Default::default()
        });
        let mut rt_state = NodeRuntimeState::new(None, None);

        // The probe's own deadline is 1s, so an inline (blocking) probe would
        // take at least that long: a 900ms budget proves the pass returned
        // before the probe did.
        let fut = pass(&store, &node_id, "install-1", &runtime, &mut rt_state);
        tokio::time::timeout(Duration::from_millis(900), fut)
            .await
            .expect("reconcile must not wait for the health probe")
            .unwrap();

        for inst in [web, solo] {
            let rec = store.get_instance(&inst.id).await.unwrap().unwrap();
            assert_eq!(rec.phase, "Running", "instance {}", inst.id);
        }
        let in_flight = rt_state
            .rt_state
            .values()
            .filter(|s| s.health_probe.is_some())
            .count();
        assert_eq!(in_flight, 1, "the hung probe is in flight, not awaited");
    }

    /// B2: a labelled orphan sandbox (owned, no desired row) is removed, but
    /// ownership is dropped only once the removal actually succeeded.
    #[tokio::test]
    async fn orphan_removal_retries_until_it_succeeds() {
        let (store, node_id) = store_with_node().await;
        let runtime = Arc::new(FakeRuntime::with_owned(&["shop--web--0"]));
        runtime.fail_removals("shop--web--0", 1).await;
        let dyn_runtime: Arc<dyn NodeRuntime> = runtime.clone();
        let mut rt_state = NodeRuntimeState::new(None, None);

        // First pass: the removal fails, so the sandbox stays owned.
        pass(&store, &node_id, "install-1", &dyn_runtime, &mut rt_state)
            .await
            .unwrap();
        assert!(
            rt_state.owned.contains("shop--web--0"),
            "a failed removal must keep ownership"
        );

        // Second pass: the retry succeeds and ownership is dropped.
        pass(&store, &node_id, "install-1", &dyn_runtime, &mut rt_state)
            .await
            .unwrap();
        assert!(rt_state.owned.is_empty(), "removed after the retry");
        assert_eq!(
            runtime.removed().await,
            vec!["shop--web--0".to_string(), "shop--web--0".to_string()],
            "exactly the owned orphan was attempted twice"
        );
    }

    /// B2: a sandbox this install does not own (no `mc2.install` label, so it is
    /// never listed) is never removed, and a desired instance keeps its
    /// placement across passes and a node rename.
    #[tokio::test]
    async fn foreign_sandboxes_are_never_touched_and_placements_survive() {
        let (store, node_id) = store_with_node().await;
        let inst = bound_instance(&store, &node_id, "shop", "web", &plain_spec()).await;

        // The runtime lists nothing (this install owns nothing yet); the
        // foreign sandbox exists in the hypervisor but is invisible here.
        let runtime = Arc::new(FakeRuntime::default());
        let dyn_runtime: Arc<dyn NodeRuntime> = runtime.clone();
        let mut rt_state = NodeRuntimeState::new(None, None);

        for _ in 0..2 {
            pass(&store, &node_id, "install-1", &dyn_runtime, &mut rt_state)
                .await
                .unwrap();
        }
        assert!(
            runtime.removed().await.is_empty(),
            "no sandbox may be removed by guesswork"
        );
        assert!(rt_state.owned.contains("shop--web--0"));

        let after = store.get_instance(&inst.id).await.unwrap().unwrap();
        assert_eq!(after.phase, "Running");
        assert_eq!(after.node_id.as_deref(), Some(node_id.as_str()));

        // Renaming the node (hostname / --node-name change) reuses its id, so
        // the placement is untouched.
        let renamed = store
            .upsert_local_node(NodeJoin {
                name: "n1-renamed".into(),
                labels_json: "{}".into(),
                arch: "aarch64".into(),
                cpus: 8,
                memory_mib: 8192,
            })
            .await
            .unwrap();
        assert_eq!(renamed.id, node_id);
        pass(
            &store,
            &renamed.id,
            "install-1",
            &dyn_runtime,
            &mut rt_state,
        )
        .await
        .unwrap();
        let after = store.get_instance(&inst.id).await.unwrap().unwrap();
        assert_eq!(after.node_id.as_deref(), Some(node_id.as_str()));
        assert_eq!(after.phase, "Running");
    }

    /// B2/B13: per-sandbox runtime state is pruned once an instance leaves the
    /// desired set and its sandbox is gone.
    #[tokio::test]
    async fn runtime_state_is_pruned_with_the_instance() {
        let (store, node_id) = store_with_node().await;
        bound_instance(&store, &node_id, "shop", "web", &plain_spec()).await;
        let runtime = Arc::new(FakeRuntime::default());
        let dyn_runtime: Arc<dyn NodeRuntime> = runtime.clone();
        let mut rt_state = NodeRuntimeState::new(None, None);

        pass(&store, &node_id, "install-1", &dyn_runtime, &mut rt_state)
            .await
            .unwrap();
        assert!(rt_state.rt_state.contains_key("shop--web--0"));

        // The stack is deleted: desired shrinks to nothing, the sandbox is
        // removed, and its bookkeeping goes with it.
        store.delete_stack("shop").await.unwrap();
        pass(&store, &node_id, "install-1", &dyn_runtime, &mut rt_state)
            .await
            .unwrap();
        assert!(rt_state.owned.is_empty());
        assert!(
            rt_state.rt_state.is_empty(),
            "stale restart bookkeeping must not be inherited by a reused name"
        );
    }

    /// B3: a restart adopts running sandboxes only when the persisted applied
    /// hash matches the desired config; a mismatch (or no hash at all) recreates.
    #[tokio::test]
    async fn stale_applied_hash_forces_a_recreate() {
        let (store, node_id) = store_with_node().await;
        let inst = bound_instance(&store, &node_id, "shop", "web", &plain_spec()).await;
        store
            .set_instance_applied_hash(&inst.id, Some("stale-config"))
            .await
            .unwrap();

        let runtime = Arc::new(FakeRuntime::with_owned(&["shop--web--0"]));
        let dyn_runtime: Arc<dyn NodeRuntime> = runtime.clone();
        let mut rt_state = NodeRuntimeState::new(None, None);
        pass(&store, &node_id, "install-1", &dyn_runtime, &mut rt_state)
            .await
            .unwrap();

        assert_eq!(
            runtime.removed().await,
            vec!["shop--web--0".to_string()],
            "the stale sandbox is removed before the new config is created"
        );
        assert_eq!(runtime.created_count(), 1, "then recreated");
        let want = expected_recreate_hash(&store, &node_id).await;
        assert_eq!(
            store.get_instance_applied_hash(&inst.id).await.unwrap(),
            Some(want),
            "the new config hash is recorded only after a successful ensure_running"
        );
    }

    /// B3: a matching persisted hash is adopted as-is (no recreate).
    #[tokio::test]
    async fn matching_applied_hash_is_adopted() {
        let (store, node_id) = store_with_node().await;
        let inst = bound_instance(&store, &node_id, "shop", "web", &plain_spec()).await;
        let want = expected_recreate_hash(&store, &node_id).await;
        store
            .set_instance_applied_hash(&inst.id, Some(&want))
            .await
            .unwrap();

        let runtime = Arc::new(FakeRuntime::with_owned(&["shop--web--0"]));
        let dyn_runtime: Arc<dyn NodeRuntime> = runtime.clone();
        let mut rt_state = NodeRuntimeState::new(None, None);
        pass(&store, &node_id, "install-1", &dyn_runtime, &mut rt_state)
            .await
            .unwrap();

        assert!(runtime.removed().await.is_empty(), "adopted, not recreated");
        let rec = store.get_instance(&inst.id).await.unwrap().unwrap();
        assert_eq!(rec.phase, "Running");
    }

    /// B3: an adopted sandbox with no recorded applied hash is recreated — an
    /// unlabelled/unknown config must never be trusted just because it runs.
    #[tokio::test]
    async fn null_applied_hash_for_an_existing_sandbox_recreates() {
        let (store, node_id) = store_with_node().await;
        bound_instance(&store, &node_id, "shop", "web", &plain_spec()).await;

        let runtime = Arc::new(FakeRuntime::with_owned(&["shop--web--0"]));
        let dyn_runtime: Arc<dyn NodeRuntime> = runtime.clone();
        let mut rt_state = NodeRuntimeState::new(None, None);
        pass(&store, &node_id, "install-1", &dyn_runtime, &mut rt_state)
            .await
            .unwrap();

        assert_eq!(runtime.removed().await, vec!["shop--web--0".to_string()]);
        assert_eq!(runtime.created_count(), 1);
    }

    /// B3: a failed removal during recreate aborts the instance's pass — the old
    /// hash is kept (it describes the VM that is still running) and the recreate
    /// is retried next pass.
    #[tokio::test]
    async fn failed_removal_during_recreate_keeps_the_old_hash() {
        let (store, node_id) = store_with_node().await;
        let inst = bound_instance(&store, &node_id, "shop", "web", &plain_spec()).await;
        store
            .set_instance_applied_hash(&inst.id, Some("old-config"))
            .await
            .unwrap();

        let runtime = Arc::new(FakeRuntime::with_owned(&["shop--web--0"]));
        runtime.fail_removals("shop--web--0", 1).await;
        let dyn_runtime: Arc<dyn NodeRuntime> = runtime.clone();
        let mut rt_state = NodeRuntimeState::new(None, None);

        pass(&store, &node_id, "install-1", &dyn_runtime, &mut rt_state)
            .await
            .unwrap();

        assert_eq!(
            store.get_instance_applied_hash(&inst.id).await.unwrap(),
            Some("old-config".to_string()),
            "the new config must never be recorded for the old VM"
        );
        assert_eq!(runtime.created_count(), 0, "no VM was created");
        assert!(rt_state.owned.contains("shop--web--0"));
        let rec = store.get_instance(&inst.id).await.unwrap().unwrap();
        assert_eq!(rec.phase, "Failed");
        assert!(rec.message.unwrap().contains("retrying next pass"));

        // Retry: the removal now succeeds and the new hash is recorded.
        pass(&store, &node_id, "install-1", &dyn_runtime, &mut rt_state)
            .await
            .unwrap();
        assert_eq!(runtime.created_count(), 1);
        let want = expected_recreate_hash(&store, &node_id).await;
        assert_eq!(
            store.get_instance_applied_hash(&inst.id).await.unwrap(),
            Some(want)
        );
    }

    /// B4: one instance with a missing secret must not stop the node — the
    /// healthy instance reconciles, the failing one is reported Failed, and its
    /// existing sandbox is not garbage-collected.
    #[tokio::test]
    async fn one_unresolvable_instance_does_not_halt_the_pass() {
        let (store, node_id) = store_with_node().await;
        let plan = StackPlan::replicas(
            "shop",
            "{}",
            "yaml",
            [
                (
                    "web",
                    vec![spec_json(&[], None, Some(("not-set-anywhere", "DB_PASS")))],
                ),
                ("solo", vec![plain_spec()]),
            ],
        );
        let insts = store.commit_stack_plan(&plan).await.unwrap();
        for i in &insts {
            store.bind_instance_to_node(&i.id, &node_id).await.unwrap();
        }
        let web = insts.iter().find(|i| i.service == "web").unwrap();
        let solo = insts.iter().find(|i| i.service == "solo").unwrap();

        // The failing instance already has a sandbox; it must survive.
        let runtime = Arc::new(FakeRuntime::with_owned(&["shop--web--0"]));
        let dyn_runtime: Arc<dyn NodeRuntime> = runtime.clone();
        let mut rt_state = NodeRuntimeState::new(None, None);
        pass(&store, &node_id, "install-1", &dyn_runtime, &mut rt_state)
            .await
            .unwrap();

        let solo_rec = store.get_instance(&solo.id).await.unwrap().unwrap();
        assert_eq!(solo_rec.phase, "Running");

        let web_rec = store.get_instance(&web.id).await.unwrap().unwrap();
        assert_eq!(web_rec.phase, "Failed");
        let msg = web_rec.message.unwrap_or_default();
        assert!(msg.contains("not-set-anywhere"), "{msg}");

        assert!(
            runtime.removed().await.is_empty(),
            "an instance whose desired state failed keeps its sandbox"
        );
        assert!(rt_state.owned.contains("shop--web--0"));
    }

    /// Desired hash the node should record for `shop/web/0`, computed through
    /// the same builder the node uses.
    async fn expected_recreate_hash(store: &Arc<MemoryStore>, node_id: &str) -> String {
        let set = build_desired_set(store.clone() as Arc<dyn Store>, &key(), node_id)
            .await
            .unwrap();
        let desired = set
            .sandboxes
            .iter()
            .find(|d| d.service == "web")
            .expect("web in desired set");
        mc2_runtime::desired_recreate_hash(desired)
    }

    /// A store whose observed-state writes fail, to prove the pass fails with
    /// them instead of silently keeping the last known state (D4).
    /// Everything else delegates to the memory store.
    struct FailingStatusStore {
        inner: Arc<MemoryStore>,
    }

    #[async_trait::async_trait]
    impl Store for FailingStatusStore {
        async fn get_cluster_meta(&self) -> Result<Option<ClusterMeta>, StoreError> {
            self.inner.get_cluster_meta().await
        }
        async fn init_cluster(&self, api_token_hash: &str) -> Result<ClusterMeta, StoreError> {
            self.inner.init_cluster(api_token_hash).await
        }
        async fn verify_api_token(&self, token: &str) -> Result<bool, StoreError> {
            self.inner.verify_api_token(token).await
        }
        async fn replace_api_token_hash(&self, api_token_hash: &str) -> Result<(), StoreError> {
            self.inner.replace_api_token_hash(api_token_hash).await
        }
        async fn cluster_counts(&self) -> Result<ClusterCounts, StoreError> {
            self.inner.cluster_counts().await
        }
        async fn get_setting(&self, key: &str) -> Result<Option<String>, StoreError> {
            self.inner.get_setting(key).await
        }
        async fn set_setting(&self, key: &str, value: &str) -> Result<(), StoreError> {
            self.inner.set_setting(key, value).await
        }
        async fn upsert_local_node(&self, join: NodeJoin) -> Result<NodeRecord, StoreError> {
            self.inner.upsert_local_node(join).await
        }
        async fn touch_node(
            &self,
            node_id: &str,
            hb: NodeHeartbeat,
        ) -> Result<NodeRecord, StoreError> {
            self.inner.touch_node(node_id, hb).await
        }
        async fn list_nodes(&self) -> Result<Vec<NodeRecord>, StoreError> {
            self.inner.list_nodes().await
        }
        async fn get_node(&self, node_id: &str) -> Result<Option<NodeRecord>, StoreError> {
            self.inner.get_node(node_id).await
        }
        async fn list_stacks(&self) -> Result<Vec<StackRecord>, StoreError> {
            self.inner.list_stacks().await
        }
        async fn get_stack(&self, name: &str) -> Result<Option<StackRecord>, StoreError> {
            self.inner.get_stack(name).await
        }
        async fn commit_stack_plan(
            &self,
            plan: &StackPlan,
        ) -> Result<Vec<InstanceRecord>, StoreError> {
            self.inner.commit_stack_plan(plan).await
        }
        async fn delete_stack(&self, name: &str) -> Result<bool, StoreError> {
            self.inner.delete_stack(name).await
        }
        async fn list_instances(&self) -> Result<Vec<InstanceRecord>, StoreError> {
            self.inner.list_instances().await
        }
        async fn list_instances_for_node(
            &self,
            node_id: &str,
        ) -> Result<Vec<InstanceRecord>, StoreError> {
            self.inner.list_instances_for_node(node_id).await
        }
        async fn list_pending_instances(&self) -> Result<Vec<InstanceRecord>, StoreError> {
            self.inner.list_pending_instances().await
        }
        async fn bind_instance_to_node(
            &self,
            instance_id: &str,
            node_id: &str,
        ) -> Result<InstanceRecord, StoreError> {
            self.inner.bind_instance_to_node(instance_id, node_id).await
        }
        async fn set_instance_applied_hash(
            &self,
            instance_id: &str,
            applied_hash: Option<&str>,
        ) -> Result<(), StoreError> {
            self.inner
                .set_instance_applied_hash(instance_id, applied_hash)
                .await
        }
        async fn get_instance_applied_hash(
            &self,
            instance_id: &str,
        ) -> Result<Option<String>, StoreError> {
            self.inner.get_instance_applied_hash(instance_id).await
        }
        async fn get_instance(
            &self,
            instance_id: &str,
        ) -> Result<Option<InstanceRecord>, StoreError> {
            self.inner.get_instance(instance_id).await
        }
        async fn update_instance_health(
            &self,
            instance_id: &str,
            healthy: bool,
        ) -> Result<InstanceRecord, StoreError> {
            self.inner
                .update_instance_health(instance_id, healthy)
                .await
        }
        async fn put_secret_blob(
            &self,
            name: &str,
            nonce: &[u8],
            ciphertext: &[u8],
        ) -> Result<SecretMeta, StoreError> {
            self.inner.put_secret_blob(name, nonce, ciphertext).await
        }
        async fn get_secret_blob(&self, name: &str) -> Result<Option<SecretBlob>, StoreError> {
            self.inner.get_secret_blob(name).await
        }
        async fn list_secret_meta(&self) -> Result<Vec<SecretMeta>, StoreError> {
            self.inner.list_secret_meta().await
        }
        async fn delete_secret(&self, name: &str) -> Result<bool, StoreError> {
            self.inner.delete_secret(name).await
        }
        async fn put_ssh_key(
            &self,
            name: &str,
            public_key: &str,
        ) -> Result<SshAuthorizedKey, StoreError> {
            self.inner.put_ssh_key(name, public_key).await
        }
        async fn get_ssh_key(&self, name: &str) -> Result<Option<SshAuthorizedKey>, StoreError> {
            self.inner.get_ssh_key(name).await
        }
        async fn list_ssh_keys(&self) -> Result<Vec<SshAuthorizedKey>, StoreError> {
            self.inner.list_ssh_keys().await
        }
        async fn delete_ssh_key(&self, name: &str) -> Result<bool, StoreError> {
            self.inner.delete_ssh_key(name).await
        }
        async fn get_instance_ssh(
            &self,
            instance_id: &str,
        ) -> Result<Option<InstanceSshRecord>, StoreError> {
            self.inner.get_instance_ssh(instance_id).await
        }
        async fn put_instance_ssh_desired(
            &self,
            rec: &InstanceSshRecord,
        ) -> Result<InstanceSshRecord, StoreError> {
            self.inner.put_instance_ssh_desired(rec).await
        }
        async fn clear_instance_ssh_override(
            &self,
            instance_id: &str,
        ) -> Result<Option<InstanceSshRecord>, StoreError> {
            self.inner.clear_instance_ssh_override(instance_id).await
        }
        async fn update_instance_ssh_observed(
            &self,
            instance_id: &str,
            phase: &str,
            bind: Option<&str>,
            port: Option<u16>,
            message: Option<&str>,
        ) -> Result<InstanceSshRecord, StoreError> {
            self.inner
                .update_instance_ssh_observed(instance_id, phase, bind, port, message)
                .await
        }
        async fn list_instance_ssh(&self) -> Result<Vec<InstanceSshRecord>, StoreError> {
            self.inner.list_instance_ssh().await
        }
        async fn update_instance_network_observed(
            &self,
            instance_id: &str,
            phase: &str,
            observed_json: &str,
            message: Option<&str>,
        ) -> Result<InstanceNetworkRecord, StoreError> {
            self.inner
                .update_instance_network_observed(instance_id, phase, observed_json, message)
                .await
        }
        async fn get_instance_network(
            &self,
            instance_id: &str,
        ) -> Result<Option<InstanceNetworkRecord>, StoreError> {
            self.inner.get_instance_network(instance_id).await
        }

        async fn update_instance_status(
            &self,
            instance_id: &str,
            phase: &str,
            runtime_id: Option<&str>,
            message: Option<&str>,
        ) -> Result<InstanceRecord, StoreError> {
            let _ = (instance_id, phase, runtime_id, message);
            Err(StoreError::Other(anyhow::anyhow!(
                "injected update_instance_status failure"
            )))
        }
    }

    /// D4: an observed-state write failure must be logged and make the pass
    /// count as failed; the instance itself is still reconciled.
    #[tokio::test]
    async fn failing_status_writes_fail_the_pass() {
        let (store, node_id) = store_with_node().await;
        let inst = bound_instance(&store, &node_id, "shop", "web", &plain_spec()).await;

        let failing: Arc<dyn Store> = Arc::new(FailingStatusStore {
            inner: store.clone(),
        });
        let runtime = Arc::new(FakeRuntime::default());
        let dyn_runtime: Arc<dyn NodeRuntime> = runtime.clone();
        let mut rt_state = NodeRuntimeState::new(None, None);

        let err = reconcile(
            failing,
            &key(),
            &node_id,
            &node_cfg("install-1"),
            &dyn_runtime,
            &mut rt_state,
        )
        .await
        .expect_err("the pass must fail when observed state cannot be persisted");
        let msg = format!("{err:#}");
        assert!(msg.contains("update_instance_status"), "{msg}");
        assert!(msg.contains(&inst.id), "{msg}");

        assert_eq!(runtime.created_count(), 1, "the instance was reconciled");
        assert!(rt_state.owned.contains("shop--web--0"));
    }

    /// Per-runtime-id answers for [`ScriptedRuntime`].
    #[derive(Debug, Clone, Copy)]
    struct Script {
        outcome: mc2_runtime::EnsureOutcome,
        phase: mc2_runtime::SandboxPhase,
        /// Exit code of the health probe (`exec_command`).
        probe_exit: i32,
    }

    impl Default for Script {
        fn default() -> Self {
            Self {
                outcome: mc2_runtime::EnsureOutcome::Created,
                phase: mc2_runtime::SandboxPhase::Running,
                probe_exit: 0,
            }
        }
    }

    /// A runtime whose per-instance answers are scripted, so restart-backoff and
    /// dependency-gating decisions can be driven without a hypervisor (B10/B11).
    #[derive(Default)]
    struct ScriptedRuntime {
        scripts: tokio::sync::Mutex<HashMap<String, Script>>,
        /// Runtime ids whose sandbox is up: what `status` observes.
        up: tokio::sync::Mutex<HashSet<String>>,
        ensure_calls: tokio::sync::Mutex<HashMap<String, usize>>,
        removed: tokio::sync::Mutex<Vec<String>>,
    }

    impl ScriptedRuntime {
        async fn script(
            &self,
            id: &str,
            outcome: mc2_runtime::EnsureOutcome,
            phase: mc2_runtime::SandboxPhase,
        ) {
            self.scripts.lock().await.insert(
                id.to_string(),
                Script {
                    outcome,
                    phase,
                    ..Default::default()
                },
            );
        }

        async fn set_probe_exit(&self, id: &str, exit: i32) {
            self.scripts
                .lock()
                .await
                .entry(id.to_string())
                .or_default()
                .probe_exit = exit;
        }

        /// The sandbox is up before this process ever saw it (adoption).
        async fn mark_up(&self, id: &str) {
            self.up.lock().await.insert(id.to_string());
        }

        async fn ensure_calls(&self, id: &str) -> usize {
            self.ensure_calls.lock().await.get(id).copied().unwrap_or(0)
        }
    }

    #[async_trait::async_trait]
    impl NodeRuntime for ScriptedRuntime {
        async fn ensure_running(
            &self,
            d: &mc2_runtime::DesiredSandbox,
        ) -> anyhow::Result<mc2_runtime::EnsureRunning> {
            *self
                .ensure_calls
                .lock()
                .await
                .entry(d.runtime_id.clone())
                .or_default() += 1;
            let script = self
                .scripts
                .lock()
                .await
                .get(&d.runtime_id)
                .copied()
                .unwrap_or_default();
            if script.phase == mc2_runtime::SandboxPhase::Running {
                self.up.lock().await.insert(d.runtime_id.clone());
            }
            Ok(mc2_runtime::EnsureRunning {
                status: mc2_runtime::SandboxStatus {
                    runtime_id: d.runtime_id.clone(),
                    phase: script.phase,
                    message: None,
                },
                outcome: script.outcome,
            })
        }
        async fn ensure_removed(&self, id: &str) -> anyhow::Result<()> {
            self.removed.lock().await.push(id.to_string());
            self.up.lock().await.remove(id);
            Ok(())
        }
        async fn status(&self, id: &str) -> anyhow::Result<mc2_runtime::SandboxStatus> {
            let up = self.up.lock().await.contains(id);
            Ok(mc2_runtime::SandboxStatus {
                runtime_id: id.to_string(),
                phase: if up {
                    mc2_runtime::SandboxPhase::Running
                } else {
                    mc2_runtime::SandboxPhase::Stopped
                },
                message: None,
            })
        }
        async fn list_owned(&self, _install_id: &str) -> anyhow::Result<Vec<String>> {
            Ok(vec![])
        }
        async fn exec_command(&self, id: &str, _argv: &[String]) -> anyhow::Result<i32> {
            let exit = self
                .scripts
                .lock()
                .await
                .get(id)
                .copied()
                .unwrap_or_default()
                .probe_exit;
            Ok(exit)
        }
        async fn exec_with_output(
            &self,
            _id: &str,
            _argv: &[String],
            _stdin: &[u8],
        ) -> anyhow::Result<mc2_runtime::ExecResult> {
            anyhow::bail!("unused")
        }
        async fn guest_shell(&self, _id: &str, _script: &str) -> anyhow::Result<String> {
            anyhow::bail!("unused")
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
        ) -> anyhow::Result<std::sync::Arc<dyn mc2_runtime::SshServer>> {
            anyhow::bail!("unused")
        }
    }

    /// The node's `DesiredSandbox` for `service`, built the way the loop does.
    async fn desired_sandbox(
        store: &Arc<MemoryStore>,
        node_id: &str,
        service: &str,
    ) -> mc2_runtime::DesiredSandbox {
        build_desired_set(store.clone() as Arc<dyn Store>, &key(), node_id)
            .await
            .unwrap()
            .sandboxes
            .into_iter()
            .find(|d| d.service == service)
            .unwrap_or_else(|| panic!("{service} in the desired set"))
    }

    /// The stored instance for `service` at `ordinal`.
    async fn record_of(store: &MemoryStore, service: &str, ordinal: u32) -> InstanceRecord {
        store
            .list_instances()
            .await
            .unwrap()
            .into_iter()
            .find(|i| i.service == service && i.ordinal == ordinal)
            .unwrap_or_else(|| panic!("no {service}/{ordinal} instance"))
    }

    async fn phase_of(store: &MemoryStore, service: &str, ordinal: u32) -> String {
        record_of(store, service, ordinal).await.phase
    }

    /// Let the spawned health probe complete (it never touches a hypervisor).
    async fn let_probe_finish() {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    /// Seed a stack with one or more replicas per service.
    async fn seed_stack_replicas(
        store: &MemoryStore,
        node_id: &str,
        stack: &str,
        services: &[(&str, Vec<String>)],
    ) -> Vec<mc2_store::InstanceRecord> {
        let plan = StackPlan::replicas(
            stack,
            "{}",
            "yaml",
            services.iter().map(|(s, specs)| (*s, specs.clone())),
        );
        let insts = store.commit_stack_plan(&plan).await.unwrap();
        for i in &insts {
            store.bind_instance_to_node(&i.id, node_id).await.unwrap();
        }
        insts
    }

    /// B10: a backend that restarts the sandbox itself reports `Restarted`, and
    /// that counts like a failure — attempts are spaced by the backoff schedule
    /// instead of once per pass — and the counter resets after 60s of Running.
    #[tokio::test]
    async fn sdk_restarts_are_spaced_by_backoff_and_reset_after_sustained_running() {
        let (mem, node_id) = store_with_node().await;
        bound_instance(
            &mem,
            &node_id,
            "shop",
            "web",
            &spec_json_deps("always", None, BTreeMap::new()),
        )
        .await;
        let store: Arc<dyn Store> = mem.clone();
        let runtime = Arc::new(ScriptedRuntime::default());
        runtime
            .script(
                "shop--web--0",
                mc2_runtime::EnsureOutcome::Restarted,
                mc2_runtime::SandboxPhase::Running,
            )
            .await;
        let dyn_runtime: Arc<dyn NodeRuntime> = runtime.clone();
        let mut rt_state = NodeRuntimeState::new(None, None);
        let mut d = desired_sandbox(&mem, &node_id, "web").await;
        let mut live = InstanceLiveness::default();
        let mut errors = PassErrors::default();
        let t0 = Instant::now();

        // Pass 1: the backend restarted the sandbox itself.
        let r = rt_state
            .reconcile_instance(&store, &dyn_runtime, &mut d, t0, &mut live, &mut errors)
            .await
            .unwrap();
        assert_eq!(r.phase, "Running");
        assert_eq!(runtime.ensure_calls("shop--web--0").await, 1);
        let st = rt_state.rt_state.get("shop--web--0").unwrap();
        assert_eq!(st.restart_count, 1, "an SDK restart counts as a restart");
        assert_eq!(st.next_restart_ok, Some(t0 + Duration::from_secs(2)));

        // One second later the pass is held back: a crash loop cannot restart on
        // every pass.
        let r = rt_state
            .reconcile_instance(
                &store,
                &dyn_runtime,
                &mut d,
                t0 + Duration::from_secs(1),
                &mut live,
                &mut errors,
            )
            .await
            .unwrap();
        assert_eq!(
            runtime.ensure_calls("shop--web--0").await,
            1,
            "restart attempts are spaced by the backoff schedule"
        );
        assert!(r.message.contains("restart backoff"), "{}", r.message);
        assert_eq!(rt_state.rt_state["shop--web--0"].restart_count, 1);

        // Past each deadline the next attempt happens and the schedule grows.
        for (offset, calls, next_secs) in [(2u64, 2usize, 5u64), (7, 3, 15), (22, 4, 30)] {
            let at = t0 + Duration::from_secs(offset);
            rt_state
                .reconcile_instance(&store, &dyn_runtime, &mut d, at, &mut live, &mut errors)
                .await
                .unwrap();
            assert_eq!(
                runtime.ensure_calls("shop--web--0").await,
                calls,
                "attempt at +{offset}s"
            );
            assert_eq!(
                rt_state.rt_state["shop--web--0"].next_restart_ok,
                Some(at + Duration::from_secs(next_secs))
            );
        }

        // The VM survives (no restart) and runs: after 60s the counter resets.
        runtime
            .script(
                "shop--web--0",
                mc2_runtime::EnsureOutcome::AlreadyRunning,
                mc2_runtime::SandboxPhase::Running,
            )
            .await;
        let survived = t0 + Duration::from_secs(22);
        rt_state
            .reconcile_instance(
                &store,
                &dyn_runtime,
                &mut d,
                survived,
                &mut live,
                &mut errors,
            )
            .await
            .unwrap();
        assert_eq!(
            rt_state.rt_state["shop--web--0"].restart_count, 4,
            "a restart-free pass does not advance the counter"
        );
        rt_state
            .reconcile_instance(
                &store,
                &dyn_runtime,
                &mut d,
                survived + Duration::from_secs(61),
                &mut live,
                &mut errors,
            )
            .await
            .unwrap();
        assert_eq!(
            rt_state.rt_state["shop--web--0"].restart_count, 0,
            "60s of Running resets the restart counter"
        );
        assert_eq!(rt_state.rt_state["shop--web--0"].next_restart_ok, None);

        // ...so the next restart starts the schedule over.
        runtime
            .script(
                "shop--web--0",
                mc2_runtime::EnsureOutcome::Restarted,
                mc2_runtime::SandboxPhase::Running,
            )
            .await;
        let again = survived + Duration::from_secs(62);
        rt_state
            .reconcile_instance(&store, &dyn_runtime, &mut d, again, &mut live, &mut errors)
            .await
            .unwrap();
        assert_eq!(rt_state.rt_state["shop--web--0"].restart_count, 1);
        assert_eq!(
            rt_state.rt_state["shop--web--0"].next_restart_ok,
            Some(again + Duration::from_secs(2))
        );
    }

    /// B11: `depends_on` orders startup only. Once a dependent's VM is running it
    /// is neither reported Pending nor dropped from routing when its dependency
    /// later goes unhealthy — it keeps being reconciled.
    #[tokio::test]
    async fn a_running_dependent_is_not_re_gated_when_its_dependency_fails() {
        let (store, node_id) = store_with_node().await;
        seed_stack(
            &store,
            &node_id,
            "shop",
            &[
                (
                    "db",
                    spec_json_deps("always", Some(healthcheck_spec()), BTreeMap::new()),
                ),
                (
                    "web",
                    spec_json_deps("always", None, dep("db", "service_healthy")),
                ),
            ],
        )
        .await;

        let runtime = Arc::new(ScriptedRuntime::default());
        runtime.set_probe_exit("shop--db--0", 0).await;
        let dyn_runtime: Arc<dyn NodeRuntime> = runtime.clone();
        let mut rt_state = NodeRuntimeState::new(None, None);

        // Pass 1: db comes up. Its probe result is only read by the next pass, so
        // web is still gated.
        pass(&store, &node_id, "install-1", &dyn_runtime, &mut rt_state)
            .await
            .unwrap();
        assert_eq!(phase_of(&store, "web", 0).await, "Pending");
        assert_eq!(runtime.ensure_calls("shop--web--0").await, 0);

        // Pass 2: db is healthy, so web starts.
        let_probe_finish().await;
        pass(&store, &node_id, "install-1", &dyn_runtime, &mut rt_state)
            .await
            .unwrap();
        assert_eq!(phase_of(&store, "web", 0).await, "Running");
        assert_eq!(runtime.ensure_calls("shop--web--0").await, 1);

        // Pass 3: db fails. web is already running: it must keep its phase, stay
        // reconciled, and not be reported as waiting on its dependency.
        runtime
            .script(
                "shop--db--0",
                mc2_runtime::EnsureOutcome::Restarted,
                mc2_runtime::SandboxPhase::Failed,
            )
            .await;
        pass(&store, &node_id, "install-1", &dyn_runtime, &mut rt_state)
            .await
            .unwrap();
        assert_eq!(phase_of(&store, "db", 0).await, "Failed");
        let web = record_of(&store, "web", 0).await;
        assert_eq!(
            web.phase, "Running",
            "a running dependent is not held back by a failing dependency"
        );
        // The store keeps the last non-empty message (the old `depends_on:
        // waiting …` text survives), so the proof that web was not re-gated is
        // the phase above plus the continued reconcile below.
        assert!(
            rt_state.rt_state["shop--web--0"].started,
            "web keeps its started flag once its VM is up"
        );
        assert_eq!(
            runtime.ensure_calls("shop--web--0").await,
            2,
            "web is still reconciled"
        );
    }

    /// B11: an adopted sandbox (node restart) that is already running is not
    /// re-gated either, even though this process has never seen it come up.
    #[tokio::test]
    async fn an_adopted_running_dependent_is_not_gated() {
        let (store, node_id) = store_with_node().await;
        seed_stack(
            &store,
            &node_id,
            "shop",
            &[
                (
                    "db",
                    spec_json_deps("always", Some(healthcheck_spec()), BTreeMap::new()),
                ),
                (
                    "web",
                    spec_json_deps("always", None, dep("db", "service_healthy")),
                ),
            ],
        )
        .await;

        let runtime = Arc::new(ScriptedRuntime::default());
        runtime.mark_up("shop--web--0").await;
        runtime
            .script(
                "shop--db--0",
                mc2_runtime::EnsureOutcome::Restarted,
                mc2_runtime::SandboxPhase::Failed,
            )
            .await;
        let dyn_runtime: Arc<dyn NodeRuntime> = runtime.clone();
        let mut rt_state = NodeRuntimeState::new(None, None);

        pass(&store, &node_id, "install-1", &dyn_runtime, &mut rt_state)
            .await
            .unwrap();

        assert_eq!(
            phase_of(&store, "web", 0).await,
            "Running",
            "an observed running sandbox is never reported Pending"
        );
    }

    /// B11: the service reduction is order-independent — a Failed replica must
    /// not hide a healthy one, whichever is processed last.
    #[tokio::test]
    async fn a_healthy_replica_keeps_the_service_live_when_another_fails() {
        let (store, node_id) = store_with_node().await;
        let db = spec_json_deps("always", Some(healthcheck_spec()), BTreeMap::new());
        seed_stack_replicas(
            &store,
            &node_id,
            "shop",
            &[
                ("db", vec![db.clone(), db]),
                (
                    "web",
                    vec![spec_json_deps("always", None, dep("db", "service_healthy"))],
                ),
            ],
        )
        .await;

        let runtime = Arc::new(ScriptedRuntime::default());
        runtime.set_probe_exit("shop--db--0", 0).await;
        // Replica 1 is processed after replica 0 and has failed.
        runtime
            .script(
                "shop--db--1",
                mc2_runtime::EnsureOutcome::Restarted,
                mc2_runtime::SandboxPhase::Failed,
            )
            .await;
        let dyn_runtime: Arc<dyn NodeRuntime> = runtime.clone();
        let mut rt_state = NodeRuntimeState::new(None, None);

        // Pass 1: replica 0 is up but not yet healthy, so web is still gated.
        pass(&store, &node_id, "install-1", &dyn_runtime, &mut rt_state)
            .await
            .unwrap();
        assert_eq!(phase_of(&store, "db", 1).await, "Failed");
        assert_eq!(phase_of(&store, "web", 0).await, "Pending");

        // Pass 2: replica 0 is healthy, replica 1 still Failed — the Failed
        // replica is reduced with the healthy one, so db is live and web starts.
        let_probe_finish().await;
        pass(&store, &node_id, "install-1", &dyn_runtime, &mut rt_state)
            .await
            .unwrap();
        assert_eq!(phase_of(&store, "db", 0).await, "Running");
        assert_ne!(
            phase_of(&store, "db", 1).await,
            "Running",
            "the failed replica stays down (it is in restart backoff)"
        );
        assert_eq!(
            phase_of(&store, "web", 0).await,
            "Running",
            "one healthy replica keeps the service live for its dependents"
        );
    }

    /// A `NodeRuntime` whose calls never return, standing in for a microVM call
    /// that wedges mid-pass.
    struct HangingRuntime {
        entered: Arc<tokio::sync::Notify>,
    }

    #[async_trait::async_trait]
    impl NodeRuntime for HangingRuntime {
        async fn ensure_running(
            &self,
            _d: &mc2_runtime::DesiredSandbox,
        ) -> anyhow::Result<mc2_runtime::EnsureRunning> {
            std::future::pending().await
        }
        async fn ensure_removed(&self, _id: &str) -> anyhow::Result<()> {
            std::future::pending().await
        }
        async fn status(&self, _id: &str) -> anyhow::Result<mc2_runtime::SandboxStatus> {
            std::future::pending().await
        }
        async fn list_owned(&self, _install_id: &str) -> anyhow::Result<Vec<String>> {
            self.entered.notify_one();
            std::future::pending().await
        }
        async fn exec_command(&self, _id: &str, _argv: &[String]) -> anyhow::Result<i32> {
            std::future::pending().await
        }
        async fn exec_with_output(
            &self,
            _id: &str,
            _argv: &[String],
            _stdin: &[u8],
        ) -> anyhow::Result<mc2_runtime::ExecResult> {
            std::future::pending().await
        }
        async fn guest_shell(&self, _id: &str, _script: &str) -> anyhow::Result<String> {
            anyhow::bail!("unused")
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
        ) -> anyhow::Result<std::sync::Arc<dyn mc2_runtime::SshServer>> {
            anyhow::bail!("unused")
        }
    }

    /// D2: a node loop wedged inside a runtime call that never returns is still
    /// stopped within the shutdown deadline — the token cannot interrupt the
    /// in-flight call, so the supervisor aborts the task.
    #[tokio::test]
    async fn a_wedged_runtime_call_cannot_hold_shutdown_open() {
        let (store, node_id) = store_with_node().await;
        let entered = Arc::new(tokio::sync::Notify::new());
        let runtime: Arc<dyn NodeRuntime> = Arc::new(HangingRuntime {
            entered: entered.clone(),
        });
        let cancel = CancellationToken::new();
        let cancel_node = cancel.clone();
        let liveness = Arc::new(Liveness::for_interval(Duration::from_secs(10)));

        let mut critical: tokio::task::JoinSet<(&'static str, Result<()>)> =
            tokio::task::JoinSet::new();
        critical.spawn(async move {
            let res = run(
                store as Arc<dyn Store>,
                Arc::new(key()),
                runtime,
                node_id,
                node_cfg("install-1"),
                cancel_node,
                liveness,
            )
            .await;
            ("node", res)
        });

        // The first pass reaches the wedged call; nothing there will return.
        tokio::time::timeout(Duration::from_secs(5), entered.notified())
            .await
            .expect("the reconcile pass must start");

        cancel.cancel();
        // Cancelling alone cannot interrupt an in-flight runtime call.
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            !critical.is_empty(),
            "the wedged pass must not exit on cancel alone"
        );

        let started = Instant::now();
        crate::drain_or_abort(&mut critical, Duration::from_millis(200)).await;
        assert!(critical.is_empty(), "the wedged task must be aborted");
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "shutdown must be bounded: {:?}",
            started.elapsed()
        );
    }
}
