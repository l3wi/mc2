//! SQLite-backed store (default production backend).

use crate::{
    ssh_fingerprint, validate_public_key, verify_token, ClusterCounts, ClusterMeta, InstanceFabricRecord,
    InstanceRecord, InstanceSshRecord, NodeHeartbeat, NodeJoin, NodeRecord, SecretBlob, SecretMeta,
    SshAuthorizedKey, StackRecord, Store, StoreError,
};
use anyhow::{Context, Result as AnyResult};
use async_trait::async_trait;
use chrono::{Duration as ChronoDuration, Utc};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use uuid::Uuid;

/// SQLite implementation of [`Store`].
#[derive(Clone)]
pub struct SqliteStore {
    pool: SqlitePool,
    path: PathBuf,
}

impl SqliteStore {
    /// Open (or create) a SQLite database at `path` and run migrations.
    pub async fn open(path: impl AsRef<Path>) -> AnyResult<Arc<Self>> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create data dir {}", parent.display()))?;
        }

        let options = SqliteConnectOptions::from_str(&format!("sqlite:{}", path.display()))?
            .create_if_missing(true)
            .foreign_keys(true);

        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(options)
            .await
            .with_context(|| format!("open sqlite {}", path.display()))?;

        sqlx::migrate!("./migrations")
            .run(&pool)
            .await
            .context("run migrations")?;

        Ok(Arc::new(Self { pool, path }))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    fn map_node(row: &sqlx::sqlite::SqliteRow) -> NodeRecord {
        NodeRecord {
            id: row.get("id"),
            name: row.get("name"),
            labels_json: row.get("labels_json"),
            arch: row.get("arch"),
            cpus: row.get::<i64, _>("cpus") as u32,
            memory_mib: row.get::<i64, _>("memory_mib") as u64,
            status: row.get("status"),
            last_heartbeat: row.get("last_heartbeat"),
            created_at: row.get("created_at"),
            node_token_hash: row.get("node_token_hash"),
        }
    }

    fn map_instance_ssh(row: &sqlx::sqlite::SqliteRow) -> InstanceSshRecord {
        InstanceSshRecord {
            instance_id: row.get("instance_id"),
            has_override: row.get::<i64, _>("has_override") != 0,
            desired: row.get::<i64, _>("desired") != 0,
            desired_bind: row.get("desired_bind"),
            desired_port: row.get::<Option<i64>, _>("desired_port").map(|p| p as u16),
            desired_user: row.get("desired_user"),
            desired_sftp: row.get::<Option<i64>, _>("desired_sftp").map(|v| v != 0),
            desired_key_names_json: row.get("desired_key_names_json"),
            phase: row.get("phase"),
            bind: row.get("bind"),
            port: row.get::<Option<i64>, _>("port").map(|p| p as u16),
            message: row.get("message"),
            updated_at: row.get("updated_at"),
        }
    }

    fn map_instance(row: &sqlx::sqlite::SqliteRow) -> InstanceRecord {
        InstanceRecord {
            id: row.get("id"),
            stack: row.get("stack"),
            service: row.get("service"),
            ordinal: row.get::<i64, _>("ordinal") as u32,
            node_id: row.get("node_id"),
            phase: row.get("phase"),
            runtime_id: row.get("runtime_id"),
            message: row.get("message"),
            spec_json: row.get("spec_json"),
            updated_at: row.get("updated_at"),
        }
    }
}

#[async_trait]
impl Store for SqliteStore {
    async fn get_cluster_meta(&self) -> Result<Option<ClusterMeta>, StoreError> {
        let row = sqlx::query(
            r#"
            SELECT initialized, api_token_hash, join_token_hash, created_at
            FROM cluster_meta WHERE id = 1
            "#,
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| StoreError::Other(e.into()))?;

        Ok(row.map(|r| ClusterMeta {
            initialized: r.get::<i64, _>("initialized") != 0,
            api_token_hash: r.get("api_token_hash"),
            join_token_hash: r.get("join_token_hash"),
            created_at: r.get("created_at"),
        }))
    }

    async fn init_cluster(
        &self,
        api_token_hash: &str,
        join_token_hash: &str,
    ) -> Result<ClusterMeta, StoreError> {
        if self.get_cluster_meta().await?.is_some() {
            return Err(StoreError::AlreadyExists("cluster".into()));
        }

        let created_at = Utc::now().to_rfc3339();
        sqlx::query(
            r#"
            INSERT INTO cluster_meta (id, initialized, api_token_hash, join_token_hash, created_at)
            VALUES (1, 1, ?1, ?2, ?3)
            "#,
        )
        .bind(api_token_hash)
        .bind(join_token_hash)
        .bind(&created_at)
        .execute(&self.pool)
        .await
        .map_err(|e| StoreError::Other(e.into()))?;

        Ok(ClusterMeta {
            initialized: true,
            api_token_hash: api_token_hash.to_string(),
            join_token_hash: join_token_hash.to_string(),
            created_at,
        })
    }

