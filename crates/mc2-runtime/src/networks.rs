//! Network naming and desired-state helpers for the host-mediated service
//! network (L4 splice + network-scoped DNS).

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

/// One east–west reachability edge in a sandbox's create-time plan.
///
/// `to_service` is the **owner** of the exposed port (the service that declares
/// it), which may be the instance's own service (replicas of one service reach
/// each other) or a peer on a shared network. Exposed ports are exclusive
/// server-wide, so the port alone identifies the shared splice that serves it.
/// Backend identity/placement is deliberately not part of the plan: it churns
/// without changing the create-time policy (see `desired_recreate_hash`).
#[derive(Debug, Clone)]
pub struct NetworkAllowDesired {
    pub to_service: String,
    pub port: u16,
    pub protocol: String,
    pub fqdn: String,
    pub short_name: String,
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
/// Sorted and de-duplicated: the create-time policy is a set of ports (the
/// guest's `gateway:P` maps to host `127.0.0.1:P`, so the port — not the peer
/// name — is the identity).
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
