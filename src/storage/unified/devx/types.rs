//! DevX Result Types
//!
//! Types for similarity search and linked entity results.

use super::super::{EntityId, RefType, UnifiedEntity};

/// Result of a similarity search
#[derive(Debug, Clone)]
pub struct SimilarResult {
    pub entity_id: EntityId,
    pub score: f32,
    pub entity: UnifiedEntity,
}

/// Entity with link information
#[derive(Debug, Clone)]
pub struct LinkedEntity {
    pub entity: UnifiedEntity,
    pub ref_type: RefType,
    pub collection: String,
}
