//! Crash-safe derived COUNT projection components.
//!
//! A building component is never authoritative. Each bounded chunk is sealed
//! with a chained checksum and synced independently; only a synced completion
//! footer followed by an atomic rename makes the component queryable. A torn
//! building tail is truncated to the last sealed chunk on resume.

use super::secondary_index::SecondaryIndexIdentity;
use crc32fast::Hasher;
use fs2::FileExt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

const HEADER_MAGIC: &[u8; 8] = b"RDCNT001";
const CHUNK_MAGIC: &[u8; 4] = b"CHNK";
const FOOTER_MAGIC: &[u8; 4] = b"DONE";
const FORMAT_VERSION: u32 = 1;
const CHUNK_HEADER_BYTES: usize = 36;
const FOOTER_BYTES: usize = 44;
const RECORD_BYTES: usize = 25;
const MAX_HEADER_BYTES: usize = 4096;
const MAX_CHUNK_RECORDS: usize = 4096;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct CountProjectionBinding {
    pub(super) generation: [u8; 32],
    pub(super) source_root: u32,
    pub(super) identity: SecondaryIndexIdentity,
    pub(super) collection: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Footer {
    chunks: u64,
    scanned: u64,
    records: u64,
    last_source_id: u64,
    last_crc: u32,
}

#[derive(Clone)]
pub(super) struct CountProjectionReader {
    path: PathBuf,
    binding: CountProjectionBinding,
    header_len: u64,
    footer: Footer,
}

pub(super) enum CountProjectionOpen {
    Ready(CountProjectionReader),
    Building(CountProjectionWriter),
}

pub(super) struct CountProjectionWriter {
    file: File,
    building_path: PathBuf,
    final_path: PathBuf,
    binding: CountProjectionBinding,
    header_len: u64,
    chunks: u64,
    scanned: u64,
    records: u64,
    last_source_id: u64,
    last_crc: u32,
}

impl CountProjectionReader {
    pub(super) fn open(
        path: impl AsRef<Path>,
        expected: &CountProjectionBinding,
    ) -> io::Result<Self> {
        let path = path.as_ref();
        let mut file = File::open(path)?;
        let (binding, header_len) = read_header(&mut file)?;
        if &binding != expected {
            return Err(invalid(
                "COUNT projection belongs to another source generation",
            ));
        }
        let file_len = file.metadata()?.len();
        if file_len < header_len + FOOTER_BYTES as u64 {
            return Err(invalid("COUNT projection completion footer is missing"));
        }
        file.seek(SeekFrom::Start(file_len - FOOTER_BYTES as u64))?;
        let mut bytes = [0; FOOTER_BYTES];
        file.read_exact(&mut bytes)?;
        let footer = decode_footer(&bytes)?;
        Ok(Self {
            path: path.to_path_buf(),
            binding,
            header_len,
            footer,
        })
    }

    pub(super) fn path(&self) -> &Path {
        &self.path
    }

    pub(super) fn binding(&self) -> &CountProjectionBinding {
        &self.binding
    }

    /// Move an unchanged component to the catalog generation published by an
    /// unrelated checkpoint. The main catalog is already durable when this is
    /// called, so interruption can only discard this derived cache.
    pub(super) fn rebind(
        self,
        binding: CountProjectionBinding,
        final_path: PathBuf,
    ) -> io::Result<Self> {
        if self.binding.collection != binding.collection
            || self.binding.source_root != binding.source_root
            || self.binding.identity != binding.identity
        {
            return Err(invalid("cannot rebind COUNT projection to another source"));
        }
        if let Ok(reader) = Self::open(&final_path, &binding) {
            if self.path != final_path {
                let _ = fs::remove_file(&self.path);
            }
            return Ok(reader);
        }
        let header = encode_header(&binding)?;
        if header.len() as u64 != self.header_len {
            return Err(invalid(
                "COUNT projection header size changed during rebind",
            ));
        }
        let mut file = OpenOptions::new().read(true).write(true).open(&self.path)?;
        file.try_lock_exclusive()?;
        file.seek(SeekFrom::Start(0))?;
        file.write_all(&header)?;
        file.sync_all()?;
        drop(file);
        if self.path != final_path {
            if final_path.exists() {
                fs::remove_file(&final_path)?;
            }
            fs::rename(&self.path, &final_path)?;
            sync_parent(&final_path)?;
        }
        Self::open(final_path, &binding)
    }

    pub(super) fn try_for_each(
        &self,
        mut visit: impl FnMut(&[u8; RECORD_BYTES]) -> io::Result<()>,
    ) -> io::Result<()> {
        let mut file = File::open(&self.path)?;
        file.seek(SeekFrom::Start(self.header_len))?;
        let mut previous_crc = 0;
        let mut scanned = 0_u64;
        let mut records = 0_u64;
        let mut last_source_id = 0_u64;
        for ordinal in 0..self.footer.chunks {
            let (header, payload) = read_chunk(&mut file)?;
            if header.ordinal != ordinal || header.previous_crc != previous_crc {
                return Err(invalid("COUNT projection chunk chain is discontinuous"));
            }
            if ordinal != 0 && header.last_source_id <= last_source_id {
                return Err(invalid("COUNT projection source cursor moved backwards"));
            }
            previous_crc = header.crc;
            scanned = scanned
                .checked_add(u64::from(header.scanned))
                .ok_or_else(|| invalid("COUNT projection scan count overflow"))?;
            records = records
                .checked_add(u64::from(header.records))
                .ok_or_else(|| invalid("COUNT projection record count overflow"))?;
            last_source_id = header.last_source_id;
            for record in payload.as_chunks::<RECORD_BYTES>().0 {
                visit(record)?;
            }
        }
        let mut footer_bytes = [0; FOOTER_BYTES];
        file.read_exact(&mut footer_bytes)?;
        let footer = decode_footer(&footer_bytes)?;
        if footer != self.footer
            || footer.last_crc != previous_crc
            || footer.scanned != scanned
            || footer.records != records
            || footer.last_source_id != last_source_id
        {
            return Err(invalid("COUNT projection footer does not match its chunks"));
        }
        if file.stream_position()? != file.metadata()?.len() {
            return Err(invalid("COUNT projection contains bytes after its footer"));
        }
        Ok(())
    }
}

impl CountProjectionWriter {
    pub(super) fn open_or_create(
        building_path: PathBuf,
        final_path: PathBuf,
        binding: CountProjectionBinding,
    ) -> io::Result<CountProjectionOpen> {
        if final_path.exists() {
            match CountProjectionReader::open(&final_path, &binding) {
                Ok(reader) => return Ok(CountProjectionOpen::Ready(reader)),
                Err(_) => {
                    let _ = fs::remove_file(&final_path);
                }
            }
        }
        if building_path.exists() {
            match Self::resume(building_path.clone(), final_path.clone(), binding.clone()) {
                Ok(Resume::Building(writer)) => return Ok(CountProjectionOpen::Building(writer)),
                Ok(Resume::Complete(reader)) => return Ok(CountProjectionOpen::Ready(reader)),
                Err(_) => {
                    let _ = fs::remove_file(&building_path);
                }
            }
        }
        let mut file = OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(&building_path)?;
        file.try_lock_exclusive()?;
        let header = encode_header(&binding)?;
        file.write_all(&header)?;
        file.sync_all()?;
        sync_parent(&building_path)?;
        Ok(CountProjectionOpen::Building(Self {
            file,
            building_path,
            final_path,
            binding,
            header_len: header.len() as u64,
            chunks: 0,
            scanned: 0,
            records: 0,
            last_source_id: 0,
            last_crc: 0,
        }))
    }

    fn resume(
        building_path: PathBuf,
        final_path: PathBuf,
        expected: CountProjectionBinding,
    ) -> io::Result<Resume> {
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&building_path)?;
        file.try_lock_exclusive()?;
        let (binding, header_len) = read_header(&mut file)?;
        if binding != expected {
            return Err(invalid(
                "COUNT projection build belongs to another generation",
            ));
        }
        let mut writer = Self {
            file,
            building_path,
            final_path,
            binding,
            header_len,
            chunks: 0,
            scanned: 0,
            records: 0,
            last_source_id: 0,
            last_crc: 0,
        };
        let mut valid_len = header_len;
        writer.file.seek(SeekFrom::Start(header_len))?;
        loop {
            let position = writer.file.stream_position()?;
            let mut magic = [0; 4];
            match writer.file.read_exact(&mut magic) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => break,
                Err(error) => return Err(error),
            }
            writer.file.seek(SeekFrom::Start(position))?;
            if &magic == CHUNK_MAGIC {
                let Ok((chunk, _)) = read_chunk(&mut writer.file) else {
                    break;
                };
                if chunk.ordinal != writer.chunks
                    || chunk.previous_crc != writer.last_crc
                    || (writer.chunks != 0 && chunk.last_source_id <= writer.last_source_id)
                {
                    break;
                }
                writer.chunks += 1;
                writer.scanned = writer
                    .scanned
                    .checked_add(u64::from(chunk.scanned))
                    .ok_or_else(|| invalid("COUNT projection scan count overflow"))?;
                writer.records = writer
                    .records
                    .checked_add(u64::from(chunk.records))
                    .ok_or_else(|| invalid("COUNT projection record count overflow"))?;
                writer.last_source_id = chunk.last_source_id;
                writer.last_crc = chunk.crc;
                valid_len = writer.file.stream_position()?;
                continue;
            }
            if &magic == FOOTER_MAGIC {
                let mut bytes = [0; FOOTER_BYTES];
                if writer.file.read_exact(&mut bytes).is_err() {
                    break;
                }
                let Ok(footer) = decode_footer(&bytes) else {
                    break;
                };
                if footer
                    == (Footer {
                        chunks: writer.chunks,
                        scanned: writer.scanned,
                        records: writer.records,
                        last_source_id: writer.last_source_id,
                        last_crc: writer.last_crc,
                    })
                    && writer.file.stream_position()? == writer.file.metadata()?.len()
                {
                    drop(writer.file);
                    publish_component(&writer.building_path, &writer.final_path)?;
                    return Ok(Resume::Complete(CountProjectionReader::open(
                        &writer.final_path,
                        &writer.binding,
                    )?));
                }
            }
            break;
        }
        writer.file.set_len(valid_len)?;
        writer.file.seek(SeekFrom::Start(valid_len))?;
        writer.file.sync_all()?;
        Ok(Resume::Building(writer))
    }

    pub(super) fn last_source_id(&self) -> Option<u64> {
        (self.chunks != 0).then_some(self.last_source_id)
    }

    pub(super) fn discard(self) {
        let path = self.building_path.clone();
        drop(self.file);
        let _ = fs::remove_file(path);
    }

    #[cfg(test)]
    pub(super) fn record_count(&self) -> u64 {
        self.records
    }

    #[cfg(test)]
    pub(super) fn scanned_count(&self) -> u64 {
        self.scanned
    }

    pub(super) fn append(
        &mut self,
        scanned: usize,
        last_source_id: u64,
        records: &[[u8; RECORD_BYTES]],
    ) -> io::Result<()> {
        if scanned == 0 || records.len() > scanned || records.len() > MAX_CHUNK_RECORDS {
            return Err(invalid("invalid COUNT projection chunk bounds"));
        }
        if self.chunks != 0 && last_source_id <= self.last_source_id {
            return Err(invalid("COUNT projection source cursor did not advance"));
        }
        let scanned = u32::try_from(scanned)
            .map_err(|_| invalid("COUNT projection scan count exceeds u32"))?;
        let record_count = u32::try_from(records.len())
            .map_err(|_| invalid("COUNT projection record count exceeds u32"))?;
        let mut payload = Vec::with_capacity(records.len() * RECORD_BYTES);
        for record in records {
            payload.extend_from_slice(record);
        }
        let mut header = Vec::with_capacity(CHUNK_HEADER_BYTES);
        header.extend_from_slice(CHUNK_MAGIC);
        header.extend_from_slice(&self.chunks.to_le_bytes());
        header.extend_from_slice(&scanned.to_le_bytes());
        header.extend_from_slice(&record_count.to_le_bytes());
        header.extend_from_slice(&last_source_id.to_le_bytes());
        header.extend_from_slice(&self.last_crc.to_le_bytes());
        let mut checksum = Hasher::new();
        checksum.update(&header[4..]);
        checksum.update(&payload);
        let crc = checksum.finalize();
        header.extend_from_slice(&crc.to_le_bytes());
        debug_assert_eq!(header.len(), CHUNK_HEADER_BYTES);
        self.file.write_all(&header)?;
        self.file.write_all(&payload)?;
        #[cfg(test)]
        if std::env::var_os("REDDB_TEST_EXIT_DURING_COUNT_BATCH").is_some() {
            std::process::exit(183);
        }
        self.file.sync_data()?;
        self.chunks += 1;
        self.scanned += u64::from(scanned);
        self.records += u64::from(record_count);
        self.last_source_id = last_source_id;
        self.last_crc = crc;
        Ok(())
    }

    pub(super) fn finish(mut self) -> io::Result<CountProjectionReader> {
        let footer = Footer {
            chunks: self.chunks,
            scanned: self.scanned,
            records: self.records,
            last_source_id: self.last_source_id,
            last_crc: self.last_crc,
        };
        self.file.write_all(&encode_footer(footer))?;
        self.file.sync_all()?;
        drop(self.file);
        publish_component(&self.building_path, &self.final_path)?;
        CountProjectionReader::open(&self.final_path, &self.binding)
    }
}

