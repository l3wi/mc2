//! Disk-limit detection (A9): root-disk and volume usage → typed conditions.
//!
//! Root-disk usage comes from the microsandbox metrics registry once per
//! reconcile pass (a shared-memory read). Volume usage needs a host directory
//! walk, which is expensive for a large volume, so samples are cached and at
//! most [`VOLUME_SAMPLE_TTL`] old (the runtime measures usage fresh before
//! every VM create, where the quota must be exact).
//!
//! Active conditions are appended to the instance report message as the
//! text + `[mc2:disk]` marker pair produced by [`mc2_api::disk`], so `mc2 ps`
//! can show a short note and `mc2 exec`/`ssh`/`logs` the full explanation.

use mc2_api::disk::DiskCondition;
use mc2_api::ServiceSpec;
use mc2_runtime::{volume_name, DesiredSandbox, DiskUsage, InstanceReport};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};
use tracing::{debug, info, warn};

/// Maximum age of a cached volume-usage sample.
pub const VOLUME_SAMPLE_TTL: Duration = Duration::from_secs(60);

/// Per-process detection state: volume-usage samples and the last reported
/// condition notes per instance (for change logging).
#[derive(Default)]
struct Monitor {
    volumes: HashMap<(PathBuf, String), (Instant, u64)>,
    last_notes: HashMap<String, String>,
}

static MONITOR: LazyLock<Mutex<Monitor>> = LazyLock::new(|| Mutex::new(Monitor::default()));

/// Run `f` with the monitor, recovering from a poisoned lock (a panic while
/// holding it must not wedge disk reporting).
fn with_monitor<T>(f: impl FnOnce(&mut Monitor) -> T) -> T {
    let mut guard = MONITOR
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    f(&mut guard)
}

/// Measured usage (MiB) of one volume directory, cached for
/// [`VOLUME_SAMPLE_TTL`].
pub fn volume_usage_mib(root: &Path, dir_name: &str) -> u64 {
    let key = (root.to_path_buf(), dir_name.to_string());
    if let Some(used) = with_monitor(|m| {
        m.volumes
            .get(&key)
            .filter(|(at, _)| at.elapsed() < VOLUME_SAMPLE_TTL)
            .map(|(_, used)| *used)
    }) {
        return used;
    }
    let used = measure_volume_mib(root, dir_name);
    with_monitor(|m| {
        m.volumes.insert(key, (Instant::now(), used));
    });
    used
}

/// Measure one volume directory now, bypassing the cache.
fn measure_volume_mib(root: &Path, dir_name: &str) -> u64 {
    let path = root.join(dir_name);
    if !path.is_dir() {
        return 0;
    }
    mc2_runtime::dir_size_mib(&path)
}

/// Active disk conditions for one desired sandbox.
///
/// `root_disk` is `None` when the backend has no root-disk metrics for the
/// sandbox (stopped, or metrics disabled). `volume_used_mib` returns the
/// measured size of a volume directory.
pub fn conditions_for(
    spec: &ServiceSpec,
    root_disk: Option<DiskUsage>,
    volume_used_mib: impl Fn(&str) -> u64,
) -> Vec<DiskCondition> {
    let mut out = Vec::new();
    if let Some(usage) = root_disk {
        let limit = usage.limit_mib(spec.root_disk_mib());
        // The suggestion doubles the *declared* size: `limit` is the smaller
        // guest-visible capacity (ext4 metadata).
        if let Some(c) = DiskCondition::root_disk(usage.used_mib, limit, spec.root_disk_mib()) {
            out.push(c);
        }
    }
    for mount in &spec.volumes {
        let used = volume_used_mib(&mount.name);
        if let Some(c) = DiskCondition::volume(&mount.name, &mount.mount, used, mount.size_mib) {
            out.push(c);
        }
    }
    out
}

