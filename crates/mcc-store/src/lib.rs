//! Storage abstraction for MicroCommandControl.
//!
//! Server logic depends on [`Store`], not on SQLite types.
//! Default backend: SQLite ([`SqliteStore`]). [`MemoryStore`] remains for unit tests.

mod crypto;
mod instance;
mod memory;
mod node;
mod sqlite;
mod token;

pub use crypto::{CryptoError, SecretsKey};
pub use instance::{InstancePhase, InstanceRecord, StackRecord};
pub use memory::MemoryStore;
pub use node::{NodeHeartbeat, NodeJoin, NodeRecord, NodeStatus};
pub use sqlite::SqliteStore;
pub use token::{hash_token, verify_token, TokenKind};

/// Metadata for a secret (never includes plaintext).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SecretMeta {
    pub name: String,
    pub created_at: String,
    pub updated_at: String,
}

/// Ciphertext blob stored at rest.
#[derive(Debug, Clone)]
pub struct SecretBlob {
    pub name: String,
    pub nonce: Vec<u8>,
    pub ciphertext: Vec<u8>,
    pub created_at: String,
    pub updated_at: String,
}

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

    /// Operator REST: if no API token was configured (empty hash), returns true
    /// for any caller (including missing bearer). Otherwise checks the bearer.
    async fn verify_api_token(&self, token: &str) -> Result<bool, StoreError>;

    /// Agent join: if no join token was configured (empty hash), returns true
    /// even when `token` is empty. Otherwise checks the join token.
    async fn verify_join_token(&self, token: &str) -> Result<bool, StoreError>;

    /// True when the cluster requires an operator API bearer token.
    async fn api_auth_required(&self) -> Result<bool, StoreError>;

    /// True when the cluster requires a join token to register agents.
    async fn join_auth_required(&self) -> Result<bool, StoreError>;

    async fn cluster_counts(&self) -> Result<ClusterCounts, StoreError>;

    async fn upsert_node_join(&self, join: NodeJoin) -> Result<NodeRecord, StoreError>;
    async fn heartbeat_node(
        &self,
        node_id: &str,
        node_token: &str,
        hb: NodeHeartbeat,
    ) -> Result<NodeRecord, StoreError>;
    async fn list_nodes(&self) -> Result<Vec<NodeRecord>, StoreError>;
    async fn get_node(&self, node_id: &str) -> Result<Option<NodeRecord>, StoreError>;
    async fn mark_stale_nodes(&self, grace: Duration) -> Result<u32, StoreError>;

    // --- stacks / instances (Phase 3) ---

    async fn upsert_stack(
        &self,
        name: &str,
        labels_json: &str,
        raw_yaml: &str,
    ) -> Result<StackRecord, StoreError>;

    async fn list_stacks(&self) -> Result<Vec<StackRecord>, StoreError>;

    async fn get_stack(&self, name: &str) -> Result<Option<StackRecord>, StoreError>;

    /// Ensure instance rows 0..replicas-1 exist for (stack, service); remove higher ordinals.
    async fn reconcile_service_replicas(
        &self,
        stack: &str,
        service: &str,
        replicas: u32,
        spec_json: &str,
    ) -> Result<Vec<InstanceRecord>, StoreError>;

    async fn list_instances(&self) -> Result<Vec<InstanceRecord>, StoreError>;

    async fn list_instances_for_node(
        &self,
        node_id: &str,
    ) -> Result<Vec<InstanceRecord>, StoreError>;

    async fn list_pending_instances(&self) -> Result<Vec<InstanceRecord>, StoreError>;

    async fn bind_instance_to_node(
        &self,
        instance_id: &str,
        node_id: &str,
    ) -> Result<InstanceRecord, StoreError>;

    async fn update_instance_status(
        &self,
        instance_id: &str,
        phase: &str,
        runtime_id: Option<&str>,
        message: Option<&str>,
    ) -> Result<InstanceRecord, StoreError>;

    async fn get_instance(&self, instance_id: &str) -> Result<Option<InstanceRecord>, StoreError>;

    // --- secrets (Phase 5) ---

    async fn put_secret_blob(
        &self,
        name: &str,
        nonce: &[u8],
        ciphertext: &[u8],
    ) -> Result<SecretMeta, StoreError>;

    async fn get_secret_blob(&self, name: &str) -> Result<Option<SecretBlob>, StoreError>;

    async fn list_secret_meta(&self) -> Result<Vec<SecretMeta>, StoreError>;

    async fn delete_secret(&self, name: &str) -> Result<bool, StoreError>;
}
