//! Write Ingress catalog files for BYO Traefik/Caddy (D7).

use anyhow::{Context, Result};
use mc2_api::agent::IngressRouteDesired;
use mc2_runtime::{
    render_caddyfile, render_catalog_json, render_traefik_dynamic, DesiredIngressRoute,
    SandboxPhase,
};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::net::TcpStream;
use tokio::time::{timeout, Duration};
use tracing::{debug, info, warn};

/// Manages atomic writes under `--ingress-config-dir`.
pub struct IngressFileWriter {
    dir: PathBuf,
    node_name: String,
    last_fingerprint: Option<String>,
}

impl IngressFileWriter {
    pub fn new(dir: PathBuf, node_name: String) -> Self {
        Self {
            dir,
            node_name,
            last_fingerprint: None,
        }
    }

    /// Render ready routes from Sync plan + local instance phases; write files if changed.
    ///
    /// `phases` maps instance_id → phase string (`Running`, …).
    pub async fn reconcile(
        &mut self,
        routes: &[IngressRouteDesired],
        phases: &HashMap<String, String>,
    ) -> Result<IngressRenderStatus> {
        let mut ready = Vec::new();
        let mut pending = Vec::new();

        for r in routes {
            let desired = DesiredIngressRoute {
                id: r.id.clone(),
                stack: r.stack.clone(),
                host: r.host.clone(),
                path: r.path.clone(),
                path_type: r.path_type.clone(),
                service: r.service.clone(),
                guest_port: r.guest_port as u16,
                host_port: r.host_port as u16,
                bind: if r.bind.is_empty() {
                    "127.0.0.1".into()
                } else {
                    r.bind.clone()
                },
                tls_enabled: r.tls_enabled,
                cert_resolver: r.cert_resolver.clone(),
                caddy_tls: r.caddy_tls.clone(),
                backend_instance_id: r.backend_instance_id.clone(),
                backend_ordinal: r.backend_ordinal,
            };

            let phase = phases
                .get(&r.backend_instance_id)
                .map(|s| s.as_str())
                .unwrap_or("");
            let running = phase == SandboxPhase::Running.as_str() || phase == "Running";
            let bind_host = if desired.bind.is_empty() {
                "127.0.0.1"
            } else {
                desired.bind.as_str()
            };

            if running
                && !r.backend_instance_id.is_empty()
                && desired.host_port > 0
                && port_accepting(bind_host, desired.host_port).await
            {
                ready.push(desired.to_ready(bind_host));
            } else {
                pending.push(desired.to_ready(bind_host));
            }
        }

        ready.sort_by(|a, b| a.id.cmp(&b.id));
        pending.sort_by(|a, b| a.id.cmp(&b.id));

        let traefik = render_traefik_dynamic(&ready);
        let caddy = render_caddyfile(&ready);
        let generated_at = iso_now();
        let catalog = render_catalog_json(&self.node_name, &generated_at, &ready, &pending);

        let fingerprint = format!(
            "{:x}",
            simple_hash(&format!("{traefik}\n---\n{caddy}\n---\n{catalog}"))
        );

        if self.last_fingerprint.as_ref() == Some(&fingerprint) {
            return Ok(IngressRenderStatus {
                ready: ready.len(),
                pending: pending.len(),
                wrote: false,
            });
        }

        std::fs::create_dir_all(self.dir.join("traefik"))
            .with_context(|| format!("mkdir {}", self.dir.join("traefik").display()))?;
        std::fs::create_dir_all(self.dir.join("caddy"))
            .with_context(|| format!("mkdir {}", self.dir.join("caddy").display()))?;

        atomic_write(&self.dir.join("catalog.json"), catalog.as_bytes())?;
        atomic_write(
            &self.dir.join("traefik").join("dynamic.yml"),
            traefik.as_bytes(),
        )?;
        atomic_write(&self.dir.join("caddy").join("Caddyfile"), caddy.as_bytes())?;

        self.last_fingerprint = Some(fingerprint);
        info!(
            dir = %self.dir.display(),
            ready = ready.len(),
            pending = pending.len(),
            "ingress catalog files written"
        );

        Ok(IngressRenderStatus {
            ready: ready.len(),
            pending: pending.len(),
            wrote: true,
        })
    }

}

#[derive(Debug, Default)]
pub struct IngressRenderStatus {
    pub ready: usize,
    pub pending: usize,
    pub wrote: bool,
}

