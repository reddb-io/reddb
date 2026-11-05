//! Canonical storage errors shared by native and browser hosts.
use super::entity::EntityId;
use super::segment::SegmentError;
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageIntegrityError {
    pub zone: String,
    pub id: String,
    pub collection: Option<String>,
    pub detail: String,
}

impl StorageIntegrityError {
    pub fn new(
        zone: impl Into<String>,
        id: impl Into<String>,
        collection: Option<String>,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            zone: zone.into(),
            id: id.into(),
            collection,
            detail: detail.into(),
        }
    }
}

impl fmt::Display for StorageIntegrityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "storage integrity failure: zone={} id={}",
            self.zone, self.id
        )?;
        if let Some(collection) = &self.collection {
            write!(f, " collection={collection}")?;
        }
        write!(
            f,
            ": {}. RedDB refused to serve unverified bytes; run the `SCRUB` statement to classify the fault, or `red salvage` to recover trusted rows.",
            self.detail
        )
    }
}

/// Errors from UnifiedStore operations
#[derive(Debug)]
pub enum StoreError {
    /// Collection already exists
    CollectionExists(String),
    /// Collection not found
    CollectionNotFound(String),
    /// Entity not found
    EntityNotFound(EntityId),
    /// Too many cross-references
    TooManyRefs(EntityId),
    /// Segment error
    Segment(SegmentError),
    /// I/O error
    Io(std::io::Error),
    /// Checksummed storage bytes failed validation.
    StorageIntegrity(StorageIntegrityError),
    /// Serialization error
    Serialization(String),
    /// A read must be rejected before allocating beyond its allowance.
    ReadLimitExceeded {
        required_bytes: usize,
        limit_bytes: usize,
    },
    /// Internal error (lock poisoning, invariant violation)
    Internal(String),
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CollectionExists(name) => write!(f, "Collection already exists: {}", name),
            Self::CollectionNotFound(name) => write!(f, "Collection not found: {}", name),
            Self::EntityNotFound(id) => write!(f, "Entity not found: {}", id),
            Self::TooManyRefs(id) => write!(f, "Too many cross-references for entity: {}", id),
            Self::Segment(e) => write!(f, "Segment error: {:?}", e),
            Self::Io(e) => write!(f, "I/O error: {}", e),
            Self::StorageIntegrity(e) => write!(f, "{e}"),
            Self::Serialization(msg) => write!(f, "Serialization error: {}", msg),
            Self::ReadLimitExceeded {
                required_bytes,
                limit_bytes,
            } => write!(
                f,
                "read needs {required_bytes} bytes over read memory limit {limit_bytes}"
            ),
            Self::Internal(msg) => write!(f, "Internal error: {}", msg),
        }
    }
}

impl std::error::Error for StoreError {}

impl From<SegmentError> for StoreError {
    fn from(e: SegmentError) -> Self {
        Self::Segment(e)
    }
}

impl From<std::io::Error> for StoreError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}
