//! Local client context config (`~/.mc2/config.toml`, mode 0600) and
//! connection resolution for local vs remote control planes.
//!
//! Precedence (highest first): `--api`/`--token` flags, `MC2_API`/`MC2_API_KEY`
//! env, `--context`/`MC2_CONTEXT`, the `current` context in the config file,
//! then the built-in loopback default (local mode).

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// Built-in default API URL (local loopback).
pub const DEFAULT_API_URL: &str = "http://127.0.0.1:7443";

/// Client mode derived from the effective URL host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientMode {
    Local,
    Remote,
}

impl ClientMode {
    pub fn as_str(self) -> &'static str {
        match self {
            ClientMode::Local => "local",
            ClientMode::Remote => "remote",
        }
    }
}

/// One named context (a saved API endpoint + optional bearer token).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextEntry {
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
}

/// `~/.mc2/config.toml` shape.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ClientConfig {
    #[serde(default)]
    pub current: Option<String>,
    #[serde(default)]
    pub contexts: BTreeMap<String, ContextEntry>,
}

/// Effective connection after precedence resolution.
#[derive(Debug, Clone)]
pub struct Conn {
    pub url: String,
    pub token: Option<String>,
    pub context: Option<String>,
    pub mode: ClientMode,
}

impl ClientConfig {
    /// Upsert a context entry. Token is only replaced when non-empty (keeps
    /// an existing key when the caller omits one).
    pub fn set_context(&mut self, name: &str, url: &str, token: Option<&str>) {
        let entry = self
            .contexts
            .entry(name.to_string())
            .or_insert_with(|| ContextEntry {
                url: String::new(),
                api_key: None,
            });
        entry.url = url.trim_end_matches('/').to_string();
        if let Some(t) = token.filter(|t| !t.is_empty()) {
            entry.api_key = Some(t.to_string());
        }
    }

    /// Make `name` the current context; errors if it does not exist.
    pub fn set_current(&mut self, name: &str) -> Result<()> {
        if !self.contexts.contains_key(name) {
            let available = if self.contexts.is_empty() {
                "(none configured)".to_string()
            } else {
                self.contexts.keys().cloned().collect::<Vec<_>>().join(", ")
            };
            bail!("context '{name}' not found (available: {available})");
        }
        self.current = Some(name.to_string());
        Ok(())
    }
}

/// Absolute path of the client config file (`~/.mc2/config.toml`).
pub fn config_path() -> PathBuf {
    mc2_server::expand_data_dir("~/.mc2/config.toml")
}

/// Load the client config; a missing file yields the empty default.
pub fn load() -> Result<ClientConfig> {
    let path = config_path();
    if !path.exists() {
        return Ok(ClientConfig::default());
    }
    let raw = std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    toml::from_str(&raw).with_context(|| format!("parse {}", path.display()))
}

/// Persist the config with mode 0600.
pub fn save(cfg: &ClientConfig) -> Result<()> {
    let path = config_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("mkdir {}", parent.display()))?;
    }
    let raw = toml::to_string_pretty(cfg).context("serialize config")?;
    std::fs::write(&path, raw).with_context(|| format!("write {}", path.display()))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("chmod 600 {}", path.display()))?;
    }

    Ok(())
}

/// True when the URL host is a loopback address (`local` mode).
pub fn is_loopback_url(url: &str) -> bool {
    let host = url::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string));
    match host.as_deref() {
        Some(h) => is_loopback_host(h),
        None => {
            let s = url.to_ascii_lowercase();
            s.starts_with("http://127.0.0.1")
                || s.starts_with("http://localhost")
                || s.starts_with("http://[::1]")
        }
    }
}

/// True for loopback host literals (`127.0.0.1` / `localhost` / `::1`).
pub fn is_loopback_host(host: &str) -> bool {
    matches!(host, "127.0.0.1" | "localhost" | "::1" | "[::1]")
}

/// Mode for a URL: loopback host → local; anything else → remote.
pub fn mode_of(url: &str) -> ClientMode {
    if is_loopback_url(url) {
        ClientMode::Local
    } else {
        ClientMode::Remote
    }
}

