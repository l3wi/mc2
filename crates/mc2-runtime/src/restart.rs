//! `restart` policy evaluation (node-local sandbox lifecycle).

use crate::SandboxPhase;

/// Normalized restart policy from stack YAML (compose values).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestartPolicy {
    Always,
    OnFailure,
    Never,
}

impl RestartPolicy {
    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "always" | "unless-stopped" => Self::Always,
            "no" | "never" => Self::Never,
            _ => Self::OnFailure, // default + unknown → on-failure
        }
    }
}

/// What the runtime should do when observing a non-Running desired sandbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestartAction {
    /// Leave as-is; report current phase.
    Leave,
    /// Call start_detached (Stopped sandbox still configured).
    Start,
    /// remove + create_detached (Crashed / unrecoverable).
    Recreate,
}

/// Decide action for an observed phase under a restart policy.
///
/// Desired instances always want to be Running from the control plane's view;
/// policy only controls *how* we react to Failed/Stopped.
pub fn action_for_phase(policy: RestartPolicy, phase: SandboxPhase) -> RestartAction {
    match phase {
        SandboxPhase::Running | SandboxPhase::Creating | SandboxPhase::Pending => {
            RestartAction::Leave
        }
        SandboxPhase::Unknown => RestartAction::Start,
        SandboxPhase::Failed => match policy {
            RestartPolicy::Never => RestartAction::Leave,
            RestartPolicy::OnFailure | RestartPolicy::Always => RestartAction::Recreate,
        },
        // Stopped while still desired: treat as unexpected exit for Always and
        // OnFailure (msb often maps process exit → Stopped, not Failed).
        // Scale-down uses ensure_removed; it never leaves a desired Stopped instance.
        SandboxPhase::Stopped => match policy {
            RestartPolicy::Always | RestartPolicy::OnFailure => RestartAction::Start,
            RestartPolicy::Never => RestartAction::Leave,
        },
    }
}

/// Simple exponential backoff index → seconds until next recreate allowed.
pub fn backoff_secs(restart_count: u32) -> u64 {
    match restart_count {
        0 => 0,
        1 => 2,
        2 => 5,
        3 => 15,
        _ => 30,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn never_leaves_failed() {
        assert_eq!(
            action_for_phase(RestartPolicy::Never, SandboxPhase::Failed),
            RestartAction::Leave
        );
    }

    #[test]
    fn on_failure_recreates_failed_and_starts_stopped() {
        assert_eq!(
            action_for_phase(RestartPolicy::OnFailure, SandboxPhase::Failed),
            RestartAction::Recreate
        );
        // Process exit often surfaces as Stopped under msb; bring it back.
        assert_eq!(
            action_for_phase(RestartPolicy::OnFailure, SandboxPhase::Stopped),
            RestartAction::Start
        );
    }

    #[test]
    fn always_starts_stopped() {
        assert_eq!(
            action_for_phase(RestartPolicy::Always, SandboxPhase::Stopped),
            RestartAction::Start
        );
        assert_eq!(
            action_for_phase(RestartPolicy::Always, SandboxPhase::Failed),
            RestartAction::Recreate
        );
    }

    #[test]
    fn parse_defaults() {
        assert_eq!(RestartPolicy::parse("on-failure"), RestartPolicy::OnFailure);
        assert_eq!(RestartPolicy::parse("ALWAYS"), RestartPolicy::Always);
        assert_eq!(RestartPolicy::parse("weird"), RestartPolicy::OnFailure);
    }
}
