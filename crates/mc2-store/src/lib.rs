//! Storage abstraction for MicroCommandControl.
//!
//! Server logic depends on [`Store`], not on SQLite types.
//! Default backend: SQLite ([`SqliteStore`]). [`MemoryStore`] remains for unit tests.

mod crypto;
mod instance;
mod memory;
mod node;
mod sqlite;
mod ssh;
mod token;

pub use crypto::{CryptoError, SecretsKey};
pub use instance::{InstancePhase, InstanceRecord, StackRecord};
pub use memory::MemoryStore;
pub use node::{NodeHeartbeat, NodeJoin, NodeRecord, NodeStatus};
pub use sqlite::SqliteStore;
pub use ssh::{
    ssh_fingerprint, validate_public_key, InstanceNetworkRecord, InstanceSshRecord,
    SshAuthorizedKey,
};
pub use token::{hash_token, verify_token};

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

/// Settings key holding the persisted local-node id.
///
/// The embedded node is a singleton: its id is generated once and reused for
/// every later start, so `--node-name` / a hostname change only renames the
/// node and never strands placements (B8).
pub const SETTING_LOCAL_NODE_ID: &str = "local_node_id";

/// Settings key holding this install's random id.
///
/// Every sandbox is labelled with it (`mc2.install`), so the node can recover
/// ownership of its VMs after a restart and never touch a foreign workload (B2).
pub const SETTING_INSTALL_ID: &str = "install_id";

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
    /// User-supplied input rejected (e.g. an invalid SSH public key).
    #[error("{0}")]
    InvalidArgument(String),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// Persisted cluster bootstrap / meta.
#[derive(Debug, Clone)]
pub struct ClusterMeta {
    pub initialized: bool,
    pub api_token_hash: String,
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

/// The complete desired instance set for one stack, produced by
/// `mc2_server::apply::plan_stack` and written atomically by
/// [`Store::commit_stack_plan`].
///
/// The plan is the *whole* desired state, not a delta: after a commit the
/// stack owns exactly `instances` (plus the stack row). Any stored instance of
/// the stack whose `(service, ordinal)` is not planned — a service dropped from
/// the YAML, or an ordinal past a scale-down — is deleted by the commit, so
/// removals need no special case in the plan.
#[derive(Debug, Clone)]
pub struct StackPlan {
    pub stack: String,
    pub labels_json: String,
    pub raw_yaml: String,
    /// Desired instances, ordered by `(service, ordinal)`.
    pub instances: Vec<PlannedInstance>,
}

/// One desired instance in a [`StackPlan`]. Whether it is created or updated is
/// decided by the commit: an existing row for `(stack, service, ordinal)` keeps
/// its identity, placement and observed state and only gets the new spec.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedInstance {
    pub service: String,
    pub ordinal: u32,
    pub spec_json: String,
}

impl StackPlan {
    /// A plan for `stack` built from per-service replica specs
    /// (`(service, spec_json per ordinal)`).
    ///
    /// Replacing a stack that already owns instances also removes the ones the
    /// plan does not mention — see [`Store::commit_stack_plan`].
    pub fn replicas<'a>(
        stack: &str,
        labels_json: &str,
        raw_yaml: &str,
        services: impl IntoIterator<Item = (&'a str, Vec<String>)>,
    ) -> Self {
        let mut instances = Vec::new();
        for (service, specs) in services {
            for (ordinal, spec_json) in specs.into_iter().enumerate() {
                instances.push(PlannedInstance {
                    service: service.to_string(),
                    ordinal: ordinal as u32,
                    spec_json,
                });
            }
        }
        Self {
            stack: stack.to_string(),
            labels_json: labels_json.to_string(),
            raw_yaml: raw_yaml.to_string(),
            instances,
        }
    }
}

/// Persistence surface used by the control plane.
#[async_trait]
pub trait Store: Send + Sync {
    async fn get_cluster_meta(&self) -> Result<Option<ClusterMeta>, StoreError>;

    async fn init_cluster(&self, api_token_hash: &str) -> Result<ClusterMeta, StoreError>;

    /// Operator REST: verify a plaintext bearer token against the stored hash.
    ///
    /// Returns false when the cluster is uninitialized or the token is empty;
    /// an unauthenticated start is a server-process decision (`--no-auth`), not
    /// a store one.
    async fn verify_api_token(&self, token: &str) -> Result<bool, StoreError>;

    /// Replace the stored operator API token hash (`mc2 server token rotate`).
    async fn replace_api_token_hash(&self, api_token_hash: &str) -> Result<(), StoreError>;

    async fn cluster_counts(&self) -> Result<ClusterCounts, StoreError>;

    /// Server-level setting value (e.g. `public_hostname`), if set.
    async fn get_setting(&self, key: &str) -> Result<Option<String>, StoreError>;

    /// Upsert a server-level setting value.
    async fn set_setting(&self, key: &str, value: &str) -> Result<(), StoreError>;

