//! Health-probe execution and restart-on-failure for the node reconcile loop.
//!
//! B6b: a probe runs in its own task, so one hung guest command can never stall
//! the reconcile pass (and with it every other instance on the node). Each pass
//! starts at most one probe per instance, reads the result of the previous one,
//! and every probe is bounded by the compose `timeout` (default 30s).

use super::state::{InFlightProbe, InstanceRuntimeState};
use mc2_api::HealthcheckSpec;
use mc2_runtime::{DesiredSandbox, NodeRuntime, RestartPolicy, SandboxPhase, SandboxStatus};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::{info, warn};

/// True when a healthcheck should actually run (not disabled, has a test).
pub(super) fn health_active(h: &HealthcheckSpec) -> bool {
    !h.disable && h.test.as_ref().is_some_and(|t| !t.is_empty())
}

/// Effective probe deadline. Stack validation rejects an explicit `0`; the
/// compose default is applied here as well so no spec (hand-edited store row
/// included) can ever produce an unbounded probe.
fn probe_timeout(h: &HealthcheckSpec) -> Duration {
    let secs = if h.timeout_seconds == 0 {
        30
    } else {
        h.timeout_seconds
    };
    Duration::from_secs(u64::from(secs))
}

/// Spawn the probe for `generation` and hand back its result channel.
fn spawn_probe(
    runtime: &Arc<dyn NodeRuntime>,
    d: &DesiredSandbox,
    health: &HealthcheckSpec,
    generation: u64,
) -> InFlightProbe {
    let runtime = Arc::clone(runtime);
    let runtime_id = d.runtime_id.clone();
    let command = health.test.clone().unwrap_or_default();
    let timeout = probe_timeout(health);
    let (tx, result) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let fut = runtime.exec_command(&runtime_id, &command);
        let outcome = match tokio::time::timeout(timeout, fut).await {
            Ok(outcome) => outcome,
            Err(_) => Err(anyhow::anyhow!(
                "probe timed out after {}s",
                timeout.as_secs()
            )),
        };
        // The pass may have moved on (recreate, scale-down): dropping the
        // receiver is fine, this send is best-effort.
        let _ = tx.send(outcome);
    });
    InFlightProbe { generation, result }
}

/// Drop a probe started for a previous VM generation: its result belongs to a
/// sandbox that no longer exists and must not influence the current one.
fn discard_stale_probe(state: &mut InstanceRuntimeState) {
    if state
        .health_probe
        .as_ref()
        .is_some_and(|p| p.generation != state.health_generation)
    {
        state.health_probe = None;
    }
}

/// Collect a probe that finished since the last pass. `None` while the probe is
/// still running — the pass never waits for one.
fn take_completed_probe(state: &mut InstanceRuntimeState) -> Option<(u64, anyhow::Result<i32>)> {
    let probe = state.health_probe.as_mut()?;
    let (generation, step) = (probe.generation, probe.result.try_recv());
    match step {
        Ok(outcome) => {
            state.health_probe = None;
            Some((generation, outcome))
        }
        Err(tokio::sync::oneshot::error::TryRecvError::Empty) => None,
        Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
            // The task died (panic/abort) without sending: count it as a probe
            // error rather than leaving the instance without probes forever.
            state.health_probe = None;
            Some((
                generation,
                Err(anyhow::anyhow!("health probe task ended without a result")),
            ))
        }
    }
}

/// Handle a failed probe past retries: restart per policy, returning the
/// (possibly recreated) observed status.
async fn handle_health_failure(
    runtime: &Arc<dyn NodeRuntime>,
    d: &DesiredSandbox,
    policy: RestartPolicy,
    state: &mut InstanceRuntimeState,
    now: Instant,
    message: String,
) -> SandboxStatus {
    state.health_ok = false;
    if policy == RestartPolicy::Never {
        state.health_probe = None;
        return SandboxStatus {
            runtime_id: d.runtime_id.clone(),
            phase: SandboxPhase::Failed,
            message: Some(message),
        };
    }
    // New VM generation: counters reset and probe results from the old sandbox
    // are dropped, so the recreate also gets a fresh `start_period`.
    state.reset_health();
    state.restart_count = state.restart_count.saturating_add(1);
    state.next_restart_ok =
        Some(now + Duration::from_secs(mc2_runtime::backoff_secs(state.restart_count)));
    state.running_since = None;
    if let Err(e) = runtime.ensure_removed(&d.runtime_id).await {
        warn!(runtime_id = %d.runtime_id, error = %e, "remove after health fail");
    }
    match runtime.ensure_running(d).await {
        Ok(er) => er.status,
        Err(e) => SandboxStatus {
            runtime_id: d.runtime_id.clone(),
            phase: SandboxPhase::Failed,
            message: Some(format!("{message}; recreate: {e:#}")),
        },
    }
}

