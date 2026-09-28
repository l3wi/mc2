//! Node-loop readiness state (D1).
//!
//! `run()` spawns the reconcile loop as a supervised critical task. `/health`
//! must not keep answering `200 ok` when that loop is dead or wedged: the
//! orchestrator is up but no longer converging desired state to the runtime, so
//! a service manager should be told to restart it.
//!
//! [`Liveness`] is the shared hand-off: the node loop stamps every **successful**
//! pass, the supervisor stamps a fatal exit, and the (unauthenticated) `/health`
//! handler reads it. A pass is considered stale after `3 × reconcile interval`
//! plus a fixed grace, so a single slow pass never flaps readiness.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// Extra slack on top of three reconcile intervals before readiness degrades.
pub const READINESS_STALE_GRACE: Duration = Duration::from_secs(30);

/// Sentinel for "no successful pass has completed yet".
const NEVER: u64 = u64::MAX;

/// Shared liveness of the embedded node loop.
#[derive(Debug)]
pub struct Liveness {
    stale_after: Duration,
    started: Instant,
    /// Elapsed millis since `started` at the last successful pass, or `NEVER`.
    last_pass_ms: AtomicU64,
    dead: AtomicBool,
    dead_reason: OnceLock<String>,
}

impl Liveness {
    /// Readiness threshold for a loop that reconciles every `interval`:
    /// three missed intervals plus `READINESS_STALE_GRACE` (30 s).
    pub fn for_interval(interval: Duration) -> Self {
        let stale_after = interval
            .saturating_mul(3)
            .saturating_add(READINESS_STALE_GRACE);
        Self::with_stale_after(stale_after)
    }

    /// Threshold explicit (tests / unusual loops).
    pub fn with_stale_after(stale_after: Duration) -> Self {
        Self {
            stale_after,
            started: Instant::now(),
            last_pass_ms: AtomicU64::new(NEVER),
            dead: AtomicBool::new(false),
            dead_reason: OnceLock::new(),
        }
    }

    /// Record a fully successful reconcile pass.
    pub fn record_pass(&self) {
        let now = self.started.elapsed().as_millis().min(NEVER as u128 - 1) as u64;
        self.last_pass_ms.store(now, Ordering::Relaxed);
    }

    /// Record that the node loop stopped. Readiness stays degraded forever.
    pub fn mark_dead(&self, reason: impl Into<String>) {
        self.dead.store(true, Ordering::SeqCst);
        let _ = self.dead_reason.set(reason.into());
    }

    /// `Ok` when the node loop is live and current; `Err(reason)` when it has
    /// stopped or has not completed a pass within the staleness threshold.
    pub fn check(&self) -> Result<(), String> {
        if self.dead.load(Ordering::SeqCst) {
            let reason = self
                .dead_reason
                .get()
                .cloned()
                .unwrap_or_else(|| "node loop stopped".to_string());
            return Err(reason);
        }
        let age = self.age();
        if age > self.stale_after {
            return Err(format!(
                "no successful reconcile pass for {}s (readiness limit {}s)",
                age.as_secs(),
                self.stale_after.as_secs()
            ));
        }
        Ok(())
    }

    /// Age of the last successful pass, or of the process before the first one.
    fn age(&self) -> Duration {
        let elapsed = self.started.elapsed();
        let stored = self.last_pass_ms.load(Ordering::Relaxed);
        if stored == NEVER {
            elapsed
        } else {
            elapsed.saturating_sub(Duration::from_millis(stored))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn fresh_is_ready_and_stale_after_the_threshold() {
        let l = Liveness::with_stale_after(Duration::from_millis(40));
        assert!(l.check().is_ok(), "a just-started loop is ready");
        tokio::time::sleep(Duration::from_millis(80)).await;
        assert!(l.check().is_err(), "no pass within the threshold is stale");
    }

    #[tokio::test]
    async fn a_completed_pass_refreshes_readiness() {
        let l = Liveness::with_stale_after(Duration::from_millis(60));
        tokio::time::sleep(Duration::from_millis(80)).await;
        assert!(l.check().is_err());
        l.record_pass();
        assert!(
            l.check().is_ok(),
            "a pass since the deadline is fresh again"
        );
    }

    #[test]
    fn dead_is_sticky_and_reports_its_reason() {
        let l = Liveness::for_interval(Duration::from_secs(10));
        assert!(l.check().is_ok());
        l.mark_dead("node loop exited with an error");
        l.record_pass();
        let err = l.check().unwrap_err();
        assert!(err.contains("exited with an error"), "{err}");
    }
}