    async fn verify_api_token(&self, token: &str) -> Result<bool, StoreError> {
        let Some(meta) = self.get_cluster_meta().await? else {
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

    async fn verify_join_token(&self, token: &str) -> Result<bool, StoreError> {
        let Some(meta) = self.get_cluster_meta().await? else {
            return Ok(false);
        };
        if meta.join_token_hash.is_empty() {
            return Ok(true);
        }
        if token.is_empty() {
            return Ok(false);
        }
        Ok(verify_token(token, &meta.join_token_hash))
    }

    async fn api_auth_required(&self) -> Result<bool, StoreError> {
        Ok(self
            .get_cluster_meta()
            .await?
            .map(|m| !m.api_token_hash.is_empty())
            .unwrap_or(true))
    }

    async fn join_auth_required(&self) -> Result<bool, StoreError> {
        Ok(self
            .get_cluster_meta()
            .await?
            .map(|m| !m.join_token_hash.is_empty())
            .unwrap_or(true))
    }

    async fn cluster_counts(&self) -> Result<ClusterCounts, StoreError> {
        let nodes_total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM nodes")
            .fetch_one(&self.pool)
            .await
            .map_err(|e| StoreError::Other(e.into()))?;

        let nodes_ready: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM nodes WHERE status = 'Ready'")
                .fetch_one(&self.pool)
                .await
                .map_err(|e| StoreError::Other(e.into()))?;

        let stacks: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM stacks")
            .fetch_one(&self.pool)
            .await
            .map_err(|e| StoreError::Other(e.into()))?;

        let instances: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM instances")
            .fetch_one(&self.pool)
            .await
            .map_err(|e| StoreError::Other(e.into()))?;

        Ok(ClusterCounts {
            nodes_ready: nodes_ready as u32,
            nodes_total: nodes_total as u32,
            stacks: stacks as u32,
            instances: instances as u32,
        })
    }

    async fn upsert_node_join(&self, join: NodeJoin) -> Result<NodeRecord, StoreError> {
        let existing = sqlx::query(
            r#"SELECT id, name, labels_json, arch, cpus, memory_mib, status,
                      last_heartbeat, created_at, node_token_hash
               FROM nodes WHERE name = ?1"#,
        )
        .bind(&join.name)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| StoreError::Other(e.into()))?;

        let now = Utc::now().to_rfc3339();

        if let Some(row) = existing {
            let id: String = row.get("id");
            sqlx::query(
                r#"
                UPDATE nodes SET
                  labels_json = ?1,
                  arch = ?2,
                  cpus = ?3,
                  memory_mib = ?4,
                  node_token_hash = ?5,
                  status = 'Ready',
                  last_heartbeat = ?6
                WHERE id = ?7
                "#,
            )
            .bind(&join.labels_json)
            .bind(&join.arch)
            .bind(join.cpus as i64)
            .bind(join.memory_mib as i64)
            .bind(&join.node_token_hash)
            .bind(&now)
            .bind(&id)
            .execute(&self.pool)
            .await
            .map_err(|e| StoreError::Other(e.into()))?;

            return self
                .get_node(&id)
                .await?
                .ok_or_else(|| StoreError::NotFound(id));
        }

        let id = Uuid::new_v4().to_string();
        sqlx::query(
            r#"
            INSERT INTO nodes (
              id, name, labels_json, arch, cpus, memory_mib, status,
              last_heartbeat, created_at, node_token_hash
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'Ready', ?7, ?8, ?9)
            "#,
        )
        .bind(&id)
        .bind(&join.name)
        .bind(&join.labels_json)
        .bind(&join.arch)
        .bind(join.cpus as i64)
        .bind(join.memory_mib as i64)
        .bind(&now)
        .bind(&now)
        .bind(&join.node_token_hash)
        .execute(&self.pool)
        .await
        .map_err(|e| StoreError::Other(e.into()))?;

        self.get_node(&id)
            .await?
            .ok_or_else(|| StoreError::NotFound(id))
    }

    async fn heartbeat_node(
        &self,
        node_id: &str,
        node_token: &str,
        hb: NodeHeartbeat,
    ) -> Result<NodeRecord, StoreError> {
        let Some(node) = self.get_node(node_id).await? else {
            return Err(StoreError::NotFound(format!("node {node_id}")));
        };
        if !verify_token(node_token, &node.node_token_hash) {
            return Err(StoreError::Unauthorized);
        }

        let now = Utc::now().to_rfc3339();
        sqlx::query(
            r#"
            UPDATE nodes SET
              cpus = ?1,
              memory_mib = ?2,
              status = ?3,
              last_heartbeat = ?4
            WHERE id = ?5
            "#,
        )
        .bind(hb.cpus as i64)
        .bind(hb.memory_mib as i64)
        .bind(&hb.status)
        .bind(&now)
        .bind(node_id)
        .execute(&self.pool)
        .await
        .map_err(|e| StoreError::Other(e.into()))?;

        self.get_node(node_id)
            .await?
            .ok_or_else(|| StoreError::NotFound(node_id.into()))
    }

    async fn list_nodes(&self) -> Result<Vec<NodeRecord>, StoreError> {
        let rows = sqlx::query(
            r#"SELECT id, name, labels_json, arch, cpus, memory_mib, status,
                      last_heartbeat, created_at, node_token_hash
               FROM nodes ORDER BY name"#,
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| StoreError::Other(e.into()))?;

        Ok(rows.iter().map(Self::map_node).collect())
    }

    async fn get_node(&self, node_id: &str) -> Result<Option<NodeRecord>, StoreError> {
        let row = sqlx::query(
            r#"SELECT id, name, labels_json, arch, cpus, memory_mib, status,
                      last_heartbeat, created_at, node_token_hash
               FROM nodes WHERE id = ?1"#,
        )
        .bind(node_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| StoreError::Other(e.into()))?;

        Ok(row.as_ref().map(Self::map_node))
    }

    async fn mark_stale_nodes(&self, grace: Duration) -> Result<u32, StoreError> {
        let cutoff =
            Utc::now() - ChronoDuration::from_std(grace).unwrap_or(ChronoDuration::seconds(30));
        let cutoff_s = cutoff.to_rfc3339();
        let res = sqlx::query(
            r#"
            UPDATE nodes
            SET status = 'NotReady'
            WHERE status = 'Ready'
              AND (last_heartbeat IS NULL OR last_heartbeat < ?1)
            "#,
        )
        .bind(&cutoff_s)
        .execute(&self.pool)
        .await
        .map_err(|e| StoreError::Other(e.into()))?;
        Ok(res.rows_affected() as u32)
    }

    async fn upsert_stack(
        &self,
        name: &str,
        labels_json: &str,
        raw_yaml: &str,
    ) -> Result<StackRecord, StoreError> {
        let now = Utc::now().to_rfc3339();
        let existing = self.get_stack(name).await?;
        if existing.is_some() {
            sqlx::query(
                r#"UPDATE stacks SET labels_json = ?1, raw_yaml = ?2, updated_at = ?3 WHERE name = ?4"#,
            )
            .bind(labels_json)
            .bind(raw_yaml)
            .bind(&now)
            .bind(name)
            .execute(&self.pool)
            .await
            .map_err(|e| StoreError::Other(e.into()))?;
        } else {
            sqlx::query(
                r#"INSERT INTO stacks (name, labels_json, raw_yaml, created_at, updated_at)
                   VALUES (?1, ?2, ?3, ?4, ?5)"#,
            )
            .bind(name)
            .bind(labels_json)
            .bind(raw_yaml)
            .bind(&now)
            .bind(&now)
            .execute(&self.pool)
            .await
            .map_err(|e| StoreError::Other(e.into()))?;
        }
        self.get_stack(name)
            .await?
            .ok_or_else(|| StoreError::NotFound(name.into()))
    }

    async fn list_stacks(&self) -> Result<Vec<StackRecord>, StoreError> {
        let rows = sqlx::query(
            r#"SELECT name, labels_json, raw_yaml, created_at, updated_at FROM stacks ORDER BY name"#,
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| StoreError::Other(e.into()))?;
        Ok(rows
            .iter()
            .map(|r| StackRecord {
                name: r.get("name"),
                labels_json: r.get("labels_json"),
                raw_yaml: r.get("raw_yaml"),
                created_at: r.get("created_at"),
                updated_at: r.get("updated_at"),
            })
            .collect())
    }

    async fn get_stack(&self, name: &str) -> Result<Option<StackRecord>, StoreError> {
        let row = sqlx::query(
            r#"SELECT name, labels_json, raw_yaml, created_at, updated_at FROM stacks WHERE name = ?1"#,
        )
        .bind(name)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| StoreError::Other(e.into()))?;
        Ok(row.map(|r| StackRecord {
            name: r.get("name"),
            labels_json: r.get("labels_json"),
            raw_yaml: r.get("raw_yaml"),
            created_at: r.get("created_at"),
            updated_at: r.get("updated_at"),
        }))
    }

