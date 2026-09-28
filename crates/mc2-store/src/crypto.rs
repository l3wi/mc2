//! Secret encryption at rest (XChaCha20-Poly1305).

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use rand::RngCore;
use thiserror::Error;

/// 32-byte key for cluster secrets.
#[derive(Clone)]
pub struct SecretsKey([u8; 32]);

impl SecretsKey {
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub fn load_file(path: &std::path::Path) -> Result<Self, CryptoError> {
        let data = std::fs::read(path).map_err(|e| CryptoError::Io(e.to_string()))?;
        if data.len() < 32 {
            return Err(CryptoError::KeyTooShort);
        }
        let mut key = [0u8; 32];
        key.copy_from_slice(&data[..32]);
        Ok(Self(key))
    }

    /// Encrypt `plaintext` for the secret `name`.
    ///
    /// `name` is bound to the ciphertext as AEAD associated data
    /// (`aad_for`), so a row cannot be swapped between secret names.
    pub fn encrypt(&self, name: &str, plaintext: &[u8]) -> Result<(Vec<u8>, Vec<u8>), CryptoError> {
        let cipher =
            XChaCha20Poly1305::new_from_slice(&self.0).map_err(|_| CryptoError::InvalidKey)?;
        let mut nonce_bytes = [0u8; 24];
        rand::thread_rng().fill_bytes(&mut nonce_bytes);
        let nonce = XNonce::from_slice(&nonce_bytes);
        let aad = aad_for(name);
        let ciphertext = cipher
            .encrypt(
                nonce,
                Payload {
                    msg: plaintext,
                    aad: aad.as_slice(),
                },
            )
            .map_err(|_| CryptoError::Encrypt)?;
        Ok((nonce_bytes.to_vec(), ciphertext))
    }

    /// Decrypt ciphertext previously encrypted for the secret `name`.
    ///
    /// A name mismatch fails authentication (see `aad_for`).
    pub fn decrypt(
        &self,
        name: &str,
        nonce: &[u8],
        ciphertext: &[u8],
    ) -> Result<Vec<u8>, CryptoError> {
        if nonce.len() != 24 {
            return Err(CryptoError::InvalidNonce);
        }
        let cipher =
            XChaCha20Poly1305::new_from_slice(&self.0).map_err(|_| CryptoError::InvalidKey)?;
        let nonce = XNonce::from_slice(nonce);
        let aad = aad_for(name);
        cipher
            .decrypt(
                nonce,
                Payload {
                    msg: ciphertext,
                    aad: aad.as_slice(),
                },
            )
            .map_err(|_| CryptoError::Decrypt)
    }
}

/// AEAD associated data binding a ciphertext to its secret name.
///
/// Format: `mc2-secret-v1\0<name>`. The NUL separator keeps (`a`, `b`) and
/// (`ab`, ``) distinct; the version prefix allows a future format change.
fn aad_for(name: &str) -> Vec<u8> {
    let mut aad = Vec::with_capacity(14 + name.len());
    aad.extend_from_slice(b"mc2-secret-v1\0");
    aad.extend_from_slice(name.as_bytes());
    aad
}

impl std::fmt::Debug for SecretsKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SecretsKey([redacted])")
    }
}

#[derive(Debug, Error)]
pub enum CryptoError {
    #[error("secrets key too short (need 32 bytes)")]
    KeyTooShort,
    #[error("invalid secrets key")]
    InvalidKey,
    #[error("invalid nonce")]
    InvalidNonce,
    #[error("encryption failed")]
    Encrypt,
    #[error("decryption failed")]
    Decrypt,
    #[error("io: {0}")]
    Io(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let key = SecretsKey::from_bytes([7u8; 32]);
        let (n, c) = key.encrypt("DB_PASS", b"super-secret").unwrap();
        let plain = key.decrypt("DB_PASS", &n, &c).unwrap();
        assert_eq!(plain, b"super-secret");
    }

    #[test]
    fn ciphertext_is_bound_to_secret_name() {
        let key = SecretsKey::from_bytes([7u8; 32]);
        let (n, c) = key.encrypt("A", b"super-secret").unwrap();
        // Decrypting as any other name must fail authentication.
        assert!(key.decrypt("B", &n, &c).is_err());
        // Prefix collisions are not a match either (`A` vs `AB`).
        assert!(key.decrypt("AB", &n, &c).is_err());
        // The right name still works.
        assert_eq!(key.decrypt("A", &n, &c).unwrap(), b"super-secret");
    }
}