/// Resolve the effective connection from raw inputs.
///
/// `api_flag` / `token_flag` already combine the explicit flag with its env
/// var (clap merges flag > env). Returns the effective URL + token + the
/// context name used (explicit or `current`) + mode.
pub fn resolve(
    cfg: &ClientConfig,
    api_flag: Option<&str>,
    token_flag: Option<&str>,
    explicit_context: Option<&str>,
    allow_insecure_http: bool,
) -> Result<Conn> {
    let context_name = explicit_context
        .map(str::to_string)
        .or_else(|| cfg.current.clone());

    let ctx = context_name.as_deref().and_then(|n| cfg.contexts.get(n));

    // Fails fast on a dead context. An explicit `--context` must exist no
    // matter what; the implicit `current` context can be overridden by an
    // explicit URL.
    let missing_context = match explicit_context {
        Some(name) => !cfg.contexts.contains_key(name),
        None => {
            api_flag.is_none()
                && cfg
                    .current
                    .as_ref()
                    .is_some_and(|n| !cfg.contexts.contains_key(n))
        }
    };
    if missing_context {
        let name = context_name.as_deref().unwrap_or_default();
        let available = if cfg.contexts.is_empty() {
            "(none configured)".to_string()
        } else {
            cfg.contexts.keys().cloned().collect::<Vec<_>>().join(", ")
        };
        bail!(
            "context '{name}' not found in {} (available: {available}); \
             use `mc2 context set <name> --api <url>` or pass --api directly",
            config_path().display()
        );
    }

    let url = api_flag
        .map(|s| s.trim_end_matches('/').to_string())
        .or_else(|| ctx.map(|c| c.url.trim_end_matches('/').to_string()))
        .unwrap_or_else(|| DEFAULT_API_URL.to_string());

    let token = token_flag
        .map(str::to_string)
        .or_else(|| ctx.and_then(|c| c.api_key.clone()))
        .filter(|s| !s.is_empty());

    let mode = mode_of(&url);
    if mode == ClientMode::Remote
        && url.to_ascii_lowercase().starts_with("http://")
        && !allow_insecure_http
    {
        bail!(
            "refusing to use plaintext http:// for a remote control plane ({url}): \
             the operator token would cross the network unencrypted. \
             Use https:// (e.g. via Traefik), or set --allow-insecure-http / \
             MC2_ALLOW_INSECURE_HTTP=1 to override."
        );
    }

    Ok(Conn {
        url,
        token,
        context: context_name,
        mode,
    })
}

