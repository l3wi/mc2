//! Node loss → unbind eligible instances → re-run scheduler.

use crate::apply::run_scheduler;
use crate::scheduler::may_reschedule_on_node_loss;
use anyhow::{Context, Result};
use mc2_api::ServiceSpec;
use mc2_store::{NodeStatus, Store};
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, info, warn};

/// Unbind non-sticky, reschedulable instances from NotReady nodes; schedule Pending.
///
/// Returns (unbound_count, newly_scheduled).
pub async fn reschedule_not_ready(store: Arc<dyn Store>) -> Result<(u32, u32)> {
    let nodes = store.list_nodes().await.context("list nodes")?;
    let mut unbound = 0u32;

    for node in nodes {
        if node.status != NodeStatus::NotReady.as_str() {
            continue;
        }
        let instances = store
            .list_instances_for_node(&node.id)
            .await
            .with_context(|| format!("list instances for {}", node.id))?;

        for inst in instances {
            if inst.node_id.as_deref() != Some(node.id.as_str()) {
                continue;
            }
            let spec: ServiceSpec = match serde_json::from_str(&inst.spec_json) {
                Ok(s) => s,
                Err(e) => {
                    warn!(instance = %inst.id, error = %e, "skip reschedule: bad spec");
                    continue;
                }
            };

            if !may_reschedule_on_node_loss(&spec) {
                if spec.restart.eq_ignore_ascii_case("no") {
                    let _ = store
                        .update_instance_status(
                            &inst.id,
                            "Failed",
                            None,
                            Some("node lost; restart=no"),
                        )
                        .await;
                } else {
                    let _ = store
                        .update_instance_status(
                            &inst.id,
                            &inst.phase,
                            None,
                            Some("node not ready; sticky volume — not rescheduled"),
                        )
                        .await;
                }
                continue;
            }

            store
                .unbind_instance(&inst.id)
                .await
                .with_context(|| format!("unbind {}", inst.id))?;
            unbound += 1;
            info!(
                instance = %inst.id,
                from_node = %node.name,
                "unbound instance for reschedule"
            );
        }
    }

    let scheduled = run_scheduler(store).await.context("run_scheduler")?;
    mc2_metrics::record_reschedule_unbinds(u64::from(unbound));
    mc2_metrics::record_schedule_binds(u64::from(scheduled));
    if unbound > 0 || scheduled > 0 {
        info!(unbound, scheduled, "reschedule pass complete");
    } else {
        debug!("reschedule pass: nothing to do");
    }
    Ok((unbound, scheduled))
}

/// Periodic: unbind from NotReady + schedule Pending.
pub async fn reschedule_loop(store: Arc<dyn Store>, interval: Duration) {
    let mut tick = tokio::time::interval(interval);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        if let Err(e) = reschedule_not_ready(store.clone()).await {
            tracing::error!(error = %e, "reschedule_not_ready failed");
        }
    }
}

/// Periodic schedule only (Pending after late node join without NotReady).
pub async fn schedule_loop(store: Arc<dyn Store>, interval: Duration) {
    let mut tick = tokio::time::interval(interval);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        match run_scheduler(store.clone()).await {
            Ok(0) => debug!("schedule loop: no pending bound"),
            Ok(n) => info!(scheduled = n, "schedule loop bound pending instances"),
            Err(e) => tracing::error!(error = %e, "schedule loop failed"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mc2_store::{hash_token, MemoryStore, NodeHeartbeat, NodeJoin};

    fn on_failure_spec() -> String {
        serde_json::json!({
            "image": "alpine",
            "scale": 1,
            "cpus": 1.0,
            "mem_limit": 128,
            "restart": "on-failure",
            "volumes": []
        })
        .to_string()
    }

    fn sticky_spec() -> String {
        serde_json::json!({
            "image": "alpine",
            "scale": 1,
            "cpus": 1.0,
            "mem_limit": 128,
            "restart": "on-failure",
            "volumes": [{ "name": "data", "target": "/data" }]
        })
        .to_string()
    }

    #[tokio::test]
    async fn unbinds_non_sticky_from_not_ready_onto_ready_peer() {
        let store = MemoryStore::new();
        store.init_cluster(&hash_token("a")).await.unwrap();

        let a = store
            .upsert_local_node(NodeJoin {
                name: "a".into(),
                labels_json: "{}".into(),
                arch: "aarch64".into(),
                cpus: 4,
                memory_mib: 8192,
            })
            .await
            .unwrap();
        let b = store
            .upsert_local_node(NodeJoin {
                name: "b".into(),
                labels_json: "{}".into(),
                arch: "aarch64".into(),
                cpus: 4,
                memory_mib: 8192,
            })
            .await
            .unwrap();

        store.upsert_stack("demo", "{}", "yaml").await.unwrap();
        let inst = store
            .reconcile_service_replicas("demo", "web", 1, &on_failure_spec())
            .await
            .unwrap();
        let id = inst[0].id.clone();
        store.bind_instance_to_node(&id, &a.id).await.unwrap();
        store
            .update_instance_status(&id, "Running", Some("demo-web-0"), None)
            .await
            .unwrap();

        // Force A NotReady via heartbeat status; keep B Ready.
        store
            .touch_node(
                &a.id,
                NodeHeartbeat {
                    cpus: 4,
                    memory_mib: 8192,
                    status: "NotReady".into(),
                },
            )
            .await
            .unwrap();
        store
            .touch_node(
                &b.id,
                NodeHeartbeat {
                    cpus: 4,
                    memory_mib: 8192,
                    status: "Ready".into(),
                },
            )
            .await
            .unwrap();

        let store_dyn: Arc<dyn Store> = store.clone();
        let (unbound, scheduled) = reschedule_not_ready(store_dyn).await.unwrap();
        assert_eq!(unbound, 1);
        assert_eq!(scheduled, 1);

        let after = store.get_instance(&id).await.unwrap().unwrap();
        assert_eq!(after.node_id.as_deref(), Some(b.id.as_str()));
        assert_eq!(after.phase, "Scheduled");
    }

    #[tokio::test]
    async fn sticky_volume_not_unbound() {
        let store = MemoryStore::new();
        store.init_cluster(&hash_token("a")).await.unwrap();
        let a = store
            .upsert_local_node(NodeJoin {
                name: "a".into(),
                labels_json: "{}".into(),
                arch: "aarch64".into(),
                cpus: 4,
                memory_mib: 8192,
            })
            .await
            .unwrap();
        let _b = store
            .upsert_local_node(NodeJoin {
                name: "b".into(),
                labels_json: "{}".into(),
                arch: "aarch64".into(),
                cpus: 4,
                memory_mib: 8192,
            })
            .await
            .unwrap();

        store.upsert_stack("demo", "{}", "yaml").await.unwrap();
        let inst = store
            .reconcile_service_replicas("demo", "web", 1, &sticky_spec())
            .await
            .unwrap();
        let id = inst[0].id.clone();
        store.bind_instance_to_node(&id, &a.id).await.unwrap();
        store
            .touch_node(
                &a.id,
                NodeHeartbeat {
                    cpus: 4,
                    memory_mib: 8192,
                    status: "NotReady".into(),
                },
            )
            .await
            .unwrap();

        let store_dyn: Arc<dyn Store> = store.clone();
        let (unbound, _) = reschedule_not_ready(store_dyn).await.unwrap();
        assert_eq!(unbound, 0);
        let after = store.get_instance(&id).await.unwrap().unwrap();
        assert_eq!(after.node_id.as_deref(), Some(a.id.as_str()));
    }
}
