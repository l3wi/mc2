//! Disk-limit conditions (A9).
//!
//! A guest that runs out of disk only ever sees the kernel's
//! `No space left on device`. MC2 detects the condition from microsandbox
//! metrics (root disk) and host directory walks (volumes), then explains it
//! where the operator looks.
//!
//! The instance `message` column carries a human-readable explanation followed
//! by one machine-readable marker line, so `mc2 ps` can render a short NOTES
//! entry without parsing prose:
//!
//! ```text
//! shop/web/0: volume "data" full — 10 GiB of 10 GiB used (mounted at /data).
//! ...
//! [mc2:disk] {"conditions":[{"target":{"kind":"volume","name":"data","mount":"/data"},...}]}
//! ```

use serde::{Deserialize, Serialize};

/// Warn when a disk is at least this percent used.
pub const WARN_PERCENT: u64 = 90;
/// Report `full` at (or above) this percent used…
pub const FULL_PERCENT: u64 = 99;
/// …or when fewer than this many MiB remain free.
pub const MIN_FREE_MIB: u64 = 64;
/// Marker prefix for the machine-readable condition list in a message.
pub const MARKER: &str = "[mc2:disk]";

/// Severity of one disk condition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DiskSeverity {
    /// ≥ 90 % used: the workload still runs, but will not for long.
    Warning,
    /// ≥ 99 % used or < 64 MiB free: writes are failing now.
    Full,
}

impl DiskSeverity {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Warning => "warning",
            Self::Full => "full",
        }
    }
}

/// What a condition is about.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum DiskTarget {
    /// The VM's writable root disk (`services.<name>.storage_opt.size`).
    RootDisk,
    /// A named volume mount (`volumes.<name>`).
    Volume {
        /// Volume name as declared in the stack.
        name: String,
        /// Guest mount path.
        mount: String,
    },
}

impl DiskTarget {
    /// Short NOTES form: `root disk` / `volume data`.
    pub fn short_label(&self) -> String {
        match self {
            Self::RootDisk => "root disk".into(),
            Self::Volume { name, .. } => format!("volume {name}"),
        }
    }
}

/// One active disk condition for an instance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiskCondition {
    pub target: DiskTarget,
    pub used_mib: u64,
    pub limit_mib: u64,
    pub severity: DiskSeverity,
}

/// Typed payload of the `[mc2:disk]` marker line.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiskConditions {
    pub conditions: Vec<DiskCondition>,
}

/// Classify a usage against a declared limit.
///
/// `None` when nothing is worth reporting: unknown limit (`limit_mib == 0`),
/// empty disk, or below the warning band.
pub fn classify(used_mib: u64, limit_mib: u64) -> Option<DiskSeverity> {
    if limit_mib == 0 || used_mib == 0 {
        return None;
    }
    let pct = used_mib as u128 * 100 / limit_mib as u128;
    let free = limit_mib.saturating_sub(used_mib);
    if pct >= FULL_PERCENT as u128 || free < MIN_FREE_MIB {
        Some(DiskSeverity::Full)
    } else if pct >= WARN_PERCENT as u128 {
        Some(DiskSeverity::Warning)
    } else {
        None
    }
}

impl DiskCondition {
    /// Root-disk condition, or `None` when the usage is below the warning band.
    pub fn root_disk(used_mib: u64, limit_mib: u64) -> Option<Self> {
        Some(Self {
            target: DiskTarget::RootDisk,
            used_mib,
            limit_mib,
            severity: classify(used_mib, limit_mib)?,
        })
    }

    /// Volume condition, or `None` when the usage is below the warning band.
    pub fn volume(name: &str, mount: &str, used_mib: u64, limit_mib: u64) -> Option<Self> {
        Some(Self {
            target: DiskTarget::Volume {
                name: name.to_string(),
                mount: mount.to_string(),
            },
            used_mib,
            limit_mib,
            severity: classify(used_mib, limit_mib)?,
        })
    }

    /// Rounded percent used (clamped to 100 for display honesty at/over the cap).
    pub fn percent(&self) -> u64 {
        if self.limit_mib == 0 {
            return 0;
        }
        let pct =
            (self.used_mib as u128 * 100 + self.limit_mib as u128 / 2) / self.limit_mib as u128;
        pct.min(100) as u64
    }

