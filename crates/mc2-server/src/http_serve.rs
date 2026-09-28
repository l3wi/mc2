//! REST listener admission control: connection cap, header-read deadline and
//! graceful shutdown.
//!
//! `axum::serve` exposes no knob for a header-read deadline, so the accept
//! loop is built directly on hyper's HTTP/1 builder. A semaphore in front of
//! the loop is the connection cap: the `N+1`th concurrent connection is shed
//! with a `503` instead of queueing (every client is `127.0.0.1` behind
//! Traefik, so the cap is global — never per-IP).

use crate::limits::{HttpLimits, SHUTDOWN_DRAIN_TIMEOUT};
use anyhow::{Context, Result};
use axum::Router;
use hyper_util::rt::{TokioIo, TokioTimer};
use hyper_util::server::graceful::GracefulShutdown;
use hyper_util::service::TowerToHyperService;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{oneshot, Semaphore};
use tracing::{debug, info, warn};

/// Serve `app` on `listener` until `shutdown` resolves, then drain.
pub async fn serve(
    listener: TcpListener,
    app: Router,
    limits: HttpLimits,
    shutdown: oneshot::Receiver<()>,
) -> Result<()> {
    let addr = listener.local_addr().context("rest local_addr")?;
    let connections = Arc::new(Semaphore::new(limits.max_connections.max(1)));
    let graceful = GracefulShutdown::new();
    let mut builder = hyper::server::conn::http1::Builder::new();
    // The header-read deadline only takes effect with a timer installed.
    builder
        .timer(TokioTimer::new())
        .header_read_timeout(limits.header_read_timeout);

    info!(
        %addr,
        max_connections = limits.max_connections,
        request_timeout_secs = limits.request_timeout.as_secs(),
        header_read_timeout_secs = limits.header_read_timeout.as_secs(),
        "REST listening (GET /health, /v1/status, /v1/nodes)"
    );

    tokio::pin!(shutdown);
    loop {
        let accepted = tokio::select! {
            biased;
            _ = &mut shutdown => break,
            accepted = listener.accept() => accepted,
        };
        let (stream, peer) = match accepted {
            Ok(accepted) => accepted,
            Err(e) => {
                warn!(error = %e, "REST accept failed");
                // Transient per-connection errors retry immediately; anything
                // else (e.g. `EMFILE`) backs off so the loop cannot spin.
                if !is_connection_error(&e) {
                    tokio::select! {
                        biased;
                        _ = &mut shutdown => break,
                        _ = tokio::time::sleep(ACCEPT_ERROR_BACKOFF) => {}
                    }
                }
                continue;
            }
        };
        let Ok(permit) = connections.clone().try_acquire_owned() else {
            debug!(
                %peer,
                max_connections = limits.max_connections,
                "REST connection limit reached; shedding the connection"
            );
            tokio::spawn(shed_connection(stream));
            continue;
        };
        let _ = stream.set_nodelay(true);
        // `serve_connection` borrows the builder and returns an owned
        // connection, so the builder is reused for the whole listener.
        //
        // No `with_upgrades()`: no MC2 route uses HTTP upgrades, and
        // hyper-util's `GracefulShutdown` has no `GracefulConnection` impl for
        // the upgradeable HTTP/1 connection.
        let conn =
            builder.serve_connection(TokioIo::new(stream), TowerToHyperService::new(app.clone()));
        let watcher = graceful.watcher();
        tokio::spawn(async move {
            if let Err(e) = watcher.watch(conn).await {
                debug!(%peer, error = %e, "REST connection ended");
            }
            drop(permit);
        });
    }
    drop(listener);

    // Drain in-flight requests, but never hang shutdown on an open SSE stream
    // or a long `exec`.
    if tokio::time::timeout(SHUTDOWN_DRAIN_TIMEOUT, graceful.shutdown())
        .await
        .is_err()
    {
        warn!("REST connections still open after the drain deadline; stopping anyway");
    }
    Ok(())
}

/// Backoff after an accept error that is not a per-connection one (e.g.
/// `EMFILE`), so the accept loop cannot spin.
const ACCEPT_ERROR_BACKOFF: Duration = Duration::from_secs(1);