/// Advance the healthcheck for a Running sandbox (compose semantics: interval /
/// timeout / retries / start_period / disable).
///
/// Non-blocking: it reads the probe that finished since the last pass and, when
/// `interval` has elapsed and none is in flight, starts the next one.
///
/// Returns an overriding observed status when a probe failure past `retries`
/// triggered a restart (so the caller reports the new sandbox state), or `None`
/// when nothing changed (including "probe still running").
pub(super) async fn run_healthcheck(
    runtime: &Arc<dyn NodeRuntime>,
    d: &DesiredSandbox,
    policy: RestartPolicy,
    state: &mut InstanceRuntimeState,
    now: Instant,
) -> Option<SandboxStatus> {
    let health = d.spec.healthcheck.as_ref()?;
    if !health_active(health) {
        state.health_probe = None;
        return None;
    }

    discard_stale_probe(state);
    let Some((generation, outcome)) = take_completed_probe(state) else {
        // A probe is already in flight for this instance.
        if state.health_probe.is_some() {
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
        let generation = state.health_generation;
        state.health_probe = Some(spawn_probe(runtime, d, health, generation));
        return None;
    };
    // Defensive: a generation change between spawn and completion (the probe
    // above is already filtered) is never actionable.
    if generation != state.health_generation {
        return None;
    }

    if matches!(outcome, Ok(0)) {
        state.health_failures = 0;
        state.health_ok = true;
        return None;
    }
    let detail = match &outcome {
        Ok(code) => format!("health: exit {code}"),
        Err(e) => format!("health: {e:#}"),
    };

    // Warm-up: probe failures inside `start_period` are not counted toward
    // `retries` (a slow-booting service must not be restarted for them).
    let in_start_period = health.start_period_seconds > 0
        && state.running_since.is_some_and(|since| {
            now.duration_since(since) < Duration::from_secs(u64::from(health.start_period_seconds))
        });
    state.health_ok = false;
    if in_start_period {
        info!(
            runtime_id = %d.runtime_id,
            detail = %detail,
            "health probe failed within start_period; not counted"
        );
        return None;
    }

    state.health_failures = state.health_failures.saturating_add(1);
    if state.health_failures >= health.retries.max(1) {
        warn!(
            runtime_id = %d.runtime_id,
            failures = state.health_failures,
            detail = %detail,
            "health probe failed past retries; restarting"
        );
        return Some(handle_health_failure(runtime, d, policy, state, now, detail).await);
    }
    warn!(
        runtime_id = %d.runtime_id,
        failures = state.health_failures,
        detail = %detail,
        "health probe failed"
    );
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn health(
        test: Option<Vec<&str>>,
        interval: u32,
        timeout: u32,
        retries: u32,
        start_period: u32,
    ) -> HealthcheckSpec {
        HealthcheckSpec {
            test: test.map(|t| t.into_iter().map(String::from).collect()),
            interval_seconds: interval,
            timeout_seconds: timeout,
            retries,
            start_period_seconds: start_period,
            disable: false,
        }
    }

    fn sandbox(healthcheck: Option<HealthcheckSpec>) -> DesiredSandbox {
        DesiredSandbox {
            instance_id: "i1".into(),
            stack: "demo".into(),
            service: "web".into(),
            ordinal: 0,
            runtime_id: "demo-web-0".into(),
            spec: mc2_api::ServiceSpec {
                image: "alpine".into(),
                scale: 1,
                cpus: 1,
                mem_limit_mib: 512,
                ports: vec![],
                network: Default::default(),
                env: BTreeMap::new(),
                secrets: vec![],
                volumes: vec![],
                restart: "no".into(),
                healthcheck,
                labels: BTreeMap::new(),
                command: None,
                node_name: None,
                node_selector: BTreeMap::new(),
                ssh: None,
                storage_opt: None,
                expose: vec![],
                networks: vec![],
                depends_on: BTreeMap::new(),
            },
            secrets: vec![],
            ssh: Default::default(),
            network: Default::default(),
        }
    }

    /// `exec_command` never returns: the guest is wedged.
    struct HangingRuntime;

    #[async_trait::async_trait]
    impl NodeRuntime for HangingRuntime {
        async fn ensure_running(
            &self,
            _d: &DesiredSandbox,
        ) -> anyhow::Result<mc2_runtime::EnsureRunning> {
            anyhow::bail!("unused")
        }
        async fn ensure_removed(&self, _id: &str) -> anyhow::Result<()> {
            Ok(())
        }
        async fn status(&self, _id: &str) -> anyhow::Result<SandboxStatus> {
            anyhow::bail!("unused")
        }
        async fn list_owned(&self, _install_id: &str) -> anyhow::Result<Vec<String>> {
            Ok(vec![])
        }
        async fn exec_command(&self, _id: &str, _argv: &[String]) -> anyhow::Result<i32> {
            std::future::pending::<anyhow::Result<i32>>().await
        }
        async fn exec_with_output(
            &self,
            _id: &str,
            _argv: &[String],
            _stdin: &[u8],
        ) -> anyhow::Result<mc2_runtime::ExecResult> {
            anyhow::bail!("unused")
        }
        async fn guest_shell(&self, _id: &str, _script: &str) -> anyhow::Result<String> {
            anyhow::bail!("unused")
        }
        async fn read_logs(
            &self,
            _id: &str,
            _tail: Option<usize>,
        ) -> anyhow::Result<Vec<mc2_runtime::LogLine>> {
            anyhow::bail!("unused")
        }
        async fn log_stream(
            &self,
            _id: &str,
            _from: Option<String>,
        ) -> anyhow::Result<futures::stream::BoxStream<'static, anyhow::Result<mc2_runtime::LogLine>>>
        {
            anyhow::bail!("unused")
        }
        async fn ssh_server(
            &self,
            _id: &str,
            _user: &str,
            _keys: &[String],
            _sftp: bool,
        ) -> anyhow::Result<std::sync::Arc<dyn mc2_runtime::SshServer>> {
            anyhow::bail!("unused")
        }
    }

    /// `exec_command` fails immediately with a non-zero exit code.
    struct FailingRuntime;

    #[async_trait::async_trait]
    impl NodeRuntime for FailingRuntime {
        async fn ensure_running(
            &self,
            _d: &DesiredSandbox,
        ) -> anyhow::Result<mc2_runtime::EnsureRunning> {
            anyhow::bail!("unused")
        }
        async fn ensure_removed(&self, _id: &str) -> anyhow::Result<()> {
            Ok(())
        }
        async fn status(&self, _id: &str) -> anyhow::Result<SandboxStatus> {
            anyhow::bail!("unused")
        }
        async fn list_owned(&self, _install_id: &str) -> anyhow::Result<Vec<String>> {
            Ok(vec![])
        }
        async fn exec_command(&self, _id: &str, _argv: &[String]) -> anyhow::Result<i32> {
            Ok(1)
        }
        async fn exec_with_output(
            &self,
            _id: &str,
            _argv: &[String],
            _stdin: &[u8],
        ) -> anyhow::Result<mc2_runtime::ExecResult> {
            anyhow::bail!("unused")
        }
        async fn guest_shell(&self, _id: &str, _script: &str) -> anyhow::Result<String> {
            anyhow::bail!("unused")
        }
        async fn read_logs(
            &self,
            _id: &str,
            _tail: Option<usize>,
        ) -> anyhow::Result<Vec<mc2_runtime::LogLine>> {
            anyhow::bail!("unused")
        }
        async fn log_stream(
            &self,
            _id: &str,
            _from: Option<String>,
        ) -> anyhow::Result<futures::stream::BoxStream<'static, anyhow::Result<mc2_runtime::LogLine>>>
        {
            anyhow::bail!("unused")
        }
        async fn ssh_server(
            &self,
            _id: &str,
            _user: &str,
            _keys: &[String],
            _sftp: bool,
        ) -> anyhow::Result<std::sync::Arc<dyn mc2_runtime::SshServer>> {
            anyhow::bail!("unused")
        }
    }

    /// `exec_command` fails after `delay`, so the probe is observably in flight.
    struct SlowFailingRuntime {
        delay: Duration,
    }

    #[async_trait::async_trait]
    impl NodeRuntime for SlowFailingRuntime {
        async fn ensure_running(
            &self,
            _d: &DesiredSandbox,
        ) -> anyhow::Result<mc2_runtime::EnsureRunning> {
            anyhow::bail!("unused")
        }
        async fn ensure_removed(&self, _id: &str) -> anyhow::Result<()> {
            Ok(())
        }
        async fn status(&self, _id: &str) -> anyhow::Result<SandboxStatus> {
            anyhow::bail!("unused")
        }
        async fn list_owned(&self, _install_id: &str) -> anyhow::Result<Vec<String>> {
            Ok(vec![])
        }
        async fn exec_command(&self, _id: &str, _argv: &[String]) -> anyhow::Result<i32> {
            tokio::time::sleep(self.delay).await;
            Ok(1)
        }
        async fn exec_with_output(
            &self,
            _id: &str,
            _argv: &[String],
            _stdin: &[u8],
        ) -> anyhow::Result<mc2_runtime::ExecResult> {
            anyhow::bail!("unused")
        }
        async fn guest_shell(&self, _id: &str, _script: &str) -> anyhow::Result<String> {
            anyhow::bail!("unused")
        }
        async fn read_logs(
            &self,
            _id: &str,
            _tail: Option<usize>,
        ) -> anyhow::Result<Vec<mc2_runtime::LogLine>> {
            anyhow::bail!("unused")
        }
        async fn log_stream(
            &self,
            _id: &str,
            _from: Option<String>,
        ) -> anyhow::Result<futures::stream::BoxStream<'static, anyhow::Result<mc2_runtime::LogLine>>>
        {
            anyhow::bail!("unused")
        }
        async fn ssh_server(
            &self,
            _id: &str,
            _user: &str,
            _keys: &[String],
            _sftp: bool,
        ) -> anyhow::Result<std::sync::Arc<dyn mc2_runtime::SshServer>> {
            anyhow::bail!("unused")
        }
    }

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

    #[test]
    fn probe_deadline_is_always_finite() {
        // The decoder rejects an explicit `0`; a `0` that still reaches the
        // runtime (hand-edited store row) falls back to the compose default.
        assert_eq!(
            probe_timeout(&health(None, 30, 0, 3, 0)),
            Duration::from_secs(30)
        );
        assert_eq!(
            probe_timeout(&health(None, 30, 5, 3, 0)),
            Duration::from_secs(5)
        );
    }

    #[tokio::test]
    async fn hung_probe_never_blocks_and_times_out_as_failure() {
        let runtime: Arc<dyn NodeRuntime> = Arc::new(HangingRuntime);
        let d = sandbox(Some(health(Some(vec!["true"]), 1, 1, 3, 0)));
        let mut state = InstanceRuntimeState {
            running_since: Some(Instant::now()),
            ..Default::default()
        };

        let started = Instant::now();
        assert!(run_healthcheck(
            &runtime,
            &d,
            RestartPolicy::Never,
            &mut state,
            Instant::now()
        )
        .await
        .is_none());
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "pass waited on the probe"
        );
        let generation = state.health_generation;
        assert_eq!(
            state.health_probe.as_ref().map(|p| p.generation),
            Some(generation)
        );

        // While it is in flight the pass is still immediate and starts nothing new.
        assert!(run_healthcheck(
            &runtime,
            &d,
            RestartPolicy::Never,
            &mut state,
            Instant::now()
        )
        .await
        .is_none());
        assert_eq!(
            state.health_probe.as_ref().map(|p| p.generation),
            Some(generation)
        );
        assert_eq!(state.health_failures, 0);

        // The probe's deadline fires: the timeout is one counted failure.
        tokio::time::sleep(Duration::from_millis(1300)).await;
        assert!(run_healthcheck(
            &runtime,
            &d,
            RestartPolicy::Never,
            &mut state,
            Instant::now()
        )
        .await
        .is_none());
        assert_eq!(state.health_failures, 1);
        assert!(!state.health_ok);
    }

    #[tokio::test]
    async fn disabled_healthcheck_never_probes() {
        let runtime: Arc<dyn NodeRuntime> = Arc::new(HangingRuntime);
        let mut h = health(Some(vec!["true"]), 1, 1, 3, 0);
        h.disable = true;
        let d = sandbox(Some(h));
        let mut state = InstanceRuntimeState::default();
        assert!(run_healthcheck(
            &runtime,
            &d,
            RestartPolicy::Never,
            &mut state,
            Instant::now()
        )
        .await
        .is_none());
        assert!(state.health_probe.is_none());
    }

    #[tokio::test]
    async fn start_period_failures_are_not_counted() {
        let runtime: Arc<dyn NodeRuntime> = Arc::new(FailingRuntime);
        // start_period 1h: every failure in this test is a warm-up failure.
        let d = sandbox(Some(health(Some(vec!["false"]), 1, 1, 3, 3600)));
        let mut state = InstanceRuntimeState {
            running_since: Some(Instant::now()),
            ..Default::default()
        };
        let mut now = Instant::now();
        for _ in 0..6 {
            assert!(
                run_healthcheck(&runtime, &d, RestartPolicy::Never, &mut state, now)
                    .await
                    .is_none()
            );
            now += Duration::from_secs(2);
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(state.health_failures, 0, "warm-up failures must not count");
        assert!(!state.health_ok);
    }

    #[tokio::test]
    async fn failures_past_start_period_restart_after_retries() {
        let runtime: Arc<dyn NodeRuntime> = Arc::new(FailingRuntime);
        let d = sandbox(Some(health(Some(vec!["false"]), 1, 1, 3, 60)));
        // Started long ago: past `start_period`, so failures count.
        let mut state = InstanceRuntimeState {
            running_since: Some(Instant::now() - Duration::from_secs(600)),
            ..Default::default()
        };
        let mut now = Instant::now();
        let mut restarted = None;
        for _ in 0..10 {
            if let Some(st) =
                run_healthcheck(&runtime, &d, RestartPolicy::Never, &mut state, now).await
            {
                restarted = Some(st);
                break;
            }
            now += Duration::from_secs(2);
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let st = restarted.expect("the 3rd counted failure restarts the sandbox");
        assert_eq!(st.phase, SandboxPhase::Failed);
        assert_eq!(state.health_failures, 3);
    }

    #[tokio::test]
    async fn stale_probe_result_from_previous_generation_is_ignored() {
        let runtime: Arc<dyn NodeRuntime> = Arc::new(SlowFailingRuntime {
            delay: Duration::from_millis(50),
        });
        let d = sandbox(Some(health(Some(vec!["false"]), 1, 5, 1, 0)));
        let mut state = InstanceRuntimeState {
            running_since: Some(Instant::now() - Duration::from_secs(600)),
            ..Default::default()
        };

        // Generation 1 probe is in flight...
        assert!(run_healthcheck(
            &runtime,
            &d,
            RestartPolicy::Never,
            &mut state,
            Instant::now()
        )
        .await
        .is_none());
        assert!(state.health_probe.is_some());
        // ...while the sandbox is recreated.
        state.reset_health();
        let generation = state.health_generation;
        tokio::time::sleep(Duration::from_millis(120)).await;

        // The old result is dropped; the new generation starts a fresh probe.
        assert!(run_healthcheck(
            &runtime,
            &d,
            RestartPolicy::Never,
            &mut state,
            Instant::now()
        )
        .await
        .is_none());
        assert_eq!(state.health_failures, 0, "stale result must not be counted");
        assert_eq!(
            state.health_probe.as_ref().map(|p| p.generation),
            Some(generation)
        );

        // The current generation's failure does count (retries = 1).
        tokio::time::sleep(Duration::from_millis(120)).await;
        assert!(run_healthcheck(
            &runtime,
            &d,
            RestartPolicy::Never,
            &mut state,
            Instant::now()
        )
        .await
        .is_some());
        assert_eq!(state.health_failures, 1);
    }
}