    /// Headline fragment: `full` or `nearly full (91 %)`.
    fn headline(&self) -> String {
        match self.severity {
            DiskSeverity::Full => "full".into(),
            DiskSeverity::Warning => format!("nearly full ({} %)", self.percent()),
        }
    }

    /// Short NOTES entry: `root disk full`, `volume data 93%`.
    pub fn short_note(&self) -> String {
        let label = self.target.short_label();
        match self.severity {
            DiskSeverity::Full => format!("{label} full"),
            DiskSeverity::Warning => format!("{label} {}%", self.percent()),
        }
    }

    /// Full operator-facing text, tagged with the instance reference and the
    /// exact stack.yaml fix plus the `mc2 up` command that applies it.
    pub fn message_text(&self, stack: &str, service: &str, ordinal: u32) -> String {
        let aref = format!("{stack}/{service}/{ordinal}");
        let used = fmt_size(self.used_mib);
        let limit = fmt_size(self.limit_mib);
        let suggest = yaml_size(self.limit_mib.saturating_mul(2));
        let head = self.headline();
        match &self.target {
            DiskTarget::RootDisk => format!(
                "{aref}: root disk {head} — {used} of {limit} used. Writes inside the VM fail with\n\
                 \"No space left on device\".\n\
                 \x20 Fix: raise the root disk in stack.yaml, then run `mc2 up -f stack.yaml`:\n\
                 \x20   services:\n\
                 \x20     {service}:\n\
                 \x20       storage_opt:\n\
                 \x20         size: {suggest}\n\
                 \x20 The VM is recreated; files outside volumes are not kept — keep data that must\n\
                 \x20 survive in a volume."
            ),
            DiskTarget::Volume { name, mount } => format!(
                "{aref}: volume \"{name}\" {head} — {used} of {limit} used (mounted at {mount}).\n\
                 Writes under {mount} fail with \"No space left on device\".\n\
                 \x20 Fix: raise the size in stack.yaml, then run `mc2 up -f stack.yaml` (data is kept,\n\
                 \x20 the VM restarts):\n\
                 \x20   volumes:\n\
                 \x20     {name}:\n\
                 \x20       size: {suggest}\n\
                 \x20 Or free space: mc2 exec {aref} -- du -sh {mount}/*"
            ),
        }
    }
}

/// Human size: MiB below 1 GiB, otherwise GiB with 2 decimals when not whole.
pub fn fmt_size(mib: u64) -> String {
    if mib < 1024 {
        return format!("{mib} MiB");
    }
    let gib = mib as f64 / 1024.0;
    if (gib - gib.round()).abs() < 0.005 {
        format!("{gib:.0} GiB")
    } else {
        format!("{gib:.2} GiB")
    }
}

/// Size literal accepted by the stack parser (`8GiB`, `768MiB`), for the
/// copy-pasteable fix in [`DiskCondition::message_text`].
pub fn yaml_size(mib: u64) -> String {
    if mib >= 1024 && mib.is_multiple_of(1024) {
        format!("{}GiB", mib / 1024)
    } else {
        format!("{mib}MiB")
    }
}

/// Encode conditions as the single marker line appended to an instance message.
///
/// Empty input yields an empty string (nothing to encode).
pub fn encode(conditions: &[DiskCondition]) -> String {
    if conditions.is_empty() {
        return String::new();
    }
    let payload = DiskConditions {
        conditions: conditions.to_vec(),
    };
    match serde_json::to_string(&payload) {
        Ok(json) => format!("{MARKER} {json}"),
        Err(_) => String::new(),
    }
}

/// Decode the condition list from an instance message (empty when absent).
pub fn decode(message: &str) -> Vec<DiskCondition> {
    message
        .lines()
        .rev()
        .find_map(|line| {
            let json = line.trim().strip_prefix(MARKER)?.trim();
            serde_json::from_str::<DiskConditions>(json)
                .ok()
                .map(|c| c.conditions)
        })
        .unwrap_or_default()
}

/// The human part of a message: everything except the marker line.
pub fn strip_marker(message: &str) -> String {
    message
        .lines()
        .filter(|line| !line.trim_start().starts_with(MARKER))
        .collect::<Vec<_>>()
        .join("\n")
        .trim_end()
        .to_string()
}

