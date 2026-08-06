//! SQLite-backed store (default production backend).

use crate::{verify_token, ClusterCounts, ClusterMeta, Store, StoreError};
use anyhow::{Context, Result as AnyResult};
use async_trait::async_trait;
use chrono::Utc;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;

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

        // instances table not yet created — always 0 in Phase 1
        Ok(ClusterCounts {
            nodes_ready: nodes_ready as u32,
            nodes_total: nodes_total as u32,
            stacks: stacks as u32,
            instances: 0,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hash_token;
    use tempfile::tempdir;

    #[tokio::test]
    async fn sqlite_init_and_status() {
        let dir = tempdir().unwrap();
        let db = dir.path().join("mcc.db");
        let store = SqliteStore::open(&db).await.unwrap();

        assert!(store.get_cluster_meta().await.unwrap().is_none());

        store
            .init_cluster(&hash_token("api"), &hash_token("join"))
            .await
            .unwrap();

        assert!(store.verify_api_token("api").await.unwrap());
        let counts = store.cluster_counts().await.unwrap();
        assert_eq!(counts.nodes_total, 0);
        assert_eq!(counts.stacks, 0);
    }
}