enum Resume {
    Building(CountProjectionWriter),
    Complete(CountProjectionReader),
}

struct ChunkHeader {
    ordinal: u64,
    scanned: u32,
    records: u32,
    last_source_id: u64,
    previous_crc: u32,
    crc: u32,
}

fn read_chunk(file: &mut File) -> io::Result<(ChunkHeader, Vec<u8>)> {
    let mut bytes = [0; CHUNK_HEADER_BYTES];
    file.read_exact(&mut bytes)?;
    if &bytes[..4] != CHUNK_MAGIC {
        return Err(invalid("invalid COUNT projection chunk marker"));
    }
    let header = ChunkHeader {
        ordinal: u64::from_le_bytes(bytes[4..12].try_into().expect("chunk ordinal")),
        scanned: u32::from_le_bytes(bytes[12..16].try_into().expect("chunk scan count")),
        records: u32::from_le_bytes(bytes[16..20].try_into().expect("chunk record count")),
        last_source_id: u64::from_le_bytes(bytes[20..28].try_into().expect("chunk cursor")),
        previous_crc: u32::from_le_bytes(bytes[28..32].try_into().expect("chunk chain")),
        crc: u32::from_le_bytes(bytes[32..36].try_into().expect("chunk checksum")),
    };
    if header.scanned == 0
        || header.records > header.scanned
        || header.records as usize > MAX_CHUNK_RECORDS
    {
        return Err(invalid("invalid COUNT projection chunk dimensions"));
    }
    let payload_len = header.records as usize * RECORD_BYTES;
    let mut payload = vec![0; payload_len];
    file.read_exact(&mut payload)?;
    let mut checksum = Hasher::new();
    checksum.update(&bytes[4..32]);
    checksum.update(&payload);
    if checksum.finalize() != header.crc {
        return Err(invalid("COUNT projection chunk checksum mismatch"));
    }
    Ok((header, payload))
}

