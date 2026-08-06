//! Integration: mock runtime ensure_running / remove.

use mcc_api::{ResourceSpec, ServiceSpec};
use mcc_runtime::{DesiredSandbox, MockRuntime, NodeRuntime, SandboxPhase};
use std::collections::BTreeMap;

fn desired(name: &str) -> DesiredSandbox {
    DesiredSandbox {
        instance_id: "inst-1".into(),
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
            command: None,
            node_name: None,
            node_selector: BTreeMap::new(),
        },
    }
}

#[tokio::test]
async fn mock_runtime_lifecycle() {
    let rt = MockRuntime::new();
    let d = desired("demo-web-0");
    let st = rt.ensure_running(&d).await.unwrap();
    assert_eq!(st.phase, SandboxPhase::Running);
    assert!(st.runtime_id.starts_with("mock://"));

    let list = rt.list().await.unwrap();
    assert_eq!(list.len(), 1);

    rt.ensure_removed(&st.runtime_id).await.unwrap();
    let list = rt.list().await.unwrap();
    assert!(list.is_empty());
}
