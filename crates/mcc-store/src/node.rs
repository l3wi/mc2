//! Node records (agents registered with the control plane).

use serde::{Deserialize, Serialize};

/// Lifecycle status for a registered node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum NodeStatus {
    Unknown,
    Ready,
    NotReady,
    Draining,
}

impl NodeStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "Unknown",
            Self::Ready => "Ready",
            Self::NotReady => "NotReady",
            Self::Draining => "Draining",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "Ready" => Self::Ready,
            "NotReady" => Self::NotReady,
            "Draining" => Self::Draining,
            _ => Self::Unknown,
        }
    }
}

/// Persisted node (agent) record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeRecord {
    pub id: String,
    pub name: String,
    pub labels_json: String,
    pub arch: String,
    pub cpus: u32,
    pub memory_mib: u64,
    pub status: String,
    pub last_heartbeat: Option<String>,
    pub created_at: String,
    /// SHA-256 hex of node token; never returned over operator REST.
    #[serde(skip_serializing)]
    pub node_token_hash: String,
}

/// Inputs for join / re-join.
#[derive(Debug, Clone)]
pub struct NodeJoin {
    pub name: String,
    pub labels_json: String,
    pub arch: String,
    pub cpus: u32,
    pub memory_mib: u64,
    pub node_token_hash: String,
}

/// Heartbeat capacity update.
#[derive(Debug, Clone)]
pub struct NodeHeartbeat {
    pub cpus: u32,
    pub memory_mib: u64,
    pub status: String,
}
