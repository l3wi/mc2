//! Host-side SSH serve for instances via the **microsandbox SDK only**.
//!
//! No `msb` CLI subprocess. Uses `Sandbox::ssh().server_with(...).serve(stream)`
//! over a host TCP listener (default bind 127.0.0.1, auto port when port=0).
//!
//! Admission control (A10): every accepted socket is placed behind a *global*
//! and a *per-listener* session cap (rejected connections are closed), gets
//! TCP keepalive, and must complete the SSH handshake within 30 s
//! (`limits::SSH_HANDSHAKE_TIMEOUT`). There is deliberately **no idle
//! timeout**: `tmux` and agent sessions must survive long quiet periods.

use crate::limits::SshLimits;
use mc2_runtime::{DesiredSandbox, SshObserved, SshPhase};
use microsandbox::Sandbox;
use std::collections::HashMap;
use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::{Arc, OnceLock};
use std::task::{ready, Context as TaskContext, Poll};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::task::JoinHandle;
use tokio::time::Sleep;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

/// Bytes each side must exchange before the handshake deadline is disarmed.
///
/// Both peers send their SSH identification string first (well under this),
/// then a key-exchange packet that is far larger, so any client that actually
/// starts the key exchange clears this bar within the first round trip. After
/// that MC2 never interrupts the session.
const HANDSHAKE_ARM_BYTES: usize = 64;

/// Process-wide SSH admission limits, installed once at startup.
static SSH_LIMITS: OnceLock<SshLimits> = OnceLock::new();

/// Install the SSH admission limits for this process (first call wins).
pub fn configure_limits(limits: SshLimits) {
    let _ = SSH_LIMITS.set(limits);
}

fn ssh_limits() -> SshLimits {
    SSH_LIMITS.get().copied().unwrap_or_default()
}

struct ActiveServe {
    config_hash: String,
    bind: String,
    port: u16,
    /// Per-listener shutdown (a child of the process token): cancelling it
    /// stops the accept loop.
    shutdown: CancellationToken,
    join: JoinHandle<()>,
}

/// Per-node map of instance_id → in-process SSH accept loop.
pub struct SshServeTable {
    active: HashMap<String, ActiveServe>,
    limits: SshLimits,
    /// Global session cap, shared by every listener in this process.
    sessions: Arc<Semaphore>,
    /// Process shutdown token: every listener's accept loop selects on a child
    /// of it so `run()`'s cancel stops accepting immediately (D2).
    cancel: CancellationToken,
}

impl SshServeTable {
    pub(crate) fn new() -> Self {
        let limits = ssh_limits();
        info!("ssh serve: microsandbox SDK (in-process)");
        Self {
            active: HashMap::new(),
            limits,
            sessions: Arc::new(Semaphore::new(limits.max_sessions.max(1))),
            cancel: CancellationToken::new(),
        }
    }

    /// Install the process shutdown token. Called once, before the first pass.
    pub fn set_cancel(&mut self, cancel: CancellationToken) {
        self.cancel = cancel;
    }

    pub async fn reconcile(
        &mut self,
        desired: &DesiredSandbox,
        sandbox_running: bool,
    ) -> SshObserved {
        let want = desired.ssh.enabled
            && sandbox_running
            && !desired.ssh.authorized_public_keys.is_empty();

        if !want {
            self.close(&desired.instance_id).await;
            return SshObserved {
                phase: SshPhase::Closed.as_str().into(),
                bind: String::new(),
                port: 0,
                message: if desired.ssh.enabled && !sandbox_running {
                    "waiting for Running".into()
                } else if desired.ssh.enabled && desired.ssh.authorized_public_keys.is_empty() {
                    "no authorized keys".into()
                } else {
                    String::new()
                },
            };
        }

        if let Some(active) = self.active.get(&desired.instance_id) {
            if active.config_hash == desired.ssh.config_hash && !active.join.is_finished() {
                return SshObserved {
                    phase: SshPhase::Open.as_str().into(),
                    bind: active.bind.clone(),
                    port: active.port,
                    message: "sdk".into(),
                };
            }
            self.close(&desired.instance_id).await;
        }

        let limits = self.limits;
        let listener_cancel = self.cancel.child_token();
        match start_serve_sdk(desired, limits, Arc::clone(&self.sessions), listener_cancel).await {
            Ok(active) => {
                let obs = SshObserved {
                    phase: SshPhase::Open.as_str().into(),
                    bind: active.bind.clone(),
                    port: active.port,
                    message: "sdk".into(),
                };
                info!(
                    instance = %desired.instance_id,
                    runtime_id = %desired.runtime_id,
                    bind = %active.bind,
                    port = active.port,
                    "ssh serve open (sdk)"
                );
                self.active.insert(desired.instance_id.clone(), active);
                obs
            }
            Err(e) => {
                warn!(instance = %desired.instance_id, error = %e, "ssh serve failed");
                SshObserved {
                    phase: SshPhase::Failed.as_str().into(),
                    bind: String::new(),
                    port: 0,
                    message: e.to_string(),
                }
            }
        }
    }

