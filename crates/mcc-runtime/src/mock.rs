//! In-process mock runtime for CI and Phase 3 behavior.

use crate::{DesiredSandbox, NodeRuntime, RuntimeKind, SandboxPhase, SandboxStatus};
use anyhow::Result;
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Mutex;

/// Fake runtime: immediately "Running" with `mock://` ids.
#[derive(Debug, Default)]
pub struct MockRuntime {
    running: Mutex<HashMap<String, String>>, // runtime_id -> instance_id
}

impl MockRuntime {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl NodeRuntime for MockRuntime {
    fn kind(&self) -> RuntimeKind {
        RuntimeKind::Mock
    }

    async fn ensure_running(&self, desired: &DesiredSandbox) -> Result<SandboxStatus> {
        let rid = format!("mock://{}", desired.runtime_id);
        self.running
            .lock()
            .unwrap()
            .insert(desired.runtime_id.clone(), desired.instance_id.clone());
        Ok(SandboxStatus {
            runtime_id: rid,
            phase: SandboxPhase::Running,
            message: Some("phase mock runtime".into()),
        })
    }

    async fn ensure_removed(&self, runtime_id: &str) -> Result<()> {
        let key = runtime_id.strip_prefix("mock://").unwrap_or(runtime_id);
        self.running.lock().unwrap().remove(key);
        Ok(())
    }

    async fn status(&self, runtime_id: &str) -> Result<SandboxStatus> {
        let key = runtime_id.strip_prefix("mock://").unwrap_or(runtime_id);
        let running = self.running.lock().unwrap().contains_key(key);
        Ok(SandboxStatus {
            runtime_id: if runtime_id.starts_with("mock://") {
                runtime_id.into()
            } else {
                format!("mock://{runtime_id}")
            },
            phase: if running {
                SandboxPhase::Running
            } else {
                SandboxPhase::Stopped
            },
            message: None,
        })
    }

    async fn list(&self) -> Result<Vec<String>> {
        Ok(self
            .running
            .lock()
            .unwrap()
            .keys()
            .map(|k| format!("mock://{k}"))
            .collect())
    }
}
