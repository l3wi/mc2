//! Deterministic microsandbox names and MC2-owned volume paths.

use crate::spec::DesiredSandbox;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Longest runtime name microsandbox accepts.
const MAX_NAME_BYTES: usize = 128;

/// Build a stable msb sandbox name: `{stack}--{service}--{ordinal}`.
///
/// Injective for validated names: stack and service names may not contain `--`
/// or start/end with `-`, so the separators are unambiguous. Stack ≤ 40 and
/// service ≤ 63 characters keep the result ≤ 128 bytes (asserted in debug
/// builds; `u32` ordinals add at most 10 digits).
pub fn sandbox_name(stack: &str, service: &str, ordinal: u32) -> String {
    let name = format!("{stack}--{service}--{ordinal}");
    debug_assert!(
        name.len() <= MAX_NAME_BYTES,
        "sandbox name {name:?} exceeds {MAX_NAME_BYTES} bytes; stack/service names must be validated"
    );
    name
}

/// Build the resolved msb named-volume identity: `mc2-{stack}--{volume}`.
///
/// `--` separates stack from volume; validation rejects `--` inside either
/// name, so the split stays unambiguous. Stack ≤ 40 and volume ≤ 63 keep the
/// result ≤ 128 bytes (asserted in debug builds).
pub fn volume_name(stack: &str, volume: &str) -> String {
    let name = format!("mc2-{stack}--{volume}");
    debug_assert!(
        name.len() <= MAX_NAME_BYTES,
        "volume name {name:?} exceeds {MAX_NAME_BYTES} bytes; stack/volume names must be validated"
    );
    name
}

/// Parse a resolved volume identity back into `(stack, volume)`.
///
/// Exact inverse of [`volume_name`] for validated stack/volume names: neither
/// component may contain `--`, so the first `--` after the `mc2-` prefix is
/// the separator. Returns `None` for a name that is not an MC2 volume identity.
pub fn parse_volume_name(name: &str) -> Option<(String, String)> {
    let stripped = name.strip_prefix("mc2-")?;
    let (stack, volume) = stripped.split_once("--")?;
    if stack.is_empty() || volume.is_empty() {
        return None;
    }
    Some((stack.to_string(), volume.to_string()))
}

/// Create `dir` when missing and return its canonical path.
///
/// microsandbox refuses to follow symlinks when resolving a bind-mount host
/// path, so the runtime must hand it a canonical path (macOS `/tmp` →
/// `/private/tmp`). Creating first also makes the canonicalize meaningful for
/// a volume that does not exist yet.
pub fn ensure_volume_dir(dir: &Path) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    std::fs::canonicalize(dir)
}

/// Recursive logical size of `path` in bytes. Directories only; symlinked
/// directories are not followed, so a symlink cycle cannot loop.
pub fn dir_size_bytes(path: &Path) -> u64 {
    let mut total = 0u64;
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(ft) = entry.file_type() else {
                continue;
            };
            if ft.is_dir() {
                stack.push(entry.path());
            } else {
                total = total.saturating_add(entry.metadata().map(|m| m.len()).unwrap_or(0));
            }
        }
    }
    total
}

/// Recursive logical size of `path` in MiB.
pub fn dir_size_mib(path: &Path) -> u64 {
    dir_size_bytes(path) / (1024 * 1024)
}

/// Guest-write quota for a volume bind mount: `size − used`, floored at 0.
///
/// `0` is valid and meaningful: microsandbox accepts `quota=0`, the guest may
/// add nothing beyond what is already there (writes fail with `ENOSPC`) and a
/// read-only workload keeps working. The quota is carried as `u32` MiB, so the
/// result is clamped to `u32::MAX`.
pub fn remaining_quota_mib(size_mib: u64, used_mib: u64) -> u32 {
    size_mib.saturating_sub(used_mib).min(u32::MAX as u64) as u32
}

/// One planned volume bind mount: guest path, host directory, write quota.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeMountPlan {
    /// Guest mount path.
    pub guest: String,
    /// Host directory, under the MC2 volume root.
    pub host: PathBuf,
    /// Always present: microsandbox silently applies its own 4 GiB default to
    /// a bind mount without an explicit quota, so MC2 never omits one.
    pub quota_mib: u32,
}

