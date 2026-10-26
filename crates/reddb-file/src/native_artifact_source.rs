//! Optional native-index source proofs, stored in an ordinary in-file blob.
//! Legacy readers ignore the reference after the artifact descriptors. Legacy
//! writers drop that reference, so a later reader must rebuild its proofs.

use crate::embedded::{RdbFileError, RdbFileResult};
use crate::native_store::{NativeVectorArtifactPageSummary, NativeVectorArtifactSummary};

/// Bump when source hashing or native derivation semantics change.
pub const NATIVE_ARTIFACT_SOURCE_VERSION: u32 = 1;
const SOURCE_MAGIC: &[u8; 4] = b"RASP";
const REFERENCE_MAGIC: &[u8; 4] = b"RASR";

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct NativeArtifactSourceProof {
    pub collection: String,
    pub source_digest: [u8; 32],
    pub summaries: Vec<NativeVectorArtifactSummary>,
    pub pages: Vec<NativeVectorArtifactPageSummary>,
    pub fulltext_document_ids_complete: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NativeArtifactSourceReference {
    pub root_page: u32,
    pub checksum: u64,
}

pub fn encode_native_artifact_sources(
    proofs: &[NativeArtifactSourceProof],
) -> RdbFileResult<Vec<u8>> {
    let mut bytes = SOURCE_MAGIC.to_vec();
    bytes.extend_from_slice(&NATIVE_ARTIFACT_SOURCE_VERSION.to_le_bytes());
    let payload = serde_json::to_vec(proofs)
        .map_err(|err| RdbFileError::InvalidOperation(format!("encode artifact sources: {err}")))?;
    bytes.extend_from_slice(&payload);
    Ok(bytes)
}

pub fn decode_native_artifact_sources(
    bytes: &[u8],
) -> RdbFileResult<Vec<NativeArtifactSourceProof>> {
    if bytes.get(..4) != Some(SOURCE_MAGIC.as_slice())
        || bytes.get(4..8) != Some(NATIVE_ARTIFACT_SOURCE_VERSION.to_le_bytes().as_slice())
    {
        return Err(RdbFileError::InvalidOperation(
            "unsupported native artifact source proof".into(),
        ));
    }
    serde_json::from_slice(&bytes[8..])
        .map_err(|err| RdbFileError::InvalidOperation(format!("decode artifact sources: {err}")))
}

pub fn append_native_artifact_source_reference(
    bytes: &mut Vec<u8>,
    reference: NativeArtifactSourceReference,
) {
    bytes.extend_from_slice(REFERENCE_MAGIC);
    bytes.extend_from_slice(&reference.root_page.to_le_bytes());
    bytes.extend_from_slice(&reference.checksum.to_le_bytes());
}

pub fn decode_native_artifact_source_reference(
    bytes: &[u8],
) -> RdbFileResult<Option<NativeArtifactSourceReference>> {
    if bytes.get(..4) != Some(REFERENCE_MAGIC.as_slice()) {
        return Ok(None);
    }
    let payload = bytes.get(4..16).ok_or_else(|| {
        RdbFileError::InvalidOperation("truncated native artifact source reference".into())
    })?;
    let mut root = [0; 4];
    root.copy_from_slice(&payload[..4]);
    let mut checksum = [0; 8];
    checksum.copy_from_slice(&payload[4..]);
    let root_page = u32::from_le_bytes(root);
    if root_page == 0 {
        return Err(RdbFileError::InvalidOperation(
            "zero native artifact source root".into(),
        ));
    }
    Ok(Some(NativeArtifactSourceReference {
        root_page,
        checksum: u64::from_le_bytes(checksum),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_artifact_sources_reject_unknown_versions_and_truncated_proofs() {
        let proofs = vec![NativeArtifactSourceProof {
            collection: "rows".into(),
            source_digest: [17; 32],
            summaries: Vec::new(),
            pages: Vec::new(),
            fulltext_document_ids_complete: false,
        }];
        let bytes = encode_native_artifact_sources(&proofs).expect("encode proofs");
        assert_eq!(
            decode_native_artifact_sources(&bytes).expect("proofs"),
            proofs
        );
        for length in 0..bytes.len() {
            assert!(decode_native_artifact_sources(&bytes[..length]).is_err());
        }
        let mut unknown = bytes;
        unknown[4] ^= 1;
        assert!(decode_native_artifact_sources(&unknown).is_err());
        assert_eq!(
            decode_native_artifact_source_reference(&[0; 16]).expect("legacy"),
            None
        );
        let reference = NativeArtifactSourceReference {
            root_page: 9,
            checksum: 27,
        };
        let mut bytes = Vec::new();
        append_native_artifact_source_reference(&mut bytes, reference);
        assert_eq!(
            decode_native_artifact_source_reference(&bytes).expect("reference"),
            Some(reference)
        );
        for length in 4..bytes.len() {
            assert!(decode_native_artifact_source_reference(&bytes[..length]).is_err());
        }
    }
}
