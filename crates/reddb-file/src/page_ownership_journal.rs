//! Checksummed records for the page-ownership recovery journal.
//!
//! A record describes an intent; a caller must make it durable before acting
//! on that intent. Catalog publication and allocator durability are separate
//! facts and must be checked against their own persisted generations.

use std::io;

const MAGIC: &[u8; 4] = b"RDOJ";
const VERSION: u8 = 1;
/// Fixed-size records permit a recovery reader to identify a partial tail.
pub const RECORD_SIZE: usize = 32;

/// One page-ownership operation in a single nonzero journal epoch.
/// Page IDs must also be checked against the pager's allocatable range and
/// file bounds before recovery acts on them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageOwnershipRecord {
    /// Start a new ownership epoch before any page is reserved for it.
    Begin { epoch: u64 },
    /// Reserve a page before removing it from the allocator or writing it.
    Reserve { epoch: u64, page_id: u32 },
    /// Retire a page after its previous owner has been identified.
    Retire { epoch: u64, page_id: u32 },
    /// A free page detached from a persisted trunk, before changing its head.
    DetachFree { epoch: u64, page_id: u32 },
    /// Mark a journal epoch complete after all allocator and catalog fences.
    Finish { epoch: u64 },
}

impl PageOwnershipRecord {
    pub fn epoch(self) -> u64 {
        self.fields().1
    }

    fn fields(self) -> (u8, u64, u32) {
        match self {
            Self::Begin { epoch } => (1, epoch, 0),
            Self::Reserve { epoch, page_id } => (2, epoch, page_id),
            Self::Retire { epoch, page_id } => (3, epoch, page_id),
            Self::Finish { epoch } => (4, epoch, 0),
            Self::DetachFree { epoch, page_id } => (5, epoch, page_id),
        }
    }
}

/// Encode one complete record. Does not write or synchronize any file.
pub fn encode(record: PageOwnershipRecord) -> io::Result<[u8; RECORD_SIZE]> {
    let (kind, epoch, page_id) = record.fields();
    if epoch == 0 || ((kind == 2 || kind == 3 || kind == 5) && page_id == 0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid ownership record",
        ));
    }
    let mut bytes = [0; RECORD_SIZE];
    bytes[..4].copy_from_slice(MAGIC);
    bytes[4] = VERSION;
    bytes[5] = kind;
    bytes[8..16].copy_from_slice(&epoch.to_le_bytes());
    bytes[16..20].copy_from_slice(&page_id.to_le_bytes());
    let checksum = crc32fast::hash(&bytes[..28]);
    bytes[28..32].copy_from_slice(&checksum.to_le_bytes());
    Ok(bytes)
}

/// Decode exactly one record, rejecting truncated or unrecognized data.
/// A caller must not discard a corrupt tail as though its intent never existed.
pub fn decode(bytes: &[u8]) -> io::Result<PageOwnershipRecord> {
    if bytes.len() != RECORD_SIZE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "incomplete ownership record",
        ));
    }
    let invalid = || io::Error::new(io::ErrorKind::InvalidData, "invalid ownership record");
    if &bytes[..4] != MAGIC
        || bytes[4] != VERSION
        || bytes[6..8] != [0; 2]
        || bytes[20..28] != [0; 8]
    {
        return Err(invalid());
    }
    let expected = u32::from_le_bytes(bytes[28..32].try_into().map_err(|_| invalid())?);
    if crc32fast::hash(&bytes[..28]) != expected {
        return Err(invalid());
    }
    let epoch = u64::from_le_bytes(bytes[8..16].try_into().map_err(|_| invalid())?);
    let page_id = u32::from_le_bytes(bytes[16..20].try_into().map_err(|_| invalid())?);
    if epoch == 0 {
        return Err(invalid());
    }
    match (bytes[5], page_id) {
        (1, 0) => Ok(PageOwnershipRecord::Begin { epoch }),
        (2, 1..) => Ok(PageOwnershipRecord::Reserve { epoch, page_id }),
        (3, 1..) => Ok(PageOwnershipRecord::Retire { epoch, page_id }),
        (4, 0) => Ok(PageOwnershipRecord::Finish { epoch }),
        (5, 1..) => Ok(PageOwnershipRecord::DetachFree { epoch, page_id }),
        _ => Err(invalid()),
    }
}

/// Intents belonging to an epoch that has no durable completion record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingOwnershipEpoch {
    pub epoch: u64,
    pub reserved_pages: std::collections::BTreeSet<u32>,
    pub retired_pages: std::collections::BTreeSet<u32>,
    pub detached_free_pages: std::collections::BTreeSet<u32>,
}

