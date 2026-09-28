//! Data directory bootstrap: SQLite path, secrets key, first-run token.
//!
//! A token is **always** created on first init — `--no-auth` is a per-start
//! server switch, never persisted. The plaintext is delivered once by
//! [`deliver_bootstrap_token`].
//!
//! The secrets key and `mc2.db` are a pair: once the database holds secrets, a
//! missing key file is a **startup error** (generating a new one would render
//! the stored ciphertext undecryptable) and an existing key must decrypt a
//! stored row before the server starts.

use anyhow::{anyhow, bail, Context, Result};
use mc2_store::{hash_token, SecretsKey, SqliteStore, Store};
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
    /// The verified secrets key (generated on first run, otherwise read back
    /// and checked against the stored ciphertext).
    pub secrets_key: SecretsKey,
    /// Present only when this process created the cluster for the first time.
    pub fresh_credentials: Option<FreshCredentials>,
    /// This install's random id, generated once and persisted (B2).
    ///
    /// Every sandbox is labelled `mc2.install=<id>`, so the node recovers
    /// ownership of its own VMs after a restart and never touches another
    /// install's workload.
    pub install_id: String,
}

/// Bootstrap configuration.
pub struct Bootstrap {
    pub data_dir: PathBuf,
    pub secrets_key_path: PathBuf,
}

impl Bootstrap {
    /// Create data dir, secrets key, open DB, init cluster token if needed.
    ///
    /// The database is opened **before** the key is resolved: if it already
    /// holds secrets, the key that encrypted them is mandatory.
    pub async fn ensure(self) -> Result<BootstrapResult> {
        std::fs::create_dir_all(&self.data_dir)
            .with_context(|| format!("mkdir {}", self.data_dir.display()))?;

        let db_path = self.data_dir.join("mc2.db");
        let store = SqliteStore::open(&db_path).await?;

        let secrets_key =
            resolve_secrets_key(&self.secrets_key_path, &db_path, store.as_ref()).await?;

        let install_id = ensure_install_id(store.as_ref()).await?;

        let fresh_credentials = if store.get_cluster_meta().await?.is_none() {
            let api_token = generate_api_token();
            store
                .init_cluster(&hash_token(&api_token))
                .await
                .context("init cluster meta")?;
            info!("initialized new cluster in {}", self.data_dir.display());
            Some(FreshCredentials { api_token })
        } else {
            None
        };

        Ok(BootstrapResult {
            db_path,
            secrets_key_path: self.secrets_key_path,
            secrets_key,
            fresh_credentials,
            install_id,
        })
    }
}

/// Read this install's id, generating and persisting it on first use.
///
/// The id lives in the settings table (not a file), so it moves with the
/// database: restoring `mc2.db` keeps ownership of the VMs that database's
/// sandboxes are labelled with.
pub async fn ensure_install_id(store: &dyn Store) -> Result<String> {
    if let Some(id) = store.get_setting(mc2_store::SETTING_INSTALL_ID).await? {
        if !id.is_empty() {
            return Ok(id);
        }
    }
    let id = generate_token("mc2in");
    store
        .set_setting(mc2_store::SETTING_INSTALL_ID, &id)
        .await
        .context("persist install id")?;
    info!(install_id = %id, "generated install id (labels every sandbox)");
    Ok(id)
}

/// Generate a fresh operator API token (`mc2at_…`).
pub fn generate_api_token() -> String {
    generate_token("mc2at")
}

fn generate_token(prefix: &str) -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    format!("{prefix}_{}", hex::encode(bytes))
}

/// Generate a new operator token, replace the stored hash, return the plaintext.
///
/// Used by `mc2 server token rotate`: the caller writes the DB directly, so
/// rotation works while the server runs (tokens are checked per request).
pub async fn rotate_api_token(store: &dyn Store) -> Result<String> {
    let token = generate_api_token();
    store
        .replace_api_token_hash(&hash_token(&token))
        .await
        .context("replace API token hash")?;
    Ok(token)
}

/// Deliver the one-time bootstrap token.
///
/// On a TTY it goes to stderr so it never lands in a pipe or a log line.
/// Otherwise it is written to `<data-dir>/bootstrap-token` (mode 0600) and only
/// the path is logged — token values never reach journald/container logs.
pub fn deliver_bootstrap_token(data_dir: &Path, token: &str) -> Result<()> {
    use std::io::IsTerminal;
    if std::io::stderr().is_terminal() {
        eprintln!("=== MicroCommandControl bootstrap credentials (save these; shown once) ===");
        eprintln!("API token  (REST Authorization: Bearer …): {token}");
        eprintln!("Data dir: {}", data_dir.display());
        eprintln!("=========================================================================");
        Ok(())
    } else {
        let path = write_bootstrap_token_file(data_dir, token)?;
        // Plain stderr, not tracing: the operator must see where the token is
        // regardless of log filtering.
        eprintln!(
            "bootstrap API token written to {} (mode 0600 — read it, then delete the file)",
            path.display()
        );
        Ok(())
    }
}

