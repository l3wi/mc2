//! SQLite-backed store (default production backend).

use crate::{
    verify_token, ClusterCounts, ClusterMeta, NodeHeartbeat, NodeJoin, NodeRecord, Store,
    StoreError,
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
        Ok(verify_token(token, &meta.api_token_hash))
    }

    async fn verify_join_token(&self, token: &str) -> Result<bool, StoreError> {
        let Some(meta) = self.get_cluster_meta().await? else {
            return Ok(false);
        };
        Ok(verify_token(token, &meta.join_token_hash))
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

        Ok(ClusterCounts {
            nodes_ready: nodes_ready as u32,
            nodes_total: nodes_total as u32,
            stacks: stacks as u32,
            instances: 0,
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{hash_token, NodeStatus};
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
}
