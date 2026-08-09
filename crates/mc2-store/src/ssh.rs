//! SSH authorized keys + per-instance desired/observed SSH state.

use serde::{Deserialize, Serialize};

/// Cluster-registered OpenSSH public key (no private material).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SshAuthorizedKey {
    pub name: String,
    pub public_key: String,
    pub fingerprint: String,
    #[serde(default)]
    pub labels_json: String,
    pub created_at: String,
    pub updated_at: String,
}

/// Desired + observed SSH for one instance.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstanceSshRecord {
    pub instance_id: String,
    /// True when desired fields come from API override (not only YAML).
    pub has_override: bool,
    pub desired: bool,
    pub desired_bind: Option<String>,
    pub desired_port: Option<u16>,
    pub desired_user: Option<String>,
    pub desired_sftp: Option<bool>,
    /// JSON array of key names, e.g. `["lewi-laptop"]`.
    pub desired_key_names_json: Option<String>,
    /// Closed | Opening | Open | Failed
    pub phase: String,
    pub bind: Option<String>,
    pub port: Option<u16>,
    pub message: Option<String>,
    pub updated_at: String,
}

impl Default for InstanceSshRecord {
    fn default() -> Self {
        Self {
            instance_id: String::new(),
            has_override: false,
            desired: false,
            desired_bind: None,
            desired_port: None,
            desired_user: None,
            desired_sftp: None,
            desired_key_names_json: None,
            phase: "Closed".into(),
            bind: None,
            port: None,
            message: None,
            updated_at: String::new(),
        }
    }
}

/// Agent-reported network observed snapshot for one instance.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstanceNetworkRecord {
    pub instance_id: String,
    /// Pending | Ready | Failed | Mixed
    pub phase: String,
    pub observed_json: String,
    pub message: Option<String>,
    pub updated_at: String,
}

/// SHA256 fingerprint of an OpenSSH public key line (`SHA256:<hex>`).
pub fn ssh_fingerprint(public_key: &str) -> String {
    use sha2::{Digest, Sha256};
    let line = public_key.trim();
    let digest = Sha256::digest(line.as_bytes());
    format!("SHA256:{}", hex::encode(digest))
}

/// Validate a one-line OpenSSH public key.
pub fn validate_public_key(public_key: &str) -> Result<(), String> {
    let line = public_key.trim();
    if line.is_empty() {
        return Err("public key is empty".into());
    }
    let mut parts = line.split_whitespace();
    let alg = parts.next().unwrap_or("");
    let data = parts.next().unwrap_or("");
    let ok_alg = matches!(
        alg,
        "ssh-rsa"
            | "ssh-ed25519"
            | "ecdsa-sha2-nistp256"
            | "ecdsa-sha2-nistp384"
            | "ecdsa-sha2-nistp521"
    ) || alg.starts_with("sk-ssh-")
        || alg.starts_with("ssh-");
    if !ok_alg {
        return Err(format!("unsupported key type {alg:?}"));
    }
    if data.len() < 16 {
        return Err("public key blob too short".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_ed25519_line() {
        let k = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIJustAFakeKeyMaterialHere00 user@host";
        assert!(validate_public_key(k).is_ok());
        assert!(ssh_fingerprint(k).starts_with("SHA256:"));
    }

    #[test]
    fn rejects_empty() {
        assert!(validate_public_key("").is_err());
    }
}
