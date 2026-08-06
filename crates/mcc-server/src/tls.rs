//! Dev TLS material for gRPC (self-signed CA + server cert).

use anyhow::{Context, Result};
use rcgen::{BasicConstraints, CertificateParams, DnType, IsCa, KeyPair, SanType};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::path::{Path, PathBuf};
use tracing::info;

/// Paths to generated (or existing) TLS files under the data dir.
#[derive(Debug, Clone)]
pub struct TlsPaths {
    pub dir: PathBuf,
    pub ca_cert: PathBuf,
    pub server_cert: PathBuf,
    pub server_key: PathBuf,
}

impl TlsPaths {
    pub fn under(data_dir: &Path) -> Self {
        let dir = data_dir.join("tls");
        Self {
            ca_cert: dir.join("ca.pem"),
            server_cert: dir.join("server.pem"),
            server_key: dir.join("server.key"),
            dir,
        }
    }

    pub fn all_exist(&self) -> bool {
        self.ca_cert.is_file() && self.server_cert.is_file() && self.server_key.is_file()
    }
}

/// Ensure a lab CA and server certificate exist (SAN: localhost, 127.0.0.1).
pub fn ensure_dev_tls(data_dir: &Path) -> Result<TlsPaths> {
    let paths = TlsPaths::under(data_dir);
    if paths.all_exist() {
        return Ok(paths);
    }

    std::fs::create_dir_all(&paths.dir)
        .with_context(|| format!("mkdir {}", paths.dir.display()))?;

    let mut ca_params = CertificateParams::default();
    ca_params
        .distinguished_name
        .push(DnType::CommonName, "MCC Dev CA");
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    let ca_key = KeyPair::generate().context("generate CA key")?;
    let ca_cert = ca_params
        .self_signed(&ca_key)
        .context("self-sign CA cert")?;

    let mut server_params = CertificateParams::default();
    server_params
        .distinguished_name
        .push(DnType::CommonName, "mcc-server");
    server_params.subject_alt_names = vec![
        SanType::DnsName("localhost".try_into().context("dns SAN")?),
        SanType::IpAddress(IpAddr::V4(Ipv4Addr::LOCALHOST)),
        SanType::IpAddress(IpAddr::V6(Ipv6Addr::LOCALHOST)),
    ];
    let server_key = KeyPair::generate().context("generate server key")?;
    let server_cert = server_params
        .signed_by(&server_key, &ca_cert, &ca_key)
        .context("sign server cert")?;

    std::fs::write(&paths.ca_cert, ca_cert.pem())?;
    std::fs::write(&paths.server_cert, server_cert.pem())?;
    std::fs::write(&paths.server_key, server_key.serialize_pem())?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o600);
        std::fs::set_permissions(&paths.server_key, perms)?;
    }

    info!(
        ca = %paths.ca_cert.display(),
        cert = %paths.server_cert.display(),
        "generated dev TLS material for gRPC"
    );
    Ok(paths)
}
