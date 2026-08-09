//! Per-node runtime bookkeeping threaded through each reconcile pass.

use crate::ingress_files::{IngressFileWriter, SelfIngressRoute};
use crate::network_serve::NetworkTable;
use crate::ssh_serve::SshServeTable;
use std::collections::{HashMap, HashSet};
use std::time::Instant;

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
    /// Runtime ids we created so we can GC on scale-down.
    pub(super) owned: HashSet<String>,
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
            rt_state: HashMap::new(),
            ssh_table: SshServeTable::new(),
            network_table: NetworkTable::new(),
            ingress_writer,
            self_route,
        }
    }
}