    pub async fn close(&mut self, instance_id: &str) {
        if let Some(active) = self.active.remove(instance_id) {
            active.shutdown.cancel();
            active.join.abort();
            debug!(instance = %instance_id, "ssh serve closed");
        }
    }

    /// Stop every listener. Called as the node loop exits (D2).
    pub async fn close_all(&mut self) {
        let ids: Vec<String> = self.active.keys().cloned().collect();
        for id in ids {
            self.close(&id).await;
        }
    }

    pub async fn close_missing(&mut self, keep: &std::collections::HashSet<String>) {
        let stale: Vec<String> = self
            .active
            .keys()
            .filter(|id| !keep.contains(*id))
            .cloned()
            .collect();
        for id in stale {
            self.close(&id).await;
        }
    }
}

/// One session slot from each cap.
struct SessionCaps {
    global: Arc<Semaphore>,
    per_listener: Arc<Semaphore>,
}

impl SessionCaps {
    fn new(global: Arc<Semaphore>, per_listener: usize) -> Self {
        Self {
            global,
            per_listener: Arc::new(Semaphore::new(per_listener.max(1))),
        }
    }

    /// Take a slot from both caps, or `None` when either is exhausted.
    fn try_acquire(&self) -> Option<(OwnedSemaphorePermit, OwnedSemaphorePermit)> {
        let global = Arc::clone(&self.global).try_acquire_owned().ok()?;
        match Arc::clone(&self.per_listener).try_acquire_owned() {
            Ok(per_listener) => Some((global, per_listener)),
            // `global` is dropped here, releasing the slot it took.
            Err(_) => None,
        }
    }
}

/// An accepted SSH socket with a handshake deadline.
///
/// Reads and writes fail once the deadline passes, which closes a client that
/// never starts the key exchange. The deadline is disarmed permanently as soon
/// as both sides have exchanged more than their identification strings, so an
/// established session is never interrupted (no idle timeout).
pub(crate) struct SshConnection {
    inner: TcpStream,
    deadline: Option<Pin<Box<Sleep>>>,
    read_bytes: usize,
    written_bytes: usize,
}

impl SshConnection {
    fn new(inner: TcpStream, handshake_timeout: Duration) -> Self {
        Self {
            inner,
            deadline: Some(Box::pin(tokio::time::sleep(handshake_timeout))),
            read_bytes: 0,
            written_bytes: 0,
        }
    }

    fn disarm_if_handshaked(&mut self) {
        if self.read_bytes >= HANDSHAKE_ARM_BYTES && self.written_bytes >= HANDSHAKE_ARM_BYTES {
            self.deadline = None;
        }
    }

    /// `Err` once the handshake deadline has passed while still armed.
    fn poll_deadline(&mut self, cx: &mut TaskContext<'_>) -> Poll<io::Result<()>> {
        let Some(deadline) = self.deadline.as_mut() else {
            return Poll::Ready(Ok(()));
        };
        match Future::poll(deadline.as_mut(), cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(()) => Poll::Ready(Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "ssh handshake deadline exceeded",
            ))),
        }
    }
}

impl AsyncRead for SshConnection {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        if let Poll::Ready(Err(e)) = this.poll_deadline(cx) {
            return Poll::Ready(Err(e));
        }
        let before = buf.filled().len();
        ready!(Pin::new(&mut this.inner).poll_read(cx, buf))?;
        this.read_bytes += buf.filled().len() - before;
        this.disarm_if_handshaked();
        Poll::Ready(Ok(()))
    }
}

impl AsyncWrite for SshConnection {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        if let Poll::Ready(Err(e)) = this.poll_deadline(cx) {
            return Poll::Ready(Err(e));
        }
        let n = ready!(Pin::new(&mut this.inner).poll_write(cx, buf))?;
        this.written_bytes += n;
        this.disarm_if_handshaked();
        Poll::Ready(Ok(n))
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }
}

/// Keepalive on accepted SSH sockets so a peer that vanished (suspended
/// laptop, dead NAT) is detected by TCP instead of pinning a session slot.
fn set_tcp_keepalive(stream: &TcpStream) {
    let keepalive = socket2::TcpKeepalive::new()
        .with_time(Duration::from_secs(60))
        .with_interval(Duration::from_secs(30));
    if let Err(e) = socket2::SockRef::from(stream).set_tcp_keepalive(&keepalive) {
        debug!(error = %e, "ssh: could not enable TCP keepalive");
    }
}