/// Append disk conditions to every report, log condition changes and export
/// per-instance usage gauges.
pub fn annotate_reports(
    reports: &mut [InstanceReport],
    desired: &[DesiredSandbox],
    root_disk_usage: &HashMap<String, DiskUsage>,
    volume_root: &Path,
) {
    let by_runtime: HashMap<&str, &DesiredSandbox> =
        desired.iter().map(|d| (d.runtime_id.as_str(), d)).collect();

    for report in reports.iter_mut() {
        let Some(desired) = by_runtime.get(report.runtime_id.as_str()) else {
            continue;
        };
        let root = root_disk_usage.get(&report.runtime_id).copied();
        let usage_for =
            |name: &str| volume_usage_mib(volume_root, &volume_name(&desired.stack, name));

        if let Some(usage) = root {
            mc2_metrics::record_instance_disk(
                &report.instance_id,
                "root-disk",
                usage.used_mib,
                usage.limit_mib(desired.spec.root_disk_mib()),
            );
        }
        for mount in &desired.spec.volumes {
            mc2_metrics::record_instance_disk(
                &report.instance_id,
                &format!("volume:{}", mount.name),
                usage_for(&mount.name),
                mount.size_mib,
            );
        }

        let conditions = conditions_for(&desired.spec, root, usage_for);
        log_change(&report.instance_id, &conditions);
        if conditions.is_empty() {
            continue;
        }
        let block = mc2_api::disk::block(
            &conditions,
            &desired.stack,
            &desired.service,
            desired.ordinal,
        );
        if block.is_empty() {
            continue;
        }
        if !report.message.is_empty() {
            report.message.push('\n');
        }
        report.message.push_str(&block);
    }
}

/// Log a line whenever an instance's active condition set changes.
fn log_change(instance_id: &str, conditions: &[DiskCondition]) {
    let notes = mc2_api::disk::notes_line(conditions);
    let previous = with_monitor(|m| m.last_notes.insert(instance_id.to_string(), notes.clone()));
    if previous.as_deref() == Some(notes.as_str()) {
        return;
    }
    if notes.is_empty() {
        info!(instance_id, "disk conditions cleared");
        return;
    }
    let full = conditions
        .iter()
        .map(|c| format!("{} ({} of {} MiB)", c.short_note(), c.used_mib, c.limit_mib))
        .collect::<Vec<_>>()
        .join("; ");
    let full_severity = conditions
        .iter()
        .any(|c| c.severity == mc2_api::disk::DiskSeverity::Full);
    if full_severity {
        warn!(instance_id, conditions = %full, "disk full: guest writes are failing");
    } else {
        warn!(instance_id, conditions = %full, "disk nearly full");
    }
    debug!(instance_id, notes = %notes, "disk conditions updated");
}

#[cfg(test)]
mod tests {
    use super::*;
    use mc2_api::{VolumeMount, DEFAULT_ROOT_DISK_MIB};
    use std::collections::BTreeMap;

    fn spec(root_disk_mib: u64, volumes: &[(&str, &str, u64)]) -> ServiceSpec {
        ServiceSpec {
            image: "alpine:3.20".into(),
            scale: 1,
            cpus: 1,
            mem_limit_mib: 512,
            ports: vec![],
            network: Default::default(),
            env: Default::default(),
            secrets: vec![],
            volumes: volumes
                .iter()
                .map(|(name, mount, size_mib)| VolumeMount {
                    name: (*name).into(),
                    mount: (*mount).into(),
                    size_mib: *size_mib,
                })
                .collect(),
            restart: "no".into(),
            healthcheck: None,
            labels: Default::default(),
            command: None,
            node_name: None,
            node_selector: Default::default(),
            ssh: None,
            storage_opt: Some(mc2_api::StorageOptSpec {
                size_mib: root_disk_mib,
            }),
            expose: vec![],
            networks: vec![],
            depends_on: BTreeMap::new(),
        }
    }

    fn desired(root_disk_mib: u64, volumes: &[(&str, &str, u64)]) -> DesiredSandbox {
        DesiredSandbox {
            instance_id: "i1".into(),
            stack: "shop".into(),
            service: "web".into(),
            ordinal: 0,
            runtime_id: "shop--web--0".into(),
            spec: spec(root_disk_mib, volumes),
            secrets: vec![],
            ssh: Default::default(),
            network: Default::default(),
        }
    }

    #[test]
    fn conditions_only_appear_at_or_above_the_thresholds() {
        let spec = spec(4096, &[("data", "/data", 10 * 1024)]);
        let used = |mib| {
            Some(DiskUsage {
                used_mib: mib,
                capacity_mib: None,
            })
        };
        let none = conditions_for(&spec, used(1024), |_| 512);
        assert!(none.is_empty(), "{none:?}");

        let both = conditions_for(&spec, used(4096), |_| 10 * 1024);
        assert_eq!(both.len(), 2);
        assert_eq!(both[0].short_note(), "root disk full");
        assert_eq!(both[1].short_note(), "volume data full");
    }

    #[test]
    fn missing_root_metrics_do_not_report_a_root_condition() {
        let spec = spec(DEFAULT_ROOT_DISK_MIB, &[("data", "/data", 10 * 1024)]);
        let conds = conditions_for(&spec, None, |_| 9216);
        assert_eq!(conds.len(), 1, "{conds:?}");
        assert_eq!(conds[0].short_note(), "volume data 90%");
    }

