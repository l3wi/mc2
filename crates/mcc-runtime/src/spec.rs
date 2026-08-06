//! Shared desired-state types for the runtime.

use mcc_api::agent::DesiredInstance;
use mcc_api::ServiceSpec;
use serde::{Deserialize, Serialize};

use crate::sandbox_name;

/// Which backend the agent should use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum RuntimeKind {
    /// Prefer `msb` CLI if available, else mock.
    #[default]
    Auto,
    /// Always mock (CI / no hypervisor).
    Mock,
    /// Real microVMs via `msb` CLI + Sandboxfile project.
    MsbCli,
}

impl RuntimeKind {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "auto" => Some(Self::Auto),
            "mock" => Some(Self::Mock),
            "msb" | "msb-cli" | "cli" => Some(Self::MsbCli),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Mock => "mock",
            Self::MsbCli => "msb-cli",
        }
    }
}

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

    pub fn from_msb_status(s: &str) -> Self {
        match s.to_ascii_uppercase().as_str() {
            "RUNNING" => Self::Running,
            "STOPPED" | "STOPPING" => Self::Stopped,
            "FAILED" | "CRASHED" | "ERROR" => Self::Failed,
            "CREATING" | "STARTING" => Self::Creating,
            _ => Self::Unknown,
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

/// Map MCC network profiles to msb `network.scope`.
pub fn msb_network_scope(profiles: &[String]) -> &'static str {
    if profiles.iter().any(|p| p == "none") {
        return "none";
    }
    if profiles.iter().any(|p| p == "host" || p == "any") {
        return "any";
    }
    if profiles.iter().any(|p| p == "private" || p == "local") {
        return "local";
    }
    // default / public
    "public"
}

/// Build the guest start command line for Sandboxfile `scripts.start`.
pub fn start_command(spec: &ServiceSpec) -> String {
    if let Some(ref cmd) = spec.command {
        if !cmd.is_empty() {
            return shell_join(cmd);
        }
    }
    // Keep the VM alive if no command was provided.
    "sleep infinity".into()
}

fn shell_join(parts: &[String]) -> String {
    parts
        .iter()
        .map(|p| {
            if p.chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_./:@=".contains(c))
            {
                p.clone()
            } else {
                format!("'{}'", p.replace('\'', "'\\''"))
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_mapping() {
        assert_eq!(msb_network_scope(&["public".into()]), "public");
        assert_eq!(msb_network_scope(&["private".into()]), "local");
        assert_eq!(msb_network_scope(&[]), "public");
    }
}
