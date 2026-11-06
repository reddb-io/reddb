//! Resumable validation of version-one append-only segment frames.
//!
//! Rows delivered to the callback are provisional until `complete` is true:
//! the frame and all checksums are verified only at the end of the stream.

use std::io;

use zstd::stream::raw::{Decoder, Operation};
use zstd::zstd_safe::DParameter;

use super::{
    AppendOnlySegmentChunkChecksum, AppendOnlySegmentCodec, AppendOnlySegmentRow, RowJson,
    SegmentHeader, APPEND_ONLY_SEGMENT_CHUNK_BYTES, APPEND_ONLY_SEGMENT_VERSION, FIXED_HEADER_LEN,
    MAGIC,
};
use crate::VfsFile;

const IO_CHUNK: usize = 64 * 1024;
const MAX_HEADER_BYTES: usize = 16 * 1024 * 1024;

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

fn json_whitespace(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\r' | b'\n')
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct SegmentStep {
    pub file_bytes: usize,
    pub plain_bytes: usize,
    pub rows: usize,
    pub complete: bool,
}

struct Checksums {
    expected: Vec<AppendOnlySegmentChunkChecksum>,
    index: usize,
    offset: u64,
    used: usize,
    hash: crc32fast::Hasher,
}

impl Checksums {
    fn new(expected: Vec<AppendOnlySegmentChunkChecksum>) -> Self {
        Self {
            expected,
            index: 0,
            offset: 0,
            used: 0,
            hash: crc32fast::Hasher::new(),
        }
    }

    fn update(&mut self, mut bytes: &[u8]) -> io::Result<()> {
        while !bytes.is_empty() {
            let chunk = self
                .expected
                .get(self.index)
                .ok_or_else(|| invalid("missing segment chunk checksum"))?;
            let length = chunk.len as usize;
            if chunk.offset != self.offset
                || length == 0
                || length > APPEND_ONLY_SEGMENT_CHUNK_BYTES as usize
                || self.used >= length
            {
                return Err(invalid("invalid segment chunk layout"));
            }
            let take = bytes.len().min(length - self.used);
            self.hash.update(&bytes[..take]);
            self.used += take;
            bytes = &bytes[take..];
            if self.used == length {
                let hash = std::mem::replace(&mut self.hash, crc32fast::Hasher::new()).finalize();
                if chunk.checksum != format!("crc32:{hash:08x}") {
                    return Err(invalid("segment chunk checksum mismatch"));
                }
                self.offset = self
                    .offset
                    .checked_add(length as u64)
                    .ok_or_else(|| invalid("segment size overflow"))?;
                self.index += 1;
                self.used = 0;
            }
        }
        Ok(())
    }

    fn finish(&self) -> io::Result<()> {
        if self.index != self.expected.len() || self.used != 0 {
            return Err(invalid("incomplete segment chunk checksums"));
        }
        Ok(())
    }
}

#[derive(Clone, Copy)]
enum JsonState {
    Start,
    First,
    Next,
    Row,
    Delimiter,
    Done,
}

struct RowParser {
    state: JsonState,
    row: Vec<u8>,
    depth: usize,
    in_string: bool,
    escaped: bool,
    max_row_bytes: usize,
}

impl RowParser {
    fn new(max_row_bytes: usize) -> Self {
        Self {
            state: JsonState::Start,
            row: Vec::new(),
            depth: 0,
            in_string: false,
            escaped: false,
            max_row_bytes,
        }
    }

    fn consume(
        &mut self,
        bytes: &[u8],
        rows_left: &mut usize,
        visit: &mut impl FnMut(AppendOnlySegmentRow) -> io::Result<()>,
    ) -> io::Result<usize> {
        let mut consumed = 0;
        for &byte in bytes {
            if *rows_left == 0 {
                break;
            }
            consumed += 1;
            match self.state {
                JsonState::Start if json_whitespace(byte) => {}
                JsonState::Start if byte == b'[' => self.state = JsonState::First,
                JsonState::First | JsonState::Next if json_whitespace(byte) => {}
                JsonState::First if byte == b']' => self.state = JsonState::Done,
                JsonState::First | JsonState::Next if byte == b'{' => {
                    self.row.clear();
                    self.row.push(byte);
                    self.depth = 1;
                    self.in_string = false;
                    self.escaped = false;
                    self.state = JsonState::Row;
                }
                JsonState::Row => {
                    if self.row.len() >= self.max_row_bytes {
                        return Err(invalid("segment row exceeds validation memory limit"));
                    }
                    self.row.push(byte);
                    if self.in_string {
                        if self.escaped {
                            self.escaped = false;
                        } else if byte == b'\\' {
                            self.escaped = true;
                        } else if byte == b'"' {
                            self.in_string = false;
                        }
                    } else {
                        match byte {
                            b'"' => self.in_string = true,
                            b'{' | b'[' => {
                                self.depth = self
                                    .depth
                                    .checked_add(1)
                                    .ok_or_else(|| invalid("segment JSON nesting overflow"))?
                            }
                            b'}' | b']' => {
                                self.depth = self
                                    .depth
                                    .checked_sub(1)
                                    .ok_or_else(|| invalid("invalid segment JSON nesting"))?;
                                if self.depth == 0 {
                                    let row: RowJson = serde_json::from_slice(&self.row)
                                        .map_err(|error| invalid(error.to_string()))?;
                                    visit(AppendOnlySegmentRow {
                                        primary_key: hex::decode(row.primary_key_hex)
                                            .map_err(|error| invalid(error.to_string()))?,
                                        payload: hex::decode(row.payload_hex)
                                            .map_err(|error| invalid(error.to_string()))?,
                                    })?;
                                    *rows_left -= 1;
                                    self.state = JsonState::Delimiter;
                                }
                            }
                            _ => {}
                        }
                    }
                }
                JsonState::Delimiter if json_whitespace(byte) => {}
                JsonState::Delimiter if byte == b',' => self.state = JsonState::Next,
                JsonState::Delimiter if byte == b']' => self.state = JsonState::Done,
                JsonState::Done if json_whitespace(byte) => {}
                _ => return Err(invalid("invalid append-only row array")),
            }
        }
        Ok(consumed)
    }

    fn finished(&self) -> bool {
        matches!(self.state, JsonState::Done)
    }
}

/// A single immutable segment, read and decompressed in bounded increments.
pub struct StreamingAppendOnlySegment<F: VfsFile> {
    file: F,
    file_len: u64,
    whole: Checksums,
    fixed: Vec<u8>,
    header_bytes: Vec<u8>,
    header_len: Option<usize>,
    expected_body_len: u64,
    expected_header_crc: u32,
    expected_body_crc: u32,
    header: Option<SegmentHeader>,
    body_checksums: Option<Checksums>,
    body_crc: crc32fast::Hasher,
    body_read: u64,
    input: Vec<u8>,
    input_pos: usize,
    output: Vec<u8>,
    output_pos: usize,
    decoder: Option<Decoder<'static>>,
    frame_finished: bool,
    plain_seen: u64,
    rows_seen: u64,
    parser: RowParser,
    complete: bool,
}

impl<F: VfsFile> StreamingAppendOnlySegment<F> {
    pub fn new(
        file: F,
        checksums: Vec<AppendOnlySegmentChunkChecksum>,
        max_row_bytes: usize,
    ) -> io::Result<Self> {
        if max_row_bytes == 0 {
            return Err(invalid("segment row limit must be positive"));
        }
        let file_len = file.file_len()?;
        Ok(Self {
            file,
            file_len,
            whole: Checksums::new(checksums),
            fixed: Vec::with_capacity(FIXED_HEADER_LEN),
            header_bytes: Vec::new(),
            header_len: None,
            expected_body_len: 0,
            expected_header_crc: 0,
            expected_body_crc: 0,
            header: None,
            body_checksums: None,
            body_crc: crc32fast::Hasher::new(),
            body_read: 0,
            input: Vec::new(),
            input_pos: 0,
            output: Vec::new(),
            output_pos: 0,
            decoder: None,
            frame_finished: false,
            plain_seen: 0,
            rows_seen: 0,
            parser: RowParser::new(max_row_bytes),
            complete: false,
        })
    }

    /// Metadata is trusted only after the entire frame has been verified.
    pub fn verified_metadata(&self) -> Option<(AppendOnlySegmentCodec, u64)> {
        self.complete.then(|| {
            let header = self.header.as_ref().expect("completed stream has a header");
            (header.codec, header.row_count)
        })
    }

    fn read_chunk(&mut self, limit: usize) -> io::Result<Vec<u8>> {
        let mut bytes = vec![0; limit.min(IO_CHUNK)];
        let read = loop {
            match self.file.read(&mut bytes) {
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                result => break result?,
            }
        };
        if read == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        bytes.truncate(read);
        self.whole.update(&bytes)?;
        Ok(bytes)
    }

    fn parse_fixed(&mut self) -> io::Result<()> {
        if &self.fixed[..8] != MAGIC {
            return Err(invalid("invalid append-only segment magic"));
        }
        let header_len = u32::from_le_bytes(self.fixed[8..12].try_into().unwrap()) as usize;
        if header_len > MAX_HEADER_BYTES {
            return Err(invalid("segment header exceeds validation memory limit"));
        }
        let body_len = u64::from_le_bytes(self.fixed[12..20].try_into().unwrap());
        let expected_len = (FIXED_HEADER_LEN as u64)
            .checked_add(header_len as u64)
            .and_then(|len| len.checked_add(body_len))
            .ok_or_else(|| invalid("segment frame size overflow"))?;
        if expected_len != self.file_len {
            return Err(invalid("append-only segment frame length mismatch"));
        }
        self.expected_header_crc = u32::from_le_bytes(self.fixed[20..24].try_into().unwrap());
        self.expected_body_crc = u32::from_le_bytes(self.fixed[24..28].try_into().unwrap());
        self.expected_body_len = body_len;
        self.header_len = Some(header_len);
        Ok(())
    }

    fn parse_header(&mut self) -> io::Result<()> {
        if crc32fast::hash(&self.header_bytes) != self.expected_header_crc {
            return Err(invalid("segment header checksum mismatch"));
        }
        let mut header: SegmentHeader = serde_json::from_slice(&self.header_bytes)
            .map_err(|error| invalid(error.to_string()))?;
        if header.version != APPEND_ONLY_SEGMENT_VERSION
            || header.chunk_size != APPEND_ONLY_SEGMENT_CHUNK_BYTES
        {
            return Err(invalid("unsupported append-only segment header"));
        }
        self.body_checksums = Some(Checksums::new(std::mem::take(&mut header.chunk_checksums)));
        if header.codec == AppendOnlySegmentCodec::Zstd {
            let mut decoder = Decoder::new()?;
            decoder.set_parameter(DParameter::WindowLogMax(23))?;
            self.decoder = Some(decoder);
        }
        self.header = Some(header);
        self.header_bytes = Vec::new();
        Ok(())
    }

    /// Read at most `file_budget` bytes and process at most `plain_budget`
    /// decompressed bytes and `row_budget` rows during this call.
    pub fn step(
        &mut self,
        file_budget: usize,
        plain_budget: usize,
        row_budget: usize,
        mut visit: impl FnMut(AppendOnlySegmentRow) -> io::Result<()>,
    ) -> io::Result<SegmentStep> {
        let mut progress = SegmentStep::default();
        if self.complete {
            progress.complete = true;
            return Ok(progress);
        }
        if file_budget == 0 || plain_budget == 0 || row_budget == 0 {
            return Ok(progress);
        }
        let mut rows_left = row_budget;
        loop {
            if self.fixed.len() < FIXED_HEADER_LEN {
                if progress.file_bytes == file_budget {
                    break;
                }
                let amount =
                    (FIXED_HEADER_LEN - self.fixed.len()).min(file_budget - progress.file_bytes);
                let bytes = self.read_chunk(amount)?;
                progress.file_bytes += bytes.len();
                self.fixed.extend_from_slice(&bytes);
                if self.fixed.len() == FIXED_HEADER_LEN {
                    self.parse_fixed()?;
                }
                continue;
            }
            if self.header.is_none() {
                let len = self.header_len.expect("fixed header parsed");
                if self.header_bytes.len() < len {
                    if progress.file_bytes == file_budget {
                        break;
                    }
                    let amount =
                        (len - self.header_bytes.len()).min(file_budget - progress.file_bytes);
                    let bytes = self.read_chunk(amount)?;
                    progress.file_bytes += bytes.len();
                    self.header_bytes.extend_from_slice(&bytes);
                    continue;
                }
                self.parse_header()?;
                continue;
            }
            if self.output_pos < self.output.len() {
                if progress.plain_bytes == plain_budget || rows_left == 0 {
                    break;
                }
                let end = self
                    .output
                    .len()
                    .min(self.output_pos + (plain_budget - progress.plain_bytes));
                let before = rows_left;
                let consumed = self.parser.consume(
                    &self.output[self.output_pos..end],
                    &mut rows_left,
                    &mut visit,
                )?;
                self.output_pos += consumed;
                progress.plain_bytes += consumed;
                self.plain_seen = self
                    .plain_seen
                    .checked_add(consumed as u64)
                    .ok_or_else(|| invalid("segment plain length overflow"))?;
                let completed = before - rows_left;
                progress.rows += completed;
                self.rows_seen = self
                    .rows_seen
                    .checked_add(completed as u64)
                    .ok_or_else(|| invalid("segment row count overflow"))?;
                if self.plain_seen
                    > self
                        .header
                        .as_ref()
                        .expect("header parsed")
                        .uncompressed_len
                {
                    return Err(invalid("segment exceeds declared plain length"));
                }
                if consumed == 0 {
                    break;
                }
                continue;
            }
            if rows_left == 0 || progress.plain_bytes == plain_budget {
                break;
            }
            if self.input_pos == self.input.len() && self.body_read < self.expected_body_len {
                if progress.file_bytes == file_budget {
                    break;
                }
                let left =
                    usize::try_from((self.expected_body_len - self.body_read).min(IO_CHUNK as u64))
                        .unwrap();
                let bytes = self.read_chunk(left.min(file_budget - progress.file_bytes))?;
                progress.file_bytes += bytes.len();
                self.body_read += bytes.len() as u64;
                self.body_crc.update(&bytes);
                self.body_checksums
                    .as_mut()
                    .expect("header parsed")
                    .update(&bytes)?;
                self.input = bytes;
                self.input_pos = 0;
            }
            if self.frame_finished {
                if self.input_pos != self.input.len() || self.body_read != self.expected_body_len {
                    return Err(invalid("trailing compressed segment data"));
                }
                self.finish()?;
                progress.complete = true;
                break;
            }
            let output_limit = (plain_budget - progress.plain_bytes).min(IO_CHUNK);
            self.output.clear();
            self.output.resize(output_limit, 0);
            self.output_pos = 0;
            match self.header.as_ref().expect("header parsed").codec {
                AppendOnlySegmentCodec::None => {
                    let count = output_limit.min(self.input.len() - self.input_pos);
                    self.output[..count]
                        .copy_from_slice(&self.input[self.input_pos..self.input_pos + count]);
                    self.input_pos += count;
                    self.output.truncate(count);
                    self.frame_finished = self.body_read == self.expected_body_len
                        && self.input_pos == self.input.len();
                }
                AppendOnlySegmentCodec::Zstd => {
                    let status = self
                        .decoder
                        .as_mut()
                        .expect("decoder initialized")
                        .run_on_buffers(&self.input[self.input_pos..], &mut self.output)?;
                    self.input_pos += status.bytes_read;
                    self.output.truncate(status.bytes_written);
                    self.frame_finished = status.remaining == 0;
                    if status.bytes_read == 0 && status.bytes_written == 0 && !self.frame_finished {
                        if self.body_read == self.expected_body_len {
                            return Err(invalid("truncated compressed segment"));
                        }
                        if self.input_pos != self.input.len() {
                            return Err(invalid("stalled segment decompressor"));
                        }
                    }
                }
            }
        }
        Ok(progress)
    }

    fn finish(&mut self) -> io::Result<()> {
        let header = self.header.as_ref().expect("header parsed");
        if self.file.file_len()? != self.file_len
            || self.plain_seen != header.uncompressed_len
            || self.rows_seen != header.row_count
            || !self.parser.finished()
        {
            return Err(invalid("append-only segment content mismatch"));
        }
        if self.body_crc.clone().finalize() != self.expected_body_crc {
            return Err(invalid("segment body checksum mismatch"));
        }
        self.whole.finish()?;
        self.body_checksums
            .as_ref()
            .expect("header parsed")
            .finish()?;
        self.complete = true;
        Ok(())
    }
}
