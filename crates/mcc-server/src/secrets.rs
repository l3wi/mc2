//! Cluster secret set/get helpers (encrypt at rest; never log values).

use anyhow::{Context, Result};
use mcc_api::SecretRef;
use mcc_store::{SecretMeta, SecretsKey, Store};
use std::sync::Arc;

/// Encrypt and store a secret value by name.
pub async fn set_secret(
    store: Arc<dyn Store>,
    key: &SecretsKey,
    name: &str,
    value: &str,
) -> Result<SecretMeta> {
    let name = name.trim();
    if name.is_empty() {
        anyhow::bail!("secret name is required");
    }
    if value.is_empty() {
        anyhow::bail!("secret value must not be empty");
    }
    let (nonce, ciphertext) = key.encrypt(value.as_bytes()).context("encrypt secret")?;
    store
        .put_secret_blob(name, &nonce, &ciphertext)
        .await
        .context("store secret")
}

/// Decrypt a secret by name (for agent injection only).
pub async fn decrypt_secret(store: Arc<dyn Store>, key: &SecretsKey, name: &str) -> Result<String> {
    let blob = store
        .get_secret_blob(name)
        .await
        .context("load secret")?
        .with_context(|| format!("secret not found: {name}"))?;
    let plain = key
        .decrypt(&blob.nonce, &blob.ciphertext)
        .context("decrypt secret")?;
    String::from_utf8(plain).context("secret is not valid utf-8")
}

/// Resolve service secret refs to injection material.
pub async fn resolve_injections(
    store: Arc<dyn Store>,
    key: &SecretsKey,
    refs: &[SecretRef],
) -> Result<Vec<ResolvedInjection>> {
    let mut out = Vec::with_capacity(refs.len());
    for r in refs {
        if r.env.trim().is_empty() {
            anyhow::bail!("secret ref {:?}: env is required", r.name);
        }
        if r.allow_hosts.is_empty() {
            anyhow::bail!(
                "secret ref {:?}: allowHosts must list at least one host (msb injection allowlist)",
                r.name
            );
        }
        let value = decrypt_secret(store.clone(), key, &r.name).await?;
        out.push(ResolvedInjection {
            env: r.env.clone(),
            value,
            allow_hosts: r.allow_hosts.clone(),
        });
    }
    Ok(out)
}

#[derive(Debug, Clone)]
pub struct ResolvedInjection {
    pub env: String,
    pub value: String,
    pub allow_hosts: Vec<String>,
}
