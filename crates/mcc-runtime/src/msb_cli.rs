//! Microsandbox backend using the `msb` CLI and a managed Sandboxfile project.

use crate::spec::{msb_network_scope, start_command};
use crate::{DesiredSandbox, NodeRuntime, RuntimeKind, SandboxPhase, SandboxStatus};
use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use serde_yaml::{Mapping, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::process::Command;
use tracing::{debug, info, warn};

/// Real microVM backend via `msb` project commands.
pub struct MsbCliRuntime {
    project_dir: PathBuf,
    msb_bin: PathBuf,
}

impl MsbCliRuntime {
    pub fn new(project_dir: impl Into<PathBuf>, msb_bin: impl Into<PathBuf>) -> Result<Self> {
        let project_dir = project_dir.into();
        let msb_bin = msb_bin.into();
        std::fs::create_dir_all(&project_dir)
            .with_context(|| format!("mkdir {}", project_dir.display()))?;
        let rt = Self {
            project_dir,
            msb_bin,
        };
        rt.ensure_project()?;
        Ok(rt)
    }

    pub fn project_dir(&self) -> &Path {
        &self.project_dir
    }

    fn sandboxfile(&self) -> PathBuf {
        self.project_dir.join("Sandboxfile")
    }

    fn ensure_project(&self) -> Result<()> {
        if self.sandboxfile().is_file() {
            return Ok(());
        }
        info!(
            dir = %self.project_dir.display(),
            "initializing msb project for MCC agent"
        );
        let status = std::process::Command::new(&self.msb_bin)
            .args(["init", "-f"])
            .arg(&self.project_dir)
            .status()
            .with_context(|| format!("spawn {} init", self.msb_bin.display()))?;
        if !status.success() {
            bail!("msb init failed with {status}");
        }
        Ok(())
    }

    async fn run_msb(&self, args: &[&str]) -> Result<std::process::Output> {
        debug!(?args, "msb");
        let output = Command::new(&self.msb_bin)
            .args(args)
            .arg("-f")
            .arg(&self.project_dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await
            .with_context(|| format!("spawn {}", self.msb_bin.display()))?;
        Ok(output)
    }

    fn load_sandboxfile(&self) -> Result<Value> {
        let path = self.sandboxfile();
        if !path.is_file() {
            return Ok(Value::Mapping(Mapping::from_iter([(
                Value::String("sandboxes".into()),
                Value::Mapping(Mapping::new()),
            )])));
        }
        let text =
            std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        let v: Value = serde_yaml::from_str(&text).context("parse Sandboxfile")?;
        Ok(v)
    }

    fn save_sandboxfile(&self, doc: &Value) -> Result<()> {
        let path = self.sandboxfile();
        let text = serde_yaml::to_string(doc).context("serialize Sandboxfile")?;
        // Preserve a short header comment
        let body = format!("# Managed by MicroCommandControl agent\n{text}");
        std::fs::write(&path, body).with_context(|| format!("write {}", path.display()))?;
        Ok(())
    }

    fn sandboxes_map_mut(doc: &mut Value) -> Result<&mut Mapping> {
        let root = doc
            .as_mapping_mut()
            .ok_or_else(|| anyhow::anyhow!("Sandboxfile root must be a map"))?;
        if !root.contains_key(Value::String("sandboxes".into())) {
            root.insert(
                Value::String("sandboxes".into()),
                Value::Mapping(Mapping::new()),
            );
        }
        root.get_mut(Value::String("sandboxes".into()))
            .and_then(|v| v.as_mapping_mut())
            .ok_or_else(|| anyhow::anyhow!("sandboxes must be a map"))
    }

    fn upsert_definition(&self, desired: &DesiredSandbox) -> Result<()> {
        let mut doc = self.load_sandboxfile()?;
        {
            let sandboxes = Self::sandboxes_map_mut(&mut doc)?;
            let entry = definition_value(desired);
            sandboxes.insert(Value::String(desired.runtime_id.clone()), entry);
        }
        self.save_sandboxfile(&doc)?;
        Ok(())
    }

    fn remove_definition(&self, name: &str) -> Result<()> {
        let mut doc = self.load_sandboxfile()?;
        {
            let sandboxes = Self::sandboxes_map_mut(&mut doc)?;
            sandboxes.remove(Value::String(name.into()));
        }
        self.save_sandboxfile(&doc)?;
        Ok(())
    }

    async fn parse_status(&self, name: &str) -> Result<SandboxPhase> {
        let out = self.run_msb(&["status", "-s", name]).await?;
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        let combined = format!("{stdout}\n{stderr}");
        // Lines look like: `name   RUNNING   ...`
        for line in combined.lines() {
            let cols: Vec<_> = line.split_whitespace().collect();
            if cols.len() >= 2 && cols[0] == name {
                return Ok(SandboxPhase::from_msb_status(cols[1]));
            }
        }
        if combined.to_ascii_uppercase().contains("RUNNING") {
            return Ok(SandboxPhase::Running);
        }
        if !out.status.success() {
            return Ok(SandboxPhase::Unknown);
        }
        Ok(SandboxPhase::Stopped)
    }

    async fn start_detached(&self, name: &str) -> Result<()> {
        let out = self.run_msb(&["run", "-s", "-d", name]).await?;
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr);
            let stdout = String::from_utf8_lossy(&out.stdout);
            // Already running is OK
            if err.contains("already") || stdout.contains("already") {
                return Ok(());
            }
            bail!("msb run -d {name} failed: {}\n{stdout}\n{err}", out.status);
        }
        Ok(())
    }

    async fn stop_sandbox(&self, name: &str) -> Result<()> {
        let out = self.run_msb(&["down", "-s", name]).await?;
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr);
            warn!(%name, %err, "msb down non-zero (continuing)");
        }
        Ok(())
    }
}

