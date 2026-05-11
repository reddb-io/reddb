use crate::crypto::os_random;
use std::fmt;

#[derive(Clone, Copy, Eq, PartialEq, Hash)]
pub struct Uuid([u8; 16]);

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct UuidParseError;

impl Uuid {
    pub fn new_v4() -> Self {
        let mut bytes = [0u8; 16];
        os_random::fill_bytes(&mut bytes).expect("OS CSPRNG unavailable");

        bytes[6] = (bytes[6] & 0x0f) | 0x40;
        bytes[8] = (bytes[8] & 0x3f) | 0x80;

        Self(bytes)
    }

    /// UUID v7: time-sortable. Bytes 0-5 = 48-bit Unix ms timestamp,
    /// byte 6 = version 7 nibble + 4 rand bits, byte 8 = variant 10xx,
    /// rest random. Monotonic within the same millisecond is not
    /// guaranteed — callers that need strict monotonicity should add
    /// a sequence counter.
    pub fn new_v7() -> Self {
        let ms = crate::utils::now_unix_millis();
        let mut rand = [0u8; 10];
        os_random::fill_bytes(&mut rand).expect("OS CSPRNG unavailable");

        let mut bytes = [0u8; 16];
        bytes[0] = ((ms >> 40) & 0xFF) as u8;
        bytes[1] = ((ms >> 32) & 0xFF) as u8;
        bytes[2] = ((ms >> 24) & 0xFF) as u8;
        bytes[3] = ((ms >> 16) & 0xFF) as u8;
        bytes[4] = ((ms >> 8) & 0xFF) as u8;
        bytes[5] = (ms & 0xFF) as u8;
        bytes[6] = (rand[0] & 0x0F) | 0x70;
        bytes[7] = rand[1];
        bytes[8] = (rand[2] & 0x3F) | 0x80;
        bytes[9..16].copy_from_slice(&rand[3..10]);

        Self(bytes)
    }

    /// Parse a hyphenated UUID string (xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx).
    pub fn parse_str(s: &str) -> Result<Self, UuidParseError> {
        let normalized: String = s.chars().filter(|&c| c != '-').collect();
        if normalized.len() != 32 {
            return Err(UuidParseError);
        }
        let mut bytes = [0u8; 16];
        for i in 0..16 {
            bytes[i] = u8::from_str_radix(&normalized[i * 2..i * 2 + 2], 16)
                .map_err(|_| UuidParseError)?;
        }
        Ok(Self(bytes))
    }

    pub fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

impl fmt::Display for Uuid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let b = self.0;
        write!(
            f,
            "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7], b[8], b[9], b[10], b[11],
            b[12], b[13], b[14], b[15]
        )
    }
}

impl fmt::Debug for Uuid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}
