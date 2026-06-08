//! Causal bookmark token contract.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CausalBookmark {
    term: u64,
    commit_lsn: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BookmarkDecodeError {
    InvalidPrefix,
    InvalidLength,
    InvalidHex,
}

impl std::fmt::Display for BookmarkDecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidPrefix => write!(f, "invalid causal bookmark prefix"),
            Self::InvalidLength => write!(f, "invalid causal bookmark length"),
            Self::InvalidHex => write!(f, "invalid causal bookmark hex payload"),
        }
    }
}

impl std::error::Error for BookmarkDecodeError {}

impl CausalBookmark {
    pub fn new(term: u64, commit_lsn: u64) -> Self {
        Self { term, commit_lsn }
    }

    pub fn term(self) -> u64 {
        self.term
    }

    pub fn commit_lsn(self) -> u64 {
        self.commit_lsn
    }

    pub fn encode(self) -> String {
        format!("rbm1.{:016x}{:016x}", self.term, self.commit_lsn)
    }

    pub fn decode(token: &str) -> Result<Self, BookmarkDecodeError> {
        let Some(payload) = token.strip_prefix("rbm1.") else {
            return Err(BookmarkDecodeError::InvalidPrefix);
        };
        if payload.len() != 32 {
            return Err(BookmarkDecodeError::InvalidLength);
        }
        let term =
            u64::from_str_radix(&payload[..16], 16).map_err(|_| BookmarkDecodeError::InvalidHex)?;
        let commit_lsn =
            u64::from_str_radix(&payload[16..], 16).map_err(|_| BookmarkDecodeError::InvalidHex)?;
        Ok(Self { term, commit_lsn })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn causal_bookmark_round_trips_stable_wire_token() {
        let bookmark = CausalBookmark::new(0x12, 0x345);
        let token = bookmark.encode();
        assert_eq!(token, "rbm1.00000000000000120000000000000345");
        assert_eq!(CausalBookmark::decode(&token).unwrap(), bookmark);
    }

    #[test]
    fn causal_bookmark_rejects_bad_tokens() {
        assert_eq!(
            CausalBookmark::decode("bad.00000000000000010000000000000002").unwrap_err(),
            BookmarkDecodeError::InvalidPrefix
        );
        assert_eq!(
            CausalBookmark::decode("rbm1.1").unwrap_err(),
            BookmarkDecodeError::InvalidLength
        );
        assert_eq!(
            CausalBookmark::decode("rbm1.000000000000000x0000000000000002").unwrap_err(),
            BookmarkDecodeError::InvalidHex
        );
    }
}