    #[test]
    fn root_disk_fullness_uses_the_guest_visible_capacity() {
        // A 256 MiB root disk shows ~186 MiB usable after ext4 overhead; 181
        // used of that is full even though it is only 70% of the declared size.
        let spec = spec(256, &[]);
        let usage = DiskUsage {
            used_mib: 181,
            capacity_mib: Some(186),
        };
        let conds = conditions_for(&spec, Some(usage), |_| 0);
        assert_eq!(conds.len(), 1, "{conds:?}");
        assert_eq!(conds[0].short_note(), "root disk full");
    }

    #[test]
    fn root_disk_suggestion_uses_the_declared_size_not_the_capacity() {
        // Declared 256 MiB, guest-visible capacity 180 MiB: the guest is full,
        // and the fix doubles the declared size (512MiB) not the capacity
        // (which would suggest 360MiB).
        let spec = spec(256, &[]);
        let usage = DiskUsage {
            used_mib: 180,
            capacity_mib: Some(180),
        };
        let conds = conditions_for(&spec, Some(usage), |_| 0);
        assert_eq!(conds.len(), 1, "{conds:?}");
        assert_eq!(conds[0].declared_mib, Some(256));
        let text = conds[0].message_text("shop", "web", 0);
        assert!(text.contains("size: 512MiB"), "{text}");
    }

    #[test]
    fn volume_usage_is_measured_from_the_directory() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("mc2-shop--data");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("payload"), vec![0u8; 3 * 1024 * 1024]).unwrap();
        assert_eq!(volume_usage_mib(root.path(), "mc2-shop--data"), 3);
        // A missing directory is empty, not an error.
        assert_eq!(volume_usage_mib(root.path(), "mc2-shop--absent"), 0);
        // The sample is cached: growth inside the TTL is not re-walked.
        std::fs::write(dir.join("more"), vec![0u8; 5 * 1024 * 1024]).unwrap();
        assert_eq!(volume_usage_mib(root.path(), "mc2-shop--data"), 3);
    }

    #[test]
    fn annotate_appends_notes_and_full_text() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("mc2-shop--data");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("payload"), vec![0u8; 10 * 1024 * 1024 + 1024]).unwrap();

        let d = desired(4096, &[("data", "/data", 10)]);
        let mut reports = vec![InstanceReport {
            instance_id: "i1".into(),
            phase: "Running".into(),
            message: "microsandbox sdk (local)".into(),
            runtime_id: d.runtime_id.clone(),
            ssh: None,
            network: None,
        }];
        let mut usage = HashMap::new();
        usage.insert(
            d.runtime_id.clone(),
            DiskUsage {
                used_mib: 4096,
                capacity_mib: None,
            },
        );
        annotate_reports(&mut reports, std::slice::from_ref(&d), &usage, root.path());

        let message = reports[0].message.clone();
        assert!(message.starts_with("microsandbox sdk (local)"), "{message}");
        assert!(message.contains("root disk full"), "{message}");
        assert!(message.contains("volume \"data\""), "{message}");
        assert_eq!(
            mc2_api::disk::notes_from_message(&message),
            "root disk full, volume data full"
        );
        // The full text keeps the YAML fix and the command.
        let human = mc2_api::disk::strip_marker(&message);
        assert!(human.contains("storage_opt:"), "{human}");
        assert!(human.contains("mc2 up -f stack.yaml"), "{human}");
    }

    #[test]
    fn annotate_leaves_healthy_instances_untouched() {
        let root = tempfile::tempdir().unwrap();
        let d = desired(4096, &[("data", "/data", 10 * 1024)]);
        let mut reports = vec![InstanceReport {
            instance_id: "i1".into(),
            phase: "Running".into(),
            message: "ok".into(),
            runtime_id: d.runtime_id.clone(),
            ssh: None,
            network: None,
        }];
        annotate_reports(
            &mut reports,
            std::slice::from_ref(&d),
            &HashMap::new(),
            root.path(),
        );
        assert_eq!(reports[0].message, "ok");
    }

    #[test]
    fn change_logging_only_fires_on_transitions() {
        let full = vec![DiskCondition::root_disk(4096, 4096, 4096).unwrap()];
        log_change("i-transition", &[]);
        log_change("i-transition", &full);
        log_change("i-transition", &full);
        log_change("i-transition", &[]);
        let notes = with_monitor(|m| m.last_notes.get("i-transition").cloned());
        assert_eq!(notes.as_deref(), Some(""));
    }
}
