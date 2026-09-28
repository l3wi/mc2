//! Custom serde deserializers for compose-style flexible values.

use crate::stack::schema::{
    default_proto, default_ssh_bind, default_ssh_user, default_true, DependsOnSpec, ExposeSpec,
    PortSpec, SshSpec,
};
use serde::{Deserialize, Deserializer, Serializer};

use super::validate::validate_hostname;

/// Serialize a MiB size as **bytes** (matching the byte-size deserializers,
/// which treat a bare number as bytes per compose semantics), so a
/// `size` round-trip through JSON is lossless: 10 GiB → `10737418240`.
pub(crate) fn se_disk_size_mib<S>(mib: &u64, s: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    s.serialize_u64(mib.saturating_mul(1024 * 1024))
}

pub(crate) fn se_mem_limit<S>(mib: &u64, s: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    se_disk_size_mib(mib, s)
}

pub(crate) fn de_expose<'de, D>(d: D) -> Result<Vec<ExposeSpec>, D::Error>
where
    D: Deserializer<'de>,
{
    use serde::de::Error;
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Entry {
        Num(u16),
        Str(String),
        Map(ExposeSpec),
    }
    let entries = Vec::<Entry>::deserialize(d)?;
    let mut out = Vec::with_capacity(entries.len());
    for e in entries {
        match e {
            Entry::Num(p) => out.push(ExposeSpec {
                port: p,
                protocol: "tcp".into(),
                name: None,
            }),
            Entry::Str(s) => {
                let (port, proto) = match s.split_once('/') {
                    Some((p, pr)) => (p, pr.to_ascii_lowercase()),
                    None => (s.as_str(), "tcp".to_string()),
                };
                let port = port.parse::<u16>().map_err(Error::custom)?;
                out.push(ExposeSpec {
                    port,
                    protocol: proto,
                    name: None,
                });
            }
            Entry::Map(m) => out.push(m),
        }
    }
    Ok(out)
}

/// Accept `ssh` as a boolean flag (`true`/`false`) or the long map form.
pub(crate) fn de_ssh<'de, D>(d: D) -> Result<Option<SshSpec>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Entry {
        Bool(bool),
        Spec(SshSpec),
    }
    Ok(match Option::<Entry>::deserialize(d)? {
        Some(Entry::Spec(s)) => Some(s),
        Some(Entry::Bool(enabled)) => Some(SshSpec {
            enabled,
            bind: default_ssh_bind(),
            port: 0,
            user: default_ssh_user(),
            sftp: default_true(),
            authorized_keys: vec![],
        }),
        None => None,
    })
}

/// Parse a compose byte size (`512m`, `1g`, `1.5g`, `10GiB`, bare bytes) → MiB.
pub(crate) fn parse_byte_size_mib(raw: &str) -> Result<u64, String> {
    let s = raw.trim().to_ascii_lowercase();
    let split = s.find(|c: char| c.is_ascii_alphabetic()).unwrap_or(s.len());
    let (num, unit) = s.split_at(split);
    let mult = match unit.trim() {
        "" | "b" => 1.0 / (1024.0 * 1024.0),
        "k" | "kb" | "kib" => 1.0 / 1024.0,
        "m" | "mb" | "mib" => 1.0,
        "g" | "gb" | "gib" => 1024.0,
        "t" | "tb" | "tib" => 1024.0 * 1024.0,
        other => {
            return Err(format!(
                "unsupported size unit {other:?} (use b, k, m, g, t, or the `ib` forms)"
            ))
        }
    };
    let val: f64 = num
        .trim()
        .parse()
        .map_err(|_| format!("invalid size {raw:?}"))?;
    if !val.is_finite() || val < 0.0 {
        return Err(format!(
            "invalid size {raw:?}: must be a non-negative number"
        ));
    }
    Ok((val * mult).ceil() as u64)
}

/// Shared byte-size visitor (`mem_limit`, `volumes.<name>.size`,
/// `storage_opt.size`). A bare number is bytes, per compose semantics.
pub(crate) fn de_byte_size_mib<'de, D>(d: D) -> Result<u64, D::Error>
where
    D: Deserializer<'de>,
{
    use serde::de::Error;
    struct V;
    impl<'de> serde::de::Visitor<'de> for V {
        type Value = u64;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            write!(f, "a byte size (e.g. 512m, 1g, 10GiB, or bytes)")
        }
        fn visit_u64<E: Error>(self, v: u64) -> Result<u64, E> {
            Ok(v.div_ceil(1024 * 1024))
        }
        fn visit_str<E: Error>(self, s: &str) -> Result<u64, E> {
            parse_byte_size_mib(s).map_err(E::custom)
        }
    }
    d.deserialize_any(V)
}