/// Page sets selected for allocator reconciliation after a crash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnershipRecoveryPlan {
    pub release: Vec<u32>,
    pub preserve: Vec<u32>,
}

/// Select pages using the epoch present in the durable catalog.
///
/// The caller must prove the journal epoch had exclusive ownership of new
/// allocations and that unpublished work did not modify a published tree.
/// A page in both reserve and retire sets is ambiguous after publication and
/// is rejected until a stronger reachability proof exists.
pub fn recovery_plan(
    pending: &PendingOwnershipEpoch,
    catalog_epoch: u64,
    first_allocatable_page: u32,
    page_count: u32,
) -> io::Result<OwnershipRecoveryPlan> {
    let invalid = || {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid ownership recovery state",
        )
    };
    if pending.epoch == 0 || catalog_epoch > pending.epoch || page_count < first_allocatable_page {
        return Err(invalid());
    }
    let published = catalog_epoch == pending.epoch;
    let mut release = std::collections::BTreeSet::new();
    let mut preserve = std::collections::BTreeSet::new();

    for &page in &pending.reserved_pages {
        if page < first_allocatable_page {
            return Err(invalid());
        }
        if page >= page_count {
            // A reservation can be durable before the new page is allocated.
            if published {
                return Err(invalid());
            }
            continue;
        }
        if published {
            if pending.retired_pages.contains(&page) {
                return Err(invalid());
            }
            preserve.insert(page);
        } else {
            release.insert(page);
        }
    }
    for &page in &pending.detached_free_pages {
        if page < first_allocatable_page || page >= page_count {
            return Err(invalid());
        }
        if !preserve.contains(&page) {
            release.insert(page);
        }
    }
    for &page in &pending.retired_pages {
        if page < first_allocatable_page || page >= page_count {
            return Err(invalid());
        }
        if published {
            release.insert(page);
        }
    }
    Ok(OwnershipRecoveryPlan {
        release: release.into_iter().collect(),
        preserve: preserve.into_iter().collect(),
    })
}

