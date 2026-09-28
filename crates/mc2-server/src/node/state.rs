//! Per-node runtime bookkeeping threaded through each reconcile pass.

use crate::ingress_files::{IngressFileWriter, SelfIngressRoute};
use crate::network_serve::NetworkTable;
use crate::ssh_serve::SshServeTable;
use std::collections::{HashMap, HashSet};
use std::time::Instant;

/// A health probe running in its own task (B6b): the reconcile pass never waits
/// on it, it only reads the finished result.
#[derive(Debug)]
pub(super) struct InFlightProbe {
    /// VM generation the probe was started for. A result whose generation no
    /// longer matches the instance is from a sandbox that has been recreated
    /// and must be discarded.
    pub(super) generation: u64,
    /// Completed probe result (`Ok(exit code)` / probe error or timeout).
    pub(super) result: tokio::sync::oneshot::Receiver<anyhow::Result<i32>>,
}

/// Per-sandbox restart / health bookkeeping on the node.
#[derive(Debug, Default)]
pub(super) struct InstanceRuntimeState {
    pub(super) spec_hash: Option<String>,
    pub(super) running_since: Option<Instant>,
    pub(super) restart_count: u32,
    pub(super) next_restart_ok: Option<Instant>,
    pub(super) last_health: Option<Instant>,
    /// Healthcheck passed at least once since the last (re)create.
    pub(super) health_ok: bool,
    /// Consecutive health-probe failures (reset on success).
    pub(super) health_failures: u32,
    /// Probe currently in flight for this instance (at most one, B6b).
    pub(super) health_probe: Option<InFlightProbe>,
    /// VM generation, bumped on every (re)create; probe results from an older
    /// generation are dropped.
    pub(super) health_generation: u64,
}

impl InstanceRuntimeState {
    /// Start a fresh VM generation: counters reset, probe results from the
    /// previous sandbox are discarded. Called when a sandbox is (re)created, so
    /// a recreated instance gets a clean `start_period`/`retries` slate.
    pub(super) fn reset_health(&mut self) {
        self.health_generation = self.health_generation.wrapping_add(1);
        self.last_health = None;
        self.health_ok = false;
        self.health_failures = 0;
    }
}

/// Collects the errors of one reconcile pass without stopping it.
///
/// A pass keeps working through every instance (one bad secret or one failing
/// store write must not stop the others), but any recorded error makes the pass
/// count as failed so the operator sees it in the metrics and the log (D4).
#[derive(Debug, Default)]
pub(super) struct PassErrors(Option<anyhow::Error>);

impl PassErrors {
    /// Record the first error of the pass; later ones are already logged.
    pub(super) fn record(&mut self, error: anyhow::Error) {
        if self.0.is_none() {
            self.0 = Some(error);
        }
    }

    /// `Err` when any error was recorded this pass.
    pub(super) fn into_result(self) -> anyhow::Result<()> {
        match self.0 {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
}

/// Aggregate liveness of one service within a stack, used to gate `depends_on`.
#[derive(Debug, Clone, Default)]
pub(super) struct ServiceLive {
    pub(super) running: bool,
    pub(super) healthy: bool,
}

/// Mutable per-node state threaded through each reconcile pass.
///
/// Bundles the previously-separate mutable reconcile arguments (owned set,
/// per-sandbox bookkeeping, ssh/network tables, ingress writer) so the
/// reconcile entrypoint stays small.
pub(super) struct NodeRuntimeState {
    /// Runtime ids we own: created by this install, recovered from the
    /// `mc2.install` label at startup (B2). An entry only leaves this set once
    /// its sandbox is confirmed gone.
    pub(super) owned: HashSet<String>,
    /// Set once `owned` has been seeded from the install's sandboxes; the first
    /// pass refuses to do anything else until it succeeds, so ownership is
    /// never guessed from an empty list.
    pub(super) owned_seeded: bool,
    pub(super) rt_state: HashMap<String, InstanceRuntimeState>,
    pub(super) ssh_table: SshServeTable,
    pub(super) network_table: NetworkTable,
    pub(super) ingress_writer: Option<IngressFileWriter>,
    pub(super) self_route: Option<SelfIngressRoute>,
}

impl NodeRuntimeState {
    pub(super) fn new(
        ingress_writer: Option<IngressFileWriter>,
        self_route: Option<SelfIngressRoute>,
    ) -> Self {
        Self {
            owned: HashSet::new(),
            owned_seeded: false,
            rt_state: HashMap::new(),
            ssh_table: SshServeTable::new(),
            network_table: NetworkTable::new(),
            ingress_writer,
            self_route,
        }
    }
}
