//! Data directory bootstrap: SQLite path, secrets key, first-run tokens.

use anyhow::{Context, Result};
use mc2_store::{hash_token, SqliteStore, Store};
use rand::RngCore;
use std::path::{Path, PathBuf};
use tracing::info;

/// Expand a leading `~` in a path string.
pub fn expand_data_dir(raw: &str) -> PathBuf {
    if let Some(rest) = raw.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    if raw == "~" {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home);
        }
    }
    PathBuf::from(raw)
}

/// Fresh plaintext credentials (shown once on first init).
#[derive(Debug, Clone)]
pub struct FreshCredentials {
    pub api_token: String,
}

/// Result of ensuring the data directory is ready.
#[derive(Debug)]
pub struct BootstrapResult {
    pub db_path: PathBuf,
    pub secrets_key_path: PathBuf,
    /// Present only when this process created the cluster for the first time.
    pub fresh_credentials: Option<FreshCredentials>,
}

/// Bootstrap configuration.
pub struct Bootstrap {
    pub data_dir: PathBuf,
    pub secrets_key_path: PathBuf,
    /// When true, initialize without an API token (open lab cluster).
    pub no_auth: bool,
}

impl Bootstrap {
    /// Create data dir, secrets key, open DB, init cluster token if needed.
    pub async fn ensure(self) -> Result<BootstrapResult> {
        std::fs::create_dir_all(&self.data_dir)
            .with_context(|| format!("mkdir {}", self.data_dir.display()))?;

        ensure_secrets_key(&self.secrets_key_path)?;

        let db_path = self.data_dir.join("mc2.db");
        let store = SqliteStore::open(&db_path).await?;

        let fresh_credentials = if store.get_cluster_meta().await?.is_none() {
            if self.no_auth {
                // Empty hashes ⇒ auth not required (see Store::verify_*).
                store
                    .init_cluster("")
                    .await
                    .context("init open cluster meta")?;
                info!(
                    "initialized open cluster (no auth) in {}",
                    self.data_dir.display()
                );
                None
            } else {
                let api_token = generate_token("mc2at");
                store
                    .init_cluster(&hash_token(&api_token))
                    .await
                    .context("init cluster meta")?;
                info!("initialized new cluster in {}", self.data_dir.display());
                Some(FreshCredentials { api_token })
            }
        } else {
            None
        };

        Ok(BootstrapResult {
            db_path,
            secrets_key_path: self.secrets_key_path,
            fresh_credentials,
        })
    }
}

fn generate_token(prefix: &str) -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    format!("{prefix}_{}", hex::encode(bytes))
}

/// Ensure a 32-byte secrets key file exists (mode 0600 on Unix).
fn ensure_secrets_key(path: &Path) -> Result<()> {
    if path.exists() {
        let meta = std::fs::metadata(path)?;
        if meta.len() < 32 {
            anyhow::bail!(
                "secrets key {} is too short (need 32 bytes)",
                path.display()
            );
        }
        return Ok(());
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let mut key = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut key);
    std::fs::write(path, key).with_context(|| format!("write {}", path.display()))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o600);
        std::fs::set_permissions(path, perms)?;
    }

    info!(path = %path.display(), "wrote new secrets key");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn bootstrap_twice_only_fresh_once() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().to_path_buf();
        let key = data.join("secrets.key");

        let r1 = Bootstrap {
            data_dir: data.clone(),
            secrets_key_path: key.clone(),
            no_auth: false,
        }
        .ensure()
        .await
        .unwrap();
        assert!(r1.fresh_credentials.is_some());

        let r2 = Bootstrap {
            data_dir: data,
            secrets_key_path: key,
            no_auth: false,
        }
        .ensure()
        .await
        .unwrap();
        assert!(r2.fresh_credentials.is_none());
    }

    #[tokio::test]
    async fn bootstrap_no_auth() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().to_path_buf();
        let key = data.join("secrets.key");
        let r = Bootstrap {
            data_dir: data.clone(),
            secrets_key_path: key,
            no_auth: true,
        }
        .ensure()
        .await
        .unwrap();
        assert!(r.fresh_credentials.is_none());
        let store = SqliteStore::open(data.join("mc2.db")).await.unwrap();
        assert!(!store.api_auth_required().await.unwrap());
        assert!(store.verify_api_token("").await.unwrap());
    }
}
