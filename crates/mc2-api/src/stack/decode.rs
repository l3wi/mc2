//! Custom serde deserializers for compose-style flexible values.

use crate::stack::schema::{
    default_proto, default_ssh_bind, default_ssh_user, default_true, DependsOnSpec, ExposeSpec,
    PortSpec, SshSpec,
};
use serde::{Deserialize, Deserializer, Serializer};

/// Serialize `mem_limit_mib` as **bytes** (matching [`de_mem_limit`], which
/// treats a bare number as bytes per compose semantics). This keeps a
/// `ServiceSpec` JSON round-trip lossless: 512 MiB → `"mem_limit": 536870912`.
pub(crate) fn se_mem_limit<S>(mib: &u64, s: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    s.serialize_u64(mib.saturating_mul(1024 * 1024))
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

/// Parse compose `mem_limit` (`512m`, `1g`, `1.5g`, or bytes) → MiB.
pub(crate) fn de_mem_limit<'de, D>(d: D) -> Result<u64, D::Error>
where
    D: Deserializer<'de>,
{
    use serde::de::Error;
    struct V;
    impl<'de> serde::de::Visitor<'de> for V {
        type Value = u64;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            write!(f, "a memory size (e.g. 512m, 1g, or bytes)")
        }
        fn visit_u64<E: Error>(self, v: u64) -> Result<u64, E> {
            Ok(v.div_ceil(1024 * 1024))
        }
        fn visit_str<E: Error>(self, s: &str) -> Result<u64, E> {
            let s = s.trim().to_ascii_lowercase();
            let (num, mult) = if let Some(n) = s.strip_suffix('g') {
                (n, 1024.0)
            } else if let Some(n) = s.strip_suffix('m') {
                (n, 1.0)
            } else if let Some(n) = s.strip_suffix('k') {
                (n, 1.0 / 1024.0)
            } else {
                (s.as_str(), 1.0 / (1024.0 * 1024.0))
            };
            let val: f64 = num.trim().parse().map_err(E::custom)?;
            Ok((val * mult).ceil() as u64)
        }
    }
    d.deserialize_any(V)
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
    let cmd: Vec<String> = match Test::deserialize(d)? {
        Test::Str(s) => vec!["/bin/sh".into(), "-c".into(), s],
        Test::Seq(seq) => seq,
    };
    let mut cmd = cmd;
    if let Some(first) = cmd.first().map(|s| s.to_ascii_lowercase()) {
        if first == "cmd" || first == "cmd-shell" {
            cmd.remove(0);
        }
    }
    if cmd.is_empty() {
        Ok(None)
    } else {
        Ok(Some(cmd))
    }
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

/// Parse a compose duration (`30s`, `1m`, `2h`, bare seconds) → seconds.
/// Unlike `de_interval`, `0` is allowed (no timeout / no grace period).
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
            Ok((val * mult).max(0.0).round() as u32)
        }
    }
    d.deserialize_any(V)
}

pub(crate) fn de_ports<'de, D>(d: D) -> Result<Vec<PortSpec>, D::Error>
where
    D: Deserializer<'de>,
{
    use serde::de::Error;
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Entry {
        Short(String),
        Long {
            target: u16,
            published: Option<u16>,
            protocol: Option<String>,
            hostname: Option<String>,
        },
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