fn definition_value(desired: &DesiredSandbox) -> Value {
    let mut map = Mapping::new();
    map.insert(
        Value::String("image".into()),
        Value::String(desired.spec.image.clone()),
    );
    map.insert(
        Value::String("memory".into()),
        Value::Number(desired.spec.resources.memory_mib.into()),
    );
    let cpus = desired.spec.resources.cpus.clamp(1, 255) as u64;
    map.insert(Value::String("cpus".into()), Value::Number(cpus.into()));
    map.insert(
        Value::String("shell".into()),
        Value::String("/bin/sh".into()),
    );

    if !desired.spec.ports.is_empty() {
        let ports: Vec<Value> = desired
            .spec
            .ports
            .iter()
            .map(|p| Value::String(format!("{}:{}", p.host, p.guest)))
            .collect();
        map.insert(Value::String("ports".into()), Value::Sequence(ports));
    }

    if !desired.spec.env.is_empty() {
        let envs: Vec<Value> = desired
            .spec
            .env
            .iter()
            .map(|(k, v)| Value::String(format!("{k}={v}")))
            .collect();
        map.insert(Value::String("envs".into()), Value::Sequence(envs));
    }

    let mut scripts = Mapping::new();
    scripts.insert(
        Value::String("start".into()),
        Value::String(start_command(&desired.spec)),
    );
    map.insert(Value::String("scripts".into()), Value::Mapping(scripts));

    let mut network = Mapping::new();
    network.insert(
        Value::String("scope".into()),
        Value::String(msb_network_scope(&desired.spec.network.profiles).into()),
    );
    map.insert(Value::String("network".into()), Value::Mapping(network));

    // Attribution labels (msb may ignore unknown keys; kept for Sandboxfile clarity)
    if !desired.spec.labels.is_empty() {
        let labels: BTreeMap<String, Value> = desired
            .spec
            .labels
            .iter()
            .map(|(k, v)| (k.clone(), Value::String(v.clone())))
            .collect();
        let mut lm = Mapping::new();
        for (k, v) in labels {
            lm.insert(Value::String(k), v);
        }
        // also MCC attribution
        lm.insert(
            Value::String("mcc.stack".into()),
            Value::String(desired.stack.clone()),
        );
        lm.insert(
            Value::String("mcc.service".into()),
            Value::String(desired.service.clone()),
        );
        map.insert(Value::String("labels".into()), Value::Mapping(lm));
    }

    Value::Mapping(map)
}

