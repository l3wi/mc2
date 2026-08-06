//! Shared API surface for MicroCommandControl.
//!
//! Phase 0: version constants + core object placeholders.
//! Phase 1+: REST DTOs, Phase 2+: prost/tonic generated agent API.

use serde::{Deserialize, Serialize};

/// Stack / API schema version string used in YAML `apiVersion`.
pub const API_VERSION: &str = "mcc/v1";

/// High-level object kinds (YAML `kind` / REST resources).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum Kind {
    Stack,
    Secret,
    Node,
    Ingress,
}

/// Cluster-wide status summary (REST `/v1/status`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ClusterStatus {
    pub version: String,
    pub api_version: String,
    pub nodes_ready: u32,
    pub nodes_total: u32,
    pub stacks: u32,
    pub instances: u32,
    pub message: Option<String>,
}

impl ClusterStatus {
    pub fn bootstrap_stub() -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION").to_string(),
            api_version: API_VERSION.to_string(),
            nodes_ready: 0,
            nodes_total: 0,
            stacks: 0,
            instances: 0,
            message: Some("bootstrap stub".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_version_is_v1() {
        assert_eq!(API_VERSION, "mcc/v1");
    }

    #[test]
    fn status_stub_serializes() {
        let s = ClusterStatus::bootstrap_stub();
        assert_eq!(s.api_version, "mcc/v1");
        assert!(s.message.is_some());
    }
}