/// Short NOTES text for a condition list (`root disk full, volume data 93%`).
pub fn notes_line(conditions: &[DiskCondition]) -> String {
    conditions
        .iter()
        .map(DiskCondition::short_note)
        .collect::<Vec<_>>()
        .join(", ")
}

/// Short NOTES text decoded straight from a stored instance message.
pub fn notes_from_message(message: &str) -> String {
    notes_line(&decode(message))
}

/// Full text block for one instance: the human explanation plus the marker.
///
/// Appended to whatever message the node loop already produced, so the runtime
/// phase text stays visible.
pub fn block(conditions: &[DiskCondition], stack: &str, service: &str, ordinal: u32) -> String {
    if conditions.is_empty() {
        return String::new();
    }
    let mut out = conditions
        .iter()
        .map(|c| c.message_text(stack, service, ordinal))
        .collect::<Vec<_>>()
        .join("\n\n");
    out.push('\n');
    out.push_str(&encode(conditions));
    out
}

/// The disk-condition text stored in `message`, without the marker line or any
/// unrelated runtime text (`mc2 exec`/`ssh`/`logs` print exactly this).
///
/// Empty when the message carries no condition.
pub fn full_text_from_message(message: &str, stack: &str, service: &str, ordinal: u32) -> String {
    decode(message)
        .iter()
        .map(|c| c.message_text(stack, service, ordinal))
        .collect::<Vec<_>>()
        .join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thresholds_warn_at_90_and_full_at_99_or_64_mib_free() {
        // 10 GiB volume.
        let ten_gib = 10 * 1024;
        assert_eq!(classify(0, ten_gib), None);
        assert_eq!(classify(1024, ten_gib), None, "10% used");
        assert_eq!(classify(9215, ten_gib), None, "89% used");
        assert_eq!(
            classify(9216, ten_gib),
            Some(DiskSeverity::Warning),
            "90% used"
        );
        assert_eq!(
            classify(10 * 1024 - 200, ten_gib),
            Some(DiskSeverity::Warning),
            "98% used, 200 MiB free"
        );
        assert_eq!(
            classify(10 * 1024 - 64, ten_gib),
            Some(DiskSeverity::Full),
            "99%+ used"
        );
        assert_eq!(classify(10 * 1024, ten_gib), Some(DiskSeverity::Full));
        // Free-space rule fires below 90% when the disk is small.
        assert_eq!(classify(100, 150), Some(DiskSeverity::Full), "50 MiB free");
        assert_eq!(classify(63, 128), None, "65 MiB free is not below 64");
        assert_eq!(classify(65, 128), Some(DiskSeverity::Full), "63 MiB free");
        // Unknown limit → unknown, never a false alarm.
        assert_eq!(classify(999, 0), None);
    }

    #[test]
    fn suggested_size_parses_as_stack_yaml() {
        for mib in [100, 768, 1024, 8192, 1536] {
            let lit = yaml_size(mib);
            let yaml = format!(
                "name: s\nvolumes:\n  data:\n    kind: dir\n    size: {lit}\nservices:\n  w:\n    image: alpine\n"
            );
            let doc = crate::parse_stack_yaml(&yaml).unwrap();
            assert_eq!(doc.volumes["data"].size_mib, mib, "{lit}");
        }
    }

    #[test]
    fn short_notes_are_compact() {
        let full = DiskCondition::root_disk(4096, 4096).unwrap();
        assert_eq!(full.short_note(), "root disk full");
        let warn = DiskCondition::volume("data", "/data", 9216, 10 * 1024).unwrap();
        assert_eq!(warn.short_note(), "volume data 90%");
        assert_eq!(
            notes_line(&[full.clone(), warn]),
            "root disk full, volume data 90%"
        );
    }

    #[test]
    fn root_disk_message_names_the_yaml_fix_and_the_command() {
        let c = DiskCondition::root_disk(4076, 4096).unwrap();
        let text = c.message_text("shop", "web", 0);
        assert!(
            text.starts_with("shop/web/0: root disk full — 3.98 GiB of 4 GiB used."),
            "{text}"
        );
        assert!(text.contains("No space left on device"), "{text}");
        assert!(text.contains("services:"), "{text}");
        assert!(text.contains("web:"), "{text}");
        assert!(text.contains("storage_opt:"), "{text}");
        assert!(text.contains("size: 8GiB"), "{text}");
        assert!(text.contains("mc2 up -f stack.yaml"), "{text}");
        assert!(text.contains("not kept"), "{text}");
    }

    #[test]
    fn volume_message_names_the_yaml_fix_data_retention_and_the_command() {
        let c = DiskCondition::volume("data", "/data", 10 * 1024, 10 * 1024).unwrap();
        let text = c.message_text("shop", "web", 0);
        assert!(
            text.starts_with(
                "shop/web/0: volume \"data\" full — 10 GiB of 10 GiB used (mounted at /data)."
            ),
            "{text}"
        );
        assert!(text.contains("No space left on device"), "{text}");
        assert!(text.contains("volumes:"), "{text}");
        assert!(text.contains("size: 20GiB"), "{text}");
        assert!(text.contains("mc2 up -f stack.yaml"), "{text}");
        assert!(text.contains("data is kept"), "{text}");
        assert!(text.contains("du -sh /data/*"), "{text}");
    }

    #[test]
    fn warnings_use_the_same_text_with_nearly_full() {
        let c = DiskCondition::volume("data", "/data", 9216, 10 * 1024).unwrap();
        let text = c.message_text("shop", "web", 0);
        assert!(text.contains("nearly full (90 %)"), "{text}");
        assert!(!text.contains("— 10 GiB of 10 GiB used"), "{text}");
    }

    #[test]
    fn encode_decode_roundtrip_and_strip() {
        let conds = vec![
            DiskCondition::root_disk(4096, 4096).unwrap(),
            DiskCondition::volume("data", "/data", 9216, 10 * 1024).unwrap(),
        ];
        let block = block(&conds, "shop", "web", 2);
        assert!(block.starts_with("shop/web/2: root disk full"), "{block}");
        assert!(block.contains(MARKER), "{block}");
        assert_eq!(decode(&block), conds);
        assert_eq!(
            notes_from_message(&block),
            "root disk full, volume data 90%"
        );
        let human = strip_marker(&block);
        assert!(!human.contains(MARKER), "{human}");
        assert!(human.contains("No space left on device"), "{human}");
        assert_eq!(human, human.trim_end());
    }

    #[test]
    fn empty_and_markerless_messages_decode_to_nothing() {
        assert_eq!(encode(&[]), "");
        assert!(decode("microsandbox sdk (local)").is_empty());
        assert!(decode("").is_empty());
        assert_eq!(notes_from_message("starting"), "");
        // A malformed marker line is ignored, not fatal.
        assert!(decode("[mc2:disk] {not json").is_empty());
        assert_eq!(strip_marker("plain"), "plain");
    }

    #[test]
    fn message_appends_after_existing_runtime_text() {
        let conds = vec![DiskCondition::root_disk(4096, 4096).unwrap()];
        let mut message = String::from("microsandbox sdk (local)");
        let block = block(&conds, "s", "w", 0);
        message.push('\n');
        message.push_str(&block);
        assert!(message.contains("microsandbox sdk (local)"), "{message}");
        assert_eq!(notes_from_message(&message), "root disk full");
    }

    #[test]
    fn full_text_from_message_excludes_runtime_text_and_marker() {
        let conds = vec![DiskCondition::volume("data", "/data", 10 * 1024, 10 * 1024).unwrap()];
        let mut message = String::from("microsandbox sdk (local)");
        message.push('\n');
        message.push_str(&block(&conds, "shop", "web", 1));
        let text = full_text_from_message(&message, "shop", "web", 1);
        assert!(
            text.starts_with("shop/web/1: volume \"data\" full"),
            "{text}"
        );
        assert!(!text.contains("microsandbox sdk"), "{text}");
        assert!(!text.contains(MARKER), "{text}");
        assert!(text.contains("mc2 up -f stack.yaml"), "{text}");
        // A message with no condition yields nothing to print.
        assert!(full_text_from_message("starting", "s", "w", 0).is_empty());
    }
}