fn encode_header(binding: &CountProjectionBinding) -> io::Result<Vec<u8>> {
    let collection = binding.collection.as_bytes();
    if collection.is_empty() || collection.len() > 1024 {
        return Err(invalid("invalid COUNT projection collection name"));
    }
    let header_len = 96_usize
        .checked_add(collection.len())
        .ok_or_else(|| invalid("COUNT projection header length overflow"))?;
    let mut bytes = Vec::with_capacity(header_len);
    bytes.extend_from_slice(HEADER_MAGIC);
    bytes.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
    bytes.extend_from_slice(&(header_len as u32).to_le_bytes());
    bytes.extend_from_slice(&binding.generation);
    bytes.extend_from_slice(&binding.source_root.to_le_bytes());
    bytes.extend_from_slice(&binding.identity.codec_version.to_le_bytes());
    bytes.extend_from_slice(&binding.identity.definition);
    bytes.extend_from_slice(&(collection.len() as u32).to_le_bytes());
    bytes.extend_from_slice(collection);
    let crc = crc32fast::hash(&bytes);
    bytes.extend_from_slice(&crc.to_le_bytes());
    debug_assert_eq!(bytes.len(), header_len);
    Ok(bytes)
}

fn read_header(file: &mut File) -> io::Result<(CountProjectionBinding, u64)> {
    file.seek(SeekFrom::Start(0))?;
    let mut prefix = [0; 16];
    file.read_exact(&mut prefix)?;
    if &prefix[..8] != HEADER_MAGIC {
        return Err(invalid("invalid COUNT projection header marker"));
    }
    if u32::from_le_bytes(prefix[8..12].try_into().expect("header version")) != FORMAT_VERSION {
        return Err(invalid("unsupported COUNT projection format"));
    }
    let header_len = u32::from_le_bytes(prefix[12..16].try_into().expect("header length")) as usize;
    if !(96..=MAX_HEADER_BYTES).contains(&header_len) {
        return Err(invalid("invalid COUNT projection header length"));
    }
    let mut bytes = vec![0; header_len];
    bytes[..16].copy_from_slice(&prefix);
    file.read_exact(&mut bytes[16..])?;
    let stored_crc =
        u32::from_le_bytes(bytes[header_len - 4..].try_into().expect("header checksum"));
    if crc32fast::hash(&bytes[..header_len - 4]) != stored_crc {
        return Err(invalid("COUNT projection header checksum mismatch"));
    }
    let collection_len =
        u32::from_le_bytes(bytes[88..92].try_into().expect("collection length")) as usize;
    if collection_len == 0 || 92 + collection_len + 4 != header_len {
        return Err(invalid("invalid COUNT projection collection length"));
    }
    let collection = std::str::from_utf8(&bytes[92..92 + collection_len])
        .map_err(|_| invalid("COUNT projection collection is not UTF-8"))?
        .to_owned();
    Ok((
        CountProjectionBinding {
            generation: bytes[16..48].try_into().expect("generation digest"),
            source_root: u32::from_le_bytes(bytes[48..52].try_into().expect("source root")),
            identity: SecondaryIndexIdentity {
                codec_version: u32::from_le_bytes(bytes[52..56].try_into().expect("codec version")),
                definition: bytes[56..88].try_into().expect("definition digest"),
            },
            collection,
        },
        header_len as u64,
    ))
}

