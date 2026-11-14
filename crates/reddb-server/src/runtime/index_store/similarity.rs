use super::IndexMethodKind;
use crate::storage::engine::distance::DistanceMetric;
use crate::storage::engine::hnsw::{HnswConfig, HnswIndex};
use crate::storage::unified::entity::EntityId;
use parking_lot::RwLock;
use reddb_engine::biometric_matcher::{fingerprint_descriptor_bands, FINGERPRINT_MATCHER_VERSION};
use reddb_types::{BiometricSample, BitSignature, FeatureVector, Value};
use std::collections::{HashMap, HashSet};

const FEATURE_INDEX_VERSION: u32 = 1;
const BIT_INDEX_VERSION: u32 = 1;
const FEATURE_OVERSAMPLE: usize = 8;
const MIN_FEATURE_CANDIDATES: usize = 64;
const HAMMING_BAND_BITS: usize = 4;
const HAMMING_BAND_MIN_SCORE: f64 = 0.75;
const HAMMING_POSTING_PAGE_ENTRIES: usize = 64;
const MAX_BANDED_HAMMING_BYTES: usize = 4_096;

type IndexKey = (String, String);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SimilarityContract {
    Feature {
        space_id: [u8; 16],
        dimensions: usize,
        version: u32,
    },
    Bits {
        space_id: [u8; 16],
        bytes: usize,
        version: u32,
    },
    Minutiae {
        version: u32,
    },
}

impl SimilarityContract {
    pub(crate) fn is_current(self) -> bool {
        match self {
            Self::Feature { version, .. } => version == FEATURE_INDEX_VERSION,
            Self::Bits { version, .. } => version == BIT_INDEX_VERSION,
            Self::Minutiae { version } => version == FINGERPRINT_MATCHER_VERSION,
        }
    }

    pub(crate) fn version(self) -> u32 {
        match self {
            Self::Feature { version, .. }
            | Self::Bits { version, .. }
            | Self::Minutiae { version } => version,
        }
    }

    pub(crate) fn space_id(self) -> Option<[u8; 16]> {
        match self {
            Self::Feature { space_id, .. } | Self::Bits { space_id, .. } => Some(space_id),
            Self::Minutiae { .. } => None,
        }
    }

    pub(crate) fn width(self) -> Option<usize> {
        match self {
            Self::Feature { dimensions, .. } => Some(dimensions),
            Self::Bits { bytes, .. } => Some(bytes),
            Self::Minutiae { .. } => None,
        }
    }
}

pub(super) struct SimilarityIndexManager {
    indices: RwLock<HashMap<IndexKey, SimilarityIndex>>,
}

enum SimilarityIndex {
    Feature(FeatureIndex),
    Hamming(HammingIndex),
    Minutiae(MinutiaeIndex),
}

struct FeatureIndex {
    column: String,
    contract: Option<FeatureContract>,
    hnsw: Option<HnswIndex>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct FeatureContract {
    space_id: [u8; 16],
    dimensions: usize,
    version: u32,
}

struct HammingIndex {
    column: String,
    contract: Option<BitContract>,
    entries: HashMap<EntityId, BitSignature>,
    postings: HashMap<(usize, u8, u8), PostingPages>,
}

#[derive(Default)]
struct PostingPages {
    pages: Vec<Vec<EntityId>>,
    len: usize,
}

impl PostingPages {
    fn insert(&mut self, entity_id: EntityId) {
        if self
            .pages
            .last()
            .is_none_or(|page| page.len() == HAMMING_POSTING_PAGE_ENTRIES)
        {
            self.pages
                .push(Vec::with_capacity(HAMMING_POSTING_PAGE_ENTRIES));
        }
        self.pages
            .last_mut()
            .expect("a posting page was just allocated")
            .push(entity_id);
        self.len += 1;
    }

    fn remove(&mut self, entity_id: EntityId) {
        for page in &mut self.pages {
            if let Some(position) = page.iter().position(|candidate| *candidate == entity_id) {
                page.swap_remove(position);
                self.len -= 1;
                break;
            }
        }
        self.pages.retain(|page| !page.is_empty());
    }

    fn iter(&self) -> impl Iterator<Item = EntityId> + '_ {
        self.pages.iter().flatten().copied()
    }

