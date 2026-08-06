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

    pub fn encrypt(&self, plaintext: &[u8]) -> Result<(Vec<u8>, Vec<u8>), CryptoError> {
        let cipher =
            XChaCha20Poly1305::new_from_slice(&self.0).map_err(|_| CryptoError::InvalidKey)?;
        let mut nonce_bytes = [0u8; 24];
        rand::thread_rng().fill_bytes(&mut nonce_bytes);
        let nonce = XNonce::from_slice(&nonce_bytes);
        let ciphertext = cipher
            .encrypt(
                nonce,
                Payload {
                    msg: plaintext,
                    aad: b"",
                },
            )
            .map_err(|_| CryptoError::Encrypt)?;
        Ok((nonce_bytes.to_vec(), ciphertext))
    }

    pub fn decrypt(&self, nonce: &[u8], ciphertext: &[u8]) -> Result<Vec<u8>, CryptoError> {
        if nonce.len() != 24 {
            return Err(CryptoError::InvalidNonce);
        }
        let cipher =
            XChaCha20Poly1305::new_from_slice(&self.0).map_err(|_| CryptoError::InvalidKey)?;
        let nonce = XNonce::from_slice(nonce);
        cipher
            .decrypt(
                nonce,
                Payload {
                    msg: ciphertext,
                    aad: b"",
                },
            )
            .map_err(|_| CryptoError::Decrypt)
    }
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
        let (n, c) = key.encrypt(b"super-secret").unwrap();
        let plain = key.decrypt(&n, &c).unwrap();
        assert_eq!(plain, b"super-secret");
    }
}