fn encode_footer(footer: Footer) -> [u8; FOOTER_BYTES] {
    let mut bytes = [0; FOOTER_BYTES];
    bytes[..4].copy_from_slice(FOOTER_MAGIC);
    bytes[4..12].copy_from_slice(&footer.chunks.to_le_bytes());
    bytes[12..20].copy_from_slice(&footer.scanned.to_le_bytes());
    bytes[20..28].copy_from_slice(&footer.records.to_le_bytes());
    bytes[28..36].copy_from_slice(&footer.last_source_id.to_le_bytes());
    bytes[36..40].copy_from_slice(&footer.last_crc.to_le_bytes());
    let crc = crc32fast::hash(&bytes[..40]);
    bytes[40..44].copy_from_slice(&crc.to_le_bytes());
    bytes
}

fn decode_footer(bytes: &[u8; FOOTER_BYTES]) -> io::Result<Footer> {
    if &bytes[..4] != FOOTER_MAGIC {
        return Err(invalid("invalid COUNT projection footer marker"));
    }
    let stored_crc = u32::from_le_bytes(bytes[40..44].try_into().expect("footer checksum"));
    if crc32fast::hash(&bytes[..40]) != stored_crc {
        return Err(invalid("COUNT projection footer checksum mismatch"));
    }
    Ok(Footer {
        chunks: u64::from_le_bytes(bytes[4..12].try_into().expect("footer chunks")),
        scanned: u64::from_le_bytes(bytes[12..20].try_into().expect("footer scanned")),
        records: u64::from_le_bytes(bytes[20..28].try_into().expect("footer records")),
        last_source_id: u64::from_le_bytes(bytes[28..36].try_into().expect("footer cursor")),
        last_crc: u32::from_le_bytes(bytes[36..40].try_into().expect("footer chain")),
    })
}

fn publish_component(building: &Path, final_path: &Path) -> io::Result<()> {
    match fs::rename(building, final_path) {
        Ok(()) => {}
        Err(error) if final_path.exists() => {
            let _ = fs::remove_file(building);
            if !final_path.exists() {
                return Err(error);
            }
        }
        Err(error) => return Err(error),
    }
    sync_parent(final_path)
}

#[cfg(unix)]
fn sync_parent(path: &Path) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        File::open(parent)?.sync_all()?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn sync_parent(_: &Path) -> io::Result<()> {
    // The component itself is synced before rename. If Windows loses the
    // directory update, the complete `.building` file is resumed on open.
    Ok(())
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}
