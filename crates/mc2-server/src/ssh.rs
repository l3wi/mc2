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

    if key_names.is_empty() {
        anyhow::bail!("ssh enabled but authorizedKeys is empty");
    }

    let mut public_keys = Vec::with_capacity(key_names.len());
    for name in &key_names {
        let key = store
            .get_ssh_key(name)
            .await
            .context("load ssh key")?
            .with_context(|| format!("ssh key not found: {name}"))?;
        public_keys.push(key.public_key);
    }

    let _ = key_names;
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
        phase: "Closed".into(),
        bind: None,
        port: None,
        message: None,
        updated_at: String::new(),
    }
}
