//! Stable fingerprint of desired sandbox config that requires recreate on change.

use crate::DesiredSandbox;
use sha2::{Digest, Sha256};

/// Hash of fields that cannot be live-updated safely under msb create-time config
/// (image, command, ports, network network policy, resources, env, secrets, labels…).
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
    // Root-disk size is a create-time property (msb `root_disk`): a change
    // recreates the VM, so files outside volumes are not kept.
    h.update(d.spec.root_disk_mib().to_le_bytes());

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
    // Network: only the *effective create-time policy* — the sorted,
    // de-duplicated set of allowed guest ports plus the sorted exposes. Peer
    // identity (`to_service`), DNS names, backend placement and backend ids are
    // excluded: they churn without changing what the create-time `Host:tcp:P`
    // rule and publishes encode, and hashing them recreated every consumer VM
    // on a network whenever a neighbour moved (B1).
    let mut allow_ports: Vec<u16> = d.network.allows.iter().map(|a| a.port).collect();
    allow_ports.sort_unstable();
    allow_ports.dedup();
    h.update(b"net-allow-ports:");
    for p in allow_ports {
        h.update(p.to_le_bytes());
    }
    let mut exposes: Vec<(u16, &str)> = d
        .network
        .exposes
        .iter()
        .map(|e| (e.guest_port, e.protocol.as_str()))
        .collect();
    exposes.sort_unstable();
    h.update(b"net-exposes:");
    for (port, protocol) in exposes {
        h.update(port.to_le_bytes());
        h.update(protocol.as_bytes());
    }
    for v in &d.spec.volumes {
        h.update(v.name.as_bytes());
        h.update(v.mount.as_bytes());
        // Declared volume size: the mount quota is passed at every start, but a
        // declared-size change also changes the state an operator expects, so
        // keep the recreate semantics uniform (the data is kept either way).
        h.update(v.size_mib.to_le_bytes());
    }

    format!("{:x}", h.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::networks::{DesiredNetwork, NetworkAllowDesired};
    use mc2_api::ServiceSpec;
    use std::collections::BTreeMap;

    fn bare(image: &str) -> DesiredSandbox {
        DesiredSandbox {
            instance_id: "i".into(),
            stack: "s".into(),
            service: "w".into(),
            ordinal: 0,
            runtime_id: "s--w--0".into(),
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
                storage_opt: None,
                expose: vec![],
                networks: vec![],
                depends_on: BTreeMap::new(),
            },
            secrets: vec![],
            ssh: Default::default(),
            network: DesiredNetwork::default(),
        }
    }

    #[test]
    fn hash_changes_with_image() {
        let a = bare("alpine:3.20");
        let b = bare("python:3.12-alpine");
        assert_ne!(desired_recreate_hash(&a), desired_recreate_hash(&b));
    }

    fn allow(to_service: &str, port: u16) -> NetworkAllowDesired {
        NetworkAllowDesired {
            to_service: to_service.into(),
            port,
            protocol: "tcp".into(),
            fqdn: format!("{to_service}.s.svc.mc2"),
            short_name: to_service.into(),
        }
    }

    #[test]
    fn hash_changes_with_network_allow() {
        let mut a = bare("alpine:3.20");
        let mut b = bare("alpine:3.20");
        b.network.allows.push(allow("db", 5432));
        assert_ne!(desired_recreate_hash(&a), desired_recreate_hash(&b));
        a.network = b.network.clone();
        assert_eq!(desired_recreate_hash(&a), desired_recreate_hash(&b));
        // A *new port* on an existing peer still forces a recreate (the egress
        // rule is create-time)…
        b.network.allows.push(allow("db", 5433));
        assert_ne!(desired_recreate_hash(&a), desired_recreate_hash(&b));
    }

    #[test]
    fn hash_ignores_peer_identity_and_order() {
        // Permuted/allows-renamed but the same port set → identical hash: a
        // neighbour rename or move must not recreate an unrelated VM (B1).
        let mut a = bare("alpine:3.20");
        a.network.allows = vec![allow("db", 5432), allow("redis", 6379)];
        let mut b = bare("alpine:3.20");
        b.network.allows = vec![allow("redis", 6379), allow("db", 5432)];
        assert_eq!(desired_recreate_hash(&a), desired_recreate_hash(&b));

        // Same ports, different owner names / DNS names → still identical.
        let mut c = bare("alpine:3.20");
        c.network.allows = vec![allow("db-v2", 5432), allow("cache", 6379)];
        assert_eq!(desired_recreate_hash(&a), desired_recreate_hash(&c));

        // Duplicate ports across edges collapse to the same set.
        let mut d = bare("alpine:3.20");
        d.network.allows = vec![allow("db", 5432), allow("db-replica", 5432)];
        let mut e = bare("alpine:3.20");
        e.network.allows = vec![allow("db", 5432)];
        assert_eq!(desired_recreate_hash(&d), desired_recreate_hash(&e));
    }

    #[test]
    fn hash_changes_with_exposes_order_independently() {
        let mut a = bare("alpine:3.20");
        a.network.exposes = vec![
            crate::networks::NetworkExposeDesired {
                guest_port: 8080,
                protocol: "tcp".into(),
            },
            crate::networks::NetworkExposeDesired {
                guest_port: 9090,
                protocol: "tcp".into(),
            },
        ];
        let mut b = bare("alpine:3.20");
        b.network.exposes = vec![
            crate::networks::NetworkExposeDesired {
                guest_port: 9090,
                protocol: "tcp".into(),
            },
            crate::networks::NetworkExposeDesired {
                guest_port: 8080,
                protocol: "tcp".into(),
            },
        ];
        assert_eq!(desired_recreate_hash(&a), desired_recreate_hash(&b));
        b.network
            .exposes
            .push(crate::networks::NetworkExposeDesired {
                guest_port: 7070,
                protocol: "tcp".into(),
            });
        assert_ne!(desired_recreate_hash(&a), desired_recreate_hash(&b));
    }

    #[test]
    fn volume_mount_change_forces_recreate() {
        let mut a = bare("alpine:3.20");
        let mut b = bare("alpine:3.20");
        b.spec.volumes.push(mc2_api::VolumeMount {
            name: "data".into(),
            mount: "/data".into(),
            size_mib: mc2_api::DEFAULT_VOLUME_SIZE_MIB,
        });
        assert_ne!(desired_recreate_hash(&a), desired_recreate_hash(&b));
        a.spec.volumes = b.spec.volumes.clone();
        assert_eq!(desired_recreate_hash(&a), desired_recreate_hash(&b));

        // Mount path change also forces recreate.
        b.spec.volumes[0].mount = "/srv/data".into();
        assert_ne!(desired_recreate_hash(&a), desired_recreate_hash(&b));
    }

    #[test]
    fn root_disk_size_change_forces_recreate() {
        let a = bare("alpine:3.20");
        assert_eq!(a.spec.root_disk_mib(), 4 * 1024, "default root disk");

        let mut b = bare("alpine:3.20");
        b.spec.storage_opt = Some(mc2_api::StorageOptSpec { size_mib: 8 * 1024 });
        assert_ne!(desired_recreate_hash(&a), desired_recreate_hash(&b));
        assert_eq!(b.spec.root_disk_mib(), 8 * 1024);

        // Same size declared explicitly is the same desired state.
        let mut c = bare("alpine:3.20");
        c.spec.storage_opt = Some(mc2_api::StorageOptSpec { size_mib: 4 * 1024 });
        assert_eq!(desired_recreate_hash(&a), desired_recreate_hash(&c));
    }

    #[test]
    fn declared_volume_size_change_forces_recreate() {
        let mut a = bare("alpine:3.20");
        a.spec.volumes.push(mc2_api::VolumeMount {
            name: "data".into(),
            mount: "/data".into(),
            size_mib: 10 * 1024,
        });
        let mut b = bare("alpine:3.20");
        b.spec.volumes.push(mc2_api::VolumeMount {
            name: "data".into(),
            mount: "/data".into(),
            size_mib: 20 * 1024,
        });
        assert_ne!(desired_recreate_hash(&a), desired_recreate_hash(&b));
        b.spec.volumes[0].size_mib = 10 * 1024;
        assert_eq!(desired_recreate_hash(&a), desired_recreate_hash(&b));
    }
}