/// Accept loop for one instance's SSH listener.
///
/// `serve_connection` runs a single accepted connection. It is called only
/// while both session caps have a free slot; beyond that the socket is closed.
/// The loop stops accepting when `cancel` fires (per-listener close or process
/// shutdown, D2).
async fn ssh_accept_loop<F, Fut>(
    listener: TcpListener,
    caps: SessionCaps,
    limits: SshLimits,
    serve_connection: F,
    cancel: CancellationToken,
) where
    F: Fn(SshConnection) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    loop {
        let accepted = tokio::select! {
            biased;
            _ = cancel.cancelled() => break,
            accepted = listener.accept() => accepted,
        };
        let (stream, peer) = match accepted {
            Ok(accepted) => accepted,
            Err(e) => {
                warn!(error = %e, "ssh accept failed");
                break;
            }
        };
        let Some(session) = caps.try_acquire() else {
            debug!(%peer, "ssh session cap reached; closing connection");
            // Dropping `stream` closes the connection.
            continue;
        };
        set_tcp_keepalive(&stream);
        let conn = SshConnection::new(stream, limits.handshake_timeout);
        let fut = serve_connection(conn);
        tokio::spawn(async move {
            // Hold both permits for the whole session.
            let _session = session;
            fut.await;
        });
    }
}

