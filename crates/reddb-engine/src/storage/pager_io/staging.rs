//! Bounded preparation and recovery of the existing RDDW frame.
use super::*;

/// One native DWB frame. Only page identities live in RAM; images occupy the
/// reserved on-disk zone until the complete frame is synchronized.
#[derive(Default)]
pub(crate) struct StagedPageBatch {
    pages: Vec<u32>,
}

fn entry_offset(index: usize) -> u64 {
    dwb_zone_offset()
        + reddb_file::PAGED_DWB_HEADER_SIZE as u64
        + index as u64 * reddb_file::PAGED_DWB_ENTRY_SIZE as u64
}

impl StagedPageBatch {
    pub(crate) fn stage<F: VfsFile>(
        &mut self,
        file: &mut F,
        page_id: u32,
        page: &Page,
    ) -> Result<(), PagerIoError> {
        if page_id == 0
            || (DWB_ZONE_START_PAGE..FIRST_ALLOCATABLE_PAGE).contains(&page_id)
            || page.page_id() != page_id
        {
            return Err(PagerIoError::InvalidDatabase(
                "prepared page aliases a reserved zone or has the wrong identity".into(),
            ));
        }
        let index = match self.pages.iter().position(|id| *id == page_id) {
            Some(index) => index,
            None => {
                if self.pages.len() == DWB_MAX_PAGES_PER_FRAME {
                    return Err(PagerIoError::InvalidDatabase(
                        "write batch exceeds native double-write frame capacity".into(),
                    ));
                }
                self.pages.push(page_id);
                self.pages.len() - 1
            }
        };
        file.seek(SeekFrom::Start(entry_offset(index)))?;
        file.write_all(&page_id.to_le_bytes())?;
        file.write_all(page.as_bytes())?;
        Ok(())
    }

    pub(crate) fn read<F: VfsFile>(
        &self,
        file: &mut F,
        page_id: u32,
    ) -> Result<Option<Page>, PagerIoError> {
        let Some(index) = self.pages.iter().position(|id| *id == page_id) else {
            return Ok(None);
        };
        read_entry(file, index).map(|(_, page)| Some(page))
    }

    pub(crate) fn publish<F: VfsFile>(self, file: &mut F) -> Result<(), PagerIoError> {
        if self.pages.is_empty() {
            return Ok(());
        }
        let last_page = self.pages.iter().copied().max().expect("nonempty batch");
        let required = (u64::from(last_page) + 1)
            .checked_mul(PAGE_SIZE as u64)
            .ok_or_else(|| PagerIoError::InvalidDatabase("prepared file length overflow".into()))?;
        let length = file.file_len()?;
        if required > length {
            let reservation = file.reserve(length, required - length);
            if let Err(cause) = reservation {
                if cause.kind() != io::ErrorKind::Unsupported {
                    return Err(cause.into());
                }
            }
            // Capacity admission and logical extension both precede the valid
            // header that makes recovery responsible for the destination.
            file.set_len(required)?;
        }
        let checksum = checksum(file, self.pages.len())?;
        let count = u32::try_from(self.pages.len()).expect("bounded native DWB frame");
        file.seek(SeekFrom::Start(dwb_zone_offset()))?;
        file.write_all(&reddb_file::encode_paged_dwb_header(count, checksum))?;
        file.sync_all()?;
        let dirty_pages = (reddb_file::PAGED_DWB_HEADER_SIZE
            + self.pages.len() * reddb_file::PAGED_DWB_ENTRY_SIZE)
            .div_ceil(PAGE_SIZE);
        apply_frame(
            file,
            self.pages.len(),
            u32::try_from(dirty_pages).expect("DWB prefix fits zone"),
        )
    }
}

fn checksum<F: VfsFile>(file: &mut F, count: usize) -> Result<u32, PagerIoError> {
    file.seek(SeekFrom::Start(entry_offset(0)))?;
    let mut left = count * reddb_file::PAGED_DWB_ENTRY_SIZE;
    let mut buffer = [0; 4096];
    let mut hash = crc32fast::Hasher::new();
    while left != 0 {
        let bytes = left.min(buffer.len());
        file.read_exact(&mut buffer[..bytes])?;
        hash.update(&buffer[..bytes]);
        left -= bytes;
    }
    Ok(hash.finalize())
}

fn read_entry<F: VfsFile>(file: &mut F, index: usize) -> Result<(u32, Page), PagerIoError> {
    file.seek(SeekFrom::Start(entry_offset(index)))?;
    let mut identity = [0; 4];
    file.read_exact(&mut identity)?;
    let mut bytes = [0; PAGE_SIZE];
    file.read_exact(&mut bytes)?;
    Ok((u32::from_le_bytes(identity), Page::from_bytes(bytes)))
}

fn validated_count<F: VfsFile>(file: &mut F) -> Result<Option<usize>, PagerIoError> {
    if file.file_len()? < dwb_zone_offset() + DWB_ZONE_BYTES as u64 {
        return Ok(None);
    }
    file.seek(SeekFrom::Start(dwb_zone_offset()))?;
    let mut header = [0; reddb_file::PAGED_DWB_HEADER_SIZE];
    file.read_exact(&mut header)?;
    let Ok((count, expected)) = reddb_file::decode_paged_dwb_header(&header) else {
        return Ok(None);
    };
    let count = count as usize;
    if count > DWB_MAX_PAGES_PER_FRAME || checksum(file, count)? != expected {
        return Ok(None);
    }
    Ok(Some(count))
}

pub(super) fn recover<F: VfsFile>(file: &mut F) -> Result<(), PagerIoError> {
    if file.file_len()? < dwb_zone_offset() + DWB_ZONE_BYTES as u64 {
        return Ok(());
    }
    let Some(count) = validated_count(file)? else {
        for id in DWB_ZONE_START_PAGE..FIRST_ALLOCATABLE_PAGE {
            let page = read_page(file, id, false)?;
            if page.page_id() != id
                || page.page_type().ok() != Some(PageType::Header)
                || page.verify_checksum().is_err()
            {
                return clear_dwb_zone(file);
            }
        }
        return Ok(());
    };
    apply_frame(file, count, DWB_ZONE_PAGES)
}

fn apply_frame<F: VfsFile>(
    file: &mut F,
    count: usize,
    clear_pages: u32,
) -> Result<(), PagerIoError> {
    for index in 0..count {
        let (id, page) = read_entry(file, index)?;
        write_page(file, id, &page)?;
    }
    file.sync_all()?;
    for id in DWB_ZONE_START_PAGE..DWB_ZONE_START_PAGE + clear_pages {
        let mut page = Page::new(PageType::Header, id);
        page.update_checksum();
        write_page(file, id, &page)?;
    }
    file.sync_all()?;
    Ok(())
}

pub(super) fn read_pages<F: VfsFile>(file: &mut F) -> Result<Vec<(u32, Page)>, PagerIoError> {
    let Some(count) = validated_count(file)? else {
        return Ok(Vec::new());
    };
    if count.saturating_mul(PAGE_SIZE) > file.max_read_buffer_bytes() {
        return Err(PagerIoError::InvalidDatabase(
            "double-write recovery exceeds the owned read budget".into(),
        ));
    }
    (0..count).map(|index| read_entry(file, index)).collect()
}
