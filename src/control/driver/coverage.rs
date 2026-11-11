//! Ordinary projection transcript foundation; not yet connected to live publication.
use sha2::{Digest, Sha256};

pub(super) struct Producer {
    hash: Sha256,
    unknown: bool,
    sequence: u64,
    scalars: u64,
    bytes: u64,
}
pub(super) struct FrozenSeal {
    pub(super) digest: [u8; 32],
    pub(super) next_sequence: u64,
    pub(super) scalar_end: u64,
    pub(super) utf8_bytes: u64,
}
impl Producer {
    pub(super) fn begin(incarnation: [u8; 16]) -> Self {
        let mut hash = Sha256::new();
        hash.update(b"shellboy-preview-ordinary-v1\0");
        hash.update(incarnation);
        Self {
            hash,
            unknown: false,
            sequence: 0,
            scalars: 0,
            bytes: 0,
        }
    }
    pub(super) fn admit_ordinary(&mut self, offset: u64, text: &str) -> Result<(), ()> {
        if self.unknown {
            return Err(());
        }
        let counts = (|| {
            if offset != self.scalars {
                return None;
            }
            Some((
                self.sequence.checked_add(1)?,
                self.scalars
                    .checked_add(text.chars().count().try_into().ok()?)?,
                self.bytes.checked_add(text.len().try_into().ok()?)?,
            ))
        })();
        let Some((sequence, scalars, bytes)) = counts else {
            self.unknown = true;
            return Err(());
        };
        self.hash.update([1]);
        self.hash.update(self.sequence.to_be_bytes());
        self.hash.update([0]);
        self.hash.update(offset.to_be_bytes());
        self.hash.update((text.len() as u64).to_be_bytes());
        self.hash.update(text.as_bytes());
        self.sequence = sequence;
        self.scalars = scalars;
        self.bytes = bytes;
        Ok(())
    }
    pub(super) fn freeze(mut self) -> Result<FrozenSeal, ()> {
        if self.unknown {
            return Err(());
        }
        self.hash.update([2]);
        for n in [self.sequence, self.scalars, self.bytes] {
            self.hash.update(n.to_be_bytes());
        }
        Ok(FrozenSeal {
            digest: self.hash.finalize().into(),
            next_sequence: self.sequence,
            scalar_end: self.scalars,
            utf8_bytes: self.bytes,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn canonical_transcripts_match_independent_byte_goldens() {
        let fixtures: serde_json::Value = serde_json::from_str(include_str!(
            "../../../docs/design/coverage-canonical-vectors.json"
        ))
        .unwrap();
        for fixture in fixtures["vectors"].as_array().unwrap() {
            let mut producer =
                Producer::begin([0, 0, 0, 0, 0, 0, 0x40, 0, 0x80, 0, 0, 0, 0, 0, 0, 1]);
            for part in fixture["segments"].as_array().unwrap() {
                producer
                    .admit_ordinary(
                        part["scalar_offset"].as_str().unwrap().parse().unwrap(),
                        part["text"].as_str().unwrap(),
                    )
                    .unwrap();
            }
            let seal = producer.freeze().unwrap();
            let digest = seal
                .digest
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>();
            assert_eq!(digest, fixture["sha256"].as_str().unwrap());
            assert_eq!(
                seal.next_sequence.to_string(),
                fixture["next_sequence"].as_str().unwrap()
            );
            assert_eq!(
                seal.scalar_end.to_string(),
                fixture["scalar_end"].as_str().unwrap()
            );
            assert_eq!(
                seal.utf8_bytes.to_string(),
                fixture["utf8_bytes"].as_str().unwrap()
            );
        }
    }
    #[test]
    fn wrong_offsets_and_overflow_fail_without_advancing_hash() {
        let mut p = Producer::begin([0; 16]);
        assert!(p.admit_ordinary(1, "a").is_err());
        assert_eq!(p.sequence, 0);
        assert!(p.admit_ordinary(0, "valid-after-error").is_err());
        assert!(p.freeze().is_err());
        let mut p = Producer::begin([0; 16]);
        p.sequence = u64::MAX;
        assert!(p.admit_ordinary(0, "a").is_err());
        assert_eq!(p.bytes, 0);
        assert!(p.freeze().is_err());
        let mut p = Producer::begin([0; 16]);
        p.bytes = u64::MAX;
        assert!(p.admit_ordinary(0, "a").is_err());
        assert_eq!(p.sequence, 0);
    }
}
