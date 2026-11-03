//! Fixed-width candidates for the prepared collection's physical key index.
//! SHA-256 fingerprints are only candidates: readers verify the decoded key.
use crate::{RdbFileError, RdbFileResult};
use sha2::{Digest, Sha256};

pub const FINGERPRINT_BYTES: usize = 32;
pub const CANDIDATE_BYTES: usize = FINGERPRINT_BYTES + 8;

pub fn fingerprint(key: &str) -> [u8; FINGERPRINT_BYTES] {
    Sha256::digest(key.as_bytes()).into()
}

pub fn encode_candidate(fingerprint: &[u8; FINGERPRINT_BYTES], id: u64) -> [u8; CANDIDATE_BYTES] {
    let mut bytes = [0; CANDIDATE_BYTES];
    bytes[..FINGERPRINT_BYTES].copy_from_slice(fingerprint);
    bytes[FINGERPRINT_BYTES..].copy_from_slice(&id.to_be_bytes());
    bytes
}

pub fn decode_candidate(bytes: &[u8]) -> RdbFileResult<([u8; FINGERPRINT_BYTES], u64)> {
    if bytes.len() != CANDIDATE_BYTES {
        return Err(RdbFileError::InvalidOperation(
            "invalid native key-index candidate width".into(),
        ));
    }
    let fingerprint = bytes[..FINGERPRINT_BYTES]
        .try_into()
        .expect("invariant: fingerprint width");
    let id = u64::from_be_bytes(
        bytes[FINGERPRINT_BYTES..]
            .try_into()
            .expect("invariant: identity width"),
    );
    Ok((fingerprint, id))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_key_candidates_have_stable_fingerprints_and_identity_order() {
        assert_eq!(
            hex::encode(fingerprint("key")),
            "2c70e12b7a0646f92279f427c7b38e7334d8e5389cff167a1dc30e73f826b683"
        );
        let fingerprint = fingerprint("key");
        let mut previous = None;
        for id in [0, 1, 255, 256, u64::MAX] {
            let bytes = encode_candidate(&fingerprint, id);
            assert_eq!(
                decode_candidate(&bytes).expect("candidate"),
                (fingerprint, id)
            );
            assert!(previous.is_none_or(|previous| previous < bytes));
            previous = Some(bytes);
        }
        for length in [0, CANDIDATE_BYTES - 1, CANDIDATE_BYTES + 1] {
            assert!(decode_candidate(&vec![0; length]).is_err());
        }
    }
}
