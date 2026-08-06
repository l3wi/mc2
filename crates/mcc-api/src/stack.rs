//! Compose-like stack YAML schema (mcc/v1).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Top-level stack document.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StackDocument {
    pub api_version: String,
    pub kind: String,
    pub metadata: StackMetadata,
    pub services: BTreeMap<String, ServiceSpec>,
    #[serde(default)]
    pub volumes: BTreeMap<String, VolumeSpec>,
    /// Reserved — not implemented in MVP.
    #[serde(default)]
    pub ingress: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StackMetadata {
    pub name: String,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceSpec {
    pub image: String,
    #[serde(default = "default_replicas")]
    pub replicas: u32,
    #[serde(default)]
    pub resources: ResourceSpec,
    #[serde(default)]
    pub ports: Vec<PortSpec>,
    #[serde(default)]
    pub network: NetworkSpec,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub secrets: Vec<SecretRef>,
    #[serde(default)]
    pub volumes: Vec<VolumeMount>,
    #[serde(default = "default_restart")]
    pub restart_policy: String,
    #[serde(default)]
    pub health: Option<HealthSpec>,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
    #[serde(default)]
    pub command: Option<Vec<String>>,
    /// Hard pin to a node name.
    #[serde(default)]
    pub node_name: Option<String>,
    /// Soft placement by node labels.
    #[serde(default)]
    pub node_selector: BTreeMap<String, String>,
    /// Host-side msb SSH serve (not guest sshd). Optional.
    #[serde(default)]
    pub ssh: Option<SshSpec>,
}

/// Desired host-side SSH front end for a service (msb `ssh` feature).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SshSpec {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_ssh_bind")]
    pub bind: String,
    /// Host port on the agent; `0` = auto-allocate.
    #[serde(default)]
    pub port: u16,
    #[serde(default = "default_ssh_user")]
    pub user: String,
    #[serde(default = "default_true")]
    pub sftp: bool,
    /// Cluster key names (`mcc ssh-key` / `/v1/ssh/keys`).
    #[serde(default)]
    pub authorized_keys: Vec<String>,
}

fn default_ssh_bind() -> String {
    "127.0.0.1".into()
}

fn default_ssh_user() -> String {
    "root".into()
}

fn default_true() -> bool {
    true
}

fn default_replicas() -> u32 {
    1
}

fn default_restart() -> String {
    "on-failure".into()
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceSpec {
    #[serde(default = "default_cpus")]
    pub cpus: u32,
    #[serde(default = "default_memory", rename = "memoryMiB")]
    pub memory_mib: u64,
}

fn default_cpus() -> u32 {
    1
}

fn default_memory() -> u64 {
    512
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PortSpec {
    pub host: u16,
    pub guest: u16,
    #[serde(default = "default_proto")]
    pub protocol: String,
    #[serde(default = "default_bind")]
    pub bind: String,
}

fn default_proto() -> String {
    "tcp".into()
}

fn default_bind() -> String {
    "127.0.0.1".into()
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NetworkSpec {
    #[serde(default)]
    pub profiles: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SecretRef {
    pub name: String,
    pub env: String,
    #[serde(default)]
    pub allow_hosts: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VolumeMount {
    pub name: String,
    pub mount: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VolumeSpec {
    #[serde(default = "default_vol_kind")]
    pub kind: String,
}

fn default_vol_kind() -> String {
    "dir".into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthSpec {
    pub kind: String,
    #[serde(default)]
    pub command: Vec<String>,
    #[serde(default = "default_health_interval")]
    pub interval_seconds: u32,
}

fn default_health_interval() -> u32 {
    30
}

/// Parse and validate a stack YAML document.
pub fn parse_stack_yaml(yaml: &str) -> Result<StackDocument, String> {
    let doc: StackDocument =
        serde_yaml::from_str(yaml).map_err(|e| format!("invalid stack YAML: {e}"))?;
    validate_stack(&doc)?;
    Ok(doc)
}

fn validate_stack(doc: &StackDocument) -> Result<(), String> {
    if doc.api_version != crate::API_VERSION {
        return Err(format!(
            "unsupported apiVersion {:?} (want {})",
            doc.api_version,
            crate::API_VERSION
        ));
    }
    if doc.kind != "Stack" {
        return Err(format!("unsupported kind {:?} (want Stack)", doc.kind));
    }
    if doc.metadata.name.trim().is_empty() {
        return Err("metadata.name is required".into());
    }
    if doc.ingress.is_some() {
        return Err(
            "ingress is reserved and not implemented in MVP (remove the ingress key)".into(),
        );
    }
    if doc.services.is_empty() {
        return Err("services must not be empty".into());
    }
    for (name, svc) in &doc.services {
        if svc.image.trim().is_empty() {
            return Err(format!("service {name}: image is required"));
        }
        if svc.replicas == 0 {
            return Err(format!("service {name}: replicas must be >= 1 for MVP"));
        }
        let rp = svc.restart_policy.trim().to_ascii_lowercase();
        if !matches!(rp.as_str(), "always" | "on-failure" | "never") {
            return Err(format!(
                "service {name}: restartPolicy must be always|on-failure|never (got {:?})",
                svc.restart_policy
            ));
        }
        if let Some(ref h) = svc.health {
            let kind = h.kind.trim().to_ascii_lowercase();
            if !matches!(kind.as_str(), "exec" | "none") {
                return Err(format!(
                    "service {name}: health.kind must be exec|none (got {:?})",
                    h.kind
                ));
            }
            if kind == "exec" && h.command.is_empty() {
                return Err(format!(
                    "service {name}: health.kind=exec requires a non-empty command"
                ));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEMO: &str = r#"
apiVersion: mcc/v1
kind: Stack
metadata:
  name: demo
services:
  web:
    image: python:3.12
    replicas: 2
    resources:
      cpus: 1
      memoryMiB: 512
"#;

    #[test]
    fn parse_demo() {
        let doc = parse_stack_yaml(DEMO).unwrap();
        assert_eq!(doc.metadata.name, "demo");
        assert_eq!(doc.services["web"].replicas, 2);
        assert_eq!(doc.services["web"].resources.memory_mib, 512);
    }

    #[test]
    fn rejects_ingress() {
        let yaml = r#"
apiVersion: mcc/v1
kind: Stack
metadata:
  name: x
services:
  a:
    image: busybox
ingress:
  rules: []
"#;
        let err = parse_stack_yaml(yaml).unwrap_err();
        assert!(err.contains("ingress"), "{err}");
    }
}
