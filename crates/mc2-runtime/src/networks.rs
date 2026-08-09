//! Network naming and desired-state helpers (D13).

use serde::Serialize;

/// Runtime view of network plan for one sandbox.
#[derive(Debug, Clone, Default)]
pub struct DesiredNetwork {
    pub exposes: Vec<NetworkExposeDesired>,
    pub allows: Vec<NetworkAllowDesired>,
}

#[derive(Debug, Clone)]
pub struct NetworkExposeDesired {
    pub guest_port: u16,
    pub protocol: String,
}

#[derive(Debug, Clone)]
pub struct NetworkAllowDesired {
    pub to_service: String,
    pub port: u16,
    pub protocol: String,
    pub fqdn: String,
    pub short_name: String,
    pub backend_instance_id: String,
    pub backend_node_id: String,
    pub backend_local: bool,
    pub backend_ordinal: u32,
}

/// Observed network state for one instance (reported to the store).
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NetworkObserved {
    pub exposes: Vec<NetworkExposeStatus>,
    pub edges: Vec<NetworkEdgeStatus>,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NetworkExposeStatus {
    pub guest_port: u16,
    pub host_port: u16,
    pub phase: String, // Pending | Ready | Failed
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NetworkEdgeStatus {
    pub to_service: String,
    pub port: u16,
    pub phase: String, // Pending | Ready | Failed
    pub message: String,
}

/// Guest ports that need narrow Host egress allows at create time.
///
/// Includes all mesh edge ports (even if backend is not yet local) so a later
/// co-location does not require sandbox recreate. Cross-node remains failed at
/// the splice layer.
pub fn network_host_allow_ports(network: &DesiredNetwork) -> Vec<u16> {
    let mut ports: Vec<u16> = network.allows.iter().map(|a| a.port).collect();
    ports.sort_unstable();
    ports.dedup();
    ports
}

/// Publish ports for `expose` (host ephemeral chosen by caller).
pub fn network_expose_guest_ports(network: &DesiredNetwork) -> Vec<u16> {
    network.exposes.iter().map(|e| e.guest_port).collect()
}
