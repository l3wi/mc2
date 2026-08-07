//! Fabric naming and desired-state helpers (D13).

use mc2_api::agent::FabricDesired;

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

pub fn fabric_from_proto(f: Option<&FabricDesired>) -> DesiredFabric {
    let Some(f) = f else {
        return DesiredFabric::default();
    };
    DesiredFabric {
        exposes: f
            .exposes
            .iter()
            .map(|e| FabricExposeDesired {
                guest_port: e.guest_port as u16,
                protocol: if e.protocol.is_empty() {
                    "tcp".into()
                } else {
                    e.protocol.clone()
                },
            })
            .collect(),
        allows: f
            .allows
            .iter()
            .map(|a| FabricAllowDesired {
                to_service: a.to_service.clone(),
                port: a.port as u16,
                protocol: if a.protocol.is_empty() {
                    "tcp".into()
                } else {
                    a.protocol.clone()
                },
                fqdn: a.fqdn.clone(),
                short_name: a.short_name.clone(),
                backend_instance_id: a.backend_instance_id.clone(),
                backend_node_id: a.backend_node_id.clone(),
                backend_local: a.backend_local,
                backend_ordinal: a.backend_ordinal,
            })
            .collect(),
    }
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
