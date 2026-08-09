//! Shared desired-state and report types for the runtime.

use mc2_api::ServiceSpec;
use serde::{Deserialize, Serialize};

use crate::networks::DesiredNetwork;

/// Phase reported to the control plane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum SandboxPhase {
    Pending,
    Creating,
    Running,
    Failed,
    Stopped,
    Unknown,
}

impl SandboxPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "Pending",
            Self::Creating => "Creating",
            Self::Running => "Running",
            Self::Failed => "Failed",
            Self::Stopped => "Stopped",
            Self::Unknown => "Unknown",
        }
    }
}

/// Host-side secret injection material (plaintext only on the node).
#[derive(Debug, Clone)]
pub struct InjectedSecret {
    pub env: String,
    pub value: String,
    pub allow_hosts: Vec<String>,
}

/// Host-side SSH serve desired state (from control plane).
#[derive(Debug, Clone, Default)]
pub struct DesiredSsh {
    pub enabled: bool,
    pub bind: String,
    pub port: u16,
    pub user: String,
    pub sftp: bool,
    pub authorized_public_keys: Vec<String>,
    pub config_hash: String,
}

/// Desired sandbox derived from a control-plane instance assignment.
#[derive(Debug, Clone)]
pub struct DesiredSandbox {
    pub instance_id: String,
    pub stack: String,
    pub service: String,
    pub ordinal: u32,
    pub runtime_id: String,
    pub spec: ServiceSpec,
    pub secrets: Vec<InjectedSecret>,
    pub ssh: DesiredSsh,
    pub network: DesiredNetwork,
}

/// Observed SSH serve state for one instance (reported to the store).
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SshObserved {
    pub phase: String, // Closed | Opening | Open | Failed
    pub bind: String,
    pub port: u16,
    pub message: String,
}

/// One instance's reconcile report (phase + observed ssh/network).
#[derive(Debug, Clone)]
pub struct InstanceReport {
    pub instance_id: String,
    pub phase: String, // Pending | Scheduled | Creating | Running | Failed | Stopped
    pub message: String,
    pub runtime_id: String,
    pub ssh: Option<SshObserved>,
    pub network: Option<NetworkObservedReport>,
}

/// Alias: network observed snapshot carried in an [`InstanceReport`].
pub type NetworkObservedReport = crate::networks::NetworkObserved;

/// Guest command argv for the SDK `background_command` (detached run).
pub fn start_command_parts(spec: &ServiceSpec) -> Vec<String> {
    if let Some(ref cmd) = spec.command {
        if !cmd.is_empty() {
            return cmd.clone();
        }
    }
    // Keep the VM alive if no command was provided.
    vec!["sleep".into(), "infinity".into()]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sandbox_name;
    use std::collections::BTreeMap;

    #[test]
    fn start_parts_default() {
        let spec = ServiceSpec {
            image: "alpine".into(),
            scale: 1,
            cpus: 1.0,
            mem_limit_mib: 512,
            ports: vec![],
            network: Default::default(),
            env: Default::default(),
            secrets: vec![],
            volumes: vec![],
            restart: "on-failure".into(),
            healthcheck: None,
            labels: Default::default(),
            command: None,
            node_name: None,
            node_selector: Default::default(),
            ssh: None,
            expose: vec![],
            networks: vec![],
            depends_on: BTreeMap::new(),
        };
        assert_eq!(start_command_parts(&spec), vec!["sleep", "infinity"]);
    }

    fn build_work_item() -> DesiredSandbox {
        let spec = ServiceSpec {
            image: "alpine:3.20".into(),
            scale: 1,
            cpus: 1.0,
            mem_limit_mib: 512,
            ports: vec![],
            network: Default::default(),
            env: Default::default(),
            secrets: vec![],
            volumes: vec![],
            restart: "on-failure".into(),
            healthcheck: None,
            labels: Default::default(),
            command: Some(vec!["sleep".into(), "infinity".into()]),
            node_name: None,
            node_selector: Default::default(),
            ssh: None,
            expose: vec![],
            networks: vec![],
            depends_on: BTreeMap::new(),
        };
        DesiredSandbox {
            instance_id: "i1".into(),
            stack: "demo".into(),
            service: "web".into(),
            ordinal: 0,
            runtime_id: sandbox_name("demo", "web", 0),
            spec,
            secrets: vec![InjectedSecret {
                env: "API_TOKEN".into(),
                value: "plaintext-for-agent".into(),
                allow_hosts: vec!["api.example.com".into()],
            }],
            ssh: DesiredSsh::default(),
            network: DesiredNetwork::default(),
        }
    }

    #[test]
    fn work_item_carries_secrets_and_runtime_id() {
        let work = build_work_item();
        assert_eq!(work.runtime_id, "demo-web-0");
        assert_eq!(work.secrets.len(), 1);
        assert_eq!(work.secrets[0].env, "API_TOKEN");
        assert_eq!(work.secrets[0].value, "plaintext-for-agent");
        assert_eq!(work.secrets[0].allow_hosts, vec!["api.example.com"]);
    }

    #[test]
    fn volume_mount_plan_maps_spec_order() {
        let mut work = build_work_item();
        work.stack = "demo".into();
        work.spec.volumes = vec![
            mc2_api::VolumeMount {
                name: "data".into(),
                mount: "/data".into(),
            },
            mc2_api::VolumeMount {
                name: "cache".into(),
                mount: "/var/cache".into(),
            },
        ];
        let plan = crate::volume_mount_plan(&work);
        assert_eq!(
            plan,
            vec![
                ("/data".to_string(), "mc2-demo--data".to_string()),
                ("/var/cache".to_string(), "mc2-demo--cache".to_string()),
            ]
        );
    }
}
