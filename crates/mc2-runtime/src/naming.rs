//! Deterministic microsandbox names for MC2 instances.

use crate::spec::DesiredSandbox;

/// Build a stable msb sandbox name: `{stack}-{service}-{ordinal}`.
///
/// Constrained to msb-friendly charset and 128-byte max.
pub fn sandbox_name(stack: &str, service: &str, ordinal: u32) -> String {
    let raw = format!("{}-{}-{}", sanitize(stack), sanitize(service), ordinal);
    if raw.len() <= 128 {
        raw
    } else {
        // Keep ordinal suffix; hash-ish truncate prefix
        let suffix = format!("-{ordinal}");
        let keep = 128 - suffix.len();
        format!("{}{}", &raw[..keep], suffix)
    }
}

/// Build the resolved msb named-volume identity: `mc2-{stack}--{volume}`.
///
/// `--` separates stack from volume; both names are sanitized to the
/// msb-friendly charset. Constrained to the same 128-byte max as
/// [`sandbox_name`]. Stack/volume names containing `--` are rejected by
/// stack validation, so the split stays unambiguous.
pub fn volume_name(stack: &str, volume: &str) -> String {
    let raw = format!("mc2-{}--{}", sanitize(stack), sanitize(volume));
    if raw.len() <= 128 {
        raw
    } else {
        raw[..128].to_string()
    }
}

/// Guest mount path → resolved msb volume name, in spec order.
pub fn volume_mount_plan(desired: &DesiredSandbox) -> Vec<(String, String)> {
    desired
        .spec
        .volumes
        .iter()
        .map(|m| (m.mount.clone(), volume_name(&desired.stack, &m.name)))
        .collect()
}

fn sanitize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
            out.push(c);
        } else {
            out.push('-');
        }
    }
    let trimmed = out.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "x".into()
    } else {
        trimmed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_name() {
        assert_eq!(sandbox_name("demo", "web", 0), "demo-web-0");
    }

    #[test]
    fn sanitizes() {
        assert_eq!(sandbox_name("my stack", "web/svc", 1), "my-stack-web-svc-1");
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
    fn volume_name_sanitized_and_bounded() {
        assert_eq!(volume_name("My Stack", "Data/X"), "mc2-My-Stack--Data-X");
        let long = "a".repeat(200);
        assert_eq!(volume_name(&long, "v").len(), 128);
    }
}