/// Parse compose `mem_limit` (`512m`, `1g`, `1.5g`, or bytes) → MiB.
pub(crate) fn de_mem_limit<'de, D>(d: D) -> Result<u64, D::Error>
where
    D: Deserializer<'de>,
{
    de_byte_size_mib(d)
}

/// Parse a disk size (`volumes.<name>.size`, `storage_opt.size`) → MiB.
pub(crate) fn de_disk_size_mib<'de, D>(d: D) -> Result<u64, D::Error>
where
    D: Deserializer<'de>,
{
    de_byte_size_mib(d)
}

/// Accept compose `environment` as a map or a `KEY=VALUE` list.
pub(crate) fn de_env<'de, D>(d: D) -> Result<std::collections::BTreeMap<String, String>, D::Error>
where
    D: Deserializer<'de>,
{
    use serde::de::Error;
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Env {
        Map(std::collections::BTreeMap<String, String>),
        List(Vec<String>),
    }
    match Env::deserialize(d)? {
        Env::Map(m) => Ok(m),
        Env::List(items) => {
            let mut out = std::collections::BTreeMap::new();
            for item in items {
                let (k, v) = item.split_once('=').ok_or_else(|| {
                    Error::custom(format!("environment entry {item:?} must be KEY=VALUE"))
                })?;
                out.insert(k.to_string(), v.to_string());
            }
            Ok(out)
        }
    }
}

/// Accept compose `command` as a string (split with shell-like quoting) or a
/// sequence. Compose splits string commands but does not run them via a shell.
pub(crate) fn de_command<'de, D>(d: D) -> Result<Option<Vec<String>>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Cmd {
        Str(String),
        Seq(Vec<String>),
    }
    Ok(match Option::<Cmd>::deserialize(d)? {
        Some(Cmd::Seq(seq)) => Some(seq),
        Some(Cmd::Str(s)) => {
            let parts = split_command_string(&s);
            if parts.is_empty() {
                None
            } else {
                Some(parts)
            }
        }
        None => None,
    })
}

/// Shell-like word splitting for string `command` values: single/double quotes
/// and backslash escapes, whitespace separates words (POSIX-ish, no shell ops).
// Note: clippy wants `for nc in chars.by_ref()` for the quote loops, but the
// double-quote branch needs to consume escaped characters via `chars.next()`,
// which a `for` borrow would reject — `while let` is intentional here.
#[allow(clippy::while_let_on_iterator)]
pub fn split_command_string(s: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut in_word = false;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                in_word = true;
                while let Some(nc) = chars.next() {
                    if nc == '\'' {
                        break;
                    }
                    cur.push(nc);
                }
            }
            '"' => {
                in_word = true;
                while let Some(nc) = chars.next() {
                    match nc {
                        '"' => break,
                        '\\' => {
                            if let Some(esc) = chars.next() {
                                cur.push(esc);
                            } else {
                                cur.push('\\');
                            }
                        }
                        c => cur.push(c),
                    }
                }
            }
            '\\' => {
                in_word = true;
                if let Some(esc) = chars.next() {
                    cur.push(esc);
                } else {
                    cur.push('\\');
                }
            }
            c if c.is_whitespace() => {
                if in_word {
                    out.push(std::mem::take(&mut cur));
                    in_word = false;
                }
            }
            c => {
                in_word = true;
                cur.push(c);
            }
        }
    }
    if in_word || !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Accept compose `depends_on` as a list (`[db]`) or a map
/// (`{db: {condition: service_healthy}}`).
pub(crate) fn de_depends_on<'de, D>(
    d: D,
) -> Result<std::collections::BTreeMap<String, DependsOnSpec>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Value {
        Str(String),
        Map(DependsOnSpec),
    }
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Deps {
        Map(std::collections::BTreeMap<String, Value>),
        List(Vec<String>),
    }
    Ok(match Option::<Deps>::deserialize(d)? {
        Some(Deps::List(items)) => items
            .into_iter()
            .map(|name| (name, DependsOnSpec::service_started()))
            .collect(),
        Some(Deps::Map(map)) => map
            .into_iter()
            .map(|(name, v)| {
                let spec = match v {
                    Value::Str(_s) => DependsOnSpec::service_started(),
                    Value::Map(m) => m,
                };
                (name, spec)
            })
            .collect(),
        None => std::collections::BTreeMap::new(),
    })
}

