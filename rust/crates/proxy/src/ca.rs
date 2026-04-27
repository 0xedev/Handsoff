//! On-disk CA management for the local MITM proxy.
//!
//! Generates a self-signed root CA on first run and persists it under
//! `~/.handoff/ca/{cert,key}.pem`. The user is expected to install this
//! into the system trust store so spawned agents trust the on-the-fly
//! certs the proxy mints for upstream hosts.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use handoff_common::home_dir;
use rcgen::{
    BasicConstraints, CertificateParams, DnType, IsCa, KeyPair, KeyUsagePurpose,
};

pub fn ca_dir() -> PathBuf {
    let dir = home_dir().join("ca");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

pub fn cert_pem_path() -> PathBuf {
    ca_dir().join("cert.pem")
}

pub fn key_pem_path() -> PathBuf {
    ca_dir().join("key.pem")
}

/// Load the CA from disk if present, otherwise generate one and write it.
pub fn load_or_create() -> Result<(String, String)> {
    let cert_path = cert_pem_path();
    let key_path = key_pem_path();
    if cert_path.exists() && key_path.exists() {
        let cert = std::fs::read_to_string(&cert_path)
            .with_context(|| format!("reading {}", cert_path.display()))?;
        let key = std::fs::read_to_string(&key_path)
            .with_context(|| format!("reading {}", key_path.display()))?;
        return Ok((cert, key));
    }
    generate_and_persist(&cert_path, &key_path)
}

fn generate_and_persist(cert_path: &Path, key_path: &Path) -> Result<(String, String)> {
    let mut params = CertificateParams::default();
    params
        .distinguished_name
        .push(DnType::CommonName, "handoff CA");
    params
        .distinguished_name
        .push(DnType::OrganizationName, "handoff");
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    params.key_usages = vec![
        KeyUsagePurpose::KeyCertSign,
        KeyUsagePurpose::CrlSign,
        KeyUsagePurpose::DigitalSignature,
    ];
    let key_pair = KeyPair::generate()?;
    let cert = params.self_signed(&key_pair)?;
    let cert_pem = cert.pem();
    let key_pem = key_pair.serialize_pem();
    std::fs::write(cert_path, &cert_pem)?;
    std::fs::write(key_path, &key_pem)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(key_path, std::fs::Permissions::from_mode(0o600));
    }
    Ok((cert_pem, key_pem))
}