/// Per-connection accept errors (a client that vanished mid-handshake): retry
/// immediately instead of backing off.
fn is_connection_error(e: &std::io::Error) -> bool {
    matches!(
        e.kind(),
        std::io::ErrorKind::ConnectionRefused
            | std::io::ErrorKind::ConnectionAborted
            | std::io::ErrorKind::ConnectionReset
    )
}

/// Answer a shed connection with a bare `503` and close it.
async fn shed_connection(mut stream: TcpStream) {
    const BODY: &str = r#"{"error":"connection limit reached; retry shortly"}"#;
    let response = format!(
        "HTTP/1.1 503 Service Unavailable\r\n\
         content-type: application/json\r\n\
         content-length: {}\r\n\
         connection: close\r\n\
         \r\n{}",
        BODY.len(),
        BODY
    );
    if let Err(e) = stream.write_all(response.as_bytes()).await {
        debug!(error = %e, "failed to write the load-shed response");
    }
    let _ = stream.shutdown().await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::routing::get;
    use std::net::SocketAddr;
    use tokio::io::AsyncReadExt;

    fn ping_router() -> Router {
        Router::new().route("/health", get(|| async { "ok" }))
    }

    /// Start the real serve loop on an ephemeral loopback port.
    async fn start(limits: HttpLimits) -> (SocketAddr, oneshot::Sender<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = oneshot::channel();
        tokio::spawn(async move {
            let _ = serve(listener, ping_router(), limits, rx).await;
        });
        (addr, tx)
    }

    #[tokio::test]
    async fn health_is_served() {
        let (addr, _shutdown) = start(HttpLimits::default()).await;
        let mut client = TcpStream::connect(addr).await.unwrap();
        client
            .write_all(b"GET /health HTTP/1.1\r\nhost: mc2\r\nconnection: close\r\n\r\n")
            .await
            .unwrap();
        let mut buf = [0u8; 256];
        let n = tokio::time::timeout(Duration::from_secs(5), client.read(&mut buf))
            .await
            .expect("health should answer")
            .expect("read the response");
        assert!(
            String::from_utf8_lossy(&buf[..n]).starts_with("HTTP/1.1 200"),
            "{}",
            String::from_utf8_lossy(&buf[..n])
        );
    }

    #[tokio::test]
    async fn connection_cap_sheds_the_extra_connection() {
        let limits = HttpLimits {
            max_connections: 2,
            request_timeout: Duration::from_secs(5),
            header_read_timeout: Duration::from_secs(30),
        };
        let (addr, _shutdown) = start(limits).await;

        // Two silent connections hold both permits (a request may still arrive
        // within the header deadline) ...
        let slow_one = TcpStream::connect(addr).await.unwrap();
        let slow_two = TcpStream::connect(addr).await.unwrap();

        // ... so the third is shed immediately, with a 503 and a close.
        let mut shed = TcpStream::connect(addr).await.unwrap();
        let mut buf = [0u8; 256];
        let n = tokio::time::timeout(Duration::from_secs(5), shed.read(&mut buf))
            .await
            .expect("shed connection should answer immediately")
            .expect("read the shed response");
        let response = String::from_utf8_lossy(&buf[..n]);
        assert!(response.starts_with("HTTP/1.1 503"), "{response}");

        let n = tokio::time::timeout(Duration::from_secs(5), shed.read(&mut buf))
            .await
            .expect("shed connection should close")
            .expect("read to EOF");
        assert_eq!(n, 0, "shed connection should be closed after the 503");

        drop((slow_one, slow_two));
    }

    #[tokio::test]
    async fn silent_client_is_disconnected_at_the_header_deadline() {
        let limits = HttpLimits {
            max_connections: 8,
            request_timeout: Duration::from_secs(5),
            header_read_timeout: Duration::from_millis(200),
        };
        let (addr, _shutdown) = start(limits).await;

        // A client that never sends a request line must be closed.
        let mut client = TcpStream::connect(addr).await.unwrap();
        let give_up = tokio::time::Instant::now() + Duration::from_secs(5);
        let mut buf = [0u8; 64];
        loop {
            match tokio::time::timeout_at(give_up, client.read(&mut buf)).await {
                // Closed (with or without a final error response): done.
                Ok(Ok(0)) | Ok(Err(_)) => break,
                // Some bytes (an error response) then EOF.
                Ok(Ok(_)) => continue,
                Err(_) => panic!("connection was still open after the header-read deadline"),
            }
        }
    }
}
