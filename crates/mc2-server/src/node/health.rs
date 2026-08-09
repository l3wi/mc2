//! Health-probe execution and restart-on-failure for the node reconcile loop.

use super::state::InstanceRuntimeState;
use mc2_api::HealthcheckSpec;
use mc2_runtime::{DesiredSandbox, NodeRuntime, RestartPolicy, SandboxPhase, SandboxStatus};
use std::time::{Duration, Instant};
use tracing::warn;

/// True when a healthcheck should actually run (not disabled, has a test).
pub(super) fn health_active(h: &HealthcheckSpec) -> bool {
    !h.disable && h.test.as_ref().is_some_and(|t| !t.is_empty())
}

/// Run one health probe, honoring the compose `timeout` (0 = no timeout).
/// Returns the guest exit code, or an Err (probe error / timeout).
async fn run_health_probe(
    runtime: &dyn NodeRuntime,
    runtime_id: &str,
    health: &HealthcheckSpec,
) -> anyhow::Result<i32> {
    let command = health.test.as_ref().expect("active healthcheck has a test");
    let fut = runtime.exec_command(runtime_id, command);
    if health.timeout_seconds == 0 {
        return fut.await;
    }
    match tokio::time::timeout(Duration::from_secs(u64::from(health.timeout_seconds)), fut).await {
        Ok(res) => res,
        Err(_) => anyhow::bail!("health probe timed out"),
    }
}

/// Handle a failed probe past retries: restart per policy, returning the
/// (possibly recreated) observed status.
async fn handle_health_failure(
    runtime: &dyn NodeRuntime,
    d: &DesiredSandbox,
    policy: RestartPolicy,
    state: &mut InstanceRuntimeState,
    now: Instant,
    message: String,
) -> SandboxStatus {
    if policy == RestartPolicy::Never {
        return SandboxStatus {
            runtime_id: d.runtime_id.clone(),
            phase: SandboxPhase::Failed,
            message: Some(message),
        };
    }
    state.restart_count = state.restart_count.saturating_add(1);
    state.next_restart_ok =
        Some(now + Duration::from_secs(mc2_runtime::backoff_secs(state.restart_count)));
    state.running_since = None;
    if let Err(e) = runtime.ensure_removed(&d.runtime_id).await {
        warn!(runtime_id = %d.runtime_id, error = %e, "remove after health fail");
    }
    match runtime.ensure_running(d).await {
        Ok(st) => st,
        Err(e) => SandboxStatus {
            runtime_id: d.runtime_id.clone(),
            phase: SandboxPhase::Failed,
            message: Some(format!("{message}; recreate: {e:#}")),
        },
    }
}

/// Run the healthcheck for a Running sandbox when due (compose semantics:
/// interval / timeout / retries / start_period / disable).
///
/// Returns an overriding observed status when a probe failure past retries
/// triggered a restart (so the caller reports the new sandbox state), or
/// `None` when nothing changed.
pub(super) async fn run_healthcheck(
    runtime: &dyn NodeRuntime,
    d: &DesiredSandbox,
    policy: RestartPolicy,
    state: &mut InstanceRuntimeState,
    now: Instant,
) -> Option<SandboxStatus> {
    let health = d.spec.healthcheck.as_ref()?;
    if !health_active(health) {
        return None;
    }
    let interval = Duration::from_secs(u64::from(health.interval_seconds.max(1)));
    let due = state
        .last_health
        .map(|t| now.duration_since(t) >= interval)
        .unwrap_or(true);
    if !due {
        return None;
    }
    state.last_health = Some(now);

    let in_start_period = health.start_period_seconds > 0
        && state.running_since.is_some_and(|since| {
            now.duration_since(since) < Duration::from_secs(u64::from(health.start_period_seconds))
        });

    match run_health_probe(runtime, &d.runtime_id, health).await {
        Ok(0) => {
            state.health_failures = 0;
            state.health_ok = true;
            None
        }
        Ok(code) => {
            state.health_failures = state.health_failures.saturating_add(1);
            state.health_ok = false;
            if in_start_period {
                warn!(
                    runtime_id = %d.runtime_id,
                    code,
                    "health probe failed (within start_period; ignored)"
                );
                None
            } else if state.health_failures >= health.retries.max(1) {
                warn!(
                    runtime_id = %d.runtime_id,
                    code,
                    failures = state.health_failures,
                    "health exec failed past retries"
                );
                Some(
                    handle_health_failure(
                        runtime,
                        d,
                        policy,
                        state,
                        now,
                        format!("health: exit {code}"),
                    )
                    .await,
                )
            } else {
                warn!(
                    runtime_id = %d.runtime_id,
                    code,
                    failures = state.health_failures,
                    "health exec failed"
                );
                None
            }
        }
        Err(e) => {
            state.health_failures = state.health_failures.saturating_add(1);
            state.health_ok = false;
            if in_start_period {
                warn!(
                    runtime_id = %d.runtime_id,
                    error = %e,
                    "health probe error (within start_period; ignored)"
                );
                None
            } else if state.health_failures >= health.retries.max(1) {
                warn!(
                    runtime_id = %d.runtime_id,
                    error = %e,
                    failures = state.health_failures,
                    "health exec error past retries"
                );
                Some(
                    handle_health_failure(runtime, d, policy, state, now, format!("health: {e}"))
                        .await,
                )
            } else {
                warn!(
                    runtime_id = %d.runtime_id,
                    error = %e,
                    failures = state.health_failures,
                    "health exec error"
                );
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn active_requires_test_and_not_disabled() {
        assert!(!health_active(&HealthcheckSpec::default()));
        let h = HealthcheckSpec {
            test: Some(vec!["true".into()]),
            ..Default::default()
        };
        assert!(health_active(&h));
        assert!(!health_active(&HealthcheckSpec {
            test: Some(vec!["true".into()]),
            disable: true,
            ..Default::default()
        }));
    }
}
