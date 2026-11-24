//! Shared SHA-256 byte digest; domain framing and serialization stay at callers.
use sha2::{Digest, Sha256};
pub(crate) fn sha256_hex(bytes: impl AsRef<[u8]>) -> String {
    let mut hash = Sha256::new();
    hash.update(bytes.as_ref());
    finish_sha256_hex(hash)
}

pub(crate) const SHA256_HEX_BYTES: usize = 64;
pub(crate) fn finish_sha256_hex(hash: Sha256) -> String {
    use std::fmt::Write;
    let mut hex = String::with_capacity(SHA256_HEX_BYTES);
    for byte in hash.finalize() {
        write!(hex, "{byte:02x}").expect("formatting into a String is infallible");
    }
    hex
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