#[async_trait]
impl NodeRuntime for MsbCliRuntime {
    fn kind(&self) -> RuntimeKind {
        RuntimeKind::MsbCli
    }

    async fn ensure_running(&self, desired: &DesiredSandbox) -> Result<SandboxStatus> {
        self.upsert_definition(desired)?;
        let phase = self.parse_status(&desired.runtime_id).await?;
        if phase != SandboxPhase::Running {
            info!(
                name = %desired.runtime_id,
                image = %desired.spec.image,
                "starting microsandbox"
            );
            self.start_detached(&desired.runtime_id).await?;
        }
        // Re-check
        let mut phase = self.parse_status(&desired.runtime_id).await?;
        // Brief wait if still creating
        if phase == SandboxPhase::Creating || phase == SandboxPhase::Unknown {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            phase = self.parse_status(&desired.runtime_id).await?;
        }
        Ok(SandboxStatus {
            runtime_id: desired.runtime_id.clone(),
            phase,
            message: Some(format!("msb-cli {}", self.kind().as_str())),
        })
    }

    async fn ensure_removed(&self, runtime_id: &str) -> Result<()> {
        let name = runtime_id.strip_prefix("msb://").unwrap_or(runtime_id);
        info!(%name, "stopping/removing microsandbox");
        let _ = self.stop_sandbox(name).await;
        let out = self.run_msb(&["remove", "-s", name]).await?;
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr);
            // Not present is fine
            if !err.contains("not found") && !err.contains("No such") {
                warn!(%name, %err, "msb remove failed");
            }
        }
        let _ = self.remove_definition(name);
        Ok(())
    }

    async fn status(&self, runtime_id: &str) -> Result<SandboxStatus> {
        let name = runtime_id.strip_prefix("msb://").unwrap_or(runtime_id);
        let phase = self.parse_status(name).await?;
        Ok(SandboxStatus {
            runtime_id: name.into(),
            phase,
            message: None,
        })
    }

    async fn list(&self) -> Result<Vec<String>> {
        let doc = self.load_sandboxfile()?;
        let Some(map) = doc
            .as_mapping()
            .and_then(|m| m.get(Value::String("sandboxes".into())))
            .and_then(|v| v.as_mapping())
        else {
            return Ok(vec![]);
        };
        Ok(map
            .keys()
            .filter_map(|k| k.as_str().map(|s| s.to_string()))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mcc_api::{ResourceSpec, ServiceSpec};
    use std::collections::BTreeMap;

    fn sample_desired(name: &str) -> DesiredSandbox {
        DesiredSandbox {
            instance_id: "i1".into(),
            stack: "demo".into(),
            service: "web".into(),
            ordinal: 0,
            runtime_id: name.into(),
            spec: ServiceSpec {
                image: "alpine".into(),
                replicas: 1,
                resources: ResourceSpec {
                    cpus: 1,
                    memory_mib: 256,
                },
                ports: vec![],
                network: Default::default(),
                env: BTreeMap::new(),
                secrets: vec![],
                volumes: vec![],
                restart_policy: "on-failure".into(),
                health: None,
                labels: BTreeMap::new(),
                command: Some(vec!["sleep".into(), "infinity".into()]),
                node_name: None,
                node_selector: BTreeMap::new(),
            },
        }
    }

    #[test]
    fn definition_yaml_shape() {
        let d = sample_desired("demo-web-0");
        let v = definition_value(&d);
        let s = serde_yaml::to_string(&v).unwrap();
        assert!(s.contains("alpine"));
        assert!(s.contains("sleep infinity"));
    }

    #[tokio::test]
    async fn project_init_writes_sandboxfile() {
        let dir = tempfile::tempdir().unwrap();
        // Skip if msb missing
        if std::process::Command::new("msb")
            .arg("version")
            .output()
            .map(|o| !o.status.success())
            .unwrap_or(true)
        {
            return;
        }
        let rt = MsbCliRuntime::new(dir.path(), "msb").unwrap();
        assert!(rt.sandboxfile().is_file());
    }
}
