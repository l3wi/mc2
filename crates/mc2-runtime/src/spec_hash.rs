//! Stable fingerprint of desired sandbox config that requires recreate on change.

use crate::DesiredSandbox;
use sha2::{Digest, Sha256};

/// Hash of fields that cannot be live-updated safely under msb create-time config
/// (image, command, ports, fabric network policy, resources, env, secrets, labels…).
pub fn desired_recreate_hash(d: &DesiredSandbox) -> String {
    let mut h = Sha256::new();
    h.update(d.runtime_id.as_bytes());
    h.update(b"|");
    h.update(d.spec.image.as_bytes());
    h.update(b"|");
    h.update(d.spec.restart.as_bytes());
    h.update(b"|");
    h.update(d.spec.cpus.to_bits().to_le_bytes());
    h.update(d.spec.mem_limit_mib.to_le_bytes());

    if let Some(ref cmd) = d.spec.command {
        for c in cmd {
            h.update(c.as_bytes());
            h.update(b"\0");
        }
    } else {
        h.update(b"<default-cmd>");
    }

    for p in &d.spec.ports {
        h.update(p.published.to_le_bytes());
        h.update(p.target.to_le_bytes());
        h.update(p.protocol.as_bytes());
    }
    for p in &d.spec.network.profiles {
        h.update(p.as_bytes());
    }
    for (k, v) in &d.spec.env {
        h.update(k.as_bytes());
        h.update(b"=");
        h.update(v.as_bytes());
        h.update(b"\0");
    }
    for (k, v) in &d.spec.labels {
        h.update(k.as_bytes());
        h.update(b"=");
        h.update(v.as_bytes());
        h.update(b"\0");
    }
    for s in &d.secrets {
        h.update(s.env.as_bytes());
        h.update(b"@");
        // Value changes must recreate so msb injection updates.
        h.update(s.value.as_bytes());
        for host in &s.allow_hosts {
            h.update(host.as_bytes());
        }
    }
    // Fabric: expose guest ports + allow ports drive publish + Host:port policy at create.
    for e in &d.fabric.exposes {
        h.update(b"ex:");
        h.update(e.guest_port.to_le_bytes());
        h.update(e.protocol.as_bytes());
    }
    for a in &d.fabric.allows {
        h.update(b"al:");
        h.update(a.to_service.as_bytes());
        h.update(a.port.to_le_bytes());
        h.update(a.protocol.as_bytes());
        h.update([u8::from(a.backend_local)]);
    }
    for v in &d.spec.volumes {
        h.update(v.name.as_bytes());
        h.update(v.mount.as_bytes());
    }

    format!("{:x}", h.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fabric::{DesiredFabric, FabricAllowDesired};
    use mc2_api::ServiceSpec;
    use std::collections::BTreeMap;

    fn bare(image: &str) -> DesiredSandbox {
        DesiredSandbox {
            instance_id: "i".into(),
            stack: "s".into(),
            service: "w".into(),
            ordinal: 0,
            runtime_id: "s-w-0".into(),
            spec: ServiceSpec {
                image: image.into(),
                scale: 1,
                cpus: 1.0,
                mem_limit_mib: 128,
                ports: vec![],
                network: Default::default(),
                env: BTreeMap::new(),
                secrets: vec![],
                volumes: vec![],
                restart: "on-failure".into(),
                healthcheck: None,
                labels: BTreeMap::new(),
                command: None,
                node_name: None,
                node_selector: BTreeMap::new(),
                ssh: None,
                expose: vec![],
                networks: vec![],
            },
            secrets: vec![],
            ssh: Default::default(),
            fabric: DesiredFabric::default(),
        }
    }

    #[test]
    fn hash_changes_with_image() {
        let a = bare("alpine:3.20");
        let b = bare("python:3.12-alpine");
        assert_ne!(desired_recreate_hash(&a), desired_recreate_hash(&b));
    }

    #[test]
    fn hash_changes_with_fabric_allow() {
        let mut a = bare("alpine:3.20");
        let mut b = bare("alpine:3.20");
        b.fabric.allows.push(FabricAllowDesired {
            to_service: "db".into(),
            port: 5432,
            protocol: "tcp".into(),
            fqdn: "db.s.svc.mc2".into(),
            short_name: "db".into(),
            backend_instance_id: "x".into(),
            backend_node_id: "n".into(),
            backend_local: true,
            backend_ordinal: 0,
        });
        assert_ne!(desired_recreate_hash(&a), desired_recreate_hash(&b));
        a.fabric = b.fabric.clone();
        assert_eq!(desired_recreate_hash(&a), desired_recreate_hash(&b));
    }

    #[test]
    fn volume_mount_change_forces_recreate() {
        let mut a = bare("alpine:3.20");
        let mut b = bare("alpine:3.20");
        b.spec.volumes.push(mc2_api::VolumeMount {
            name: "data".into(),
            mount: "/data".into(),
        });
        assert_ne!(desired_recreate_hash(&a), desired_recreate_hash(&b));
        a.spec.volumes = b.spec.volumes.clone();
        assert_eq!(desired_recreate_hash(&a), desired_recreate_hash(&b));

        // Mount path change also forces recreate.
        b.spec.volumes[0].mount = "/srv/data".into();
        assert_ne!(desired_recreate_hash(&a), desired_recreate_hash(&b));
    }
}
