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

/// Desired sandbox derived from a control-plane instance assignment.
#[derive(Debug, Clone)]
pub struct DesiredSandbox {
    pub instance_id: String,
    pub stack: String,
    pub service: String,
    pub ordinal: u32,
    pub runtime_id: String,
    pub spec: ServiceSpec,
}

/// Map gRPC desired instances into runtime work items.
pub fn desired_from_sync(instances: &[DesiredInstance]) -> anyhow::Result<Vec<DesiredSandbox>> {
    let mut out = Vec::with_capacity(instances.len());
    for d in instances {
        let spec: ServiceSpec = serde_json::from_str(&d.spec_json)
            .map_err(|e| anyhow::anyhow!("parse spec for {}: {e}", d.instance_id))?;
        let runtime_id = sandbox_name(&d.stack, &d.service, d.ordinal);
        out.push(DesiredSandbox {
            instance_id: d.instance_id.clone(),
            stack: d.stack.clone(),
            service: d.service.clone(),
            ordinal: d.ordinal,
            runtime_id,
            spec,
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
        };
        assert_eq!(start_command_parts(&spec), vec!["sleep", "infinity"]);
    }
}
