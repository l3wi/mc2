//! Fabric naming and desired-state helpers (D13).

use serde::Serialize;

/// Runtime view of fabric plan for one sandbox.
#[derive(Debug, Clone, Default)]
pub struct DesiredFabric {
    pub exposes: Vec<FabricExposeDesired>,
    pub allows: Vec<FabricAllowDesired>,
}

#[derive(Debug, Clone)]
pub struct FabricExposeDesired {
    pub guest_port: u16,
    pub protocol: String,
}

#[derive(Debug, Clone)]
pub struct FabricAllowDesired {
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

/// Observed fabric state for one instance (reported to the store).
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FabricObserved {
    pub exposes: Vec<FabricExposeStatus>,
    pub edges: Vec<FabricEdgeStatus>,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FabricExposeStatus {
    pub guest_port: u16,
    pub host_port: u16,
    pub phase: String, // Pending | Ready | Failed
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FabricEdgeStatus {
    pub to_service: String,
    pub port: u16,
    pub phase: String, // Pending | Ready | Failed
    pub message: String,
}

/// Guest ports that need narrow Host egress allows at create time.
///
/// Includes all declared allow ports (even if backend is not yet local) so a
/// later co-location does not require sandbox recreate. Cross-node remains
/// failed at the splice layer.
pub fn fabric_host_allow_ports(fabric: &DesiredFabric) -> Vec<u16> {
    let mut ports: Vec<u16> = fabric.allows.iter().map(|a| a.port).collect();
    ports.sort_unstable();
    ports.dedup();
    ports
}

/// Publish ports for `expose` (host ephemeral chosen by caller).
pub fn fabric_expose_guest_ports(fabric: &DesiredFabric) -> Vec<u16> {
    fabric.exposes.iter().map(|e| e.guest_port).collect()
}
