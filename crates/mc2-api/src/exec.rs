//! Wire encoding for `mc2 exec` (`POST /v1/instances/{id}/exec`).
//!
//! Guest stdin, stdout and stderr are arbitrary byte streams — a process may
//! emit invalid UTF-8 (e.g. `0xFF`) — while the REST envelope is JSON, which is
//! UTF-8 only. Every one of the three fields therefore travels base64-encoded
//! (`stdinBase64`, `stdoutBase64`, `stderrBase64`), so bytes survive the
//! round-trip unmodified.
//!
//! The 16 MiB body cap (`mc2_server::limits::EXEC_BODY_LIMIT_BYTES`) is spent
//! on the encoded envelope, so the largest raw stdin is ~12 MiB (base64 expands
//! by 4/3).

use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;

/// Encode raw bytes for a base64 JSON field.
pub fn encode_bytes(bytes: &[u8]) -> String {
    STANDARD.encode(bytes)
}

/// Decode a base64 JSON field back to raw bytes.
///
/// `Err` means the client sent malformed base64 (a `400` at the REST layer).
pub fn decode_bytes(text: &str) -> Result<Vec<u8>, base64::DecodeError> {
    STANDARD.decode(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_decode_round_trips_non_utf8_bytes() {
        let raw = [0x00u8, 0xff, 0xfe, 0x80, b'h', b'i', 0xed, 0xa0, 0x80];
        let encoded = encode_bytes(&raw);
        assert_eq!(decode_bytes(&encoded).unwrap(), raw);
        assert_eq!(decode_bytes(&encode_bytes(b"")).unwrap(), Vec::<u8>::new());
    }

    #[test]
    fn decode_rejects_malformed_input() {
        assert!(decode_bytes("not base64!!").is_err());
    }
}
