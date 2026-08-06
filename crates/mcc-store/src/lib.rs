//! Storage abstraction for MicroCommandControl.
//!
//! Server logic depends on [`Store`], not on SQLite types.
//! Default backend: SQLite ([`SqliteStore`]). [`MemoryStore`] remains for unit tests.

mod memory;
mod node;
mod sqlite;
mod token;

pub use memory::MemoryStore;
pub use node::{NodeHeartbeat, NodeJoin, NodeRecord, NodeStatus};
pub use sqlite::SqliteStore;
pub use token::{hash_token, verify_token, TokenKind};

use anyhow::Result;
use async_trait::async_trait;
use std::time::Duration;

/// Errors from the store layer.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("not found: {0}")]
    NotFound(String),
    #[error("already exists: {0}")]
    AlreadyExists(String),
    #[error("not initialized")]
    NotInitialized,
    #[error("unauthorized")]
    Unauthorized,
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// Persisted cluster bootstrap / meta.
#[derive(Debug, Clone)]
pub struct ClusterMeta {
    pub initialized: bool,
    pub api_token_hash: String,
    pub join_token_hash: String,
    pub created_at: String,
}

/// Lightweight counts for `/v1/status`.
#[derive(Debug, Clone, Default)]
pub struct ClusterCounts {
    pub nodes_ready: u32,
    pub nodes_total: u32,
    pub stacks: u32,
    pub instances: u32,
}

/// Persistence surface used by the control plane.
#[async_trait]
pub trait Store: Send + Sync {
    async fn get_cluster_meta(&self) -> Result<Option<ClusterMeta>, StoreError>;

    async fn init_cluster(
        &self,
        api_token_hash: &str,
        join_token_hash: &str,
    ) -> Result<ClusterMeta, StoreError>;

    /// Constant-time check of operator API bearer token.
    async fn verify_api_token(&self, token: &str) -> Result<bool, StoreError>;

    /// Constant-time check of agent join token.
    async fn verify_join_token(&self, token: &str) -> Result<bool, StoreError>;

    async fn cluster_counts(&self) -> Result<ClusterCounts, StoreError>;

    /// Register or re-join a node by name. Returns the node record (with token hash).
    async fn upsert_node_join(&self, join: NodeJoin) -> Result<NodeRecord, StoreError>;

    /// Apply heartbeat if `node_token` matches stored hash.
    async fn heartbeat_node(
        &self,
        node_id: &str,
        node_token: &str,
        hb: NodeHeartbeat,
    ) -> Result<NodeRecord, StoreError>;

    async fn list_nodes(&self) -> Result<Vec<NodeRecord>, StoreError>;

    async fn get_node(&self, node_id: &str) -> Result<Option<NodeRecord>, StoreError>;

    /// Mark Ready nodes NotReady when last_heartbeat is older than `grace`.
    async fn mark_stale_nodes(&self, grace: Duration) -> Result<u32, StoreError>;
}
