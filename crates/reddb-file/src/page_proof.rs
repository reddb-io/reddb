//! Page-image references for the generation proof of a published tree.
//! The root belongs in the checksummed catalog, outside the pages it protects.

pub const PROOF_FANOUT: usize = 256;
pub const PROOF_LEVEL_MAX: u8 = 3;
pub const PROOF_HEADER_BYTES: usize = 24;
pub const PROOF_REFERENCE_BYTES: usize = 36;
const MAGIC: &[u8; 4] = b"RDPP";
const VERSION: u8 = 1;

const _: () = assert!(
    PROOF_HEADER_BYTES + PROOF_FANOUT * PROOF_REFERENCE_BYTES
        <= crate::PAGED_PAGE_SIZE - crate::PAGED_PAGE_HEADER_SIZE
);
const _: () = assert!(PROOF_LEVEL_MAX == 3 && PROOF_FANOUT == 256);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PageReference {
    pub page_id: u32,
    pub digest: [u8; 32],
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PageProofRoot {
    pub reference: PageReference,
    pub generation: u64,
}

pub fn page_digest(bytes: &[u8; crate::PAGED_PAGE_SIZE]) -> [u8; 32] {
    *blake3::hash(bytes).as_bytes()
}

/// Seals the complete physical catalog chain, including its links and page IDs.
#[derive(Default)]
pub struct CatalogDigest(blake3::Hasher);

impl CatalogDigest {
    pub fn add_page(&mut self, bytes: &[u8; crate::PAGED_PAGE_SIZE]) {
        self.0.update(bytes);
    }

    pub fn finish(self) -> [u8; 32] {
        *self.0.finalize().as_bytes()
    }
}

pub fn encode_reference(out: &mut [u8], reference: PageReference) {
    assert_eq!(out.len(), PROOF_REFERENCE_BYTES);
    out[..4].copy_from_slice(&reference.page_id.to_le_bytes());
    out[4..].copy_from_slice(&reference.digest);
}

pub fn decode_reference(bytes: &[u8]) -> Option<PageReference> {
    if bytes.len() != PROOF_REFERENCE_BYTES {
        return None;
    }
    let reference = PageReference {
        page_id: u32::from_le_bytes(bytes[..4].try_into().expect("four-byte page address")),
        digest: bytes[4..].try_into().expect("32-byte image digest"),
    };
    if (reference.page_id == 0) != (reference.digest == [0; 32]) {
        return None;
    }
    Some(reference)
}

pub fn initialize_node(content: &mut [u8], level: u8, prefix: u32, generation: u64) {
    assert!(level <= PROOF_LEVEL_MAX);
    assert_ne!(generation, 0);
    assert!(content.len() >= PROOF_HEADER_BYTES + PROOF_FANOUT * PROOF_REFERENCE_BYTES);
    content.fill(0);
    content[..4].copy_from_slice(MAGIC);
    content[4] = VERSION;
    content[5] = level;
    content[8..12].copy_from_slice(&prefix.to_le_bytes());
    content[16..24].copy_from_slice(&generation.to_le_bytes());
}

pub fn node_generation(content: &[u8], level: u8, prefix: u32) -> Option<u64> {
    if level > PROOF_LEVEL_MAX
        || content.len() < PROOF_HEADER_BYTES + PROOF_FANOUT * PROOF_REFERENCE_BYTES
        || content[..4] != *MAGIC
        || content[4] != VERSION
        || content[5] != level
        || content[6..8] != [0; 2]
        || content[8..12] != prefix.to_le_bytes()
        || content[12..16] != [0; 4]
    {
        return None;
    }
    let generation = u64::from_le_bytes(content[16..24].try_into().expect("generation field"));
    (generation != 0).then_some(generation)
}

pub fn child_prefix(prefix: u32, level: u8, index: usize) -> u32 {
    assert!(level <= PROOF_LEVEL_MAX);
    assert!(index < PROOF_FANOUT);
    prefix | (u32::try_from(index).expect("byte-sized child index") << (u32::from(level) * 8))
}

pub fn entry_range(level: u8, index: usize) -> std::ops::Range<usize> {
    assert!(level <= PROOF_LEVEL_MAX);
    assert!(index < PROOF_FANOUT);
    let size = if level == 0 {
        32
    } else {
        PROOF_REFERENCE_BYTES
    };
    let start = PROOF_HEADER_BYTES + index * size;
    start..start + size
}
