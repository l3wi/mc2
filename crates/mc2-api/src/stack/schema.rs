//! Stack YAML schema types (compose-style `mc2/v1`).
//!
//! Custom compose-value deserializers live in [`super::decode`]; validation
//! lives in [`super::validate`].

use crate::stack::decode::{
    de_command, de_depends_on, de_duration, de_env, de_expose, de_healthcheck_test, de_interval,
    de_mem_limit, de_ports, de_ssh,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Top-level stack document (Docker Compose-shaped).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StackDocument {
    /// Stack name (compose `name:`; derived from the file when absent).
    #[serde(default)]
    pub name: String,
    pub services: BTreeMap<String, ServiceSpec>,
    #[serde(default)]
    pub volumes: BTreeMap<String, VolumeSpec>,
    /// Logical network membership only (not a free mesh). See D13.
    #[serde(default)]
    pub networks: BTreeMap<String, StackNetworkSpec>,
    /// North–south HTTP(S) routes (file catalog → Traefik). D7.
    #[serde(default)]
    pub ingress: Option<IngressSpec>,
}

/// Stack-level Ingress (BYO Traefik via agent file export).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IngressSpec {
    #[serde(default)]
    pub tls: IngressTlsSpec,
    #[serde(default)]
    pub rules: Vec<IngressRule>,
    /// Traefik TCP routes, currently intended for host-side SSH endpoints.
    #[serde(default)]
    pub tcp: Vec<IngressTcpRoute>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IngressTcpRoute {
    pub name: String,
    pub entry_point: String,
    pub service: String,
}

/// TLS intent only — certs/ACME live in the proxy.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IngressTlsSpec {
    #[serde(default)]
    pub enabled: bool,
    /// Traefik certificate resolver name (static config).
    #[serde(default)]
    pub cert_resolver: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IngressRule {
    pub host: String,
    #[serde(default)]
    pub paths: Vec<IngressPath>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IngressPath {
    #[serde(default = "default_ingress_path")]
    pub path: String,
    /// `Prefix` (default) or `Exact`.
    #[serde(default = "default_path_type")]
    pub path_type: String,
    pub service: String,
    /// Guest port; must match a `ports[].guest` on the service.
    pub port: u16,
}

fn default_ingress_path() -> String {
    "/".into()
}

fn default_path_type() -> String {
    "Prefix".into()
}

/// Stack-level network group (membership / docs only in v1).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StackNetworkSpec {
    /// Only `mediated` is valid (D13).
    #[serde(default = "default_network_mode")]
    pub mode: String,
}

fn default_network_mode() -> String {
    "mediated".into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ServiceSpec {
    pub image: String,
    /// Replica count (compose `scale`).
    #[serde(default = "default_scale")]
    pub scale: u32,
    /// vCPUs (float, as compose `cpus`).
    #[serde(default = "default_cpus_f64")]
    pub cpus: f64,
    /// Guest memory MiB (compose `mem_limit`, accepts `512m`/`1g`/bytes).
    #[serde(
        default = "default_memory_mib",
        rename = "mem_limit",
        deserialize_with = "de_mem_limit"
    )]
    pub mem_limit_mib: u64,
    #[serde(default, deserialize_with = "de_ports")]
    pub ports: Vec<PortSpec>,
    #[serde(default)]
    pub network: NetworkSpec,
    /// Compose `environment` (map or `KEY=VALUE` list). Internal field stays
    /// `env`; the YAML/JSON key is `environment` (compose vocabulary).
    #[serde(default, rename = "environment", deserialize_with = "de_env")]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub secrets: Vec<SecretRef>,
    #[serde(default)]
    pub volumes: Vec<VolumeMount>,
    /// Restart policy (compose `restart`: no|on-failure|always|unless-stopped).
    #[serde(default = "default_restart")]
    pub restart: String,
    #[serde(default)]
    pub healthcheck: Option<HealthcheckSpec>,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
    /// Compose `command`: list form (`["echo", "hi"]`) or string form
    /// (`"echo hi"`, split with shell-like quoting).
    #[serde(default, deserialize_with = "de_command")]
    pub command: Option<Vec<String>>,
    /// Compose `depends_on`: list form (`[db]`) or map form
    /// (`{db: {condition: service_healthy}}`). Startup ordering only.
    #[serde(default, rename = "depends_on", deserialize_with = "de_depends_on")]
    pub depends_on: BTreeMap<String, DependsOnSpec>,
    /// Hard pin to a node name.
    #[serde(default)]
    pub node_name: Option<String>,
    /// Soft placement by node labels.
    #[serde(default)]
    pub node_selector: BTreeMap<String, String>,
    /// Host-side msb SSH serve (not guest sshd). Optional; `true` = defaults
    /// (auth from every registered key).
    #[serde(default, deserialize_with = "de_ssh")]
    pub ssh: Option<SshSpec>,
    /// Cluster-internal listeners (loopback publish; not LAN). D13 network.
    #[serde(default, deserialize_with = "de_expose")]
    pub expose: Vec<ExposeSpec>,
    /// Server-wide network membership (default-allow). Absent → the stack's
    /// implicit default network. Named networks span stacks.
    #[serde(default)]
    pub networks: Vec<String>,
}

