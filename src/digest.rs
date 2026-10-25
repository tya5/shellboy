//! Shared SHA-256 byte digest; domain framing and serialization stay at callers.
use sha2::{Digest, Sha256};
pub(crate) fn sha256_hex(bytes: impl AsRef<[u8]>) -> String {
    Sha256::digest(bytes.as_ref())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn persisted_lowercase_sha256_encoding_is_byte_exact() {
        assert_eq!(
            sha256_hex([]),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        let bytes = [0, 255, 0, 195, 169];
        let old = Sha256::digest(bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        assert_eq!(sha256_hex(bytes), old);
    }
}
