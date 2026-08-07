//! Token hashing helpers (API + join tokens).

use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

/// Which bootstrap token is being handled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    Api,
    Join,
}

/// SHA-256 hex digest of a plaintext token.
pub fn hash_token(token: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(token.as_bytes());
    hex::encode(hasher.finalize())
}

/// Constant-time compare of plaintext token against a stored hex hash.
pub fn verify_token(token: &str, expected_hash_hex: &str) -> bool {
    let actual = hash_token(token);
    if actual.len() != expected_hash_hex.len() {
        return false;
    }
    actual.as_bytes().ct_eq(expected_hash_hex.as_bytes()).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_and_verify() {
        let t = "mc2at_test_token_value";
        let h = hash_token(t);
        assert!(verify_token(t, &h));
        assert!(!verify_token("wrong", &h));
    }
}
