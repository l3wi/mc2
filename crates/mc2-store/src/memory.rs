//! In-memory store for unit tests.

use crate::{
    ssh_fingerprint, validate_public_key, verify_token, ClusterCounts, ClusterMeta,
    InstanceNetworkRecord, InstancePhase, InstanceRecord, InstanceSshRecord, NodeHeartbeat,
    NodeJoin, NodeRecord, NodeStatus, SecretBlob, SecretMeta, SshAuthorizedKey, StackPlan,
    StackRecord, Store, StoreError,
};
use async_trait::async_trait;
use chrono::Utc;
use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;
use tokio::sync::RwLock;
use uuid::Uuid;

#[derive(Debug, Default)]
struct Inner {
    meta: Option<ClusterMeta>,
    nodes: HashMap<String, NodeRecord>,
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
        if token.is_empty() {
            return Ok(false);
        }
        Ok(verify_token(token, &meta.api_token_hash))
    }

    async fn replace_api_token_hash(&self, api_token_hash: &str) -> Result<(), StoreError> {
        let mut g = self.inner.write().await;
        let Some(meta) = g.meta.as_mut() else {
            return Err(StoreError::NotInitialized);
        };
        meta.api_token_hash = api_token_hash.to_string();
        Ok(())
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
        // Singleton: the id is persisted under a settings key and reused for
        // every later start, so renaming the node keeps its placements.
        let stored = g.settings.get(crate::SETTING_LOCAL_NODE_ID).cloned();
        let existing = stored.filter(|id| !id.is_empty()).or_else(|| {
            // First start (or a row adopted from before the id was stored).
            g.nodes
                .values()
                .find(|n| n.name == join.name)
                .map(|n| n.id.clone())
        });

        let id = existing.unwrap_or_else(|| Uuid::new_v4().to_string());
        let created_at = g
            .nodes
            .get(&id)
            .map(|n| n.created_at.clone())
            .unwrap_or_else(|| now.clone());
        let rec = NodeRecord {
            id: id.clone(),
            name: join.name,
            labels_json: join.labels_json,
            arch: join.arch,
            cpus: join.cpus,
            memory_mib: join.memory_mib,
            status: NodeStatus::Ready.as_str().into(),
            last_heartbeat: Some(now),
            created_at,
        };
        g.nodes.insert(id.clone(), rec.clone());
        g.settings
            .insert(crate::SETTING_LOCAL_NODE_ID.to_string(), id);
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

    async fn commit_stack_plan(&self, plan: &StackPlan) -> Result<Vec<InstanceRecord>, StoreError> {
        // One write lock for the whole plan: the stack row plus every instance
        // create/update/delete. Readers never observe a half-applied stack, and
        // no operation here can fail, so there is nothing to roll back.
        let mut g = self.inner.write().await;
        let now = Utc::now().to_rfc3339();

        // Stack row: update in place (keeping created_at) or insert.
        let created_at = g
            .stacks
            .get(&plan.stack)
            .map(|s| s.created_at.clone())
            .unwrap_or_else(|| now.clone());
        g.stacks.insert(
            plan.stack.clone(),
            StackRecord {
                name: plan.stack.clone(),
                labels_json: plan.labels_json.clone(),
                raw_yaml: plan.raw_yaml.clone(),
                created_at,
                updated_at: now.clone(),
            },
        );

        // Drop every instance the plan does not keep — a service removed from
        // the YAML, or an ordinal past a scale-down — cascading ssh/network
        // rows exactly like SQLite's ON DELETE CASCADE (D5).
        let keep: BTreeSet<(&str, u32)> = plan
            .instances
            .iter()
            .map(|i| (i.service.as_str(), i.ordinal))
            .collect();
        let doomed: Vec<String> = g
            .instances
            .values()
            .filter(|i| i.stack == plan.stack && !keep.contains(&(i.service.as_str(), i.ordinal)))
            .map(|i| i.id.clone())
            .collect();
        for id in doomed {
            g.instances.remove(&id);
            g.instance_ssh.remove(&id);
            g.instance_network.remove(&id);
        }

        // Create or refresh each planned instance. An update touches only
        // spec_json/updated_at: placement, runtime id, observed phase and the
        // applied-config hash stay put so the node can decide on a recreate.
        for pi in &plan.instances {
            let existing_id = g
                .instances
                .values()
                .find(|i| {
                    i.stack == plan.stack && i.service == pi.service && i.ordinal == pi.ordinal
                })
                .map(|i| i.id.clone());
            match existing_id {
                Some(id) => {
                    let inst = g.instances.get_mut(&id).expect("instance just matched");
                    inst.spec_json = pi.spec_json.clone();
                    inst.updated_at = now.clone();
                }
                None => {
                    let id = Uuid::new_v4().to_string();
                    g.instances.insert(
                        id.clone(),
                        InstanceRecord {
                            id,
                            stack: plan.stack.clone(),
                            service: pi.service.clone(),
                            ordinal: pi.ordinal,
                            node_id: None,
                            phase: InstancePhase::Pending.as_str().into(),
                            runtime_id: None,
                            message: None,
                            spec_json: pi.spec_json.clone(),
                            healthy: false,
                            applied_hash: None,
                            updated_at: now.clone(),
                        },
                    );
                }
            }
        }

        let mut out: Vec<InstanceRecord> = g
            .instances
            .values()
            .filter(|i| i.stack == plan.stack)
            .cloned()
            .collect();
        out.sort_by(|a, b| (&a.service, a.ordinal).cmp(&(&b.service, b.ordinal)));
        Ok(out)
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

    async fn set_instance_applied_hash(
        &self,
        instance_id: &str,
        applied_hash: Option<&str>,
    ) -> Result<(), StoreError> {
        let mut g = self.inner.write().await;
        let inst = g
            .instances
            .get_mut(instance_id)
            .ok_or_else(|| StoreError::NotFound(instance_id.into()))?;
        inst.applied_hash = applied_hash.map(str::to_string);
        Ok(())
    }

    async fn get_instance_applied_hash(
        &self,
        instance_id: &str,
    ) -> Result<Option<String>, StoreError> {
        Ok(self
            .inner
            .read()
            .await
            .instances
            .get(instance_id)
            .and_then(|i| i.applied_hash.clone()))
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

    /// Seed `demo` with `replicas` copies of `spec` and return the instances.
    async fn seed(store: &MemoryStore, replicas: u32, spec: &str) -> Vec<InstanceRecord> {
        let specs = vec![spec.to_string(); replicas as usize];
        store
            .commit_stack_plan(&StackPlan::replicas(
                "demo",
                "{}",
                "yaml",
                vec![("web", specs)],
            ))
            .await
            .unwrap()
    }

    /// B8: the local node is a singleton — a rename must not strand placements.
    #[tokio::test]
    async fn local_node_id_survives_a_rename() {
        let store = MemoryStore::new();
        store.init_cluster(&hash_token("a")).await.unwrap();
        let first = store
            .upsert_local_node(NodeJoin {
                name: "host-a".into(),
                labels_json: "{}".into(),
                arch: "aarch64".into(),
                cpus: 4,
                memory_mib: 8192,
            })
            .await
            .unwrap();

        let renamed = store
            .upsert_local_node(NodeJoin {
                name: "host-b".into(),
                labels_json: r#"{"zone":"b"}"#.into(),
                arch: "aarch64".into(),
                cpus: 8,
                memory_mib: 16384,
            })
            .await
            .unwrap();

        assert_eq!(renamed.id, first.id, "same node, new display name");
        assert_eq!(renamed.name, "host-b");
        assert_eq!(renamed.cpus, 8);
        let nodes = store.list_nodes().await.unwrap();
        assert_eq!(nodes.len(), 1, "one embedded node");
    }

    /// B3: the applied config hash round-trips and can be cleared.
    #[tokio::test]
    async fn applied_hash_round_trip() {
        let store = MemoryStore::new();
        store.init_cluster(&hash_token("a")).await.unwrap();
        let inst = seed(&store, 1, r#"{"image":"x"}"#).await;
        let id = inst[0].id.clone();

        assert_eq!(store.get_instance_applied_hash(&id).await.unwrap(), None);
        store
            .set_instance_applied_hash(&id, Some("abc123"))
            .await
            .unwrap();
        assert_eq!(
            store.get_instance_applied_hash(&id).await.unwrap(),
            Some("abc123".into())
        );
        store.set_instance_applied_hash(&id, None).await.unwrap();
        assert_eq!(store.get_instance_applied_hash(&id).await.unwrap(), None);

        assert!(store
            .set_instance_applied_hash("missing", Some("x"))
            .await
            .is_err());
    }

    #[tokio::test]
    async fn replace_api_token_hash_swaps_the_credential() {
        let store = MemoryStore::new();
        store.init_cluster(&hash_token("old")).await.unwrap();
        assert!(store.verify_api_token("old").await.unwrap());

        store
            .replace_api_token_hash(&hash_token("new"))
            .await
            .unwrap();
        assert!(!store.verify_api_token("old").await.unwrap());
        assert!(store.verify_api_token("new").await.unwrap());
        assert!(!store.verify_api_token("").await.unwrap());
    }

    #[tokio::test]
    async fn verify_without_cluster_meta_is_false() {
        let store = MemoryStore::new();
        assert!(!store.verify_api_token("anything").await.unwrap());
        assert!(store.replace_api_token_hash("h").await.is_err());
    }
}