/// Write the bootstrap token to `<data-dir>/bootstrap-token` (mode 0600).
pub fn write_bootstrap_token_file(data_dir: &Path, token: &str) -> Result<PathBuf> {
    let path = data_dir.join("bootstrap-token");
    write_private(&path, format!("{token}\n").as_bytes(), false)
        .with_context(|| format!("write {}", path.display()))?;
    Ok(path)
}

/// Resolve the secrets key for this data directory.
///
/// * No key file and no stored secrets → generate a fresh 32-byte key (first
///   run, or a data dir that has never held a secret).
/// * No key file but stored secrets → **error**. Generating a new key here
///   would silently render every stored ciphertext undecryptable (and encrypt
///   future secrets under a different key), so the operator must restore the
///   original file.
/// * Key file present → validate it, and confirm it decrypts a stored secret.
async fn resolve_secrets_key(path: &Path, db_path: &Path, store: &dyn Store) -> Result<SecretsKey> {
    if let Some(key) = read_secrets_key(path)? {
        verify_secrets_key(path, store, &key).await?;
        return Ok(key);
    }

    let stored = store.list_secret_meta().await?;
    if let Some(first) = stored.first() {
        bail!(
            "secrets key {} is missing, but the database {} already holds {} stored \
             secret(s) (e.g. `{}`). Restore the original key file to that path — without it \
             the stored ciphertext is unrecoverable. If those values are gone for good, delete \
             them with `mc2 server secrets purge --data-dir <dir> --yes`, start the server, and \
             set them again with `mc2 secret set <name>`.",
            path.display(),
            db_path.display(),
            stored.len(),
            first.name,
        );
    }

    generate_secrets_key(path)?;
    SecretsKey::load_file(path).with_context(|| format!("load {}", path.display()))
}

/// Load and validate an existing key file; `None` when the path does not exist.
///
/// A key must be a regular file (not a symlink), ≥ 32 bytes, and not
/// world-readable; group-readable warns.
fn read_secrets_key(path: &Path) -> Result<Option<SecretsKey>> {
    let meta = match std::fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("stat {}", path.display())),
    };
    if meta.file_type().is_symlink() {
        bail!(
            "secrets key {} is a symlink; refusing to use it — point --secrets-key-path \
             at the real file (or copy it there)",
            path.display()
        );
    }
    if meta.len() < 32 {
        bail!(
            "secrets key {} is too short (need 32 bytes)",
            path.display()
        );
    }
    #[cfg(unix)]
    check_key_permissions(path, &meta)?;
    let key = SecretsKey::load_file(path).with_context(|| format!("load {}", path.display()))?;
    Ok(Some(key))
}

/// Refuse to start when `key` cannot decrypt a stored secret.
///
/// The secret name is AEAD associated data (`SecretsKey::decrypt`), so a key
/// from another data directory — or a rotated key file — fails authentication.
async fn verify_secrets_key(path: &Path, store: &dyn Store, key: &SecretsKey) -> Result<()> {
    let Some(meta) = store.list_secret_meta().await?.into_iter().next() else {
        return Ok(());
    };
    let blob = store.get_secret_blob(&meta.name).await?.with_context(|| {
        format!(
            "secret `{}` disappeared while verifying the secrets key",
            meta.name
        )
    })?;
    key.decrypt(&meta.name, &blob.nonce, &blob.ciphertext)
        .map_err(|_| {
            anyhow!(
                "wrong secrets key {}: it cannot decrypt the stored secret `{}`. Restore the \
                 key that encrypted this database. If it is lost, delete the stored secrets \
                 with `mc2 server secrets purge --data-dir <dir> --yes` and set them again.",
                path.display(),
                meta.name
            )
        })?;
    Ok(())
}

/// Generate a fresh 32-byte key, created with mode 0600 from the start on Unix.
fn generate_secrets_key(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let mut key = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut key);
    write_private(path, &key, true).with_context(|| format!("write {}", path.display()))?;
    info!(path = %path.display(), "wrote new secrets key");
    Ok(())
}