/// Decode compose `healthcheck.test` into the argv MC2 execs in the guest.
///
/// Compose forms (B6a):
/// - `test: "<script>"` — a bare string is `CMD-SHELL` in compose →
///   `["/bin/sh", "-c", "<script>"]`;
/// - `["CMD-SHELL", "<script>"]` → `["/bin/sh", "-c", "<script>"]`
///   (the script must actually reach a shell);
/// - `["CMD", a, b]` / `[a, b]` → `[a, b]` (the prefix is not an argv element);
/// - `["NONE"]` → disabled (`None`);
/// - `null` → disabled. The stored spec of `healthcheck: {disable: true}`
///   serializes `"test": null`, so rejecting it would make that spec
///   unreadable and wedge the scheduler.
pub(crate) fn de_healthcheck_test<'de, D>(d: D) -> Result<Option<Vec<String>>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Test {
        Str(String),
        Seq(Vec<String>),
    }
    let Some(test) = Option::<Test>::deserialize(d)? else {
        return Ok(None);
    };
    let cmd = match test {
        Test::Str(script) => vec!["/bin/sh".to_string(), "-c".to_string(), script],
        Test::Seq(seq) => {
            let head = seq.first().map(|s| s.to_ascii_lowercase());
            match head.as_deref() {
                None => return Ok(None),
                Some("none") => return Ok(None),
                Some("cmd") => {
                    let argv = &seq[1..];
                    if argv.is_empty() {
                        return Ok(None);
                    }
                    argv.to_vec()
                }
                Some("cmd-shell") => {
                    let script = &seq[1..];
                    if script.is_empty() {
                        return Ok(None);
                    }
                    vec!["/bin/sh".to_string(), "-c".to_string(), script.join(" ")]
                }
                Some(_) => seq,
            }
        }
    };
    if cmd.is_empty() {
        // `[]` or a leading `CMD` / `CMD-SHELL` with no argv: nothing to run.
        return Ok(None);
    }
    Ok(Some(cmd))
}

pub(crate) fn de_interval<'de, D>(d: D) -> Result<u32, D::Error>
where
    D: Deserializer<'de>,
{
    use serde::de::Error;
    struct V;
    impl<'de> serde::de::Visitor<'de> for V {
        type Value = u32;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            write!(f, "a duration (e.g. 30s)")
        }
        fn visit_u64<E: Error>(self, v: u64) -> Result<u32, E> {
            Ok(v.max(1) as u32)
        }
        fn visit_str<E: Error>(self, s: &str) -> Result<u32, E> {
            let s = s.trim().to_ascii_lowercase();
            let (num, mult) = if let Some(n) = s.strip_suffix("ms") {
                (n, 0.001)
            } else if let Some(n) = s.strip_suffix('s') {
                (n, 1.0)
            } else if let Some(n) = s.strip_suffix('m') {
                (n, 60.0)
            } else if let Some(n) = s.strip_suffix('h') {
                (n, 3600.0)
            } else {
                (s.as_str(), 1.0)
            };
            let val: f64 = num.trim().parse().map_err(E::custom)?;
            Ok((val * mult).max(1.0).ceil() as u32)
        }
    }
    d.deserialize_any(V)
}

/// Parse a compose duration (`30s`, `1m`, `2h`, `100ms`, bare seconds) →
/// **seconds**, rounded UP for a positive value so a sub-second duration never
/// collapses to `0` (B6a: `timeout: 100ms` used to mean "no timeout").
/// `0` (and anything not positive) stays `0`.
pub(crate) fn de_duration<'de, D>(d: D) -> Result<u32, D::Error>
where
    D: Deserializer<'de>,
{
    use serde::de::Error;
    struct V;
    impl<'de> serde::de::Visitor<'de> for V {
        type Value = u32;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            write!(f, "a duration (e.g. 30s) or seconds")
        }
        fn visit_u64<E: Error>(self, v: u64) -> Result<u32, E> {
            Ok(v.min(u32::MAX as u64) as u32)
        }
        fn visit_str<E: Error>(self, s: &str) -> Result<u32, E> {
            let s = s.trim().to_ascii_lowercase();
            let (num, mult) = if let Some(n) = s.strip_suffix("ms") {
                (n, 0.001)
            } else if let Some(n) = s.strip_suffix('s') {
                (n, 1.0)
            } else if let Some(n) = s.strip_suffix('m') {
                (n, 60.0)
            } else if let Some(n) = s.strip_suffix('h') {
                (n, 3600.0)
            } else {
                (s.as_str(), 1.0)
            };
            let val: f64 = num.trim().parse().map_err(E::custom)?;
            if !val.is_finite() {
                return Err(E::custom("duration must be a finite number"));
            }
            if val <= 0.0 {
                return Ok(0);
            }
            Ok((val * mult).ceil().max(1.0).min(u32::MAX as f64) as u32)
        }
    }
    d.deserialize_any(V)
}

