//! In-memory store for unit tests.

use crate::{
    ssh_fingerprint, validate_public_key, verify_token, ClusterCounts, ClusterMeta,
    InstanceNetworkRecord, InstancePhase, InstanceRecord, InstanceSshRecord, NodeHeartbeat,
    NodeJoin, NodeRecord, NodeStatus, SecretBlob, SecretMeta, SshAuthorizedKey, StackRecord, Store,
    StoreError,
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
    secrets: HashMap<String, SecretBlob>,
    ssh_keys: HashMap<String, SshAuthorizedKey>,
    instance_ssh: HashMap<String, InstanceSshRecord>,
    instance_network: HashMap<String, InstanceNetworkRecord>,
    settings: HashMap<String, String>,
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

    async fn init_cluster(&self, api_token_hash: &str) -> Result<ClusterMeta, StoreError> {
        let mut g = self.inner.write().await;
        if g.meta.is_some() {
            return Err(StoreError::AlreadyExists("cluster".into()));
        }
        let meta = ClusterMeta {
            initialized: true,
            api_token_hash: api_token_hash.to_string(),
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
        if meta.api_token_hash.is_empty() {
            return Ok(true);
        }
        if token.is_empty() {
            return Ok(false);
        }
        Ok(verify_token(token, &meta.api_token_hash))
    }

    async fn api_auth_required(&self) -> Result<bool, StoreError> {
        Ok(self
            .inner
            .read()
            .await
            .meta
            .as_ref()
            .map(|m| !m.api_token_hash.is_empty())
            .unwrap_or(true))
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

    async fn get_setting(&self, key: &str) -> Result<Option<String>, StoreError> {
        Ok(self.inner.read().await.settings.get(key).cloned())
    }

    async fn set_setting(&self, key: &str, value: &str) -> Result<(), StoreError> {
        self.inner
            .write()
            .await
            .settings
            .insert(key.to_string(), value.to_string());
        Ok(())
    }

    async fn upsert_local_node(&self, join: NodeJoin) -> Result<NodeRecord, StoreError> {
        let mut g = self.inner.write().await;
        let now = Utc::now().to_rfc3339();
        if let Some(id) = g.by_name.get(&join.name).cloned() {
            let node = g.nodes.get_mut(&id).expect("consistent");
            node.labels_json = join.labels_json;
            node.arch = join.arch;
            node.cpus = join.cpus;
            node.memory_mib = join.memory_mib;
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
        };
        g.by_name.insert(join.name, id.clone());
        g.nodes.insert(id, rec.clone());
        Ok(rec)
    }

    async fn touch_node(&self, node_id: &str, hb: NodeHeartbeat) -> Result<NodeRecord, StoreError> {
        let mut g = self.inner.write().await;
        let node = g
            .nodes
            .get_mut(node_id)
            .ok_or_else(|| StoreError::NotFound(format!("node {node_id}")))?;
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

    async fn delete_stack(&self, name: &str) -> Result<bool, StoreError> {
        let mut g = self.inner.write().await;
        let existed = g.stacks.remove(name).is_some();
        let removed_ids: Vec<String> = g
            .instances
            .values()
            .filter(|i| i.stack == name)
            .map(|i| i.id.clone())
            .collect();
        for id in &removed_ids {
            g.instances.remove(id);
            g.instance_ssh.remove(id);
            g.instance_network.remove(id);
        }
        Ok(existed)
    }

    async fn reconcile_service_replicas(
        &self,
        stack: &str,
        service: &str,
        replicas: u32,
        spec_json: &str,
    ) -> Result<Vec<InstanceRecord>, StoreError> {
        let specs: Vec<String> = (0..replicas).map(|_| spec_json.to_string()).collect();
        self.reconcile_service_replicas_multi(stack, service, &specs)
            .await
    }

    async fn reconcile_service_replicas_multi(
        &self,
        stack: &str,
        service: &str,
        spec_jsons: &[String],
    ) -> Result<Vec<InstanceRecord>, StoreError> {
        let replicas = spec_jsons.len() as u32;
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
        for (ord, spec_json) in spec_jsons.iter().enumerate() {
            let ord = ord as u32;
            if let Some(existing) = by_ord.get_mut(&ord) {
                existing.spec_json = spec_json.clone();
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
                    spec_json: spec_json.clone(),
                    healthy: false,
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

    async fn unbind_instance(&self, instance_id: &str) -> Result<InstanceRecord, StoreError> {
        let mut g = self.inner.write().await;
        let inst = g
            .instances
            .get_mut(instance_id)
            .ok_or_else(|| StoreError::NotFound(instance_id.into()))?;
        inst.node_id = None;
        inst.phase = InstancePhase::Pending.as_str().into();
        inst.runtime_id = None;
        inst.message = None;
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

    async fn update_instance_health(
        &self,
        instance_id: &str,
        healthy: bool,
    ) -> Result<InstanceRecord, StoreError> {
        let mut g = self.inner.write().await;
        let inst = g
            .instances
            .get_mut(instance_id)
            .ok_or_else(|| StoreError::NotFound(instance_id.into()))?;
        inst.healthy = healthy;
        inst.updated_at = Utc::now().to_rfc3339();
        Ok(inst.clone())
    }

    async fn put_secret_blob(
        &self,
        name: &str,
        nonce: &[u8],
        ciphertext: &[u8],
    ) -> Result<SecretMeta, StoreError> {
        let mut g = self.inner.write().await;
        let now = Utc::now().to_rfc3339();
        let created = g
            .secrets
            .get(name)
            .map(|s| s.created_at.clone())
            .unwrap_or_else(|| now.clone());
        g.secrets.insert(
            name.into(),
            SecretBlob {
                name: name.into(),
                nonce: nonce.to_vec(),
                ciphertext: ciphertext.to_vec(),
                created_at: created.clone(),
                updated_at: now.clone(),
            },
        );
        Ok(SecretMeta {
            name: name.into(),
            created_at: created,
            updated_at: now,
        })
    }

    async fn get_secret_blob(&self, name: &str) -> Result<Option<SecretBlob>, StoreError> {
        Ok(self.inner.read().await.secrets.get(name).cloned())
    }

    async fn list_secret_meta(&self) -> Result<Vec<SecretMeta>, StoreError> {
        let g = self.inner.read().await;
        let mut v: Vec<_> = g
            .secrets
            .values()
            .map(|s| SecretMeta {
                name: s.name.clone(),
                created_at: s.created_at.clone(),
                updated_at: s.updated_at.clone(),
            })
            .collect();
        v.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(v)
    }

    async fn delete_secret(&self, name: &str) -> Result<bool, StoreError> {
        Ok(self.inner.write().await.secrets.remove(name).is_some())
    }

    async fn put_ssh_key(
        &self,
        name: &str,
        public_key: &str,
    ) -> Result<SshAuthorizedKey, StoreError> {
        validate_public_key(public_key).map_err(StoreError::InvalidArgument)?;
        let now = Utc::now().to_rfc3339();
        let mut g = self.inner.write().await;
        let rec = if let Some(existing) = g.ssh_keys.get(name) {
            SshAuthorizedKey {
                name: name.into(),
                public_key: public_key.trim().into(),
                fingerprint: ssh_fingerprint(public_key),
                labels_json: existing.labels_json.clone(),
                created_at: existing.created_at.clone(),
                updated_at: now,
            }
        } else {
            SshAuthorizedKey {
                name: name.into(),
                public_key: public_key.trim().into(),
                fingerprint: ssh_fingerprint(public_key),
                labels_json: "{}".into(),
                created_at: now.clone(),
                updated_at: now,
            }
        };
        g.ssh_keys.insert(name.into(), rec.clone());
        Ok(rec)
    }

    async fn get_ssh_key(&self, name: &str) -> Result<Option<SshAuthorizedKey>, StoreError> {
        Ok(self.inner.read().await.ssh_keys.get(name).cloned())
    }

    async fn list_ssh_keys(&self) -> Result<Vec<SshAuthorizedKey>, StoreError> {
        let mut v: Vec<_> = self.inner.read().await.ssh_keys.values().cloned().collect();
        v.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(v)
    }

    async fn delete_ssh_key(&self, name: &str) -> Result<bool, StoreError> {
        Ok(self.inner.write().await.ssh_keys.remove(name).is_some())
    }

    async fn get_instance_ssh(
        &self,
        instance_id: &str,
    ) -> Result<Option<InstanceSshRecord>, StoreError> {
        Ok(self
            .inner
            .read()
            .await
            .instance_ssh
            .get(instance_id)
            .cloned())
    }

    async fn put_instance_ssh_desired(
        &self,
        rec: &InstanceSshRecord,
    ) -> Result<InstanceSshRecord, StoreError> {
        let mut g = self.inner.write().await;
        if !g.instances.contains_key(&rec.instance_id) {
            return Err(StoreError::NotFound(rec.instance_id.clone()));
        }
        let mut out = rec.clone();
        out.updated_at = Utc::now().to_rfc3339();
        if let Some(existing) = g.instance_ssh.get(&rec.instance_id) {
            // Keep observed fields unless resetting desired off
            out.phase = existing.phase.clone();
            out.bind = existing.bind.clone();
            out.port = existing.port;
            out.message = existing.message.clone();
        }
        g.instance_ssh.insert(rec.instance_id.clone(), out.clone());
        Ok(out)
    }

    async fn clear_instance_ssh_override(
        &self,
        instance_id: &str,
    ) -> Result<Option<InstanceSshRecord>, StoreError> {
        let mut g = self.inner.write().await;
        let Some(mut rec) = g.instance_ssh.get(instance_id).cloned() else {
            return Ok(None);
        };
        rec.has_override = false;
        rec.desired = false;
        rec.desired_bind = None;
        rec.desired_port = None;
        rec.desired_user = None;
        rec.desired_sftp = None;
        rec.desired_key_names_json = None;
        rec.updated_at = Utc::now().to_rfc3339();
        g.instance_ssh.insert(instance_id.into(), rec.clone());
        Ok(Some(rec))
    }

    async fn update_instance_ssh_observed(
        &self,
        instance_id: &str,
        phase: &str,
        bind: Option<&str>,
        port: Option<u16>,
        message: Option<&str>,
    ) -> Result<InstanceSshRecord, StoreError> {
        let mut g = self.inner.write().await;
        if !g.instances.contains_key(instance_id) {
            return Err(StoreError::NotFound(instance_id.into()));
        }
        let mut rec =
            g.instance_ssh
                .get(instance_id)
                .cloned()
                .unwrap_or_else(|| InstanceSshRecord {
                    instance_id: instance_id.into(),
                    ..Default::default()
                });
        rec.phase = phase.into();
        rec.bind = bind.map(str::to_string);
        rec.port = port;
        rec.message = message.map(str::to_string);
        rec.updated_at = Utc::now().to_rfc3339();
        g.instance_ssh.insert(instance_id.into(), rec.clone());
        Ok(rec)
    }

    async fn list_instance_ssh(&self) -> Result<Vec<InstanceSshRecord>, StoreError> {
        Ok(self
            .inner
            .read()
            .await
            .instance_ssh
            .values()
            .cloned()
            .collect())
    }

    async fn update_instance_network_observed(
        &self,
        instance_id: &str,
        phase: &str,
        observed_json: &str,
        message: Option<&str>,
    ) -> Result<InstanceNetworkRecord, StoreError> {
        let mut g = self.inner.write().await;
        if !g.instances.contains_key(instance_id) {
            return Err(StoreError::NotFound(instance_id.into()));
        }
        let rec = InstanceNetworkRecord {
            instance_id: instance_id.into(),
            phase: phase.into(),
            observed_json: observed_json.into(),
            message: message.map(str::to_string),
            updated_at: Utc::now().to_rfc3339(),
        };
        g.instance_network.insert(instance_id.into(), rec.clone());
        Ok(rec)
    }

    async fn get_instance_network(
        &self,
        instance_id: &str,
    ) -> Result<Option<InstanceNetworkRecord>, StoreError> {
        Ok(self
            .inner
            .read()
            .await
            .instance_network
            .get(instance_id)
            .cloned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hash_token;

    #[tokio::test]
    async fn replicas_scale() {
        let store = MemoryStore::new();
        store.init_cluster(&hash_token("a")).await.unwrap();
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

    #[tokio::test]
    async fn unbind_returns_to_pending() {
        let store = MemoryStore::new();
        store.init_cluster(&hash_token("a")).await.unwrap();
        store.upsert_stack("demo", "{}", "yaml").await.unwrap();
        let inst = store
            .reconcile_service_replicas("demo", "web", 1, r#"{"image":"x"}"#)
            .await
            .unwrap();
        let id = inst[0].id.clone();
        store.bind_instance_to_node(&id, "node-a").await.unwrap();
        let unbound = store.unbind_instance(&id).await.unwrap();
        assert_eq!(unbound.phase, "Pending");
        assert!(unbound.node_id.is_none());
        assert!(unbound.runtime_id.is_none());
        let pending = store.list_pending_instances().await.unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, id);
    }
}