/// Bind-mount plan for `desired` under `root`, from measured directory usage.
///
/// `usage_mib` maps a resolved volume directory name (`mc2-{stack}--{volume}`)
/// to its current size; an unknown entry counts as 0 (nothing there yet).
pub fn volume_bind_plan(
    desired: &DesiredSandbox,
    root: &Path,
    usage_mib: &HashMap<String, u64>,
) -> Vec<VolumeMountPlan> {
    desired
        .spec
        .volumes
        .iter()
        .map(|m| {
            let name = volume_name(&desired.stack, &m.name);
            let used = usage_mib.get(&name).copied().unwrap_or(0);
            VolumeMountPlan {
                guest: m.mount.clone(),
                host: root.join(&name),
                quota_mib: remaining_quota_mib(m.size_mib, used),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sandbox_name_separates_with_double_dash() {
        assert_eq!(sandbox_name("demo", "web", 0), "demo--web--0");
        assert_eq!(sandbox_name("demo", "web", 12), "demo--web--12");
    }

    #[test]
    fn sandbox_name_is_injective_across_components() {
        // A single `-` separator would collide these two identities.
        assert_ne!(
            sandbox_name("a-b", "c", 0),
            sandbox_name("a", "b-c", 0),
            "`--` separator must disambiguate the stack/service split"
        );
        assert_eq!(sandbox_name("a-b", "c", 0), "a-b--c--0");
        assert_eq!(sandbox_name("a", "b-c", 0), "a--b-c--0");
    }

    #[test]
    fn volume_name_is_namespaced_and_deterministic() {
        assert_eq!(volume_name("demo", "data"), "mc2-demo--data");
        assert_eq!(volume_name("demo", "data"), "mc2-demo--data");
    }

    #[test]
    fn volume_name_separator_is_collision_free() {
        // A bare `-` separator would collide these two identities.
        let a = volume_name("a-b", "x");
        let b = volume_name("a", "b-x");
        assert_ne!(a, b, "separator must disambiguate stack/volume split");
    }

    #[test]
    fn max_length_names_stay_within_msb_byte_budget() {
        let stack = "a".repeat(40);
        let service = "b".repeat(63);
        let sandbox = sandbox_name(&stack, &service, u32::MAX);
        assert!(sandbox.len() <= MAX_NAME_BYTES, "{}", sandbox.len());
        assert_eq!(sandbox.len(), 40 + 2 + 63 + 2 + 10);

        let volume = "c".repeat(63);
        let vol = volume_name(&stack, &volume);
        assert!(vol.len() <= MAX_NAME_BYTES, "{}", vol.len());
        assert_eq!(vol.len(), 4 + 40 + 2 + 63);
    }

    #[test]
    fn parse_volume_name_roundtrips() {
        assert_eq!(
            parse_volume_name("mc2-demo--data"),
            Some(("demo".into(), "data".into()))
        );
        assert_eq!(
            parse_volume_name("mc2-my-stack--cache"),
            Some(("my-stack".into(), "cache".into()))
        );
        let cases: Vec<(String, String)> = vec![
            ("demo".into(), "data".into()),
            ("a-b".into(), "c".into()),
            ("a".into(), "b-c".into()),
            ("team_a".into(), "logs.2024".into()),
            ("s".repeat(40), "v".repeat(63)),
        ];
        for (stack, volume) in cases {
            let name = volume_name(&stack, &volume);
            assert_eq!(
                parse_volume_name(&name),
                Some((stack.clone(), volume.clone())),
                "{name}"
            );
        }
    }

    #[test]
    fn parse_volume_name_rejects_non_mc2_names() {
        assert_eq!(parse_volume_name("demo--data"), None);
        assert_eq!(parse_volume_name("mc2-demo-data"), None);
        assert_eq!(parse_volume_name("mc2--"), None);
        assert_eq!(parse_volume_name(""), None);
    }

    // ---- A9: volume sizes and mount quotas -------------------------------

    #[test]
    fn ensure_volume_dir_creates_and_canonicalizes() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("mc2-demo--data");
        let got = ensure_volume_dir(&nested).unwrap();
        assert!(got.is_absolute(), "{got:?}");
        assert!(got.is_dir());
        // Canonical: no `..`/symlink components survive (macOS /tmp → /private/tmp).
        assert_eq!(got, std::fs::canonicalize(&nested).unwrap());
        // Idempotent.
        assert_eq!(ensure_volume_dir(&nested).unwrap(), got);
    }

    #[test]
    fn remaining_quota_is_size_minus_usage_floored_at_zero() {
        assert_eq!(remaining_quota_mib(10 * 1024, 0), 10 * 1024);
        assert_eq!(remaining_quota_mib(10 * 1024, 512), 10 * 1024 - 512);
        assert_eq!(remaining_quota_mib(1024, 1024), 0);
        assert_eq!(remaining_quota_mib(1024, 4096), 0, "usage over size → 0");
        assert_eq!(remaining_quota_mib(0, 0), 0);
        assert_eq!(remaining_quota_mib(u64::MAX, 0), u32::MAX, "clamped");
    }

    #[test]
    fn dir_size_walks_recursively() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("a/b")).unwrap();
        std::fs::write(dir.path().join("one"), vec![1u8; 2048]).unwrap();
        std::fs::write(dir.path().join("a/b/two"), vec![2u8; 4096]).unwrap();
        assert_eq!(dir_size_bytes(dir.path()), 2048 + 4096);
        assert_eq!(dir_size_mib(dir.path()), 0);
        assert_eq!(dir_size_mib(&dir.path().join("missing")), 0);
    }

    fn spec_with_volumes(vols: &[(&str, &str, u64)]) -> mc2_api::ServiceSpec {
        mc2_api::ServiceSpec {
            image: "alpine:3.20".into(),
            scale: 1,
            cpus: 1,
            mem_limit_mib: 512,
            ports: vec![],
            network: Default::default(),
            env: Default::default(),
            secrets: vec![],
            volumes: vols
                .iter()
                .map(|(name, mount, size_mib)| mc2_api::VolumeMount {
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
            storage_opt: None,
            expose: vec![],
            networks: vec![],
            depends_on: Default::default(),
        }
    }

    #[test]
    fn bind_plan_always_carries_an_explicit_quota() {
        let root = Path::new("/var/lib/mc2/volumes");
        let desired = DesiredSandbox {
            instance_id: "i1".into(),
            stack: "demo".into(),
            service: "web".into(),
            ordinal: 0,
            runtime_id: sandbox_name("demo", "web", 0),
            spec: spec_with_volumes(&[
                ("data", "/data", 10 * 1024),
                ("cache", "/var/cache", 1024),
                ("full", "/full", 1024),
            ]),
            secrets: vec![],
            ssh: Default::default(),
            network: Default::default(),
        };
        let usage = HashMap::from([
            ("mc2-demo--data".to_string(), 512u64),
            // `cache` has nothing on disk yet.
            ("mc2-demo--full".to_string(), 2048u64),
        ]);
        let plan = volume_bind_plan(&desired, root, &usage);
        assert_eq!(plan.len(), 3);
        assert_eq!(plan[0].guest, "/data");
        assert_eq!(plan[0].host, root.join("mc2-demo--data"));
        assert_eq!(plan[0].quota_mib, 10 * 1024 - 512);
        assert_eq!(plan[1].guest, "/var/cache");
        assert_eq!(plan[1].host, root.join("mc2-demo--cache"));
        assert_eq!(plan[1].quota_mib, 1024, "no usage → full declared size");
        // A volume already at/over its size still mounts — with quota 0, not
        // without a quota (an omitted quota means msb's implicit 4 GiB).
        assert_eq!(plan[2].quota_mib, 0);
    }

    #[test]
    fn bind_plan_without_mounts_is_empty() {
        let desired = DesiredSandbox {
            instance_id: "i1".into(),
            stack: "demo".into(),
            service: "web".into(),
            ordinal: 0,
            runtime_id: sandbox_name("demo", "web", 0),
            spec: spec_with_volumes(&[]),
            secrets: vec![],
            ssh: Default::default(),
            network: Default::default(),
        };
        assert!(volume_bind_plan(&desired, Path::new("/vols"), &HashMap::new()).is_empty());
    }
}
