//! SSH key registry helpers + resolve desired SSH for the node loop.

use anyhow::{Context, Result};
use mc2_api::SshSpec;
use mc2_runtime::DesiredSsh;
use mc2_store::{InstanceSshRecord, Store};
use sha2::{Digest, Sha256};
use std::sync::Arc;

/// Merge instance override with service YAML into a concrete desired SSH config.
pub async fn resolve_ssh_desired(
    store: Arc<dyn Store>,
    instance_id: &str,
    yaml_ssh: Option<&SshSpec>,
) -> Result<DesiredSsh> {
    let row = store
        .get_instance_ssh(instance_id)
        .await
        .context("get ssh")?;

    let (enabled, bind, port, user, sftp, key_names) = if let Some(r) = &row {
        if r.has_override {
            let names: Vec<String> = r
                .desired_key_names_json
                .as_ref()
                .and_then(|j| serde_json::from_str(j).ok())
                .unwrap_or_default();
            (
                r.desired,
                r.desired_bind.clone().unwrap_or_else(|| "127.0.0.1".into()),
                r.desired_port.unwrap_or(0),
                r.desired_user.clone().unwrap_or_else(|| "root".into()),
                r.desired_sftp.unwrap_or(true),
                names,
            )
        } else if let Some(spec) = yaml_ssh {
            (
                spec.enabled,
                spec.bind.clone(),
                spec.port,
                spec.user.clone(),
                spec.sftp,
                spec.authorized_keys.clone(),
            )
        } else {
            return Ok(DesiredSsh::default());
        }
    } else if let Some(spec) = yaml_ssh {
        (
            spec.enabled,
            spec.bind.clone(),
            spec.port,
            spec.user.clone(),
            spec.sftp,
            spec.authorized_keys.clone(),
        )
    } else {
        return Ok(DesiredSsh::default());
    };

    if !enabled {
        return Ok(DesiredSsh::default());
    }

    // Empty `authorizedKeys` → every registered cluster key (the short
    // `ssh: true` form). Still fails closed when none are registered.
    let key_names = if key_names.is_empty() {
        let all = store
            .list_ssh_keys()
            .await
            .context("list ssh keys")?
            .into_iter()
            .map(|k| k.name)
            .collect::<Vec<_>>();
        if all.is_empty() {
            anyhow::bail!(
                "ssh enabled but no keys registered — run `mc2 ssh key add NAME --file <pubkey>`"
            );
        }
        all
    } else {
        key_names
    };

    let mut public_keys = Vec::with_capacity(key_names.len());
    for name in &key_names {
        let key = store
            .get_ssh_key(name)
            .await
            .context("load ssh key")?
            .with_context(|| format!("ssh key not found: {name}"))?;
        public_keys.push(key.public_key);
    }

    let config_hash = hash_ssh_config(&bind, port, &user, sftp, &public_keys);
    Ok(DesiredSsh {
        enabled: true,
        bind,
        port,
        user,
        sftp,
        authorized_public_keys: public_keys,
        config_hash,
    })
}

fn hash_ssh_config(bind: &str, port: u16, user: &str, sftp: bool, keys: &[String]) -> String {
    let mut h = Sha256::new();
    h.update(bind.as_bytes());
    h.update(port.to_le_bytes());
    h.update(user.as_bytes());
    h.update([u8::from(sftp)]);
    for k in keys {
        h.update(k.as_bytes());
        h.update([0]);
    }
    hex::encode(h.finalize())
}

/// Build instance_ssh desired record from a PUT body.
pub fn desired_from_put(
    instance_id: &str,
    enabled: bool,
    bind: Option<String>,
    port: Option<u16>,
    user: Option<String>,
    sftp: Option<bool>,
    authorized_keys: Vec<String>,
) -> InstanceSshRecord {
    InstanceSshRecord {
        instance_id: instance_id.into(),
        has_override: true,
        desired: enabled,
        desired_bind: Some(bind.unwrap_or_else(|| "127.0.0.1".into())),
        desired_port: Some(port.unwrap_or(0)),
        desired_user: Some(user.unwrap_or_else(|| "root".into())),
        desired_sftp: Some(sftp.unwrap_or(true)),
        desired_key_names_json: Some(
            serde_json::to_string(&authorized_keys).unwrap_or_else(|_| "[]".into()),
        ),
        phase: mc2_runtime::SshPhase::Closed.as_str().into(),
        bind: None,
        port: None,
        message: None,
        updated_at: String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mc2_store::MemoryStore;

    const FAKE_KEY: &str =
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIJustAFakeKeyMaterialHere0000 test@mc2";

    fn enabled_spec() -> SshSpec {
        SshSpec {
            enabled: true,
            bind: "127.0.0.1".into(),
            port: 0,
            user: "root".into(),
            sftp: true,
            authorized_keys: vec![],
        }
    }

    #[tokio::test]
    async fn empty_authorized_keys_uses_all_registered() {
        let store = MemoryStore::new();
        store.init_cluster("").await.unwrap();
        store.put_ssh_key("dev", FAKE_KEY).await.unwrap();
        store
            .put_ssh_key("ci", &FAKE_KEY.replacen("test@mc2", "ci@mc2", 1))
            .await
            .unwrap();

        let d = resolve_ssh_desired(store.clone(), "i1", Some(&enabled_spec()))
            .await
            .unwrap();
        assert!(d.enabled);
        assert_eq!(d.authorized_public_keys.len(), 2, "all registered keys");
        assert!(!d.config_hash.is_empty());
    }

    #[tokio::test]
    async fn explicit_authorized_keys_win() {
        let store = MemoryStore::new();
        store.init_cluster("").await.unwrap();
        store.put_ssh_key("dev", FAKE_KEY).await.unwrap();
        store
            .put_ssh_key("ci", &FAKE_KEY.replacen("test@mc2", "ci@mc2", 1))
            .await
            .unwrap();
        let mut spec = enabled_spec();
        spec.authorized_keys = vec!["dev".into()];

        let d = resolve_ssh_desired(store.clone(), "i1", Some(&spec))
            .await
            .unwrap();
        assert_eq!(d.authorized_public_keys.len(), 1);
        assert_eq!(d.authorized_public_keys[0], FAKE_KEY);
    }

    #[tokio::test]
    async fn ssh_enabled_with_no_keys_fails_closed() {
        let store = MemoryStore::new();
        store.init_cluster("").await.unwrap();
        let err = resolve_ssh_desired(store.clone(), "i1", Some(&enabled_spec()))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("no keys registered"), "{err}");
    }
}
