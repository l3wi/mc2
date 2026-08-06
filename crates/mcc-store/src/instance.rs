//! Stack / service / instance records.

use serde::{Deserialize, Serialize};

/// Instance lifecycle phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum InstancePhase {
    Pending,
    Scheduled,
    Creating,
    Running,
    Failed,
    Stopped,
}

impl InstancePhase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "Pending",
            Self::Scheduled => "Scheduled",
            Self::Creating => "Creating",
            Self::Running => "Running",
            Self::Failed => "Failed",
            Self::Stopped => "Stopped",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "Scheduled" => Self::Scheduled,
            "Creating" => Self::Creating,
            "Running" => Self::Running,
            "Failed" => Self::Failed,
            "Stopped" => Self::Stopped,
            _ => Self::Pending,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StackRecord {
    pub name: String,
    pub labels_json: String,
    pub raw_yaml: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstanceRecord {
    pub id: String,
    pub stack: String,
    pub service: String,
    pub ordinal: u32,
    pub node_id: Option<String>,
    pub phase: String,
    pub runtime_id: Option<String>,
    pub message: Option<String>,
    pub spec_json: String,
    pub updated_at: String,
}
