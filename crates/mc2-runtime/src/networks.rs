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

/// Network edge/expose phase. Persisted and serialized as a string; use
/// [`Self::as_str`] / [`Self::parse`] at module boundaries instead of literals.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkPhase {
    Pending,
    Ready,
    Failed,
    Mixed,
}

impl NetworkPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "Pending",
            Self::Ready => "Ready",
            Self::Failed => "Failed",
            Self::Mixed => "Mixed",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "Ready" => Self::Ready,
            "Failed" => Self::Failed,
            "Mixed" => Self::Mixed,
            _ => Self::Pending,
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phase_parse_as_str_roundtrip() {
        for s in ["Pending", "Ready", "Failed", "Mixed"] {
            let p = NetworkPhase::parse(s);
            assert_eq!(p.as_str(), s, "roundtrip {s}");
        }
        // Unknown → Pending (fail-open default).
        assert_eq!(NetworkPhase::parse("bogus"), NetworkPhase::Pending);
        assert_eq!(NetworkPhase::parse("Ready"), NetworkPhase::Ready);
    }
}
