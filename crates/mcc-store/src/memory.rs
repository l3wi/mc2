//! In-memory store for unit tests.

use crate::{
    verify_token, ClusterCounts, ClusterMeta, NodeHeartbeat, NodeJoin, NodeRecord, NodeStatus,
    Store, StoreError,
};
use async_trait::async_trait;
use chrono::{Duration as ChronoDuration, Utc};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use uuid::Uuid;

#[derive(Debug, Default)]
struct Inner {
    meta: Option<ClusterMeta>,
    nodes: HashMap<String, NodeRecord>,
    /// name -> id
    by_name: HashMap<String, String>,
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
        let g = self.inner.read().await;
        let nodes_total = g.nodes.len() as u32;
        let nodes_ready = g
            .nodes
            .values()
            .filter(|n| n.status == NodeStatus::Ready.as_str())
            .count() as u32;
        Ok(ClusterCounts {
            nodes_ready,
            nodes_total,
            stacks: 0,
            instances: 0,
        })
    }

    async fn upsert_node_join(&self, join: NodeJoin) -> Result<NodeRecord, StoreError> {
        let mut g = self.inner.write().await;
        let now = Utc::now().to_rfc3339();
        if let Some(id) = g.by_name.get(&join.name).cloned() {
            let node = g.nodes.get_mut(&id).expect("by_name consistent");
            node.labels_json = join.labels_json;
            node.arch = join.arch;
            node.cpus = join.cpus;
            node.memory_mib = join.memory_mib;
            node.node_token_hash = join.node_token_hash;
            node.status = NodeStatus::Ready.as_str().into();
            node.last_heartbeat = Some(now);
            return Ok(node.clone());
        }
        let id = Uuid::new_v4().to_string();
        let rec = NodeRecord {
            id: id.clone(),
            name: join.name.clone(),
            labels_json: join.labels_json,
            arch: join.arch,
            cpus: join.cpus,
            memory_mib: join.memory_mib,
            status: NodeStatus::Ready.as_str().into(),
            last_heartbeat: Some(now.clone()),
            created_at: now,
            node_token_hash: join.node_token_hash,
        };
        g.by_name.insert(join.name, id.clone());
        g.nodes.insert(id, rec.clone());
        Ok(rec)
    }

    async fn heartbeat_node(
        &self,
        node_id: &str,
        node_token: &str,
        hb: NodeHeartbeat,
    ) -> Result<NodeRecord, StoreError> {
        let mut g = self.inner.write().await;
        let node = g
            .nodes
            .get_mut(node_id)
            .ok_or_else(|| StoreError::NotFound(format!("node {node_id}")))?;
        if !verify_token(node_token, &node.node_token_hash) {
            return Err(StoreError::Unauthorized);
        }
        node.cpus = hb.cpus;
        node.memory_mib = hb.memory_mib;
        node.status = hb.status;
        node.last_heartbeat = Some(Utc::now().to_rfc3339());
        Ok(node.clone())
    }

    async fn list_nodes(&self) -> Result<Vec<NodeRecord>, StoreError> {
        let g = self.inner.read().await;
        let mut v: Vec<_> = g.nodes.values().cloned().collect();
        v.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(v)
    }

    async fn get_node(&self, node_id: &str) -> Result<Option<NodeRecord>, StoreError> {
        Ok(self.inner.read().await.nodes.get(node_id).cloned())
    }

    async fn mark_stale_nodes(&self, grace: Duration) -> Result<u32, StoreError> {
        let mut g = self.inner.write().await;
        let cutoff =
            Utc::now() - ChronoDuration::from_std(grace).unwrap_or(ChronoDuration::seconds(30));
        let mut n = 0u32;
        for node in g.nodes.values_mut() {
            if node.status != NodeStatus::Ready.as_str() {
                continue;
            }
            let Some(ref hb) = node.last_heartbeat else {
                node.status = NodeStatus::NotReady.as_str().into();
                n += 1;
                continue;
            };
            if let Ok(ts) = chrono::DateTime::parse_from_rfc3339(hb) {
                if ts.with_timezone(&Utc) < cutoff {
                    node.status = NodeStatus::NotReady.as_str().into();
                    n += 1;
                }
            }
        }
        Ok(n)
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

    #[tokio::test]
    async fn join_heartbeat_and_stale() {
        let store = MemoryStore::new();
        store
            .init_cluster(&hash_token("a"), &hash_token("j"))
            .await
            .unwrap();

        let node = store
            .upsert_node_join(NodeJoin {
                name: "n1".into(),
                labels_json: "{}".into(),
                arch: "aarch64".into(),
                cpus: 4,
                memory_mib: 8192,
                node_token_hash: hash_token("ntok"),
            })
            .await
            .unwrap();

        store
            .heartbeat_node(
                &node.id,
                "ntok",
                NodeHeartbeat {
                    cpus: 4,
                    memory_mib: 8192,
                    status: "Ready".into(),
                },
            )
            .await
            .unwrap();

        assert!(store
            .heartbeat_node(
                &node.id,
                "wrong",
                NodeHeartbeat {
                    cpus: 1,
                    memory_mib: 1,
                    status: "Ready".into(),
                },
            )
            .await
            .is_err());

        // Force stale heartbeat
        {
            let mut g = store.inner.write().await;
            let n = g.nodes.get_mut(&node.id).unwrap();
            n.last_heartbeat = Some((Utc::now() - ChronoDuration::seconds(120)).to_rfc3339());
        }
        let marked = store
            .mark_stale_nodes(Duration::from_secs(30))
            .await
            .unwrap();
        assert_eq!(marked, 1);
        let list = store.list_nodes().await.unwrap();
        assert_eq!(list[0].status, "NotReady");
    }
}
