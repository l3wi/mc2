//! Admission limits for the REST and SSH listeners (Epic A / A10).
//!
//! Every listener MC2 opens is reachable by anything that can reach the host
//! (and, behind Traefik, every client looks like `127.0.0.1` — so limits are
//! **global**, never per-IP). Without caps and deadlines a stalled client can
//! pin unbounded resources. The values here are operator flags
//! (`mc2 server --help`) or documented constants.

use std::time::Duration;

/// Body limit for `POST /v1/instances/{id}/exec`.
///
/// Guest stdin/stdout/stderr are arbitrary bytes and travel base64-encoded in
/// the JSON envelope (see `mc2_api::exec`), which costs 4/3 in size — so the
/// largest raw stdin this route accepts is ~12 MiB, and a large response
/// likewise. Every other route keeps axum's 2 MiB default.
pub const EXEC_BODY_LIMIT_BYTES: usize = 16 * 1024 * 1024;

/// Default `--max-connections`.
pub const DEFAULT_MAX_CONNECTIONS: usize = 256;

/// Default `--request-timeout-secs`.
pub const DEFAULT_REQUEST_TIMEOUT_SECS: u64 = 60;

/// How long a client may take to finish sending request headers before its
/// connection is closed. Also applies to an idle keep-alive connection.
pub const HEADER_READ_TIMEOUT: Duration = Duration::from_secs(30);

/// How long in-flight REST connections get to finish on shutdown before the
/// server stops waiting for them.
pub const SHUTDOWN_DRAIN_TIMEOUT: Duration = Duration::from_secs(5);

/// Comment keepalive interval for the `logs?follow=true` SSE stream, so an
/// idle-follow client (or a proxy) can tell the stream is still alive.
pub const SSE_KEEPALIVE_INTERVAL: Duration = Duration::from_secs(15);

/// Default `--max-ssh-sessions`.
pub const DEFAULT_MAX_SSH_SESSIONS: usize = 64;

/// Default `--max-ssh-sessions-per-listener`.
pub const DEFAULT_MAX_SSH_SESSIONS_PER_LISTENER: usize = 16;

/// How long an SSH client has to complete the transport handshake (version
/// exchange + key exchange) before MC2 closes the connection. There is no
/// idle timeout after that: interactive `tmux` / agent sessions must survive.
pub const SSH_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);

/// REST listener admission limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HttpLimits {
    /// Concurrent accepted TCP connections. The `N+1`th is shed with a `503`
    /// instead of queueing.
    pub max_connections: usize,
    /// Whole-request deadline for ordinary routes (`408 Request Timeout`).
    /// Long-running routes (`exec`, `logs`) are exempt.
    pub request_timeout: Duration,
    /// Deadline for a client to finish sending request headers.
    pub header_read_timeout: Duration,
}

impl Default for HttpLimits {
    fn default() -> Self {
        Self {
            max_connections: DEFAULT_MAX_CONNECTIONS,
            request_timeout: Duration::from_secs(DEFAULT_REQUEST_TIMEOUT_SECS),
            header_read_timeout: HEADER_READ_TIMEOUT,
        }
    }
}

/// SSH listener admission limits (process-wide: one process is one node).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SshLimits {
    /// Handshake deadline for one accepted SSH connection.
    pub handshake_timeout: Duration,
    /// Concurrent SSH sessions across all listeners.
    pub max_sessions: usize,
    /// Concurrent SSH sessions for a single instance listener.
    pub max_sessions_per_listener: usize,
}

impl Default for SshLimits {
    fn default() -> Self {
        Self {
            handshake_timeout: SSH_HANDSHAKE_TIMEOUT,
            max_sessions: DEFAULT_MAX_SSH_SESSIONS,
            max_sessions_per_listener: DEFAULT_MAX_SSH_SESSIONS_PER_LISTENER,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn per_listener_cap_never_exceeds_the_global_cap() {
        // Otherwise one listener could hold every session permit.
        let ssh = SshLimits::default();
        assert!(ssh.max_sessions_per_listener <= ssh.max_sessions);
    }
}
