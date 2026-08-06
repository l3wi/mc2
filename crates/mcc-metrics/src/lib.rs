//! Optional OpenTelemetry OTLP metrics for MicroCommandControl.
//!
//! When `MCC_OTLP_ENDPOINT` or `OTEL_EXPORTER_OTLP_ENDPOINT` is set, metrics are
//! exported via OTLP/gRPC. Otherwise init is a no-op and record helpers are silent.
//!
//! Sandbox CPU/mem/net stay on **msb-metrics**; this crate only covers MCC
//! control-plane and agent process metrics.

use anyhow::{Context, Result};
use opentelemetry::metrics::{Counter, Gauge, Meter};
use opentelemetry::{global, KeyValue};
use opentelemetry_otlp::{MetricExporter, WithExportConfig};
use opentelemetry_sdk::metrics::{PeriodicReader, SdkMeterProvider};
use std::sync::OnceLock;
use std::time::Duration;
use tracing::info;

static PROVIDER: OnceLock<SdkMeterProvider> = OnceLock::new();
static METRICS: OnceLock<MccMetrics> = OnceLock::new();

/// Process-level meters (lazy; empty if OTLP not configured).
#[derive(Clone)]
pub struct MccMetrics {
    pub heartbeats: Counter<u64>,
    pub reconciles: Counter<u64>,
    pub reconcile_errors: Counter<u64>,
    pub applies: Counter<u64>,
    pub schedule_binds: Counter<u64>,
    pub reschedule_unbinds: Counter<u64>,
    pub instances: Gauge<u64>,
    pub nodes_ready: Gauge<u64>,
    pub nodes_total: Gauge<u64>,
}

impl MccMetrics {
    fn new(meter: Meter) -> Self {
        Self {
            heartbeats: meter
                .u64_counter("mcc.agent.heartbeats")
                .with_description("Agent heartbeat RPCs sent")
                .build(),
            reconciles: meter
                .u64_counter("mcc.agent.reconciles")
                .with_description("Agent reconcile loops completed")
                .build(),
            reconcile_errors: meter
                .u64_counter("mcc.agent.reconcile_errors")
                .with_description("Agent reconcile failures")
                .build(),
            applies: meter
                .u64_counter("mcc.server.applies")
                .with_description("Stack apply operations")
                .build(),
            schedule_binds: meter
                .u64_counter("mcc.server.schedule_binds")
                .with_description("Instances bound to a node by the scheduler")
                .build(),
            reschedule_unbinds: meter
                .u64_counter("mcc.server.reschedule_unbinds")
                .with_description("Instances unbound from NotReady nodes")
                .build(),
            instances: meter
                .u64_gauge("mcc.server.instances")
                .with_description("Cluster instance count by phase")
                .build(),
            nodes_ready: meter
                .u64_gauge("mcc.server.nodes_ready")
                .with_description("Ready nodes")
                .build(),
            nodes_total: meter
                .u64_gauge("mcc.server.nodes_total")
                .with_description("Registered nodes")
                .build(),
        }
    }
}

/// Resolve OTLP endpoint from env (MCC preferred, then standard OTEL).
pub fn otlp_endpoint_from_env() -> Option<String> {
    std::env::var("MCC_OTLP_ENDPOINT")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| {
            std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT")
                .ok()
                .filter(|s| !s.trim().is_empty())
        })
        .or_else(|| {
            std::env::var("OTEL_EXPORTER_OTLP_METRICS_ENDPOINT")
                .ok()
                .filter(|s| !s.trim().is_empty())
        })
}

/// Install global meter provider when an OTLP endpoint is configured.
///
/// `service_name` should be `mcc-server` or `mcc-agent`.
pub fn init(service_name: &str) -> Result<bool> {
    let Some(endpoint) = otlp_endpoint_from_env() else {
        info!(
            service = service_name,
            "OTLP metrics disabled (set MCC_OTLP_ENDPOINT or OTEL_EXPORTER_OTLP_ENDPOINT)"
        );
        return Ok(false);
    };

    let exporter = MetricExporter::builder()
        .with_tonic()
        .with_endpoint(endpoint.clone())
        .build()
        .context("build OTLP metric exporter")?;

    let reader = PeriodicReader::builder(exporter)
        .with_interval(Duration::from_secs(15))
        .build();

    let provider = SdkMeterProvider::builder().with_reader(reader).build();

    global::set_meter_provider(provider.clone());
    let _ = PROVIDER.set(provider);

    let meter = global::meter_with_scope(
        opentelemetry::InstrumentationScope::builder("mcc")
            .with_version(env!("CARGO_PKG_VERSION"))
            .with_attributes([KeyValue::new("service.name", service_name.to_string())])
            .build(),
    );
    let _ = METRICS.set(MccMetrics::new(meter));

    info!(
        service = service_name,
        %endpoint,
        "OTLP metrics export enabled"
    );
    Ok(true)
}

/// Shut down the meter provider (flush). Safe if never initialized.
pub fn shutdown() {
    if let Some(p) = PROVIDER.get() {
        let _ = p.shutdown();
    }
}

/// Global metrics handle if OTLP was initialized.
pub fn metrics() -> Option<&'static MccMetrics> {
    METRICS.get()
}

pub fn record_heartbeat() {
    if let Some(m) = metrics() {
        m.heartbeats.add(1, &[]);
    }
}

pub fn record_reconcile(ok: bool) {
    if let Some(m) = metrics() {
        m.reconciles.add(1, &[]);
        if !ok {
            m.reconcile_errors.add(1, &[]);
        }
    }
}

pub fn record_apply() {
    if let Some(m) = metrics() {
        m.applies.add(1, &[]);
    }
}

pub fn record_schedule_binds(n: u64) {
    if let Some(m) = metrics() {
        if n > 0 {
            m.schedule_binds.add(n, &[]);
        }
    }
}

pub fn record_reschedule_unbinds(n: u64) {
    if let Some(m) = metrics() {
        if n > 0 {
            m.reschedule_unbinds.add(n, &[]);
        }
    }
}

pub fn set_cluster_gauges(nodes_total: u64, nodes_ready: u64, phase_counts: &[(String, u64)]) {
    let Some(m) = metrics() else {
        return;
    };
    m.nodes_total.record(nodes_total, &[]);
    m.nodes_ready.record(nodes_ready, &[]);
    for (phase, n) in phase_counts {
        m.instances
            .record(*n, &[KeyValue::new("phase", phase.clone())]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_without_endpoint_is_noop() {
        // Typical CI: no OTLP endpoint → no provider.
        if otlp_endpoint_from_env().is_none() {
            assert!(!init("mcc-test").unwrap());
        }
    }

    #[test]
    fn record_helpers_do_not_panic_without_init() {
        record_heartbeat();
        record_reconcile(true);
        record_apply();
        record_schedule_binds(2);
        record_reschedule_unbinds(1);
        set_cluster_gauges(1, 1, &[("Running".into(), 1)]);
    }
}
