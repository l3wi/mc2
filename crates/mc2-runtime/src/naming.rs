//! Deterministic microsandbox names for MC2 instances.

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
}