fn atomic_write(path: &Path, data: &[u8]) -> Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)
        .with_context(|| format!("mkdir {}", parent.display()))?;
    let tmp = parent.join(format!(
        ".{}.tmp",
        path.file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("ingress")
    ));
    std::fs::write(&tmp, data).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, path)
        .with_context(|| format!("rename {} -> {}", tmp.display(), path.display()))?;
    debug!(path = %path.display(), bytes = data.len(), "ingress file written");
    Ok(())
}

async fn port_accepting(host: &str, port: u16) -> bool {
    let addr: SocketAddr = match format!("{host}:{port}").parse() {
        Ok(a) => a,
        Err(_) => {
            // host may be not a pure IP — try 127.0.0.1
            match format!("127.0.0.1:{port}").parse() {
                Ok(a) => a,
                Err(_) => return false,
            }
        }
    };
    matches!(
        timeout(Duration::from_millis(200), TcpStream::connect(addr)).await,
        Ok(Ok(_))
    )
}

fn iso_now() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Simple UTC-ish stamp; good enough for catalog debug.
    format!("{secs}")
}

fn simple_hash(s: &str) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

/// Log when stacks want ingress but dir is unset.
pub fn warn_ingress_dir_unset(route_count: usize) {
    if route_count > 0 {
        warn!(
            routes = route_count,
            "ingress routes in Sync but --ingress-config-dir / MC2_INGRESS_CONFIG_DIR unset; not rendering files"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    fn route(host_port: u16, instance_id: &str) -> IngressRouteDesired {
        IngressRouteDesired {
            id: "demo-app.local-/-web-8000".into(),
            stack: "demo".into(),
            host: "app.local".into(),
            path: "/".into(),
            path_type: "Prefix".into(),
            service: "web".into(),
            guest_port: 8000,
            host_port: u32::from(host_port),
            bind: "127.0.0.1".into(),
            tls_enabled: false,
            cert_resolver: String::new(),
            caddy_tls: "off".into(),
            backend_instance_id: instance_id.into(),
            backend_ordinal: 0,
        }
    }

    async fn listen_ephemeral() -> (TcpListener, u16) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        (listener, port)
    }

    #[tokio::test]
    async fn ready_when_running_and_port_live_writes_proxy_upstream() {
        let dir = tempfile::tempdir().unwrap();
        let (listener, port) = listen_ephemeral().await;
        // Keep listener alive for accept probe.
        let _listener = listener;

        let mut writer = IngressFileWriter::new(dir.path().to_path_buf(), "test-node".into());
        let routes = vec![route(port, "demo-web-0")];
        let mut phases = HashMap::new();
        phases.insert("demo-web-0".into(), "Running".into());

        let st = writer.reconcile(&routes, &phases).await.unwrap();
        assert_eq!(st.ready, 1);
        assert_eq!(st.pending, 0);
        assert!(st.wrote);

        let traefik = std::fs::read_to_string(dir.path().join("traefik/dynamic.yml")).unwrap();
        let caddy = std::fs::read_to_string(dir.path().join("caddy/Caddyfile")).unwrap();
        let catalog = std::fs::read_to_string(dir.path().join("catalog.json")).unwrap();

        assert!(
            traefik.contains(&format!("http://127.0.0.1:{port}")),
            "{traefik}"
        );
        assert!(traefik.contains("Host(`app.local`)"), "{traefik}");
        assert!(
            caddy.contains(&format!("reverse_proxy 127.0.0.1:{port}")),
            "{caddy}"
        );
        assert!(catalog.contains("\"ready\": true"), "{catalog}");
        assert!(catalog.contains("app.local"), "{catalog}");
        assert!(catalog.contains(&port.to_string()), "{catalog}");
    }

    #[tokio::test]
    async fn not_ready_when_port_dead_or_not_running() {
        let dir = tempfile::tempdir().unwrap();
        // Bind then drop so port is free but nothing accepts.
        let port = {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            listener.local_addr().unwrap().port()
        };
        // Port unbound — probe fails.

        let mut writer = IngressFileWriter::new(dir.path().to_path_buf(), "test-node".into());
        let routes = vec![route(port, "demo-web-0")];
        let mut phases = HashMap::new();
        phases.insert("demo-web-0".into(), "Running".into());

        let st = writer.reconcile(&routes, &phases).await.unwrap();
        assert_eq!(st.ready, 0);
        assert_eq!(st.pending, 1);

        let traefik = std::fs::read_to_string(dir.path().join("traefik/dynamic.yml")).unwrap();
        assert!(traefik.contains("http: {}"), "{traefik}");
        let catalog = std::fs::read_to_string(dir.path().join("catalog.json")).unwrap();
        assert!(catalog.contains("\"ready\": false"), "{catalog}");

        // Phase not Running — still pending even if we had a listener.
        let (listener, port2) = listen_ephemeral().await;
        let _listener = listener;
        let routes2 = vec![route(port2, "demo-web-0")];
        phases.insert("demo-web-0".into(), "Creating".into());
        let st2 = writer.reconcile(&routes2, &phases).await.unwrap();
        assert_eq!(st2.ready, 0);
        assert_eq!(st2.pending, 1);
    }

    #[tokio::test]
    async fn lifecycle_port_change_and_instance_stop() {
        let dir = tempfile::tempdir().unwrap();
        let mut writer = IngressFileWriter::new(dir.path().to_path_buf(), "test-node".into());

        let (listener1, port1) = listen_ephemeral().await;
        let mut phases = HashMap::new();
        phases.insert("demo-web-0".into(), "Running".into());

        writer
            .reconcile(&[route(port1, "demo-web-0")], &phases)
            .await
            .unwrap();
        let t1 = std::fs::read_to_string(dir.path().join("traefik/dynamic.yml")).unwrap();
        assert!(t1.contains(&format!(":{port1}")), "{t1}");

        // Simulate recreate: new host port, old listener gone.
        drop(listener1);
        let (listener2, port2) = listen_ephemeral().await;
        assert_ne!(port1, port2);
        writer
            .reconcile(&[route(port2, "demo-web-0")], &phases)
            .await
            .unwrap();
        let t2 = std::fs::read_to_string(dir.path().join("traefik/dynamic.yml")).unwrap();
        assert!(t2.contains(&format!(":{port2}")), "{t2}");
        assert!(!t2.contains(&format!(":{port1}")), "stale port: {t2}");

        // Instance stopped → empty ready catalog.
        phases.insert("demo-web-0".into(), "Stopped".into());
        let st = writer
            .reconcile(&[route(port2, "demo-web-0")], &phases)
            .await
            .unwrap();
        assert_eq!(st.ready, 0);
        let t3 = std::fs::read_to_string(dir.path().join("traefik/dynamic.yml")).unwrap();
        assert!(t3.contains("http: {}"), "{t3}");
        drop(listener2);
    }

    #[tokio::test]
    async fn lifecycle_route_removed_from_sync() {
        let dir = tempfile::tempdir().unwrap();
        let mut writer = IngressFileWriter::new(dir.path().to_path_buf(), "n".into());
        let (listener, port) = listen_ephemeral().await;
        let _l = listener;
        let mut phases = HashMap::new();
        phases.insert("demo-web-0".into(), "Running".into());

        writer
            .reconcile(&[route(port, "demo-web-0")], &phases)
            .await
            .unwrap();
        assert!(dir.path().join("caddy/Caddyfile").exists());

        // Empty Sync routes (stack ingress deleted).
        let st = writer.reconcile(&[], &phases).await.unwrap();
        assert_eq!(st.ready, 0);
        assert!(st.wrote);
        let caddy = std::fs::read_to_string(dir.path().join("caddy/Caddyfile")).unwrap();
        assert!(!caddy.contains("reverse_proxy"), "{caddy}");
        let catalog = std::fs::read_to_string(dir.path().join("catalog.json")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&catalog).unwrap();
        assert_eq!(v["routes"].as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn fingerprint_skips_rewrite_when_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let mut writer = IngressFileWriter::new(dir.path().to_path_buf(), "n".into());
        let (listener, port) = listen_ephemeral().await;
        let _l = listener;
        let mut phases = HashMap::new();
        phases.insert("demo-web-0".into(), "Running".into());
        let routes = vec![route(port, "demo-web-0")];

        let st1 = writer.reconcile(&routes, &phases).await.unwrap();
        assert!(st1.wrote);
        let st2 = writer.reconcile(&routes, &phases).await.unwrap();
        assert!(!st2.wrote);
        assert_eq!(st2.ready, 1);
    }
}
