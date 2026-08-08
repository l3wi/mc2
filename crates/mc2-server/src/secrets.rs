//! Cluster secret set/get helpers (encrypt at rest; never log values).

use anyhow::{Context, Result};
use mc2_api::SecretRef;
use mc2_store::{SecretMeta, SecretsKey, Store};
use std::collections::BTreeMap;
use std::sync::Arc;

/// Secret refs to actually inject: explicit `environment:` overrides
/// `secrets[].env` on a name collision (env wins). Returns the refs to inject
/// and the names of dropped colliding env vars (for a warning).
pub fn filter_secret_refs<'a>(
    refs: &'a [SecretRef],
    env: &BTreeMap<String, String>,
) -> (Vec<&'a SecretRef>, Vec<&'a str>) {
    let mut to_inject: Vec<&'a SecretRef> = Vec::new();
    let mut dropped: Vec<&'a str> = Vec::new();
    for r in refs {
        if env.contains_key(&r.env) {
            dropped.push(r.env.as_str());
        } else {
            to_inject.push(r);
        }
    }
    (to_inject, dropped)
}

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
    refs: &[&SecretRef],
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn refs() -> Vec<SecretRef> {
        vec![
            SecretRef {
                name: "TOKEN".into(),
                env: "API_TOKEN".into(),
                allow_hosts: vec!["api.example.com".into()],
            },
            SecretRef {
                name: "DB_PASS".into(),
                env: "DB_PASS".into(),
                allow_hosts: vec!["db.example.com".into()],
            },
        ]
    }

    #[test]
    fn environment_overrides_colliding_secret_env() {
        let r = refs();
        let env = BTreeMap::from([("API_TOKEN".to_string(), "inline".to_string())]);
        let (to_inject, dropped) = filter_secret_refs(&r, &env);
        assert_eq!(dropped, vec!["API_TOKEN"]);
        assert_eq!(to_inject.len(), 1);
        assert_eq!(to_inject[0].name, "DB_PASS");
        assert_eq!(to_inject[0].env, "DB_PASS");
    }

    #[test]
    fn no_collision_injects_all() {
        let r = refs();
        let env = BTreeMap::new();
        let (to_inject, dropped) = filter_secret_refs(&r, &env);
        assert!(dropped.is_empty());
        assert_eq!(to_inject.len(), 2);
    }

    #[test]
    fn collision_only_on_exact_env_name() {
        let r = refs();
        let env = BTreeMap::from([("API_TOKEN_X".to_string(), "1".to_string())]);
        let (to_inject, dropped) = filter_secret_refs(&r, &env);
        assert!(dropped.is_empty());
        assert_eq!(to_inject.len(), 2);
    }
}