async fn start_serve_sdk(
    desired: &DesiredSandbox,
    limits: SshLimits,
    sessions: Arc<Semaphore>,
    cancel: CancellationToken,
) -> anyhow::Result<ActiveServe> {
    let bind_ip: std::net::IpAddr = desired
        .ssh
        .bind
        .parse()
        .unwrap_or_else(|_| std::net::IpAddr::from([127, 0, 0, 1]));

    let listener = if desired.ssh.port == 0 {
        TcpListener::bind(SocketAddr::new(bind_ip, 0)).await?
    } else {
        TcpListener::bind(SocketAddr::new(bind_ip, desired.ssh.port)).await?
    };
    let local = listener.local_addr()?;
    let port = local.port();
    let bind = desired.ssh.bind.clone();

    let handle = Sandbox::get(&desired.runtime_id)
        .await
        .map_err(|e| anyhow::anyhow!("Sandbox::get({}): {e}", desired.runtime_id))?;
    let sb = handle
        .connect()
        .await
        .map_err(|e| anyhow::anyhow!("Sandbox::connect({}): {e}", desired.runtime_id))?;

    let keys = desired.ssh.authorized_public_keys.clone();
    let user = desired.ssh.user.clone();
    let sftp = desired.ssh.sftp;
    let server = sb
        .ssh()
        .server_with(|opts| {
            let mut o = opts.user(user).sftp(sftp);
            for k in keys {
                o = o.authorized_key(k);
            }
            o
        })
        .await
        .map_err(|e| anyhow::anyhow!("ssh server_with: {e}"))?;

    let caps = SessionCaps::new(sessions, limits.max_sessions_per_listener);
    let server = Arc::new(server);
    let join = tokio::spawn(ssh_accept_loop(
        listener,
        caps,
        limits,
        move |conn| {
            let server = Arc::clone(&server);
            async move {
                if let Err(e) = server.serve(conn).await {
                    debug!(error = %e, "ssh connection ended");
                }
            }
        },
        cancel.clone(),
    ));

    Ok(ActiveServe {
        config_hash: desired.ssh.config_hash.clone(),
        bind,
        port,
        shutdown: cancel,
        join,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// Start the accept loop on an ephemeral loopback port. Cancelling the
    /// returned token stops the accept loop (and closes the listener).
    async fn start_loop<F, Fut>(
        limits: SshLimits,
        serve_connection: F,
    ) -> (SocketAddr, CancellationToken)
    where
        F: Fn(SshConnection) -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let caps = SessionCaps::new(
            Arc::new(Semaphore::new(limits.max_sessions.max(1))),
            limits.max_sessions_per_listener,
        );
        let cancel = CancellationToken::new();
        tokio::spawn(ssh_accept_loop(
            listener,
            caps,
            limits,
            serve_connection,
            cancel.clone(),
        ));
        (addr, cancel)
    }

    #[tokio::test]
    async fn cancel_stops_accepting() {
        let (addr, cancel) = start_loop(limits(Duration::from_secs(5)), drain_forever).await;
        // Accepted while live.
        let _live = TcpStream::connect(addr).await.unwrap();
        cancel.cancel();
        // Once the loop returns its listener is dropped and connections are
        // refused.
        let mut refused = false;
        for _ in 0..100 {
            if TcpStream::connect(addr).await.is_err() {
                refused = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(refused, "the listener must stop accepting after cancel");
    }

    fn limits(handshake_timeout: Duration) -> SshLimits {
        SshLimits {
            handshake_timeout,
            max_sessions: 4,
            max_sessions_per_listener: 4,
        }
    }

    /// Read from an accepted connection until the peer goes away.
    async fn drain_forever(mut conn: SshConnection) {
        let mut buf = [0u8; 128];
        loop {
            match conn.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(_) => continue,
            }
        }
    }

    #[tokio::test]
    async fn handshake_deadline_closes_a_silent_client() {
        let (addr, _cancel) = start_loop(limits(Duration::from_millis(200)), drain_forever).await;

        let mut client = TcpStream::connect(addr).await.unwrap();
        let mut buf = [0u8; 16];
        let n = tokio::time::timeout(Duration::from_secs(5), client.read(&mut buf))
            .await
            .expect("a silent client must be closed at the handshake deadline")
            .expect("read to EOF");
        assert_eq!(n, 0, "the connection should be closed");
    }

    #[tokio::test]
    async fn handshake_deadline_is_disarmed_once_the_key_exchange_starts() {
        let (addr, _cancel) =
            start_loop(limits(Duration::from_millis(200)), |mut conn| async move {
                let mut buf = [0u8; 256];
                let mut read = 0usize;
                while read < HANDSHAKE_ARM_BYTES {
                    match conn.read(&mut buf).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => read += n,
                    }
                }
                // Answer with a key-exchange-sized packet, then hold the session
                // open well past both the handshake deadline and the client's
                // observation window below.
                if conn.write_all(&[0u8; HANDSHAKE_ARM_BYTES]).await.is_err() {
                    return;
                }
                tokio::time::sleep(Duration::from_secs(5)).await;
            })
            .await;

        let mut client = TcpStream::connect(addr).await.unwrap();
        client.write_all(&[0u8; HANDSHAKE_ARM_BYTES]).await.unwrap();
        let mut buf = [0u8; HANDSHAKE_ARM_BYTES];
        let n = tokio::time::timeout(Duration::from_secs(2), client.read(&mut buf))
            .await
            .expect("the echo should arrive")
            .expect("read the echo");
        assert_eq!(n, HANDSHAKE_ARM_BYTES);

        // Past the handshake deadline the session is still open: no idle
        // timeout, and the deadline was disarmed.
        let after = tokio::time::timeout(Duration::from_millis(1200), client.read(&mut buf)).await;
        assert!(
            after.is_err(),
            "an established session must not be closed by the handshake deadline"
        );
    }

    #[tokio::test]
    async fn session_cap_closes_connections_beyond_the_limit() {
        let limits = SshLimits {
            handshake_timeout: Duration::from_secs(30),
            max_sessions: 1,
            max_sessions_per_listener: 1,
        };
        let (addr, _cancel) = start_loop(limits, drain_forever).await;

        // The first connection holds the only session slot.
        let mut first = TcpStream::connect(addr).await.unwrap();
        let mut second = TcpStream::connect(addr).await.unwrap();

        let mut buf = [0u8; 16];
        let n = tokio::time::timeout(Duration::from_secs(5), second.read(&mut buf))
            .await
            .expect("a connection beyond the cap must be closed")
            .expect("read to EOF");
        assert_eq!(n, 0);

        // The session inside the cap is untouched.
        first.write_all(b"ping").await.unwrap();
        let still_open =
            tokio::time::timeout(Duration::from_millis(200), first.read(&mut buf)).await;
        assert!(still_open.is_err(), "the first session should stay open");
    }

    #[tokio::test]
    async fn session_caps_are_shared_and_per_listener() {
        let global = Arc::new(Semaphore::new(2));
        let listener_a = SessionCaps::new(Arc::clone(&global), 1);
        let listener_b = SessionCaps::new(Arc::clone(&global), 5);

        // Per-listener cap: the second slot on A is refused even though the
        // global cap has room.
        let a1 = listener_a.try_acquire().expect("first A session");
        assert!(listener_a.try_acquire().is_none(), "A is capped at 1");

        // Global cap: B's own limit allows more, but only one global slot is
        // left.
        let b1 = listener_b.try_acquire().expect("first B session");
        assert!(
            listener_b.try_acquire().is_none(),
            "the global cap of 2 is reached"
        );

        drop((a1, b1));
        assert!(listener_a.try_acquire().is_some(), "slots are released");
    }
}
