//! Background tasks: mark nodes NotReady when heartbeats go stale.

use mcc_store::Store;
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, info};

/// Periodically marks stale Ready nodes as NotReady.
pub async fn not_ready_loop(store: Arc<dyn Store>, grace: Duration, interval: Duration) {
    let mut tick = tokio::time::interval(interval);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        match store.mark_stale_nodes(grace).await {
            Ok(0) => debug!("not-ready watcher: no stale nodes"),
            Ok(n) => info!(count = n, "marked nodes NotReady (missed heartbeat)"),
            Err(e) => tracing::error!(error = %e, "mark_stale_nodes failed"),
        }
    }
}
