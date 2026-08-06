//! Shared desired-state types for the runtime.

use mcc_api::agent::DesiredInstance;
use mcc_api::ServiceSpec;
use serde::{Deserialize, Serialize};

use crate::sandbox_name;

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

/// Host-side secret injection material (plaintext only on agent).
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
}

/// Map gRPC desired instances into runtime work items.
pub fn desired_from_sync(instances: &[DesiredInstance]) -> anyhow::Result<Vec<DesiredSandbox>> {
    let mut out = Vec::with_capacity(instances.len());
    for d in instances {
        let spec: ServiceSpec = serde_json::from_str(&d.spec_json)
            .map_err(|e| anyhow::anyhow!("parse spec for {}: {e}", d.instance_id))?;
        let runtime_id = sandbox_name(&d.stack, &d.service, d.ordinal);
        let secrets = d
            .secrets
            .iter()
            .map(|s| InjectedSecret {
                env: s.env.clone(),
                value: s.value.clone(),
                allow_hosts: s.allow_hosts.clone(),
            })
            .collect();
        let ssh = d
            .ssh
            .as_ref()
            .map(|s| DesiredSsh {
                enabled: s.enabled,
                bind: if s.bind.is_empty() {
                    "127.0.0.1".into()
                } else {
                    s.bind.clone()
                },
                port: s.port as u16,
                user: if s.user.is_empty() {
                    "root".into()
                } else {
                    s.user.clone()
                },
                sftp: s.sftp,
                authorized_public_keys: s.authorized_public_keys.clone(),
                config_hash: s.config_hash.clone(),
            })
            .unwrap_or_default();
        out.push(DesiredSandbox {
            instance_id: d.instance_id.clone(),
            stack: d.stack.clone(),
            service: d.service.clone(),
            ordinal: d.ordinal,
            runtime_id,
            spec,
            secrets,
            ssh,
        });
    }
    Ok(out)
}

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
    use mcc_api::agent::SecretInjection;

    #[test]
    fn start_parts_default() {
        let spec = ServiceSpec {
            image: "alpine".into(),
            replicas: 1,
            resources: Default::default(),
            ports: vec![],
            network: Default::default(),
            env: Default::default(),
            secrets: vec![],
            volumes: vec![],
            restart_policy: "on-failure".into(),
            health: None,
            labels: Default::default(),
            command: None,
            node_name: None,
            node_selector: Default::default(),
            ssh: None,
        };
        assert_eq!(start_command_parts(&spec), vec!["sleep", "infinity"]);
    }

    #[test]
    fn desired_from_sync_maps_secret_injections() {
        let spec = ServiceSpec {
            image: "alpine:3.20".into(),
            replicas: 1,
            resources: Default::default(),
            ports: vec![],
            network: Default::default(),
            env: Default::default(),
            secrets: vec![],
            volumes: vec![],
            restart_policy: "on-failure".into(),
            health: None,
            labels: Default::default(),
            command: Some(vec!["sleep".into(), "infinity".into()]),
            node_name: None,
            node_selector: Default::default(),
            ssh: None,
        };
        let inst = DesiredInstance {
            instance_id: "i1".into(),
            stack: "smoke-secrets".into(),
            service: "keep".into(),
            ordinal: 0,
            spec_json: serde_json::to_string(&spec).unwrap(),
            secrets: vec![SecretInjection {
                env: "API_TOKEN".into(),
                value: "plaintext-for-agent".into(),
                allow_hosts: vec!["api.example.com".into()],
            }],
            ssh: None,
        };
        let work = desired_from_sync(&[inst]).unwrap();
        assert_eq!(work.len(), 1);
        assert_eq!(work[0].runtime_id, "smoke-secrets-keep-0");
        assert_eq!(work[0].secrets.len(), 1);
        assert_eq!(work[0].secrets[0].env, "API_TOKEN");
        assert_eq!(work[0].secrets[0].value, "plaintext-for-agent");
        assert_eq!(work[0].secrets[0].allow_hosts, vec!["api.example.com"]);
    }
}