/// Healthcheck `timeout`: [`de_duration`] with a mandatory deadline. An
/// explicit `0` would mean "wait forever" and one hung probe would hang the
/// guest exec it is waiting on, so it is rejected here; omit the key for the
/// compose default (30s) instead.
pub(crate) fn de_healthcheck_timeout<'de, D>(d: D) -> Result<u32, D::Error>
where
    D: Deserializer<'de>,
{
    let secs = de_duration(d)?;
    if secs == 0 {
        return Err(<D::Error as serde::de::Error>::custom(
            "healthcheck.timeout must be > 0; omit it for the 30s default",
        ));
    }
    Ok(secs)
}

pub(crate) fn de_ports<'de, D>(d: D) -> Result<Vec<PortSpec>, D::Error>
where
    D: Deserializer<'de>,
{
    use serde::de::{Error, MapAccess, Visitor};

    /// One `ports:` entry: the short compose string, or the long map form.
    enum Entry {
        Short(String),
        Long {
            target: u16,
            published: Option<u16>,
            protocol: Option<String>,
            hostname: Option<String>,
        },
    }

    /// Keys the long form accepts. Anything else is a typo (a misspelt
    /// `published` used to be silently dropped, yielding a random auto port),
    /// so it is rejected by name rather than ignored.
    const LONG_KEYS: &[&str] = &["target", "published", "protocol", "hostname"];

    impl<'de> Deserialize<'de> for Entry {
        fn deserialize<D2>(d: D2) -> Result<Self, D2::Error>
        where
            D2: Deserializer<'de>,
        {
            struct EntryVisitor;
            impl<'de> Visitor<'de> for EntryVisitor {
                type Value = Entry;
                fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                    write!(f, "a port string like \"8080:80\" or a map with `target`")
                }
                fn visit_str<E: Error>(self, v: &str) -> Result<Entry, E> {
                    Ok(Entry::Short(v.to_owned()))
                }
                fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Entry, A::Error> {
                    let mut target = None;
                    let mut published = None;
                    let mut protocol = None;
                    let mut hostname = None;
                    while let Some(key) = map.next_key::<String>()? {
                        match key.as_str() {
                            "target" => target = Some(map.next_value::<u16>()?),
                            "published" => published = Some(map.next_value::<u16>()?),
                            // Optional keys accept `null` (the stored JSON form).
                            "protocol" => protocol = map.next_value::<Option<String>>()?,
                            "hostname" => hostname = map.next_value::<Option<String>>()?,
                            other => return Err(A::Error::unknown_field(other, LONG_KEYS)),
                        }
                    }
                    let target = target.ok_or_else(|| A::Error::missing_field("target"))?;
                    Ok(Entry::Long {
                        target,
                        published,
                        protocol,
                        hostname,
                    })
                }
            }
            d.deserialize_any(EntryVisitor)
        }
    }

    let entries = Vec::<Entry>::deserialize(d)?;
    let mut out = Vec::with_capacity(entries.len());
    for e in entries {
        match e {
            Entry::Long {
                target,
                published,
                protocol,
                hostname,
            } => {
                if published == Some(0) {
                    return Err(Error::custom(
                        "ports entry: published must be a non-zero host port",
                    ));
                }
                if let Some(host) = &hostname {
                    validate_hostname(host)
                        .map_err(|e| Error::custom(format!("ports hostname: {e}")))?;
                }
                out.push(PortSpec {
                    published: published.unwrap_or(0),
                    target,
                    protocol: protocol.unwrap_or_else(default_proto),
                    hostname,
                });
            }
            Entry::Short(s) => {
                let (rest, proto) = match s.split_once('/') {
                    Some((r, pr)) => (r, pr.to_ascii_lowercase()),
                    None => (s.as_str(), "tcp".to_string()),
                };
                let parts: Vec<&str> = rest.split(':').collect();
                let (published, target, hostname) = match parts.as_slice() {
                    // target-only → auto host port
                    [target] => (0, target.parse::<u16>().map_err(Error::custom)?, None),
                    // "published:target" or "hostname:target"
                    [a, target] => {
                        let target = target.parse::<u16>().map_err(Error::custom)?;
                        match a.parse::<u16>() {
                            Ok(published) => (published, target, None),
                            Err(_) => (0, target, Some((*a).to_string())),
                        }
                    }
                    // "ip:published:target" — ip advisory; always loopback
                    [_ip, published, target] => (
                        published.parse::<u16>().map_err(Error::custom)?,
                        target.parse::<u16>().map_err(Error::custom)?,
                        None,
                    ),
                    _ => {
                        return Err(Error::custom(format!(
                            "port {s:?}: expected [ip:]published:target[/protocol]"
                        )))
                    }
                };
                if let Some(host) = &hostname {
                    validate_hostname(host)
                        .map_err(|e| Error::custom(format!("ports hostname: {e}")))?;
                }
                out.push(PortSpec {
                    published,
                    target,
                    protocol: proto,
                    hostname,
                });
            }
        }
    }
    Ok(out)
}
