//! Independent authority for raw DWB frames. The owner serializes this module
//! with ordinary superblock publication, including cache eviction writes.
use super::*;
use reddb_file::file_format::{self as f, DwbPhase, DwbPublication};

pub(super) enum Recovery {
    Unchanged,
    Apply(DwbPublication),
    Discard(DwbPublication),
}

fn invalid(detail: &str) -> PagerIoError {
    PagerIoError::InvalidDatabase(format!("DWB generation authority: {detail}"))
}

fn selected_slot<F: VfsFile>(
    file: &mut F,
) -> Result<
    Option<(
        reddb_file::PagedSuperblockSelection,
        [u8; PAGED_SUPERBLOCK_SLOT_SIZE],
    )>,
    PagerIoError,
> {
    let zone = read_superblock_zone(file)?;
    let Some(selected) = reddb_file::select_paged_superblock(&zone) else {
        // Raw-I/O bootstrap fixtures precede creation of the superblock pair.
        // Pager hosts validate the pair before entering ordinary page I/O.
        return Ok(None);
    };
    let start = selected.copy_index * PAGED_SUPERBLOCK_SLOT_SIZE;
    let mut image = [0; PAGED_SUPERBLOCK_SLOT_SIZE];
    image.copy_from_slice(&zone[start..start + PAGED_SUPERBLOCK_SLOT_SIZE]);
    let header = f::decode_database_header(&image).map_err(|error| invalid(&error.to_string()))?;
    Ok(
        (header.required_capabilities & f::FILE_CAPABILITY_DWB_PUBLICATION != 0)
            .then_some((selected, image)),
    )
}

pub(super) fn enabled<F: VfsFile>(file: &mut F) -> Result<bool, PagerIoError> {
    Ok(selected_slot(file)?.is_some())
}

pub(super) fn publish<F: VfsFile>(
    file: &mut F,
    publication: DwbPublication,
) -> Result<(), PagerIoError> {
    let (mut selected, mut image) = selected_slot(file)?
        .ok_or_else(|| invalid("publication requires a fenced reader capability"))?;
    f::set_database_header_dwb_publication(&mut image, Some(publication))
        .map_err(|error| invalid(&error.to_string()))?;
    for _ in 0..reddb_file::PAGED_SUPERBLOCK_SLOT_COUNT {
        let next = reddb_file::paged_superblock_next_copy(Some(selected));
        let generation = selected
            .generation
            .checked_add(1)
            .ok_or_else(|| invalid("superblock generation exhausted"))?;
        write_superblock_slot(file, &mut image, next, generation)?;
        selected = reddb_file::PagedSuperblockSelection {
            copy_index: next,
            generation,
        };
    }
    Ok(())
}

pub(super) fn publication(buffer: &[u8]) -> DwbPublication {
    DwbPublication {
        phase: DwbPhase::Preparing,
        frame_bytes: u32::try_from(buffer.len()).expect("bounded DWB frame length"),
        digest: f::dwb_frame_digest(buffer),
    }
}

pub(super) fn plan<F: VfsFile>(
    file: &mut F,
    buffer: &[u8],
) -> Result<Option<Recovery>, PagerIoError> {
    let Some((_, image)) = selected_slot(file)? else {
        return Ok(None);
    };
    let authority =
        f::database_header_dwb_publication(&image).map_err(|error| invalid(&error.to_string()))?;
    if dwb_zone_is_clear(buffer) {
        if authority.is_some_and(|value| value.phase == DwbPhase::Ready) {
            return Err(invalid("required recovery frame is missing"));
        }
        return Ok(Some(Recovery::Unchanged));
    }
    let authority = authority.ok_or_else(|| invalid("unpublished recovery frame"))?;
    let matches = buffer
        .get(..authority.frame_bytes as usize)
        .is_some_and(|bytes| f::dwb_frame_digest(bytes) == authority.digest);
    let decoded = reddb_file::decode_paged_dwb_frame(buffer);
    if !matches {
        if authority.phase != DwbPhase::Ready && decoded.is_err() {
            // Preparing cannot have touched homes; Consumed proves the homes
            // crossed their barrier before any clearing could begin.
            return Ok(Some(Recovery::Discard(authority)));
        }
        return Err(invalid(
            "frame differs from its independently published digest",
        ));
    }
    let entries = decoded.map_err(|error| invalid(&error.to_string()))?;
    let mut seen = std::collections::HashSet::new();
    if entries.is_empty()
        || entries.iter().any(|entry| {
            entry.page_id == 0
                || (DWB_ZONE_START_PAGE..FIRST_ALLOCATABLE_PAGE).contains(&entry.page_id)
                || !seen.insert(entry.page_id)
        })
    {
        return Err(invalid("invalid recovery page ownership"));
    }
    Ok(Some(if authority.phase == DwbPhase::Consumed {
        Recovery::Discard(authority)
    } else {
        Recovery::Apply(authority)
    }))
}

pub(super) fn consume<F: VfsFile>(
    file: &mut F,
    mut publication: DwbPublication,
) -> Result<(), PagerIoError> {
    publication.phase = DwbPhase::Consumed;
    publish(file, publication)?;
    clear_dwb_zone(file)
}