    async fn reconcile_service_replicas(
        &self,
        stack: &str,
        service: &str,
        replicas: u32,
        spec_json: &str,
    ) -> Result<Vec<InstanceRecord>, StoreError> {
        let now = Utc::now().to_rfc3339();
        sqlx::query(r#"DELETE FROM instances WHERE stack = ?1 AND service = ?2 AND ordinal >= ?3"#)
            .bind(stack)
            .bind(service)
            .bind(replicas as i64)
            .execute(&self.pool)
            .await
            .map_err(|e| StoreError::Other(e.into()))?;

        for ord in 0..replicas {
            let existing = sqlx::query(
                r#"SELECT id FROM instances WHERE stack = ?1 AND service = ?2 AND ordinal = ?3"#,
            )
            .bind(stack)
            .bind(service)
            .bind(ord as i64)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| StoreError::Other(e.into()))?;

            if let Some(row) = existing {
                let id: String = row.get("id");
                sqlx::query(
                    r#"UPDATE instances SET spec_json = ?1, updated_at = ?2 WHERE id = ?3"#,
                )
                .bind(spec_json)
                .bind(&now)
                .bind(&id)
                .execute(&self.pool)
                .await
                .map_err(|e| StoreError::Other(e.into()))?;
            } else {
                let id = Uuid::new_v4().to_string();
                sqlx::query(
                    r#"INSERT INTO instances
                       (id, stack, service, ordinal, node_id, phase, runtime_id, message, spec_json, updated_at)
                       VALUES (?1, ?2, ?3, ?4, NULL, 'Pending', NULL, NULL, ?5, ?6)"#,
                )
                .bind(&id)
                .bind(stack)
                .bind(service)
                .bind(ord as i64)
                .bind(spec_json)
                .bind(&now)
                .execute(&self.pool)
                .await
                .map_err(|e| StoreError::Other(e.into()))?;
            }
        }

        let rows = sqlx::query(
            r#"SELECT id, stack, service, ordinal, node_id, phase, runtime_id, message, spec_json, updated_at
               FROM instances WHERE stack = ?1 AND service = ?2 ORDER BY ordinal"#,
        )
        .bind(stack)
        .bind(service)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| StoreError::Other(e.into()))?;