/// Internal service listener (network `expose`). Compose list form
/// (`[5432]` / `["5432"]`) and map form (`{port, protocol, name}`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExposeSpec {
    pub port: u16,
    #[serde(default = "default_proto")]
    pub protocol: String,
    #[serde(default)]
    pub name: Option<String>,
}

/// Desired host-side SSH front end for a service (msb `ssh` feature).
/// Accepts the short boolean form (`ssh: true` → enabled, all defaults, auth
/// from every registered cluster key) or the long map form.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
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
    /// Cluster key names (`mc2 ssh key add …`). Empty → every registered key.
    #[serde(default)]
    pub authorized_keys: Vec<String>,
}

pub(crate) fn default_ssh_bind() -> String {
    "127.0.0.1".into()
}

pub(crate) fn default_ssh_user() -> String {
    "root".into()
}

pub(crate) fn default_true() -> bool {
    true
}

fn default_scale() -> u32 {
    1
}

fn default_restart() -> String {
    "no".into()
}

fn default_cpus_f64() -> f64 {
    1.0
}

fn default_memory_mib() -> u64 {
    512
}

/// Port mapping (compose `ports`), north-south. `published: 0` = auto host
/// port (allocated server-side at apply). `hostname` = sugar: route that
/// hostname to `target` via ingress.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PortSpec {
    /// Host port; `0` = auto-allocate.
    #[serde(default)]
    pub published: u16,
    /// In-guest port.
    #[serde(default)]
    pub target: u16,
    #[serde(default = "default_proto")]
    pub protocol: String,
    /// Hostname sugar (`"mcp.example.com:3000"`) → ingress route to `target`.
    #[serde(default)]
    pub hostname: Option<String>,
}

pub(crate) fn default_proto() -> String {
    "tcp".into()
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkSpec {
    #[serde(default)]
    pub profiles: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SecretRef {
    pub name: String,
    pub env: String,
    #[serde(default)]
    pub allow_hosts: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VolumeMount {
    pub name: String,
    /// Guest mount path (compose `target`).
    #[serde(rename = "target")]
    pub mount: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VolumeSpec {
    #[serde(default = "default_vol_kind")]
    pub kind: String,
}

fn default_vol_kind() -> String {
    "dir".into()
}

/// Exec healthcheck (compose `healthcheck`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HealthcheckSpec {
    /// Probe command (CMD / CMD-SHELL prefix stripped). None → disabled.
    #[serde(default, deserialize_with = "de_healthcheck_test")]
    pub test: Option<Vec<String>>,
    /// Probe interval in seconds (compose `interval`, e.g. `30s`).
    #[serde(
        default = "default_health_interval",
        rename = "interval",
        deserialize_with = "de_interval"
    )]
    pub interval_seconds: u32,
    /// Per-probe timeout in seconds (compose `timeout`, e.g. `5s`). 0 = no timeout.
    #[serde(default, rename = "timeout", deserialize_with = "de_duration")]
    pub timeout_seconds: u32,
    /// Consecutive failures before the service is marked unhealthy (compose `retries`).
    #[serde(default = "default_health_retries")]
    pub retries: u32,
    /// Startup grace period in seconds (compose `start_period`): probe failures
    /// within this window after start do not count toward `retries`.
    #[serde(default, rename = "start_period", deserialize_with = "de_duration")]
    pub start_period_seconds: u32,
    /// Compose `disable: true` → no healthcheck runs.
    #[serde(default)]
    pub disable: bool,
}

fn default_health_interval() -> u32 {
    30
}

fn default_health_retries() -> u32 {
    3
}

/// Compose `depends_on` condition entry (map form).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DependsOnSpec {
    /// `service_started` (default) or `service_healthy`.
    #[serde(default = "default_dep_condition")]
    pub condition: String,
}

impl DependsOnSpec {
    pub fn service_started() -> Self {
        Self {
            condition: "service_started".into(),
        }
    }
}

fn default_dep_condition() -> String {
    "service_started".into()
}
