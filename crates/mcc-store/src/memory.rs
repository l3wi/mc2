//! In-memory store for unit tests.

use crate::{verify_token, ClusterCounts, ClusterMeta, Store, StoreError};
use async_trait::async_trait;
use chrono::Utc;
use std::sync::Arc;
use tokio::sync::RwLock;

#[derive(Debug, Default)]
struct Inner {
    meta: Option<ClusterMeta>,
}

/// In-memory [`Store`] used in tests and early scaffolding.
#[derive(Debug, Default)]
pub struct MemoryStore {
    inner: RwLock<Inner>,
}

impl MemoryStore {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }
}

#[async_trait]
impl Store for MemoryStore {
    async fn get_cluster_meta(&self) -> Result<Option<ClusterMeta>, StoreError> {
        Ok(self.inner.read().await.meta.clone())
    }

    async fn init_cluster(
        &self,
        api_token_hash: &str,
        join_token_hash: &str,
    ) -> Result<ClusterMeta, StoreError> {
        let mut g = self.inner.write().await;
        if g.meta.is_some() {
            return Err(StoreError::AlreadyExists("cluster".into()));
        }
        let meta = ClusterMeta {
            initialized: true,
            api_token_hash: api_token_hash.to_string(),
            join_token_hash: join_token_hash.to_string(),
            created_at: Utc::now().to_rfc3339(),
        };
        g.meta = Some(meta.clone());
        Ok(meta)
    }

    async fn verify_api_token(&self, token: &str) -> Result<bool, StoreError> {
        let g = self.inner.read().await;
        let Some(meta) = &g.meta else {
            return Ok(false);
        };
        Ok(verify_token(token, &meta.api_token_hash))
    }

    async fn verify_join_token(&self, token: &str) -> Result<bool, StoreError> {
        let g = self.inner.read().await;
        let Some(meta) = &g.meta else {
            return Ok(false);
        };
        Ok(verify_token(token, &meta.join_token_hash))
    }

    async fn cluster_counts(&self) -> Result<ClusterCounts, StoreError> {
        Ok(ClusterCounts::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hash_token;

    #[tokio::test]
    async fn init_and_verify() {
        let store = MemoryStore::new();
        assert!(store.get_cluster_meta().await.unwrap().is_none());

        store
            .init_cluster(&hash_token("api-secret"), &hash_token("join-secret"))
            .await
            .unwrap();

        assert!(store.verify_api_token("api-secret").await.unwrap());
        assert!(!store.verify_api_token("nope").await.unwrap());
        assert!(store.verify_join_token("join-secret").await.unwrap());
    }
}