/// Truthy check for opt-in env vars (`MC2_ALLOW_INSECURE_HTTP`).
pub fn env_truthy(key: &str) -> bool {
    std::env::var(key)
        .map(|v| matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"))
        .unwrap_or(false)
}

/// Serializes tests that mutate the process-global `HOME` env. Shared across
/// `context` and `setup` test modules (single lock for the whole binary).
#[cfg(test)]
pub(crate) static HOME_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
pub(crate) fn with_home<T>(dir: &std::path::Path, f: impl FnOnce() -> T) -> T {
    let _guard = HOME_LOCK.lock().unwrap();
    std::env::set_var("HOME", dir);
    let out = f();
    std::env::remove_var("HOME");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg_with(name: &str, url: &str, token: Option<&str>, current: Option<&str>) -> ClientConfig {
        ClientConfig {
            current: current.map(str::to_string),
            contexts: BTreeMap::from([(
                name.to_string(),
                ContextEntry {
                    url: url.to_string(),
                    api_key: token.map(str::to_string),
                },
            )]),
        }
    }

    #[test]
    fn local_loopback_is_local_mode() {
        assert!(is_loopback_url("http://127.0.0.1:7443"));
        assert!(is_loopback_url("http://localhost:7443"));
        assert!(is_loopback_url("http://[::1]:7443"));
        assert_eq!(mode_of("http://127.0.0.1:7443"), ClientMode::Local);
        assert_eq!(mode_of("https://mc2.example.com"), ClientMode::Remote);
    }

    #[test]
    fn default_resolution_is_local() {
        let cfg = ClientConfig::default();
        let conn = resolve(&cfg, None, None, None, false).unwrap();
        assert_eq!(conn.url, DEFAULT_API_URL);
        assert_eq!(conn.mode, ClientMode::Local);
        assert!(conn.token.is_none());
        assert!(conn.context.is_none());
    }

    #[test]
    fn explicit_flag_beats_env_beats_context() {
        let cfg = cfg_with("prod", "https://prod.example.com", Some("t1"), Some("prod"));
        // Explicit api flag (highest).
        let conn = resolve(
            &cfg,
            Some("https://override.example.com"),
            None,
            None,
            false,
        )
        .unwrap();
        assert_eq!(conn.url, "https://override.example.com");
        assert_eq!(conn.mode, ClientMode::Remote);
        // No flag: current context supplies url + token.
        let conn = resolve(&cfg, None, None, None, false).unwrap();
        assert_eq!(conn.url, "https://prod.example.com");
        assert_eq!(conn.token.as_deref(), Some("t1"));
        assert_eq!(conn.context.as_deref(), Some("prod"));
        // Explicit --context overrides current.
        let cfg2 = cfg_with("dev", "https://dev.example.com:7443", None, Some("prod"));
        let conn = resolve(&cfg2, None, None, Some("dev"), false).unwrap();
        assert_eq!(conn.url, "https://dev.example.com:7443");
        assert_eq!(conn.context.as_deref(), Some("dev"));
        // Flag token beats context token.
        let conn = resolve(&cfg, None, Some("t2"), None, false).unwrap();
        assert_eq!(conn.token.as_deref(), Some("t2"));
    }

    #[test]
    fn remote_plaintext_http_rejected_without_opt_in() {
        let cfg = cfg_with("bad", "http://mc2.example.com", Some("t"), None);
        let err = resolve(&cfg, None, None, Some("bad"), false).unwrap_err();
        assert!(err.to_string().contains("plaintext http"), "{err}");
        // Opt-in flag lets it through.
        let conn = resolve(&cfg, None, None, Some("bad"), true).unwrap();
        assert_eq!(conn.url, "http://mc2.example.com");
        // Loopback http is always fine.
        let conn = resolve(
            &ClientConfig::default(),
            Some("http://127.0.0.1:7443"),
            None,
            None,
            false,
        )
        .unwrap();
        assert_eq!(conn.mode, ClientMode::Local);
    }

    #[test]
    fn dead_context_handling() {
        // Dead implicit `current` context errors when nothing overrides it.
        let cfg = ClientConfig {
            current: Some("gone".into()),
            contexts: BTreeMap::new(),
        };
        let err = resolve(&cfg, None, None, None, false).unwrap_err();
        assert!(err.to_string().contains("not found"), "{err}");
        // Explicit URL overrides the dead current context: no error.
        let conn = resolve(&cfg, Some("https://x.example.com"), None, None, false).unwrap();
        assert_eq!(conn.url, "https://x.example.com");
        // An explicit --context must exist even with an explicit URL.
        let err = resolve(
            &cfg,
            Some("https://x.example.com"),
            None,
            Some("missing"),
            false,
        )
        .unwrap_err();
        assert!(err.to_string().contains("not found"), "{err}");
    }

    #[test]
    fn config_save_load_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        with_home(dir.path(), || {
            let cfg = cfg_with(
                "prod",
                "https://mc2.example.com",
                Some("mc2at_abc"),
                Some("prod"),
            );
            save(&cfg).unwrap();
            let loaded = load().unwrap();
            assert_eq!(loaded.current.as_deref(), Some("prod"));
            assert_eq!(loaded.contexts["prod"].url, "https://mc2.example.com");
            assert_eq!(
                loaded.contexts["prod"].api_key.as_deref(),
                Some("mc2at_abc")
            );
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = std::fs::metadata(config_path())
                    .unwrap()
                    .permissions()
                    .mode();
                assert_eq!(mode & 0o777, 0o600, "config must be 0600");
            }
        });
    }

    #[test]
    fn missing_config_is_default() {
        let dir = tempfile::tempdir().unwrap();
        with_home(dir.path(), || assert!(load().unwrap().contexts.is_empty()));
    }
}