    fn is_empty(&self) -> bool {
        self.len == 0
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct BitContract {
    space_id: [u8; 16],
    bytes: usize,
    version: u32,
}

struct MinutiaeIndex {
    column: String,
    version: u32,
    entries: HashMap<EntityId, BiometricSample>,
    postings: HashMap<u64, HashSet<EntityId>>,
}

impl SimilarityIndexManager {
    pub(super) fn new() -> Self {
        Self {
            indices: RwLock::new(HashMap::new()),
        }
    }

    pub(super) fn replace_collection_from(&self, collection: &str, prepared: Self) {
        let mut indices = self.indices.write();
        indices.retain(|(table, _), _| table != collection);
        indices.extend(prepared.indices.into_inner());
    }

    pub(super) fn memory_bytes(&self) -> u64 {
        self.indices
            .read()
            .values()
            .map(SimilarityIndex::memory_bytes)
            .fold(0, u64::saturating_add)
    }

    pub(super) fn create(
        &self,
        name: &str,
        collection: &str,
        column: &str,
        method: IndexMethodKind,
        entities: &[(EntityId, Vec<(String, Value)>)],
    ) -> Result<usize, String> {
        let mut index = match method {
            IndexMethodKind::Hnsw => SimilarityIndex::Feature(FeatureIndex {
                column: column.to_string(),
                contract: None,
                hnsw: None,
            }),
            IndexMethodKind::Hamming => SimilarityIndex::Hamming(HammingIndex {
                column: column.to_string(),
                contract: None,
                entries: HashMap::new(),
                postings: HashMap::new(),
            }),
            IndexMethodKind::Minutiae => SimilarityIndex::Minutiae(MinutiaeIndex {
                column: column.to_string(),
                version: FINGERPRINT_MATCHER_VERSION,
                entries: HashMap::new(),
                postings: HashMap::new(),
            }),
            _ => return Err("not a typed similarity index method".to_string()),
        };
        let mut count = 0;
        for (entity_id, fields) in entities {
            let Some(value) = fields
                .iter()
                .find_map(|(field, value)| (field == column).then_some(value))
            else {
                continue;
            };
            if value.is_null() {
                continue;
            }
            index.insert(*entity_id, value)?;
            count += 1;
        }
        self.indices
            .write()
            .insert((collection.to_string(), name.to_string()), index);
        Ok(count)
    }

    pub(super) fn drop_index(&self, collection: &str, name: &str) -> bool {
        self.indices
            .write()
            .remove(&(collection.to_string(), name.to_string()))
            .is_some()
    }

    pub(super) fn insert(
        &self,
        collection: &str,
        index_name: &str,
        entity_id: EntityId,
        value: &Value,
    ) -> Result<(), String> {
        let mut indices = self.indices.write();
        let index = indices
            .get_mut(&(collection.to_string(), index_name.to_string()))
            .ok_or_else(|| format!("typed similarity index '{index_name}' was not found"))?;
        index.insert(entity_id, value)
    }

    pub(super) fn validate_insert_batch(
        &self,
        collection: &str,
        index_name: &str,
        values: &[&Value],
    ) -> Result<(), String> {
        let indices = self.indices.read();
        let index = indices
            .get(&(collection.to_string(), index_name.to_string()))
            .ok_or_else(|| format!("typed similarity index '{index_name}' was not found"))?;
        index.validate_insert_batch(values)
    }

    pub(super) fn delete(&self, collection: &str, index_name: &str, entity_id: EntityId) {
        if let Some(index) = self
            .indices
            .write()
            .get_mut(&(collection.to_string(), index_name.to_string()))
        {
            index.delete(entity_id);
        }
    }

    pub(super) fn entries(&self, collection: &str, name: &str) -> Option<u64> {
        self.indices
            .read()
            .get(&(collection.to_string(), name.to_string()))
            .map(|index| index.len() as u64)
    }

    pub(super) fn contract(&self, collection: &str, name: &str) -> Option<SimilarityContract> {
        self.indices
            .read()
            .get(&(collection.to_string(), name.to_string()))
            .and_then(SimilarityIndex::contract)
    }

    pub(super) fn install_contract(
        &self,
        collection: &str,
        name: &str,
        contract: SimilarityContract,
    ) -> Result<(), String> {
        if !contract.is_current() {
            return Err("typed similarity index contract is stale".to_string());
        }
        let mut indices = self.indices.write();
        let index = indices
            .get_mut(&(collection.to_string(), name.to_string()))
            .ok_or_else(|| format!("typed similarity index '{name}' was not found"))?;
        index.install_contract(contract)
    }

    pub(super) fn candidates(
        &self,
        collection: &str,
        column: &str,
        method: IndexMethodKind,
        query: &Value,
        limit: usize,
        minimum_score: f64,
    ) -> Option<Result<HashSet<u64>, String>> {
        let indices = self.indices.read();
        let selected = indices
            .iter()
            .filter(|((table, _), index)| {
                table == collection && index.column() == column && index.method() == method
            })
            .min_by(|((_, left), _), ((_, right), _)| left.cmp(right));
        selected.map(|(_, index)| index.candidates(query, limit, minimum_score))
    }
}

impl SimilarityIndex {
    fn validate_insert_batch(&self, values: &[&Value]) -> Result<(), String> {
        match self {
            Self::Feature(index) => index.validate_insert_batch(values),
            Self::Hamming(index) => index.validate_insert_batch(values),
            Self::Minutiae(index) => {
                for value in values {
                    let Value::Fingerprint(value) = value else {
                        return Err("MINUTIAE index accepts only FINGERPRINT minutiae templates"
                            .to_string());
                    };
                    index.descriptors_for_insert(value)?;
                }
                Ok(())
            }
        }
    }

    fn contract(&self) -> Option<SimilarityContract> {
        match self {
            Self::Feature(index) => index.contract.map(|contract| SimilarityContract::Feature {
                space_id: contract.space_id,
                dimensions: contract.dimensions,
                version: contract.version,
            }),
            Self::Hamming(index) => index.contract.map(|contract| SimilarityContract::Bits {
                space_id: contract.space_id,
                bytes: contract.bytes,
                version: contract.version,
            }),
            Self::Minutiae(index) => Some(SimilarityContract::Minutiae {
                version: index.version,
            }),
        }
    }

    fn install_contract(&mut self, contract: SimilarityContract) -> Result<(), String> {
        match (self, contract) {
            (
                Self::Feature(index),
                SimilarityContract::Feature {
                    space_id,
                    dimensions,
                    version,
                },
            ) => {
                let expected = FeatureContract {
                    space_id,
                    dimensions,
                    version,
                };
                if index.contract.is_some_and(|actual| actual != expected) {
                    return Err(
                        "persisted FEATURE_VECTOR index contract disagrees with data".into(),
                    );
                }
                index.contract = Some(expected);
                if index.hnsw.is_none() {
                    let config = HnswConfig::default().with_metric(DistanceMetric::Cosine);
                    index.hnsw = Some(HnswIndex::new(dimensions, config));
                }
                Ok(())
            }
            (
                Self::Hamming(index),
                SimilarityContract::Bits {
                    space_id,
                    bytes,
                    version,
                },
            ) => {
                let expected = BitContract {
                    space_id,
                    bytes,
                    version,
                };
                if index.contract.is_some_and(|actual| actual != expected) {
                    return Err("persisted BIT_SIGNATURE index contract disagrees with data".into());
                }
                index.contract = Some(expected);
                Ok(())
            }
            (Self::Minutiae(index), SimilarityContract::Minutiae { version }) => {
                index.version = version;
                Ok(())
            }
            _ => Err("persisted typed similarity contract has the wrong index method".into()),
        }
    }

    fn method(&self) -> IndexMethodKind {
        match self {
            Self::Feature(_) => IndexMethodKind::Hnsw,
            Self::Hamming(_) => IndexMethodKind::Hamming,
            Self::Minutiae(_) => IndexMethodKind::Minutiae,
        }
    }

    fn column(&self) -> &str {
        match self {
            Self::Feature(index) => &index.column,
            Self::Hamming(index) => &index.column,
            Self::Minutiae(index) => &index.column,
        }
    }

    fn len(&self) -> usize {
        match self {
            Self::Feature(index) => index.hnsw.as_ref().map_or(0, HnswIndex::len),
            Self::Hamming(index) => index.entries.len(),
            Self::Minutiae(index) => index.entries.len(),
        }
    }

    fn memory_bytes(&self) -> u64 {
        match self {
            Self::Feature(index) => {
                index
                    .hnsw
                    .as_ref()
                    .map_or(std::mem::size_of::<FeatureIndex>() as u64, |hnsw| {
                        hnsw.memory_bytes()
                            .saturating_add(index.column.capacity() as u64)
                    })
            }
            Self::Hamming(index) => hamming_memory_bytes(index),
            Self::Minutiae(index) => minutiae_memory_bytes(index),
        }
    }

    fn insert(&mut self, entity_id: EntityId, value: &Value) -> Result<(), String> {
        match (self, value) {
            (Self::Feature(index), Value::FeatureVector(value)) => index.insert(entity_id, value),
            (Self::Hamming(index), Value::BitSignature(value)) => index.insert(entity_id, value),
            (Self::Minutiae(index), Value::Fingerprint(value)) => index.insert(entity_id, value),
            (Self::Feature(_), _) => {
                Err("HNSW index accepts only FEATURE_VECTOR values".to_string())
            }
            (Self::Hamming(_), _) => {
                Err("HAMMING index accepts only BIT_SIGNATURE values".to_string())
            }
            (Self::Minutiae(_), _) => {
                Err("MINUTIAE index accepts only FINGERPRINT minutiae templates".to_string())
            }
        }
    }

    fn delete(&mut self, entity_id: EntityId) {
        match self {
            Self::Feature(index) => {
                if let Some(hnsw) = &mut index.hnsw {
                    hnsw.delete(entity_id.raw());
                }
            }
            Self::Hamming(index) => index.delete(entity_id),
            Self::Minutiae(index) => index.delete(entity_id),
        }
    }

    fn candidates(
        &self,
        query: &Value,
        limit: usize,
        minimum_score: f64,
    ) -> Result<HashSet<u64>, String> {
        match (self, query) {
            (Self::Feature(index), Value::FeatureVector(query)) => index.candidates(query, limit),
            (Self::Hamming(index), Value::BitSignature(query)) => {
                index.candidates(query, minimum_score)
            }
            (Self::Minutiae(index), Value::Fingerprint(query)) => {
                index.candidates(query, minimum_score)
            }
            (Self::Feature(_), _) => Err("FEATURE search requires FEATURE_VECTOR".to_string()),
            (Self::Hamming(_), _) => Err("BITS search requires BIT_SIGNATURE".to_string()),
            (Self::Minutiae(_), _) => Err("FINGERPRINT search requires FINGERPRINT".to_string()),
        }
    }
}

impl FeatureIndex {
    fn validate_insert_batch(&self, values: &[&Value]) -> Result<(), String> {
        let mut expected = self.contract;
        for value in values {
            let Value::FeatureVector(value) = value else {
                return Err("HNSW index accepts only FEATURE_VECTOR values".to_string());
            };
            let actual = FeatureContract {
                space_id: value.space_id(),
                dimensions: value.dimensions(),
                version: FEATURE_INDEX_VERSION,
            };
            if expected.is_some_and(|contract| contract != actual) {
                return Err(
                    "FEATURE_VECTOR is incompatible with the HNSW index contract".to_string(),
                );
            }
            expected.get_or_insert(actual);
        }
        Ok(())
    }

    fn insert(&mut self, entity_id: EntityId, value: &FeatureVector) -> Result<(), String> {
        let actual = FeatureContract {
            space_id: value.space_id(),
            dimensions: value.dimensions(),
            version: FEATURE_INDEX_VERSION,
        };
        if self.contract.is_some_and(|expected| expected != actual) {
            return Err("FEATURE_VECTOR is incompatible with the HNSW index contract".to_string());
        }
        if self.contract.is_none() {
            self.contract = Some(actual);
            let config = HnswConfig::default().with_metric(DistanceMetric::Cosine);
            self.hnsw = Some(HnswIndex::new(value.dimensions(), config));
        }
        if let Some(hnsw) = &mut self.hnsw {
            hnsw.delete(entity_id.raw());
            hnsw.insert_with_id(entity_id.raw(), value.values().to_vec());
        }
        Ok(())
    }

    fn candidates(&self, query: &FeatureVector, limit: usize) -> Result<HashSet<u64>, String> {
        let actual = FeatureContract {
            space_id: query.space_id(),
            dimensions: query.dimensions(),
            version: FEATURE_INDEX_VERSION,
        };
        if self.contract.is_some_and(|expected| expected != actual) {
            return Err("FEATURE_VECTOR is incompatible with the HNSW index contract".to_string());
        }
        let Some(hnsw) = &self.hnsw else {
            return Ok(HashSet::new());
        };
        let requested = limit
            .saturating_mul(FEATURE_OVERSAMPLE)
            .max(MIN_FEATURE_CANDIDATES)
            .min(hnsw.len());
        Ok(hnsw
            .search_with_ef(query.values(), requested, requested.max(64))
            .into_iter()
            .map(|result| result.id)
            .collect())
    }
}

impl HammingIndex {
    fn validate_insert_batch(&self, values: &[&Value]) -> Result<(), String> {
        let mut expected = self.contract;
        for value in values {
            let Value::BitSignature(value) = value else {
                return Err("HAMMING index accepts only BIT_SIGNATURE values".to_string());
            };
            let actual = BitContract {
                space_id: value.space_id(),
                bytes: value.len_bytes(),
                version: BIT_INDEX_VERSION,
            };
            if expected.is_some_and(|contract| contract != actual) {
                return Err(
                    "BIT_SIGNATURE is incompatible with the HAMMING index contract".to_string(),
                );
            }
            expected.get_or_insert(actual);
        }
        Ok(())
    }

    fn insert(&mut self, entity_id: EntityId, value: &BitSignature) -> Result<(), String> {
        let actual = BitContract {
            space_id: value.space_id(),
            bytes: value.len_bytes(),
            version: BIT_INDEX_VERSION,
        };
        if self.contract.is_some_and(|expected| expected != actual) {
            return Err(
                "BIT_SIGNATURE is incompatible with the HAMMING index contract".to_string(),
            );
        }
        self.delete(entity_id);
        self.contract.get_or_insert(actual);
        self.entries.insert(entity_id, value.clone());
        if value.len_bytes() <= MAX_BANDED_HAMMING_BYTES {
            for key in hamming_bands(value) {
                self.postings.entry(key).or_default().insert(entity_id);
            }
        }
        Ok(())
    }

    fn delete(&mut self, entity_id: EntityId) {
        let Some(value) = self.entries.remove(&entity_id) else {
            return;
        };
        if value.len_bytes() <= MAX_BANDED_HAMMING_BYTES {
            for key in hamming_bands(&value) {
                let empty = if let Some(postings) = self.postings.get_mut(&key) {
                    postings.remove(entity_id);
                    postings.is_empty()
                } else {
                    false
                };
                if empty {
                    self.postings.remove(&key);
                }
            }
        }
    }

    fn candidates(&self, query: &BitSignature, minimum_score: f64) -> Result<HashSet<u64>, String> {
        let actual = BitContract {
            space_id: query.space_id(),
            bytes: query.len_bytes(),
            version: BIT_INDEX_VERSION,
        };
        if self.contract.is_some_and(|expected| expected != actual) {
            return Err(
                "BIT_SIGNATURE is incompatible with the HAMMING index contract".to_string(),
            );
        }
        if self.entries.values().any(|stored| {
            !stored
                .mask()
                .iter()
                .zip(query.mask())
                .any(|(stored, query)| stored & query != 0)
        }) {
            return Err(
                "BIT_SIGNATURE search found a stored value with no common valid bits".to_string(),
            );
        }
        if minimum_score <= HAMMING_BAND_MIN_SCORE || query.len_bytes() > MAX_BANDED_HAMMING_BYTES {
            return Ok(self
                .entries
                .keys()
                .map(|entity_id| entity_id.raw())
                .collect());
        }
        let mut candidates = HashSet::new();
        for (band, query_mask, query_value) in hamming_bands(query) {
            for stored_mask in 1_u8..=0x0f {
                let common_mask = query_mask & stored_mask;
                if common_mask == 0 {
                    continue;
                }
                for stored_value in 0_u8..=0x0f {
                    if stored_value & !stored_mask != 0
                        || (stored_value ^ query_value) & common_mask != 0
                    {
                        continue;
                    }
                    if let Some(postings) = self.postings.get(&(band, stored_mask, stored_value)) {
                        candidates.extend(postings.iter().map(|entity_id| entity_id.raw()));
                    }
                }
            }
        }
        Ok(candidates)
    }
}

impl MinutiaeIndex {
    fn descriptors_for_insert(&self, value: &BiometricSample) -> Result<Vec<u64>, String> {
        if self.version != FINGERPRINT_MATCHER_VERSION {
            return Err("MINUTIAE index is stale and must be rebuilt".to_string());
        }
        let descriptors = fingerprint_descriptor_bands(value).map_err(|error| error.to_string())?;
        if descriptors.is_empty() {
            return Err("MINUTIAE index requires at least one decoded minutia".to_string());
        }
        Ok(descriptors)
    }

    fn insert(&mut self, entity_id: EntityId, value: &BiometricSample) -> Result<(), String> {
        let descriptors = self.descriptors_for_insert(value)?;
        self.delete(entity_id);
        self.entries.insert(entity_id, value.clone());
        for descriptor in descriptors {
            self.postings
                .entry(descriptor)
                .or_default()
                .insert(entity_id);
        }
        Ok(())
    }

    fn delete(&mut self, entity_id: EntityId) {
        let Some(value) = self.entries.remove(&entity_id) else {
            return;
        };
        if let Ok(descriptors) = fingerprint_descriptor_bands(&value) {
            for descriptor in descriptors {
                if let Some(postings) = self.postings.get_mut(&descriptor) {
                    postings.remove(&entity_id);
                }
            }
        }
    }

    fn candidates(
        &self,
        query: &BiometricSample,
        minimum_score: f64,
    ) -> Result<HashSet<u64>, String> {
        if self.version != FINGERPRINT_MATCHER_VERSION {
            return Err("MINUTIAE index is stale and must be rebuilt".to_string());
        }
        if minimum_score == 0.0 {
            return Ok(self
                .entries
                .keys()
                .map(|entity_id| entity_id.raw())
                .collect());
        }
        let query_positions = minutiae_positions(query)?;
        for stored in self.entries.values() {
            let stored_positions = minutiae_positions(stored)?;
            if !query_positions.iter().any(|query_position| {
                stored_positions.iter().any(|stored_position| {
                    *query_position == 0
                        || *stored_position == 0
                        || query_position == stored_position
                })
            }) {
                return Err(
                    "fingerprint templates contain incompatible finger positions".to_string(),
                );
            }
        }
        let descriptors = fingerprint_descriptor_bands(query).map_err(|error| error.to_string())?;
        let mut candidates = HashSet::new();
        for descriptor in descriptors {
            if let Some(postings) = self.postings.get(&descriptor) {
                candidates.extend(postings.iter().map(|entity_id| entity_id.raw()));
            }
        }
        Ok(candidates)
    }
}

fn minutiae_positions(sample: &BiometricSample) -> Result<Vec<u8>, String> {
    sample
        .minutiae_views()
        .map(|views| views.into_iter().map(|view| view.position).collect())
        .map_err(|error| error.to_string())
}

fn hamming_bands(signature: &BitSignature) -> impl Iterator<Item = (usize, u8, u8)> + '_ {
    signature
        .bits()
        .iter()
        .copied()
        .zip(signature.mask().iter().copied())
        .enumerate()
        .flat_map(|(byte_index, (bits, mask))| {
            [0, HAMMING_BAND_BITS].into_iter().filter_map(move |shift| {
                let band_mask = (mask >> shift) & 0x0f;
                (band_mask != 0).then_some((
                    byte_index * 2 + shift / HAMMING_BAND_BITS,
                    band_mask,
                    (bits >> shift) & band_mask,
                ))
            })
        })
}

fn hamming_memory_bytes(index: &HammingIndex) -> u64 {
    let mut bytes = (std::mem::size_of::<HammingIndex>() + index.column.capacity()) as u64;
    bytes = bytes.saturating_add(
        index
            .entries
            .capacity()
            .saturating_mul(std::mem::size_of::<(EntityId, BitSignature)>()) as u64,
    );
    for entry in index.entries.values() {
        bytes = bytes.saturating_add((entry.bits().len() + entry.mask().len()) as u64);
    }
    bytes = bytes.saturating_add(
        index
            .postings
            .capacity()
            .saturating_mul(std::mem::size_of::<((usize, u8, u8), PostingPages)>()) as u64,
    );
    for postings in index.postings.values() {
        bytes = bytes.saturating_add(
            postings
                .pages
                .capacity()
                .saturating_mul(std::mem::size_of::<Vec<EntityId>>()) as u64,
        );
        for page in &postings.pages {
            bytes = bytes.saturating_add(
                page.capacity()
                    .saturating_mul(std::mem::size_of::<EntityId>()) as u64,
            );
        }
    }
    bytes
}

fn minutiae_memory_bytes(index: &MinutiaeIndex) -> u64 {
    let mut bytes = (std::mem::size_of::<MinutiaeIndex>() + index.column.capacity()) as u64;
    bytes = bytes.saturating_add(
        index
            .entries
            .capacity()
            .saturating_mul(std::mem::size_of::<(EntityId, BiometricSample)>()) as u64,
    );
    for entry in index.entries.values() {
        bytes = bytes.saturating_add(entry.bytes().len() as u64);
    }
    bytes.saturating_add(postings_memory_bytes(&index.postings))
}

fn postings_memory_bytes<K>(postings: &HashMap<K, HashSet<EntityId>>) -> u64 {
    let mut bytes = postings
        .capacity()
        .saturating_mul(std::mem::size_of::<(K, HashSet<EntityId>)>()) as u64;
    for entities in postings.values() {
        bytes = bytes.saturating_add(
            entities
                .capacity()
                .saturating_mul(std::mem::size_of::<EntityId>()) as u64,
        );
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;
    use reddb_types::BiometricFormat;
    use std::time::Instant;

    const SPACE: [u8; 16] = [9; 16];

    #[test]
    fn hamming_candidates_keep_every_positive_score() {
        let mut index = HammingIndex {
            column: "code".to_string(),
            contract: None,
            entries: HashMap::new(),
            postings: HashMap::new(),
        };
        let stored = BitSignature::new(SPACE, vec![0b1010], None).expect("valid signature");
        index
            .insert(EntityId::new(7), &stored)
            .expect("compatible insert");
        let query = BitSignature::new(SPACE, vec![0b1000], None).expect("valid signature");
        let candidates = index.candidates(&query, 0.1).expect("compatible query");
        assert!(candidates.contains(&7));
    }

    #[test]
    fn hamming_bands_keep_every_candidate_above_the_index_threshold() {
        let mut index = HammingIndex {
            column: "code".to_string(),
            contract: None,
            entries: HashMap::new(),
            postings: HashMap::new(),
        };
        let stored = BitSignature::new(SPACE, vec![0b1010_1010], None).expect("valid signature");
        index
            .insert(EntityId::new(8), &stored)
            .expect("compatible insert");
        let query = BitSignature::new(SPACE, vec![0b1010_1011], None).expect("valid signature");
        assert_eq!(
            stored.compare(&query).expect("comparable signatures"),
            0.875
        );
        assert!(index
            .candidates(&query, 0.8)
            .expect("compatible query")
            .contains(&8));
    }

    #[test]
    fn hamming_postings_page_and_remove_entities() {
        let mut index = HammingIndex {
            column: "code".to_string(),
            contract: None,
            entries: HashMap::new(),
            postings: HashMap::new(),
        };
        let signature = BitSignature::new(SPACE, vec![0xaa], None).expect("valid signature");
        for id in 1..=HAMMING_POSTING_PAGE_ENTRIES as u64 + 1 {
            index
                .insert(EntityId::new(id), &signature)
                .expect("compatible insert");
        }
        assert!(index
            .postings
            .values()
            .all(|postings| postings.pages.len() == 2));
        index.delete(EntityId::new(1));
        assert!(!index
            .candidates(&signature, 0.8)
            .expect("compatible query")
            .contains(&1));
    }

    #[test]
    fn hamming_candidates_reject_disjoint_masks() {
        let mut index = HammingIndex {
            column: "code".to_string(),
            contract: None,
            entries: HashMap::new(),
            postings: HashMap::new(),
        };
        let stored =
            BitSignature::new(SPACE, vec![0], Some(vec![0b0001])).expect("valid signature");
        index
            .insert(EntityId::new(7), &stored)
            .expect("compatible insert");
        let query = BitSignature::new(SPACE, vec![0], Some(vec![0b0010])).expect("valid signature");
        assert!(index.candidates(&query, 0.0).is_err());
    }

    #[test]
    fn feature_index_enforces_contract_and_removes_deleted_ids() {
        let mut index = FeatureIndex {
            column: "face".to_string(),
            contract: None,
            hnsw: None,
        };
        let stored = FeatureVector::new(SPACE, vec![1.0, 0.0]).expect("valid feature");
        index
            .insert(EntityId::new(11), &stored)
            .expect("first feature fixes the contract");
        assert!(index.candidates(&stored, 10).expect("query").contains(&11));
        let incompatible = FeatureVector::new([8; 16], vec![1.0, 0.0]).expect("valid feature");
        assert!(index.insert(EntityId::new(12), &incompatible).is_err());
        index.hnsw.as_mut().expect("HNSW").delete(11);
        assert!(!index.candidates(&stored, 10).expect("query").contains(&11));
    }

    #[test]
    fn feature_preflight_rejects_mixed_contract_without_mutating_index() {
        let index = FeatureIndex {
            column: "face".to_string(),
            contract: None,
            hnsw: None,
        };
        let first =
            Value::FeatureVector(FeatureVector::new(SPACE, vec![1.0, 0.0]).expect("valid feature"));
        let second = Value::FeatureVector(
            FeatureVector::new([8; 16], vec![1.0, 0.0]).expect("other feature space"),
        );
        assert!(index.validate_insert_batch(&[&first, &second]).is_err());
        assert!(index.contract.is_none());
        assert!(index.hnsw.is_none());
    }

    #[test]
    fn minutiae_zero_threshold_keeps_all_indexed_templates() {
        let sample = three_minutiae();
        let mut index = MinutiaeIndex {
            column: "print".to_string(),
            version: FINGERPRINT_MATCHER_VERSION,
            entries: HashMap::new(),
            postings: HashMap::new(),
        };
        index
            .insert(EntityId::new(21), &sample)
            .expect("indexed minutiae");
        assert_eq!(index.candidates(&sample, 0.0), Ok(HashSet::from([21])));
        index.delete(EntityId::new(21));
        assert_eq!(index.candidates(&sample, 0.0), Ok(HashSet::new()));
    }

    #[test]
    fn minutiae_candidates_keep_single_minutia_and_reject_incompatible_positions() {
        let stored = one_minutia(1);
        let mut index = MinutiaeIndex {
            column: "print".to_string(),
            version: FINGERPRINT_MATCHER_VERSION,
            entries: HashMap::new(),
            postings: HashMap::new(),
        };
        index
            .insert(EntityId::new(22), &stored)
            .expect("single-minutia template is indexable");
        assert_eq!(index.candidates(&stored, 0.1), Ok(HashSet::from([22])));
        assert!(index.candidates(&one_minutia(2), 0.1).is_err());
    }

    #[test]
    fn resident_memory_estimate_includes_candidate_postings() {
        let mut hamming = SimilarityIndex::Hamming(HammingIndex {
            column: "code".to_string(),
            contract: None,
            entries: HashMap::new(),
            postings: HashMap::new(),
        });
        let before = hamming.memory_bytes();
        hamming
            .insert(
                EntityId::new(1),
                &Value::BitSignature(
                    BitSignature::new(SPACE, vec![0xaa; 32], None).expect("valid signature"),
                ),
            )
            .expect("indexed signature");
        let after = hamming.memory_bytes();
        assert!(after > before + 64, "postings must be part of admission");

        let sample = three_minutiae();
        let mut minutiae = SimilarityIndex::Minutiae(MinutiaeIndex {
            column: "print".to_string(),
            version: FINGERPRINT_MATCHER_VERSION,
            entries: HashMap::new(),
            postings: HashMap::new(),
        });
        let before = minutiae.memory_bytes();
        minutiae
            .insert(EntityId::new(2), &Value::Fingerprint(sample))
            .expect("indexed minutiae");
        assert!(minutiae.memory_bytes() > before);
    }

    #[test]
    #[ignore = "performance qualification; run explicitly with --ignored --nocapture"]
    fn similarity_index_build_query_and_memory_benchmark() {
        const ENTRIES: u64 = 512;

        let mut feature = SimilarityIndex::Feature(FeatureIndex {
            column: "face".to_string(),
            contract: None,
            hnsw: None,
        });
        let feature_started = Instant::now();
        for id in 1..=ENTRIES {
            let mut coordinates = vec![0.25_f32; 32];
            coordinates[0] = id as f32 / ENTRIES as f32;
            feature
                .insert(
                    EntityId::new(id),
                    &Value::FeatureVector(
                        FeatureVector::new(SPACE, coordinates).expect("valid feature"),
                    ),
                )
                .expect("feature insert");
        }
        let feature_build = feature_started.elapsed();
        let feature_query = FeatureVector::new(SPACE, vec![0.25; 32]).expect("valid query");
        let query_started = Instant::now();
        let feature_candidates = feature
            .candidates(&Value::FeatureVector(feature_query), 10, 0.8)
            .expect("feature candidates");
        let feature_query_elapsed = query_started.elapsed();
        assert!(!feature_candidates.is_empty());
        println!(
            "kind=feature entries={ENTRIES} build_us={} query_us={} resident_bytes={} candidates={}",
            feature_build.as_micros(),
            feature_query_elapsed.as_micros(),
            feature.memory_bytes(),
            feature_candidates.len()
        );

        let mut hamming = SimilarityIndex::Hamming(HammingIndex {
            column: "iris".to_string(),
            contract: None,
            entries: HashMap::new(),
            postings: HashMap::new(),
        });
        let hamming_started = Instant::now();
        for id in 1..=ENTRIES {
            let mut bits = vec![id as u8; 32];
            bits[0] = (id & 0xff) as u8;
            hamming
                .insert(
                    EntityId::new(id),
                    &Value::BitSignature(
                        BitSignature::new(SPACE, bits, None).expect("valid signature"),
                    ),
                )
                .expect("hamming insert");
        }
        let hamming_build = hamming_started.elapsed();
        let hamming_query = BitSignature::new(SPACE, vec![0x55; 32], None).expect("valid query");
        let query_started = Instant::now();
        let hamming_candidates = hamming
            .candidates(&Value::BitSignature(hamming_query), 10, 0.8)
            .expect("hamming candidates");
        let hamming_query_elapsed = query_started.elapsed();
        assert!(!hamming_candidates.is_empty());
        println!(
            "kind=hamming entries={ENTRIES} build_us={} query_us={} resident_bytes={} candidates={}",
            hamming_build.as_micros(),
            hamming_query_elapsed.as_micros(),
            hamming.memory_bytes(),
            hamming_candidates.len()
        );

        let sample = three_minutiae();
        let mut minutiae = SimilarityIndex::Minutiae(MinutiaeIndex {
            column: "print".to_string(),
            version: FINGERPRINT_MATCHER_VERSION,
            entries: HashMap::new(),
            postings: HashMap::new(),
        });
        let minutiae_started = Instant::now();
        for id in 1..=ENTRIES {
            minutiae
                .insert(EntityId::new(id), &Value::Fingerprint(sample.clone()))
                .expect("minutiae insert");
        }
        let minutiae_build = minutiae_started.elapsed();
        let query_started = Instant::now();
        let minutiae_candidates = minutiae
            .candidates(&Value::Fingerprint(sample), 10, 0.8)
            .expect("minutiae candidates");
        let minutiae_query_elapsed = query_started.elapsed();
        assert_eq!(minutiae_candidates.len(), ENTRIES as usize);
        println!(
            "kind=minutiae entries={ENTRIES} build_us={} query_us={} resident_bytes={} candidates={}",
            minutiae_build.as_micros(),
            minutiae_query_elapsed.as_micros(),
            minutiae.memory_bytes(),
            minutiae_candidates.len()
        );
    }

    fn three_minutiae() -> BiometricSample {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"FMR\0 20\0");
        bytes.extend_from_slice(&48_u32.to_be_bytes());
        bytes.extend_from_slice(&0_u16.to_be_bytes());
        bytes.extend_from_slice(&500_u16.to_be_bytes());
        bytes.extend_from_slice(&500_u16.to_be_bytes());
        bytes.extend_from_slice(&197_u16.to_be_bytes());
        bytes.extend_from_slice(&197_u16.to_be_bytes());
        bytes.extend_from_slice(&[1, 0]);
        bytes.extend_from_slice(&[1, 0, 100, 3]);
        for (x, y, angle) in [(100_u16, 120_u16, 10_u8), (180, 160, 40), (130, 230, 90)] {
            bytes.extend_from_slice(&(x | 0x4000).to_be_bytes());
            bytes.extend_from_slice(&y.to_be_bytes());
            bytes.extend_from_slice(&[angle, 90]);
        }
        bytes.extend_from_slice(&[0, 0]);
        BiometricSample::with_format(BiometricFormat::FingerprintIso19794_2, bytes)
            .expect("standard minutiae")
    }

    fn one_minutia(position: u8) -> BiometricSample {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"FMR\0 20\0");
        bytes.extend_from_slice(&36_u32.to_be_bytes());
        bytes.extend_from_slice(&0_u16.to_be_bytes());
        bytes.extend_from_slice(&500_u16.to_be_bytes());
        bytes.extend_from_slice(&500_u16.to_be_bytes());
        bytes.extend_from_slice(&197_u16.to_be_bytes());
        bytes.extend_from_slice(&197_u16.to_be_bytes());
        bytes.extend_from_slice(&[1, 0]);
        bytes.extend_from_slice(&[position, 0, 100, 1]);
        bytes.extend_from_slice(&[0x40, 1, 0, 2, 3, 90]);
        bytes.extend_from_slice(&[0, 0]);
        BiometricSample::with_format(BiometricFormat::FingerprintIso19794_2, bytes)
            .expect("single standard minutia")
    }
}