    /// Register/refresh **the** local node (singleton).
    ///
    /// The id is generated once and persisted under
    /// [`SETTING_LOCAL_NODE_ID`], so a later start — including one with a
    /// different `--node-name` or hostname — reuses the same id and keeps every
    /// placement. Only the display name and capacity fields change.
    async fn upsert_local_node(&self, join: NodeJoin) -> Result<NodeRecord, StoreError>;
    async fn touch_node(&self, node_id: &str, hb: NodeHeartbeat) -> Result<NodeRecord, StoreError>;
    async fn list_nodes(&self) -> Result<Vec<NodeRecord>, StoreError>;
    async fn get_node(&self, node_id: &str) -> Result<Option<NodeRecord>, StoreError>;

    // --- stacks / instances (Phase 3) ---

    async fn list_stacks(&self) -> Result<Vec<StackRecord>, StoreError>;

    async fn get_stack(&self, name: &str) -> Result<Option<StackRecord>, StoreError>;

    /// Publish `plan` as this stack's desired state, **all or nothing**.
    ///
    /// One transaction: the stack row (labels, `raw_yaml`, timestamps) plus every
    /// instance create/update/delete. A failure at any point leaves the previous
    /// stack and instance rows exactly as they were.
    ///
    /// Instances are matched by `(stack, service, ordinal)`: an existing row is
    /// updated **in place** (only `spec_json` and `updated_at` change — node
    /// binding, runtime id, observed phase/message, health and the applied
    /// config hash are preserved so the node decides on a recreate); anything
    /// else the stack owns is deleted (ssh/network rows cascade).
    ///
    /// Returns the stack's instances ordered by `(service, ordinal)`.
    async fn commit_stack_plan(&self, plan: &StackPlan) -> Result<Vec<InstanceRecord>, StoreError>;

    /// Delete a stack and its instances (cascades ssh/network rows) in one
    /// transaction. Returns false when the stack did not exist.
    async fn delete_stack(&self, name: &str) -> Result<bool, StoreError>;

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

    /// Persist the create-time config hash the running sandbox was confirmed
    /// with (B3). `None` clears it (no confirmed sandbox). Missing instance is
    /// [`StoreError::NotFound`].
    async fn set_instance_applied_hash(
        &self,
        instance_id: &str,
        applied_hash: Option<&str>,
    ) -> Result<(), StoreError>;

    /// The persisted applied config hash for an instance, if any.
    ///
    /// `None` also covers a missing instance: the caller treats an absent row as
    /// "nothing adopted" either way.
    async fn get_instance_applied_hash(
        &self,
        instance_id: &str,
    ) -> Result<Option<String>, StoreError>;

    async fn get_instance(&self, instance_id: &str) -> Result<Option<InstanceRecord>, StoreError>;

    /// Persist the healthcheck-passed signal for an instance (depends_on: service_healthy).
    async fn update_instance_health(
        &self,
        instance_id: &str,
        healthy: bool,
    ) -> Result<InstanceRecord, StoreError>;

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

    // --- SSH authorized keys + instance endpoints ---

    async fn put_ssh_key(
        &self,
        name: &str,
        public_key: &str,
    ) -> Result<SshAuthorizedKey, StoreError>;

    async fn get_ssh_key(&self, name: &str) -> Result<Option<SshAuthorizedKey>, StoreError>;

    async fn list_ssh_keys(&self) -> Result<Vec<SshAuthorizedKey>, StoreError>;

    /// Delete key. Returns false if missing.
    async fn delete_ssh_key(&self, name: &str) -> Result<bool, StoreError>;

    async fn get_instance_ssh(
        &self,
        instance_id: &str,
    ) -> Result<Option<InstanceSshRecord>, StoreError>;

    /// Upsert full desired SSH override for an instance (API PUT).
    async fn put_instance_ssh_desired(
        &self,
        rec: &InstanceSshRecord,
    ) -> Result<InstanceSshRecord, StoreError>;

    /// Clear API override (desired falls back to YAML). Observed phase may be closed by the runtime.
    async fn clear_instance_ssh_override(
        &self,
        instance_id: &str,
    ) -> Result<Option<InstanceSshRecord>, StoreError>;

    /// Reconcile report: update observed bind/port/phase.
    async fn update_instance_ssh_observed(
        &self,
        instance_id: &str,
        phase: &str,
        bind: Option<&str>,
        port: Option<u16>,
        message: Option<&str>,
    ) -> Result<InstanceSshRecord, StoreError>;

    async fn list_instance_ssh(&self) -> Result<Vec<InstanceSshRecord>, StoreError>;

    /// Reconcile report: network observed snapshot (JSON).
    async fn update_instance_network_observed(
        &self,
        instance_id: &str,
        phase: &str,
        observed_json: &str,
        message: Option<&str>,
    ) -> Result<InstanceNetworkRecord, StoreError>;

    async fn get_instance_network(
        &self,
        instance_id: &str,
    ) -> Result<Option<InstanceNetworkRecord>, StoreError>;
}
