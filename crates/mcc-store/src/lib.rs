//! Storage abstraction for MicroCommandControl.
//!
//! Phase 0: trait sketch + in-memory stub.
//! Phase 1: SQLite implementation + migrations.

use anyhow::Result;
use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Errors from the store layer.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("not found: {0}")]
    NotFound(String),
    #[error("already exists: {0}")]
    AlreadyExists(String),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// Minimal cluster metadata for Phase 0 wiring tests.
#[derive(Debug, Clone, Default)]
pub struct ClusterMeta {
    pub initialized: bool,
}

/// Persistence surface. All server logic depends on this, not on SQLite types.
#[async_trait]
pub trait Store: Send + Sync {
    async fn get_cluster_meta(&self) -> Result<ClusterMeta, StoreError>;
    async fn set_cluster_meta(&self, meta: ClusterMeta) -> Result<(), StoreError>;
}

/// In-memory store used until SQLite lands in Phase 1.
#[derive(Debug, Default)]
pub struct MemoryStore {
    inner: RwLock<ClusterMeta>,
}

impl MemoryStore {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }
}

#[async_trait]
impl Store for MemoryStore {
    async fn get_cluster_meta(&self) -> Result<ClusterMeta, StoreError> {
        Ok(self.inner.read().await.clone())
    }

    async fn set_cluster_meta(&self, meta: ClusterMeta) -> Result<(), StoreError> {
        *self.inner.write().await = meta;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn memory_store_roundtrip() {
        let store = MemoryStore::new();
        let meta = store.get_cluster_meta().await.unwrap();
        assert!(!meta.initialized);

        store
            .set_cluster_meta(ClusterMeta { initialized: true })
            .await
            .unwrap();
        assert!(store.get_cluster_meta().await.unwrap().initialized);
    }
}