        Ok(rows.iter().map(Self::map_instance).collect())
    }

    async fn list_instances(&self) -> Result<Vec<InstanceRecord>, StoreError> {
        let rows = sqlx::query(
            r#"SELECT id, stack, service, ordinal, node_id, phase, runtime_id, message, spec_json, updated_at
               FROM instances ORDER BY stack, service, ordinal"#,
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| StoreError::Other(e.into()))?;
        Ok(rows.iter().map(Self::map_instance).collect())
    }

    async fn list_instances_for_node(
        &self,
        node_id: &str,
    ) -> Result<Vec<InstanceRecord>, StoreError> {
        let rows = sqlx::query(
            r#"SELECT id, stack, service, ordinal, node_id, phase, runtime_id, message, spec_json, updated_at
               FROM instances WHERE node_id = ?1"#,
        )
        .bind(node_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| StoreError::Other(e.into()))?;
        Ok(rows.iter().map(Self::map_instance).collect())
    }

    async fn list_pending_instances(&self) -> Result<Vec<InstanceRecord>, StoreError> {
        let rows = sqlx::query(
            r#"SELECT id, stack, service, ordinal, node_id, phase, runtime_id, message, spec_json, updated_at
               FROM instances WHERE phase = 'Pending' AND node_id IS NULL"#,
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| StoreError::Other(e.into()))?;
        Ok(rows.iter().map(Self::map_instance).collect())
    }

    async fn bind_instance_to_node(
        &self,
        instance_id: &str,
        node_id: &str,
    ) -> Result<InstanceRecord, StoreError> {
        let now = Utc::now().to_rfc3339();
        let res = sqlx::query(
            r#"UPDATE instances SET node_id = ?1, phase = 'Scheduled', updated_at = ?2 WHERE id = ?3"#,
        )
        .bind(node_id)
        .bind(&now)
        .bind(instance_id)
        .execute(&self.pool)
        .await
        .map_err(|e| StoreError::Other(e.into()))?;
        if res.rows_affected() == 0 {
            return Err(StoreError::NotFound(instance_id.into()));
        }
        self.get_instance(instance_id)
            .await?
            .ok_or_else(|| StoreError::NotFound(instance_id.into()))
    }

    async fn unbind_instance(&self, instance_id: &str) -> Result<InstanceRecord, StoreError> {
        let now = Utc::now().to_rfc3339();
        let res = sqlx::query(
            r#"UPDATE instances SET node_id = NULL, phase = 'Pending', runtime_id = NULL,
                message = NULL, updated_at = ?1 WHERE id = ?2"#,
        )
        .bind(&now)
        .bind(instance_id)
        .execute(&self.pool)
        .await
        .map_err(|e| StoreError::Other(e.into()))?;
        if res.rows_affected() == 0 {
            return Err(StoreError::NotFound(instance_id.into()));
        }
        self.get_instance(instance_id)
            .await?
            .ok_or_else(|| StoreError::NotFound(instance_id.into()))
    }

    async fn update_instance_status(
        &self,
        instance_id: &str,
        phase: &str,
        runtime_id: Option<&str>,
        message: Option<&str>,
    ) -> Result<InstanceRecord, StoreError> {
        let now = Utc::now().to_rfc3339();
        sqlx::query(
            r#"UPDATE instances SET phase = ?1, runtime_id = COALESCE(?2, runtime_id),
                message = COALESCE(?3, message), updated_at = ?4 WHERE id = ?5"#,
        )
        .bind(phase)
        .bind(runtime_id)
        .bind(message)
        .bind(&now)
        .bind(instance_id)
        .execute(&self.pool)
        .await
        .map_err(|e| StoreError::Other(e.into()))?;
        self.get_instance(instance_id)
            .await?
            .ok_or_else(|| StoreError::NotFound(instance_id.into()))
    }

    async fn get_instance(&self, instance_id: &str) -> Result<Option<InstanceRecord>, StoreError> {
        let row = sqlx::query(
            r#"SELECT id, stack, service, ordinal, node_id, phase, runtime_id, message, spec_json, updated_at
               FROM instances WHERE id = ?1"#,
        )
        .bind(instance_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| StoreError::Other(e.into()))?;
        Ok(row.as_ref().map(Self::map_instance))
    }

    async fn put_secret_blob(
        &self,
        name: &str,
        nonce: &[u8],
        ciphertext: &[u8],
    ) -> Result<SecretMeta, StoreError> {
        let now = Utc::now().to_rfc3339();
        let existing = self.get_secret_blob(name).await?;
        let created = existing
            .as_ref()
            .map(|s| s.created_at.clone())
            .unwrap_or_else(|| now.clone());
        if existing.is_some() {
            sqlx::query(
                r#"UPDATE secrets_meta SET nonce = ?1, ciphertext = ?2, updated_at = ?3 WHERE name = ?4"#,
            )
            .bind(nonce)
            .bind(ciphertext)
            .bind(&now)
            .bind(name)
            .execute(&self.pool)
            .await
            .map_err(|e| StoreError::Other(e.into()))?;
        } else {
            sqlx::query(
                r#"INSERT INTO secrets_meta (name, nonce, ciphertext, created_at, updated_at)
                   VALUES (?1, ?2, ?3, ?4, ?5)"#,
            )
            .bind(name)
            .bind(nonce)
            .bind(ciphertext)
            .bind(&created)
            .bind(&now)
            .execute(&self.pool)
            .await
            .map_err(|e| StoreError::Other(e.into()))?;
        }
        Ok(SecretMeta {
            name: name.into(),
            created_at: created,
            updated_at: now,
        })
    }

    async fn get_secret_blob(&self, name: &str) -> Result<Option<SecretBlob>, StoreError> {
        let row = sqlx::query(
            r#"SELECT name, nonce, ciphertext, created_at, updated_at FROM secrets_meta WHERE name = ?1"#,
        )
        .bind(name)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| StoreError::Other(e.into()))?;
        Ok(row.map(|r| SecretBlob {
            name: r.get("name"),
            nonce: r.get("nonce"),
            ciphertext: r.get("ciphertext"),
            created_at: r.get("created_at"),
            updated_at: r.get("updated_at"),
        }))
    }

    async fn list_secret_meta(&self) -> Result<Vec<SecretMeta>, StoreError> {
        let rows =
            sqlx::query(r#"SELECT name, created_at, updated_at FROM secrets_meta ORDER BY name"#)
                .fetch_all(&self.pool)
                .await
                .map_err(|e| StoreError::Other(e.into()))?;
        Ok(rows
            .iter()
            .map(|r| SecretMeta {
                name: r.get("name"),
                created_at: r.get("created_at"),
                updated_at: r.get("updated_at"),
            })
            .collect())
    }

    async fn delete_secret(&self, name: &str) -> Result<bool, StoreError> {
        let res = sqlx::query(r#"DELETE FROM secrets_meta WHERE name = ?1"#)
            .bind(name)
            .execute(&self.pool)
            .await
            .map_err(|e| StoreError::Other(e.into()))?;
        Ok(res.rows_affected() > 0)
    }

    async fn put_ssh_key(
        &self,
        name: &str,
        public_key: &str,
    ) -> Result<SshAuthorizedKey, StoreError> {
        validate_public_key(public_key).map_err(|e| StoreError::Other(anyhow::anyhow!(e)))?;
        let now = Utc::now().to_rfc3339();
        let pk = public_key.trim();
        let fp = ssh_fingerprint(pk);
        let existing = self.get_ssh_key(name).await?;
        if existing.is_some() {
            sqlx::query(
                r#"UPDATE ssh_authorized_keys SET public_key = ?1, fingerprint = ?2, updated_at = ?3 WHERE name = ?4"#,
            )
            .bind(pk)
            .bind(&fp)
            .bind(&now)
            .bind(name)
            .execute(&self.pool)
            .await
            .map_err(|e| StoreError::Other(e.into()))?;
        } else {
            sqlx::query(
                r#"INSERT INTO ssh_authorized_keys (name, public_key, fingerprint, labels_json, created_at, updated_at)
                   VALUES (?1, ?2, ?3, '{}', ?4, ?5)"#,
            )
            .bind(name)
            .bind(pk)
            .bind(&fp)
            .bind(&now)
            .bind(&now)
            .execute(&self.pool)
            .await
            .map_err(|e| StoreError::Other(e.into()))?;
        }
        self.get_ssh_key(name)
            .await?
            .ok_or_else(|| StoreError::NotFound(name.into()))
    }

    async fn get_ssh_key(&self, name: &str) -> Result<Option<SshAuthorizedKey>, StoreError> {
        let row = sqlx::query(
            r#"SELECT name, public_key, fingerprint, labels_json, created_at, updated_at
               FROM ssh_authorized_keys WHERE name = ?1"#,
        )
        .bind(name)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| StoreError::Other(e.into()))?;
        Ok(row.map(|r| SshAuthorizedKey {
            name: r.get("name"),
            public_key: r.get("public_key"),
            fingerprint: r.get("fingerprint"),
            labels_json: r.get("labels_json"),
            created_at: r.get("created_at"),
            updated_at: r.get("updated_at"),
        }))
    }

    async fn list_ssh_keys(&self) -> Result<Vec<SshAuthorizedKey>, StoreError> {
        let rows = sqlx::query(
            r#"SELECT name, public_key, fingerprint, labels_json, created_at, updated_at
               FROM ssh_authorized_keys ORDER BY name"#,
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| StoreError::Other(e.into()))?;
        Ok(rows
            .into_iter()
            .map(|r| SshAuthorizedKey {
                name: r.get("name"),
                public_key: r.get("public_key"),
                fingerprint: r.get("fingerprint"),
                labels_json: r.get("labels_json"),
                created_at: r.get("created_at"),
                updated_at: r.get("updated_at"),
            })
            .collect())
    }

    async fn delete_ssh_key(&self, name: &str) -> Result<bool, StoreError> {
        let res = sqlx::query(r#"DELETE FROM ssh_authorized_keys WHERE name = ?1"#)
            .bind(name)
            .execute(&self.pool)
            .await
            .map_err(|e| StoreError::Other(e.into()))?;
        Ok(res.rows_affected() > 0)
    }

    async fn get_instance_ssh(
        &self,
        instance_id: &str,
    ) -> Result<Option<InstanceSshRecord>, StoreError> {
        let row = sqlx::query(
            r#"SELECT instance_id, has_override, desired, desired_bind, desired_port, desired_user,
                      desired_sftp, desired_key_names_json, phase, bind, port, message, updated_at
               FROM instance_ssh WHERE instance_id = ?1"#,
        )
        .bind(instance_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| StoreError::Other(e.into()))?;
        Ok(row.as_ref().map(Self::map_instance_ssh))
    }

    async fn put_instance_ssh_desired(
        &self,
        rec: &InstanceSshRecord,
    ) -> Result<InstanceSshRecord, StoreError> {
        if self.get_instance(&rec.instance_id).await?.is_none() {
            return Err(StoreError::NotFound(rec.instance_id.clone()));
        }
        let now = Utc::now().to_rfc3339();
        let existing = self.get_instance_ssh(&rec.instance_id).await?;
        let phase = existing
            .as_ref()
            .map(|e| e.phase.clone())
            .unwrap_or_else(|| "Closed".into());
        let bind = existing.as_ref().and_then(|e| e.bind.clone());
        let port = existing.as_ref().and_then(|e| e.port);
        let message = existing.as_ref().and_then(|e| e.message.clone());

        sqlx::query(
            r#"INSERT INTO instance_ssh (
                 instance_id, has_override, desired, desired_bind, desired_port, desired_user,
                 desired_sftp, desired_key_names_json, phase, bind, port, message, updated_at
               ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)
               ON CONFLICT(instance_id) DO UPDATE SET
                 has_override = excluded.has_override,
                 desired = excluded.desired,
                 desired_bind = excluded.desired_bind,
                 desired_port = excluded.desired_port,
                 desired_user = excluded.desired_user,
                 desired_sftp = excluded.desired_sftp,
                 desired_key_names_json = excluded.desired_key_names_json,
                 updated_at = excluded.updated_at"#,
        )
        .bind(&rec.instance_id)
        .bind(if rec.has_override { 1i64 } else { 0 })
        .bind(if rec.desired { 1i64 } else { 0 })
        .bind(&rec.desired_bind)
        .bind(rec.desired_port.map(|p| p as i64))
        .bind(&rec.desired_user)
        .bind(rec.desired_sftp.map(|s| if s { 1i64 } else { 0 }))
        .bind(&rec.desired_key_names_json)
        .bind(&phase)
        .bind(&bind)
        .bind(port.map(|p| p as i64))
        .bind(&message)
        .bind(&now)
        .execute(&self.pool)
        .await
        .map_err(|e| StoreError::Other(e.into()))?;

        self.get_instance_ssh(&rec.instance_id)
            .await?
            .ok_or_else(|| StoreError::NotFound(rec.instance_id.clone()))
    }

    async fn clear_instance_ssh_override(
        &self,
        instance_id: &str,
    ) -> Result<Option<InstanceSshRecord>, StoreError> {
        let Some(_) = self.get_instance_ssh(instance_id).await? else {
            return Ok(None);
        };
        let now = Utc::now().to_rfc3339();
        sqlx::query(
            r#"UPDATE instance_ssh SET has_override = 0, desired = 0,
                desired_bind = NULL, desired_port = NULL, desired_user = NULL,
                desired_sftp = NULL, desired_key_names_json = NULL, updated_at = ?1
               WHERE instance_id = ?2"#,
        )
        .bind(&now)
        .bind(instance_id)
        .execute(&self.pool)
        .await
        .map_err(|e| StoreError::Other(e.into()))?;
        self.get_instance_ssh(instance_id).await
    }

    async fn update_instance_ssh_observed(
        &self,
        instance_id: &str,
        phase: &str,
        bind: Option<&str>,
        port: Option<u16>,
        message: Option<&str>,
    ) -> Result<InstanceSshRecord, StoreError> {
        if self.get_instance(instance_id).await?.is_none() {
            return Err(StoreError::NotFound(instance_id.into()));
        }
        let now = Utc::now().to_rfc3339();
        sqlx::query(
            r#"INSERT INTO instance_ssh (
                 instance_id, has_override, desired, phase, bind, port, message, updated_at
               ) VALUES (?1, 0, 0, ?2, ?3, ?4, ?5, ?6)
               ON CONFLICT(instance_id) DO UPDATE SET
                 phase = excluded.phase,
                 bind = excluded.bind,
                 port = excluded.port,
                 message = excluded.message,
                 updated_at = excluded.updated_at"#,
        )
        .bind(instance_id)
        .bind(phase)
        .bind(bind)
        .bind(port.map(|p| p as i64))
        .bind(message)
        .bind(&now)
        .execute(&self.pool)
        .await
        .map_err(|e| StoreError::Other(e.into()))?;
        self.get_instance_ssh(instance_id)
            .await?
            .ok_or_else(|| StoreError::NotFound(instance_id.into()))
    }

    async fn list_instance_ssh(&self) -> Result<Vec<InstanceSshRecord>, StoreError> {
        let rows = sqlx::query(
            r#"SELECT instance_id, has_override, desired, desired_bind, desired_port, desired_user,
                      desired_sftp, desired_key_names_json, phase, bind, port, message, updated_at
               FROM instance_ssh"#,
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| StoreError::Other(e.into()))?;
        Ok(rows.iter().map(Self::map_instance_ssh).collect())
    }

    async fn update_instance_fabric_observed(
        &self,
        instance_id: &str,
        phase: &str,
        observed_json: &str,
        message: Option<&str>,
    ) -> Result<InstanceFabricRecord, StoreError> {
        if self.get_instance(instance_id).await?.is_none() {
            return Err(StoreError::NotFound(instance_id.into()));
        }
        let now = Utc::now().to_rfc3339();
        sqlx::query(
            r#"INSERT INTO instance_fabric (instance_id, phase, observed_json, message, updated_at)
               VALUES (?1, ?2, ?3, ?4, ?5)
               ON CONFLICT(instance_id) DO UPDATE SET
                 phase = excluded.phase,
                 observed_json = excluded.observed_json,
                 message = excluded.message,
                 updated_at = excluded.updated_at"#,
        )
        .bind(instance_id)
        .bind(phase)
        .bind(observed_json)
        .bind(message)
        .bind(&now)
        .execute(&self.pool)
        .await
        .map_err(|e| StoreError::Other(e.into()))?;
        self.get_instance_fabric(instance_id)
            .await?
            .ok_or_else(|| StoreError::NotFound(instance_id.into()))
    }

    async fn get_instance_fabric(
        &self,
        instance_id: &str,
    ) -> Result<Option<InstanceFabricRecord>, StoreError> {
        let row = sqlx::query(
            r#"SELECT instance_id, phase, observed_json, message, updated_at
               FROM instance_fabric WHERE instance_id = ?"#,
        )
        .bind(instance_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| StoreError::Other(e.into()))?;
        Ok(row.map(|r| InstanceFabricRecord {
            instance_id: r.get("instance_id"),
            phase: r.get("phase"),
            observed_json: r.get("observed_json"),
            message: r.get("message"),
            updated_at: r.get("updated_at"),
        }))
    }

    async fn list_instance_fabric(&self) -> Result<Vec<InstanceFabricRecord>, StoreError> {
        let rows = sqlx::query(
            r#"SELECT instance_id, phase, observed_json, message, updated_at FROM instance_fabric"#,
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| StoreError::Other(e.into()))?;
        Ok(rows
            .iter()
            .map(|r| InstanceFabricRecord {
                instance_id: r.get("instance_id"),
                phase: r.get("phase"),
                observed_json: r.get("observed_json"),
                message: r.get("message"),
                updated_at: r.get("updated_at"),
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{hash_token, NodeStatus, SecretsKey};
    use tempfile::tempdir;

    #[tokio::test]
    async fn sqlite_join_and_list() {
        let dir = tempdir().unwrap();
        let db = dir.path().join("mcc.db");
        let store = SqliteStore::open(&db).await.unwrap();

        store
            .init_cluster(&hash_token("api"), &hash_token("join"))
            .await
            .unwrap();

        let node = store
            .upsert_node_join(NodeJoin {
                name: "worker-1".into(),
                labels_json: r#"{"role":"worker"}"#.into(),
                arch: "aarch64".into(),
                cpus: 8,
                memory_mib: 16384,
                node_token_hash: hash_token("node-tok"),
            })
            .await
            .unwrap();

        assert_eq!(node.status, NodeStatus::Ready.as_str());
        let list = store.list_nodes().await.unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "worker-1");

        store
            .heartbeat_node(
                &node.id,
                "node-tok",
                NodeHeartbeat {
                    cpus: 8,
                    memory_mib: 16384,
                    status: "Ready".into(),
                },
            )
            .await
            .unwrap();

        let counts = store.cluster_counts().await.unwrap();
        assert_eq!(counts.nodes_total, 1);
        assert_eq!(counts.nodes_ready, 1);
    }

    #[tokio::test]
    async fn unbind_instance_clears_placement() {
        let dir = tempdir().unwrap();
        let db = dir.path().join("mcc.db");
        let store = SqliteStore::open(&db).await.unwrap();
        store.init_cluster("", "").await.unwrap();
        store.upsert_stack("demo", "{}", "yaml").await.unwrap();
        let inst = store
            .reconcile_service_replicas("demo", "web", 1, r#"{"image":"x"}"#)
            .await
            .unwrap();
        let id = inst[0].id.clone();
        store.bind_instance_to_node(&id, "n1").await.unwrap();
        store
            .update_instance_status(&id, "Running", Some("demo-web-0"), Some("ok"))
            .await
            .unwrap();
        let unbound = store.unbind_instance(&id).await.unwrap();
        assert_eq!(unbound.phase, "Pending");
        assert!(unbound.node_id.is_none());
        assert!(unbound.runtime_id.is_none());
        assert!(store.list_pending_instances().await.unwrap().len() == 1);
    }

    #[tokio::test]
    async fn secret_encrypt_store_roundtrip() {
        let dir = tempdir().unwrap();
        let db = dir.path().join("mcc.db");
        let store = SqliteStore::open(&db).await.unwrap();
        store.init_cluster("", "").await.unwrap();
        let key = SecretsKey::from_bytes([9u8; 32]);
        let (n, c) = key.encrypt(b"p@ss").unwrap();
        store.put_secret_blob("DB_PASS", &n, &c).await.unwrap();
        let blob = store.get_secret_blob("DB_PASS").await.unwrap().unwrap();
        assert_eq!(key.decrypt(&blob.nonce, &blob.ciphertext).unwrap(), b"p@ss");
        let list = store.list_secret_meta().await.unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "DB_PASS");
        assert!(store.delete_secret("DB_PASS").await.unwrap());
    }
}
