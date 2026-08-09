//! Shared API surface for MicroCommandControl.
//!
//! REST DTOs and stack YAML types.

use serde::{Deserialize, Serialize};

pub mod stack;
pub use stack::{
    default_network_fqdn, is_loopback_bind, make_ingress_route_id, network_fqdn,
    normalize_ingress_path, parse_stack_yaml, split_command_string, DependsOnSpec, ExposeSpec,
    HealthcheckSpec, IngressPath, IngressRule, IngressSpec, IngressTlsSpec, NetworkSpec, PortSpec,
    SecretRef, ServiceSpec, SshSpec, StackDocument, StackNetworkSpec, VolumeMount, VolumeSpec,
};

/// Stack / API schema version string used in YAML `apiVersion`.
pub const API_VERSION: &str = "mc2/v1";

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
    /// Public hostname advertised by the server (`--public-hostname`), if set.
    #[serde(
        rename = "publicHostname",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub public_hostname: Option<String>,
    /// Resource budget + host/consumption snapshot (`--limit-*`), when reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resources: Option<ResourceStatus>,
}

/// Resource budget + host/consumption snapshot in `/v1/status`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceStatus {
    /// Configured budget (`--limit-*`); `0` = unlimited.
    pub limits: ResourceLimitsView,
    /// Host totals the server runs on.
    pub host: HostResources,
    /// MC2's reserved CPU/RAM from instance specs (bound, not Failed/Stopped).
    pub reserved: ReservedResources,
    /// MC2's measured on-disk usage (data dir + named volumes), MiB.
    pub mc2_disk_used_mib: u64,
}

/// Configured resource budget (`0` = unlimited).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceLimitsView {
    pub cpus: u32,
    pub memory_mib: u64,
    pub disk_mib: u64,
}

/// Host capacity the server observes.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostResources {
    pub cpus: u32,
    pub memory_mib: u64,
    pub disk_total_mib: u64,
    pub disk_free_mib: u64,
}

/// MC2's reserved CPU/RAM from instance specs.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReservedResources {
    pub cpus: u32,
    pub memory_mib: u64,
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
            public_hostname: None,
            resources: None,
        }
    }
}

/// Operator-visible node (no credentials).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeView {
    pub id: String,
    pub name: String,
    pub arch: String,
    pub cpus: u32,
    pub memory_mib: u64,
    pub status: String,
    pub last_heartbeat: Option<String>,
    pub labels: serde_json::Value,
    pub created_at: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_version_is_v1() {
        assert_eq!(API_VERSION, "mc2/v1");
    }

    #[test]
    fn status_stub_serializes() {
        let s = ClusterStatus::bootstrap_stub();
        assert_eq!(s.api_version, "mc2/v1");
        assert!(s.message.is_some());
    }
}
