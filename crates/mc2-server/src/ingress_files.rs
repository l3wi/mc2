//! Write Traefik Ingress catalog files (D7).

use anyhow::{Context, Result};
use mc2_runtime::{
    render_catalog_json, render_traefik_dynamic, DesiredIngressRoute, ReadyIngressRoute,
    SandboxPhase,
};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::net::TcpStream;
use tokio::time::{timeout, Duration};
use tracing::{debug, info, warn};

/// Server-level self-route: publish the control-plane REST API itself through
/// the ingress catalog (remote client access, BYO Traefik TLS).
///
/// Unlike stack `ingress:` routes there is no instance phase involved — the
/// readiness gate is simply that the REST port accepts a connection.
#[derive(Debug, Clone)]
pub struct SelfIngressRoute {
    pub host: String,
    pub port: u16,
    pub tls_cert_resolver: String,
}

impl SelfIngressRoute {
    pub fn to_ready(&self) -> ReadyIngressRoute {
        ReadyIngressRoute {
            id: format!("mc2-self-{}", self.host),
            stack: "mc2".into(),
            host: self.host.clone(),
            path: "/".into(),
            path_type: "Prefix".into(),
            service: "mc2-api".into(),
            guest_port: 0,
            backend_host: "127.0.0.1".into(),
            backend_port: self.port,
            instance_id: String::new(),
            ordinal: 0,
            tls_enabled: true,
            cert_resolver: self.tls_cert_resolver.clone(),
            tcp: false,
            entry_point: String::new(),
        }
    }
}

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

    /// Render ready routes from the desired plan + local instance phases; write files if changed.
    ///
    /// `phases` maps instance_id → phase string (`Running`, …). `self_route`,
    /// when set, is the always-available control-plane route (gate: REST port
    /// accepting) independent of any stack.
    pub async fn reconcile(
        &mut self,
        routes: &[DesiredIngressRoute],
        phases: &HashMap<String, String>,
        self_route: Option<&SelfIngressRoute>,
    ) -> Result<IngressRenderStatus> {
        let mut ready = Vec::new();
        let mut pending = Vec::new();

        for desired in routes {
            let phase = phases
                .get(&desired.backend_instance_id)
                .map(|s| s.as_str())
                .unwrap_or("");
            let running = phase == SandboxPhase::Running.as_str() || phase == "Running";
            let bind_host = if desired.bind.is_empty() {
                "127.0.0.1"
            } else {
                desired.bind.as_str()
            };

            if running
                && !desired.backend_instance_id.is_empty()
                && desired.host_port > 0
                && port_accepting(bind_host, desired.host_port).await
            {
                ready.push(desired.to_ready(bind_host));
            } else {
                pending.push(desired.to_ready(bind_host));
            }
        }

        if let Some(sr) = self_route {
            let r = sr.to_ready();
            if port_accepting("127.0.0.1", sr.port).await {
                ready.push(r);
            } else {
                pending.push(r);
            }
        }

        ready.sort_by(|a, b| a.id.cmp(&b.id));
        pending.sort_by(|a, b| a.id.cmp(&b.id));

        let traefik = render_traefik_dynamic(&ready);
        let generated_at = iso_now();
        let catalog = render_catalog_json(&self.node_name, &generated_at, &ready, &pending);

        let fingerprint = format!("{:x}", simple_hash(&format!("{traefik}\n---\n{catalog}")));

        if self.last_fingerprint.as_ref() == Some(&fingerprint) {
            return Ok(IngressRenderStatus {
                ready: ready.len(),
                pending: pending.len(),
                wrote: false,
            });
        }

        std::fs::create_dir_all(self.dir.join("traefik"))
            .with_context(|| format!("mkdir {}", self.dir.join("traefik").display()))?;

        atomic_write(&self.dir.join("catalog.json"), catalog.as_bytes())?;
        atomic_write(
            &self.dir.join("traefik").join("dynamic.yml"),
            traefik.as_bytes(),
        )?;

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
    std::fs::create_dir_all(parent).with_context(|| format!("mkdir {}", parent.display()))?;
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
            "ingress routes planned but --ingress-config-dir / MC2_INGRESS_CONFIG_DIR unset; not rendering files"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    fn route(host_port: u16, instance_id: &str) -> DesiredIngressRoute {
        DesiredIngressRoute {
            id: "demo-app.local-/-web-8000".into(),
            stack: "demo".into(),
            host: "app.local".into(),
            path: "/".into(),
            path_type: "Prefix".into(),
            service: "web".into(),
            guest_port: 8000,
            host_port,
            bind: "127.0.0.1".into(),
            tls_enabled: false,
            cert_resolver: String::new(),
            tcp: false,
            entry_point: String::new(),
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

        let st = writer.reconcile(&routes, &phases, None).await.unwrap();
        assert_eq!(st.ready, 1);
        assert_eq!(st.pending, 0);
        assert!(st.wrote);

        let traefik = std::fs::read_to_string(dir.path().join("traefik/dynamic.yml")).unwrap();
        let catalog = std::fs::read_to_string(dir.path().join("catalog.json")).unwrap();

        assert!(
            traefik.contains(&format!("http://127.0.0.1:{port}")),
            "{traefik}"
        );
        assert!(traefik.contains("Host(`app.local`)"), "{traefik}");
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

        let st = writer.reconcile(&routes, &phases, None).await.unwrap();
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
        let st2 = writer.reconcile(&routes2, &phases, None).await.unwrap();
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
            .reconcile(&[route(port1, "demo-web-0")], &phases, None)
            .await
            .unwrap();
        let t1 = std::fs::read_to_string(dir.path().join("traefik/dynamic.yml")).unwrap();
        assert!(t1.contains(&format!(":{port1}")), "{t1}");

        // Simulate recreate: new host port, old listener gone.
        drop(listener1);
        let (listener2, port2) = listen_ephemeral().await;
        assert_ne!(port1, port2);
        writer
            .reconcile(&[route(port2, "demo-web-0")], &phases, None)
            .await
            .unwrap();
        let t2 = std::fs::read_to_string(dir.path().join("traefik/dynamic.yml")).unwrap();
        assert!(t2.contains(&format!(":{port2}")), "{t2}");
        assert!(!t2.contains(&format!(":{port1}")), "stale port: {t2}");

        // Instance stopped → empty ready catalog.
        phases.insert("demo-web-0".into(), "Stopped".into());
        let st = writer
            .reconcile(&[route(port2, "demo-web-0")], &phases, None)
            .await
            .unwrap();
        assert_eq!(st.ready, 0);
        let t3 = std::fs::read_to_string(dir.path().join("traefik/dynamic.yml")).unwrap();
        assert!(t3.contains("http: {}"), "{t3}");
        drop(listener2);
    }

    #[tokio::test]
    async fn lifecycle_route_removed_from_plan() {
        let dir = tempfile::tempdir().unwrap();
        let mut writer = IngressFileWriter::new(dir.path().to_path_buf(), "n".into());
        let (listener, port) = listen_ephemeral().await;
        let _l = listener;
        let mut phases = HashMap::new();
        phases.insert("demo-web-0".into(), "Running".into());

        writer
            .reconcile(&[route(port, "demo-web-0")], &phases, None)
            .await
            .unwrap();

        // Empty desired routes (stack ingress deleted).
        let st = writer.reconcile(&[], &phases, None).await.unwrap();
        assert_eq!(st.ready, 0);
        assert!(st.wrote);
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

        let st1 = writer.reconcile(&routes, &phases, None).await.unwrap();
        assert!(st1.wrote);
        let st2 = writer.reconcile(&routes, &phases, None).await.unwrap();
        assert!(!st2.wrote);
        assert_eq!(st2.ready, 1);
    }

    #[tokio::test]
    async fn self_route_ready_when_rest_port_live_and_tls() {
        let dir = tempfile::tempdir().unwrap();
        let (listener, port) = listen_ephemeral().await;
        let _listener = listener;

        let mut writer = IngressFileWriter::new(dir.path().to_path_buf(), "n".into());
        let self_route = SelfIngressRoute {
            host: "mc2.example.com".into(),
            port,
            tls_cert_resolver: "le".into(),
        };

        let st = writer
            .reconcile(&[], &HashMap::new(), Some(&self_route))
            .await
            .unwrap();
        assert_eq!(st.ready, 1);
        assert_eq!(st.pending, 0);
        assert!(st.wrote);

        let traefik = std::fs::read_to_string(dir.path().join("traefik/dynamic.yml")).unwrap();
        assert!(traefik.contains("Host(`mc2.example.com`)"), "{traefik}");
        assert!(traefik.contains("websecure"), "{traefik}");
        assert!(traefik.contains("certResolver: le"), "{traefik}");
        assert!(
            traefik.contains(&format!("http://127.0.0.1:{port}")),
            "{traefik}"
        );
        assert!(!traefik.contains("Host(`app.local`)"), "{traefik}");

        let catalog = std::fs::read_to_string(dir.path().join("catalog.json")).unwrap();
        assert!(catalog.contains("mc2.example.com"), "{catalog}");
        assert!(catalog.contains("\"ready\": true"), "{catalog}");
        assert!(catalog.contains("mc2-api"), "{catalog}");
    }

    #[tokio::test]
    async fn self_route_pending_when_rest_port_down() {
        let dir = tempfile::tempdir().unwrap();
        // Get a free port then drop the listener so nothing accepts.
        let port = {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            listener.local_addr().unwrap().port()
        };

        let mut writer = IngressFileWriter::new(dir.path().to_path_buf(), "n".into());
        let self_route = SelfIngressRoute {
            host: "mc2.example.com".into(),
            port,
            tls_cert_resolver: "le".into(),
        };

        let st = writer
            .reconcile(&[], &HashMap::new(), Some(&self_route))
            .await
            .unwrap();
        assert_eq!(st.ready, 0);
        assert_eq!(st.pending, 1);

        let traefik = std::fs::read_to_string(dir.path().join("traefik/dynamic.yml")).unwrap();
        assert!(traefik.contains("http: {}"), "{traefik}");
        let catalog = std::fs::read_to_string(dir.path().join("catalog.json")).unwrap();
        assert!(catalog.contains("\"ready\": false"), "{catalog}");
    }
}
