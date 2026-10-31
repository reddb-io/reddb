//! Allocation preflight for the canonical value grammar. The explicit stack
//! admits arrays before the existing typed decoder can recurse or allocate.
use super::{read_varint, type_for_tag, DataType, Value, ValueError};
use crate::utils::json::JSON_MAX_NESTING_DEPTH;

pub(super) fn preflight(data: &[u8], limit_bytes: usize) -> Result<usize, ValueError> {
    if data.is_empty() {
        return Err(ValueError::EmptyData);
    }
    let mut cursor = Cursor {
        data,
        position: 0,
        used_bytes: 0,
        limit_bytes,
    };
    cursor.charge(data.len())?;
    cursor.charge(std::mem::size_of::<Value>())?;
    let mut remaining = [0_usize; JSON_MAX_NESTING_DEPTH as usize + 1];
    remaining[0] = 1;
    let mut depth = 1;
    while depth > 0 {
        if remaining[depth - 1] == 0 {
            depth -= 1;
            continue;
        }
        remaining[depth - 1] -= 1;
        let tag = cursor.take(1)?[0];
        let kind = type_for_tag(tag).ok_or(ValueError::InvalidType(tag))?;
        match kind {
            DataType::Array => {
                if depth > JSON_MAX_NESTING_DEPTH as usize {
                    return Err(ValueError::NestingLimitExceeded {
                        maximum: JSON_MAX_NESTING_DEPTH,
                    });
                }
                let count = cursor.length()?;
                // Every child has at least one tag byte. Reject impossible
                // counts before the typed decoder reserves a Vec capacity.
                if count > data.len() - cursor.position {
                    return Err(ValueError::TruncatedData);
                }
                cursor.charge_product(count, std::mem::size_of::<Value>())?;
                if count > 0 {
                    remaining[depth] = count;
                    depth += 1;
                }
            }
            DataType::Text
            | DataType::Email
            | DataType::Url
            | DataType::DecimalText
            | DataType::AssetCode
            | DataType::TableRef
            | DataType::Password => cursor.string(2)?,
            DataType::Blob | DataType::Json | DataType::Secret => cursor.string(1)?,
            DataType::NodeRef | DataType::EdgeRef => cursor.string(6)?,
            DataType::VectorRef | DataType::RowRef => {
                cursor.string(6)?;
                cursor.take(8)?;
            }
            DataType::DocRef => {
                cursor.string(2)?;
                cursor.take(8)?;
            }
            DataType::KeyRef => {
                cursor.string(2)?;
                cursor.string(2)?;
            }
            DataType::Money => {
                cursor.string(2)?;
                cursor.take(9)?;
            }
            DataType::Vector => {
                let count = cursor.length()?;
                let bytes = count
                    .checked_mul(std::mem::size_of::<f32>())
                    .ok_or(ValueError::VarintOverflow)?;
                cursor.take(bytes)?;
                cursor.charge(bytes)?;
            }
            DataType::IpAddr => {
                let version = cursor.take(1)?[0];
                cursor.take(match version {
                    4 => 4,
                    6 => 16,
                    other => return Err(ValueError::InvalidIpVersion(other)),
                })?;
            }
            DataType::TextZstd | DataType::BlobZstd => {
                let original = cursor.length()?;
                let encoded = cursor.length()?;
                let frame = cursor.take(encoded)?;
                let content = zstd::zstd_safe::get_frame_content_size(frame)
                    .map_err(|_| ValueError::TruncatedData)?;
                if content.is_some_and(|size| usize::try_from(size).ok() != Some(original)) {
                    return Err(ValueError::TruncatedData);
                }
                // Text expands into a Vec/String and then an Arc<str>; the
                // old allocation remains alive while the Arc copy is made.
                cursor.charge_product(original, if kind == DataType::TextZstd { 2 } else { 1 })?;
                cursor.charge(64)?;
            }
            DataType::Nullable | DataType::Unknown => {}
            fixed => {
                let bytes = fixed.fixed_size().ok_or(ValueError::InvalidType(tag))?;
                cursor.take(bytes)?;
            }
        }
    }
    Ok(cursor.position)
}

struct Cursor<'a> {
    data: &'a [u8],
    position: usize,
    used_bytes: usize,
    limit_bytes: usize,
}

impl<'a> Cursor<'a> {
    fn take(&mut self, bytes: usize) -> Result<&'a [u8], ValueError> {
        let end = self
            .position
            .checked_add(bytes)
            .ok_or(ValueError::TruncatedData)?;
        let value = self
            .data
            .get(self.position..end)
            .ok_or(ValueError::TruncatedData)?;
        self.position = end;
        Ok(value)
    }

    fn length(&mut self) -> Result<usize, ValueError> {
        let data = &self.data[self.position..];
        let (length, bytes) = read_varint(data)?;
        if bytes == 10 && data[9] > 1 {
            return Err(ValueError::VarintOverflow);
        }
        self.position += bytes;
        usize::try_from(length).map_err(|_| ValueError::VarintOverflow)
    }

    fn string(&mut self, copies: usize) -> Result<(), ValueError> {
        let length = self.length()?;
        self.take(length)?;
        // Covers Arc headers and temporary UTF-8/lossy copies as well as the
        // retained payload. The typed decoder owns validation of UTF-8 bytes.
        self.charge_product(length, copies)?;
        self.charge(64)
    }

    fn charge_product(&mut self, count: usize, size_bytes: usize) -> Result<(), ValueError> {
        let bytes = count
            .checked_mul(size_bytes)
            .ok_or(ValueError::ReadLimitExceeded {
                required_bytes: usize::MAX,
                limit_bytes: self.limit_bytes,
            })?;
        self.charge(bytes)
    }

    fn charge(&mut self, bytes: usize) -> Result<(), ValueError> {
        let required_bytes =
            self.used_bytes
                .checked_add(bytes)
                .ok_or(ValueError::ReadLimitExceeded {
                    required_bytes: usize::MAX,
                    limit_bytes: self.limit_bytes,
                })?;
        if required_bytes > self.limit_bytes {
            return Err(ValueError::ReadLimitExceeded {
                required_bytes,
                limit_bytes: self.limit_bytes,
            });
        }
        self.used_bytes = required_bytes;
        Ok(())
    }
}
