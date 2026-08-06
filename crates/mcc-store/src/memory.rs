//! In-memory store for unit tests.

use crate::{
    verify_token, ClusterCounts, ClusterMeta, InstancePhase, InstanceRecord, NodeHeartbeat,
    NodeJoin, NodeRecord, NodeStatus, StackRecord, Store, StoreError,
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
    by_name: HashMap<String, String>,
    stacks: HashMap<String, StackRecord>,
    instances: HashMap<String, InstanceRecord>,
}

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
        Ok(g.meta
            .as_ref()
            .map(|m| verify_token(token, &m.api_token_hash))
            .unwrap_or(false))
    }

    async fn verify_join_token(&self, token: &str) -> Result<bool, StoreError> {
        let g = self.inner.read().await;
        Ok(g.meta
            .as_ref()
            .map(|m| verify_token(token, &m.join_token_hash))
            .unwrap_or(false))
    }

    async fn cluster_counts(&self) -> Result<ClusterCounts, StoreError> {
        let g = self.inner.read().await;
        Ok(ClusterCounts {
            nodes_total: g.nodes.len() as u32,
            nodes_ready: g
                .nodes
                .values()
                .filter(|n| n.status == NodeStatus::Ready.as_str())
                .count() as u32,
            stacks: g.stacks.len() as u32,
            instances: g.instances.len() as u32,
        })
    }

    async fn upsert_node_join(&self, join: NodeJoin) -> Result<NodeRecord, StoreError> {
        let mut g = self.inner.write().await;
        let now = Utc::now().to_rfc3339();
        if let Some(id) = g.by_name.get(&join.name).cloned() {
            let node = g.nodes.get_mut(&id).expect("consistent");
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
            let stale = match &node.last_heartbeat {
                None => true,
                Some(hb) => chrono::DateTime::parse_from_rfc3339(hb)
                    .map(|ts| ts.with_timezone(&Utc) < cutoff)
                    .unwrap_or(true),
            };
            if stale {
                node.status = NodeStatus::NotReady.as_str().into();
                n += 1;
            }
        }
        Ok(n)
    }

    async fn upsert_stack(
        &self,
        name: &str,
        labels_json: &str,
        raw_yaml: &str,
    ) -> Result<StackRecord, StoreError> {
        let mut g = self.inner.write().await;
        let now = Utc::now().to_rfc3339();
        let rec = if let Some(existing) = g.stacks.get(name) {
            StackRecord {
                name: name.into(),
                labels_json: labels_json.into(),
                raw_yaml: raw_yaml.into(),
                created_at: existing.created_at.clone(),
                updated_at: now,
            }
        } else {
            StackRecord {
                name: name.into(),
                labels_json: labels_json.into(),
                raw_yaml: raw_yaml.into(),
                created_at: now.clone(),
                updated_at: now,
            }
        };
        g.stacks.insert(name.into(), rec.clone());
        Ok(rec)
    }

    async fn list_stacks(&self) -> Result<Vec<StackRecord>, StoreError> {
        let g = self.inner.read().await;
        let mut v: Vec<_> = g.stacks.values().cloned().collect();
        v.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(v)
    }

    async fn get_stack(&self, name: &str) -> Result<Option<StackRecord>, StoreError> {
        Ok(self.inner.read().await.stacks.get(name).cloned())
    }

    async fn reconcile_service_replicas(
        &self,
        stack: &str,
        service: &str,
        replicas: u32,
        spec_json: &str,
    ) -> Result<Vec<InstanceRecord>, StoreError> {
        let mut g = self.inner.write().await;
        let now = Utc::now().to_rfc3339();
        let mut by_ord: HashMap<u32, InstanceRecord> = g
            .instances
            .values()
            .filter(|i| i.stack == stack && i.service == service)
            .cloned()
            .map(|i| (i.ordinal, i))
            .collect();

        // scale down
        let remove: Vec<String> = by_ord
            .values()
            .filter(|i| i.ordinal >= replicas)
            .map(|i| i.id.clone())
            .collect();
        for id in remove {
            g.instances.remove(&id);
            by_ord.retain(|_, i| i.id != id);
        }

        // scale up / refresh spec
        for ord in 0..replicas {
            if let Some(existing) = by_ord.get_mut(&ord) {
                existing.spec_json = spec_json.into();
                existing.updated_at = now.clone();
                g.instances.insert(existing.id.clone(), existing.clone());
            } else {
                let id = Uuid::new_v4().to_string();
                let rec = InstanceRecord {
                    id: id.clone(),
                    stack: stack.into(),
                    service: service.into(),
                    ordinal: ord,
                    node_id: None,
                    phase: InstancePhase::Pending.as_str().into(),
                    runtime_id: None,
                    message: None,
                    spec_json: spec_json.into(),
                    updated_at: now.clone(),
                };
                g.instances.insert(id, rec.clone());
                by_ord.insert(ord, rec);
            }
        }

        let mut out: Vec<_> = by_ord.into_values().collect();
        out.sort_by_key(|i| i.ordinal);
        Ok(out)
    }

    async fn list_instances(&self) -> Result<Vec<InstanceRecord>, StoreError> {
        let g = self.inner.read().await;
        let mut v: Vec<_> = g.instances.values().cloned().collect();
        v.sort_by(|a, b| (&a.stack, &a.service, a.ordinal).cmp(&(&b.stack, &b.service, b.ordinal)));
        Ok(v)
    }

    async fn list_instances_for_node(
        &self,
        node_id: &str,
    ) -> Result<Vec<InstanceRecord>, StoreError> {
        let g = self.inner.read().await;
        Ok(g.instances
            .values()
            .filter(|i| i.node_id.as_deref() == Some(node_id))
            .cloned()
            .collect())
    }

    async fn list_pending_instances(&self) -> Result<Vec<InstanceRecord>, StoreError> {
        let g = self.inner.read().await;
        Ok(g.instances
            .values()
            .filter(|i| i.phase == InstancePhase::Pending.as_str() && i.node_id.is_none())
            .cloned()
            .collect())
    }

    async fn bind_instance_to_node(
        &self,
        instance_id: &str,
        node_id: &str,
    ) -> Result<InstanceRecord, StoreError> {
        let mut g = self.inner.write().await;
        let inst = g
            .instances
            .get_mut(instance_id)
            .ok_or_else(|| StoreError::NotFound(instance_id.into()))?;
        inst.node_id = Some(node_id.into());
        inst.phase = InstancePhase::Scheduled.as_str().into();
        inst.updated_at = Utc::now().to_rfc3339();
        Ok(inst.clone())
    }

    async fn update_instance_status(
        &self,
        instance_id: &str,
        phase: &str,
        runtime_id: Option<&str>,
        message: Option<&str>,
    ) -> Result<InstanceRecord, StoreError> {
        let mut g = self.inner.write().await;
        let inst = g
            .instances
            .get_mut(instance_id)
            .ok_or_else(|| StoreError::NotFound(instance_id.into()))?;
        inst.phase = phase.into();
        if let Some(r) = runtime_id {
            inst.runtime_id = Some(r.into());
        }
        if let Some(m) = message {
            inst.message = Some(m.into());
        }
        inst.updated_at = Utc::now().to_rfc3339();
        Ok(inst.clone())
    }

    async fn get_instance(&self, instance_id: &str) -> Result<Option<InstanceRecord>, StoreError> {
        Ok(self.inner.read().await.instances.get(instance_id).cloned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hash_token;

    #[tokio::test]
    async fn replicas_scale() {
        let store = MemoryStore::new();
        store
            .init_cluster(&hash_token("a"), &hash_token("j"))
            .await
            .unwrap();
        store.upsert_stack("demo", "{}", "yaml").await.unwrap();
        let inst = store
            .reconcile_service_replicas("demo", "web", 2, r#"{"image":"x"}"#)
            .await
            .unwrap();
        assert_eq!(inst.len(), 2);
        let inst = store
            .reconcile_service_replicas("demo", "web", 1, r#"{"image":"x"}"#)
            .await
            .unwrap();
        assert_eq!(inst.len(), 1);
        assert_eq!(inst[0].ordinal, 0);
    }
}