/// Refuse a world-readable key; warn on group-readable.
#[cfg(unix)]
fn check_key_permissions(path: &Path, meta: &std::fs::Metadata) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mode = meta.permissions().mode() & 0o777;
    if mode & 0o004 != 0 {
        bail!(
            "secrets key {} is world-readable (mode {mode:03o}); run `chmod 600 {}` and restart",
            path.display(),
            path.display()
        );
    }
    if mode & 0o040 != 0 {
        tracing::warn!(
            path = %path.display(),
            mode = format!("{mode:03o}"),
            "secrets key is group-readable; consider `chmod 600 {}`",
            path.display()
        );
    }
    Ok(())
}

/// Write `contents` to `path`, creating it with mode 0600 on Unix.
///
/// `create_new` fails when the file already exists (secrets key); otherwise an
/// existing file is replaced (bootstrap token file).
fn write_private(path: &Path, contents: &[u8], create_new: bool) -> Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write as _;
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .create_new(create_new)
            .truncate(!create_new)
            .mode(0o600)
            .open(path)?;
        f.write_all(contents)?;
        f.sync_all()?;
        // `mode()` only applies at creation; enforce 0600 on overwrite too.
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(not(unix))]
    {
        let _ = create_new;
        std::fs::write(path, contents)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use mc2_store::MemoryStore;

    #[tokio::test]
    async fn bootstrap_twice_only_fresh_once() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().to_path_buf();
        let key = data.join("secrets.key");

        let r1 = Bootstrap {
            data_dir: data.clone(),
            secrets_key_path: key.clone(),
        }
        .ensure()
        .await
        .unwrap();
        assert!(r1.fresh_credentials.is_some());

        let r2 = Bootstrap {
            data_dir: data,
            secrets_key_path: key,
        }
        .ensure()
        .await
        .unwrap();
        assert!(r2.fresh_credentials.is_none());
    }

    #[tokio::test]
    async fn bootstrap_always_creates_a_token() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().to_path_buf();
        let key = data.join("secrets.key");
        let r = Bootstrap {
            data_dir: data.clone(),
            secrets_key_path: key,
        }
        .ensure()
        .await
        .unwrap();

        let token = r.fresh_credentials.unwrap().api_token;
        assert!(token.starts_with("mc2at_"), "{token}");
        let store = SqliteStore::open(data.join("mc2.db")).await.unwrap();
        assert!(store.verify_api_token(&token).await.unwrap());
        assert!(!store.verify_api_token("wrong").await.unwrap());
        assert!(!store.verify_api_token("").await.unwrap());
    }

    #[tokio::test]
    async fn rotate_replaces_the_stored_hash() {
        let store = MemoryStore::new();
        store.init_cluster(&hash_token("old")).await.unwrap();

        let new = rotate_api_token(store.as_ref()).await.unwrap();
        assert!(new.starts_with("mc2at_"), "{new}");
        assert!(store.verify_api_token(&new).await.unwrap());
        assert!(!store.verify_api_token("old").await.unwrap());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn secrets_key_created_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().to_path_buf();
        let key = data.join("secrets.key");
        Bootstrap {
            data_dir: data,
            secrets_key_path: key.clone(),
        }
        .ensure()
        .await
        .unwrap();
        let mode = std::fs::metadata(&key).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "secrets key must be 0600");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn world_readable_secrets_key_refuses_to_start() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().to_path_buf();
        let key = data.join("secrets.key");
        std::fs::create_dir_all(&data).unwrap();
        std::fs::write(&key, [0u8; 32]).unwrap();
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o644)).unwrap();

        let err = Bootstrap {
            data_dir: data.clone(),
            secrets_key_path: key.clone(),
        }
        .ensure()
        .await
        .unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("chmod 600"), "{msg}");

        // Group-readable only → warning, still starts.
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o640)).unwrap();
        Bootstrap {
            data_dir: data,
            secrets_key_path: key,
        }
        .ensure()
        .await
        .unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symlinked_secrets_key_refuses_to_start() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().to_path_buf();
        std::fs::create_dir_all(&data).unwrap();
        let real = data.join("real.key");
        std::fs::write(&real, [0u8; 32]).unwrap();
        let link = data.join("secrets.key");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        let err = Bootstrap {
            data_dir: data,
            secrets_key_path: link,
        }
        .ensure()
        .await
        .unwrap_err();
        assert!(format!("{err:#}").contains("symlink"), "{err:#}");
    }

    #[cfg(unix)]
    #[test]
    fn bootstrap_token_file_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = write_bootstrap_token_file(dir.path(), "mc2at_secret").unwrap();
        assert_eq!(path, dir.path().join("bootstrap-token"));
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap().trim(),
            "mc2at_secret"
        );
    }

    // --- secrets key vs existing database (B5) ---

    /// Fresh data dir → a key is generated and returned.
    #[tokio::test]
    async fn fresh_data_dir_generates_a_key() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().to_path_buf();
        let key_path = data.join("secrets.key");

        let boot = Bootstrap {
            data_dir: data,
            secrets_key_path: key_path.clone(),
        }
        .ensure()
        .await
        .unwrap();

        assert!(key_path.is_file(), "{}", key_path.display());
        assert_eq!(std::fs::metadata(&key_path).unwrap().len(), 32);
        assert_eq!(boot.secrets_key_path, key_path);
    }

    /// An existing DB whose secrets were encrypted with a key that is still
    /// present bootstraps fine, and the returned key decrypts them.
    #[tokio::test]
    async fn existing_key_still_decrypts_stored_secrets() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().to_path_buf();
        let key_path = data.join("secrets.key");

        let _boot = bootstrap_with_secret(&data, &key_path, "DB_PASS", b"hunter2").await;

        let again = Bootstrap {
            data_dir: data,
            secrets_key_path: key_path,
        }
        .ensure()
        .await
        .unwrap();
        assert!(again.fresh_credentials.is_none());
        let store = SqliteStore::open(&again.db_path).await.unwrap();
        let blob = store.get_secret_blob("DB_PASS").await.unwrap().unwrap();
        assert_eq!(
            again
                .secrets_key
                .decrypt("DB_PASS", &blob.nonce, &blob.ciphertext)
                .unwrap(),
            b"hunter2"
        );
    }

    /// Restoring a DB without its key must not silently mint a new one: the
    /// stored ciphertext would become undecryptable.
    #[tokio::test]
    async fn missing_key_with_stored_secrets_refuses_to_start() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().to_path_buf();
        let key_path = data.join("secrets.key");
        bootstrap_with_secret(&data, &key_path, "DB_PASS", b"hunter2").await;

        std::fs::remove_file(&key_path).unwrap();

        let err = Bootstrap {
            data_dir: data,
            secrets_key_path: key_path.clone(),
        }
        .ensure()
        .await
        .unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains(&key_path.display().to_string()), "{msg}");
        assert!(msg.contains("DB_PASS"), "{msg}");
        assert!(msg.contains("unrecoverable"), "{msg}");
        assert!(msg.contains("mc2 server secrets purge"), "{msg}");
        // The refusal must not have created a replacement key.
        assert!(!key_path.exists(), "no new key may be written");
    }

    /// A key from somewhere else must be rejected before the server starts.
    #[tokio::test]
    async fn wrong_key_refuses_to_start() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().to_path_buf();
        let key_path = data.join("secrets.key");
        bootstrap_with_secret(&data, &key_path, "DB_PASS", b"hunter2").await;

        write_private(&key_path, &[7u8; 32], false).unwrap();

        let err = Bootstrap {
            data_dir: data,
            secrets_key_path: key_path,
        }
        .ensure()
        .await
        .unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("wrong secrets key"), "{msg}");
        assert!(msg.contains("DB_PASS"), "{msg}");
    }

    /// A DB with no secrets yet is not blocked by a missing key: one is made.
    #[tokio::test]
    async fn missing_key_without_stored_secrets_is_regenerated() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().to_path_buf();
        let key_path = data.join("secrets.key");
        // First run creates DB + key (cluster token only, no secrets).
        Bootstrap {
            data_dir: data.clone(),
            secrets_key_path: key_path.clone(),
        }
        .ensure()
        .await
        .unwrap();

        std::fs::remove_file(&key_path).unwrap();

        let boot = Bootstrap {
            data_dir: data,
            secrets_key_path: key_path.clone(),
        }
        .ensure()
        .await
        .unwrap();
        assert!(key_path.is_file());
        assert!(boot.fresh_credentials.is_none());
    }

    /// Bootstrap a data dir and store one encrypted secret, returning the boot
    /// result that carries the key used.
    async fn bootstrap_with_secret(
        data: &Path,
        key_path: &Path,
        name: &str,
        value: &[u8],
    ) -> BootstrapResult {
        let boot = Bootstrap {
            data_dir: data.to_path_buf(),
            secrets_key_path: key_path.to_path_buf(),
        }
        .ensure()
        .await
        .unwrap();
        let store = SqliteStore::open(&boot.db_path).await.unwrap();
        let (nonce, ciphertext) = boot.secrets_key.encrypt(name, value).unwrap();
        store
            .put_secret_blob(name, &nonce, &ciphertext)
            .await
            .unwrap();
        boot
    }
}