/// Validate a complete journal and return the unfinished epoch, if any.
///
/// A completion record can be written only after the caller has durably
/// fenced catalog publication and allocator state. This decoder cannot prove
/// those external facts. A partial or corrupt record is an error, including
/// at the tail: silently dropping it could lose a durable ownership intent.
/// Repeated records are deduplicated; a page must not be reused within an
/// unfinished epoch. A page present in both sets requires a catalog-based
/// ownership decision, not an unconditional free.
pub fn pending_epoch(bytes: &[u8]) -> io::Result<Option<PendingOwnershipEpoch>> {
    if !bytes.len().is_multiple_of(RECORD_SIZE) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "incomplete ownership journal",
        ));
    }
    let mut pending: Option<PendingOwnershipEpoch> = None;
    let mut last_epoch = 0;
    let invalid = || io::Error::new(io::ErrorKind::InvalidData, "invalid ownership sequence");
    for frame in bytes.chunks_exact(RECORD_SIZE) {
        match decode(frame)? {
            PageOwnershipRecord::Begin { epoch } => {
                if pending.is_some() || epoch <= last_epoch {
                    return Err(invalid());
                }
                pending = Some(PendingOwnershipEpoch {
                    epoch,
                    reserved_pages: Default::default(),
                    retired_pages: Default::default(),
                    detached_free_pages: Default::default(),
                });
            }
            PageOwnershipRecord::Reserve { epoch, page_id } => {
                let current = pending
                    .as_mut()
                    .filter(|value| value.epoch == epoch)
                    .ok_or_else(invalid)?;
                current.reserved_pages.insert(page_id);
            }
            PageOwnershipRecord::Retire { epoch, page_id } => {
                let current = pending
                    .as_mut()
                    .filter(|value| value.epoch == epoch)
                    .ok_or_else(invalid)?;
                current.retired_pages.insert(page_id);
            }
            PageOwnershipRecord::DetachFree { epoch, page_id } => {
                let current = pending
                    .as_mut()
                    .filter(|value| value.epoch == epoch)
                    .ok_or_else(invalid)?;
                current.detached_free_pages.insert(page_id);
            }
            PageOwnershipRecord::Finish { epoch } => {
                if pending.as_ref().is_none_or(|value| value.epoch != epoch) {
                    return Err(invalid());
                }
                pending = None;
                last_epoch = epoch;
            }
        }
    }
    Ok(pending)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_round_trip_and_reject_every_truncated_prefix() {
        for record in [
            PageOwnershipRecord::Begin { epoch: 7 },
            PageOwnershipRecord::Reserve {
                epoch: 7,
                page_id: 67,
            },
            PageOwnershipRecord::Retire {
                epoch: 7,
                page_id: u32::MAX,
            },
            PageOwnershipRecord::DetachFree {
                epoch: 7,
                page_id: 68,
            },
            PageOwnershipRecord::Finish { epoch: 7 },
        ] {
            let bytes = encode(record).expect("encode");
            assert_eq!(decode(&bytes).expect("decode"), record);
            for size in 0..RECORD_SIZE {
                assert!(decode(&bytes[..size]).is_err(), "truncated at {size}");
            }
            for index in 0..RECORD_SIZE {
                let mut damaged = bytes;
                damaged[index] ^= 1;
                assert!(decode(&damaged).is_err(), "corrupt byte {index}");
            }
        }
    }

    #[test]
    fn invalid_epochs_and_reserved_page_zero_are_rejected() {
        assert!(encode(PageOwnershipRecord::Begin { epoch: 0 }).is_err());
        assert!(encode(PageOwnershipRecord::Reserve {
            epoch: 1,
            page_id: 0
        })
        .is_err());
        assert!(encode(PageOwnershipRecord::Retire {
            epoch: 1,
            page_id: 0
        })
        .is_err());
        assert!(encode(PageOwnershipRecord::DetachFree {
            epoch: 1,
            page_id: 0
        })
        .is_err());
    }

    #[test]
    fn journal_recovers_unfinished_epoch_and_rejects_invalid_sequence() {
        let bytes = |records: &[PageOwnershipRecord]| {
            records
                .iter()
                .flat_map(|record| encode(*record).expect("encode"))
                .collect::<Vec<_>>()
        };
        let begin = PageOwnershipRecord::Begin { epoch: 7 };
        let reserve = PageOwnershipRecord::Reserve {
            epoch: 7,
            page_id: 67,
        };
        let retire = PageOwnershipRecord::Retire {
            epoch: 7,
            page_id: 68,
        };
        let finish = PageOwnershipRecord::Finish { epoch: 7 };
        let detach = PageOwnershipRecord::DetachFree {
            epoch: 7,
            page_id: 69,
        };
        let pending = pending_epoch(&bytes(&[begin, reserve, reserve, retire, detach]))
            .expect("pending")
            .expect("active epoch");
        assert_eq!(pending.epoch, 7);
        assert_eq!(pending.reserved_pages.into_iter().collect::<Vec<_>>(), [67]);
        assert_eq!(pending.retired_pages.into_iter().collect::<Vec<_>>(), [68]);
        assert_eq!(
            pending.detached_free_pages.into_iter().collect::<Vec<_>>(),
            [69]
        );
        assert_eq!(
            pending_epoch(&bytes(&[begin, reserve, finish])).expect("finished"),
            None
        );
        assert!(pending_epoch(&bytes(&[reserve])).is_err());
        assert!(pending_epoch(&bytes(&[begin, begin])).is_err());
        assert!(pending_epoch(&bytes(&[begin, PageOwnershipRecord::Finish { epoch: 8 }])).is_err());
        assert!(pending_epoch(&bytes(&[begin, finish, begin])).is_err());
        let mut partial = bytes(&[begin, reserve]);
        partial.pop();
        assert!(pending_epoch(&partial).is_err());
    }

    #[test]
    fn recovery_plan_uses_catalog_epoch_and_preserves_published_reservations() {
        let pending = PendingOwnershipEpoch {
            epoch: 7,
            reserved_pages: [70, 71, 120].into_iter().collect(),
            retired_pages: [80].into_iter().collect(),
            detached_free_pages: [70, 72].into_iter().collect(),
        };
        let before = recovery_plan(&pending, 6, 67, 100).expect("unpublished plan");
        assert_eq!(before.release, [70, 71, 72]);
        assert!(before.preserve.is_empty());
        assert!(
            recovery_plan(&pending, 7, 67, 100).is_err(),
            "published page is outside the file"
        );

        let mut published = pending.clone();
        published.reserved_pages.remove(&120);
        let after = recovery_plan(&published, 7, 67, 100).expect("published plan");
        assert_eq!(after.preserve, [70, 71]);
        assert_eq!(after.release, [72, 80]);
        published.retired_pages.insert(70);
        assert!(
            recovery_plan(&published, 7, 67, 100).is_err(),
            "ambiguous live page must not be freed"
        );
        assert!(
            recovery_plan(&pending, 8, 67, 100).is_err(),
            "catalog must not outrun the journal"
        );
    }
}
